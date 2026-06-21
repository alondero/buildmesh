//! Agent Node service — creation, deletion, and lifecycle orchestration

use crate::db;
use crate::env;
use crate::git::worktree::{self, WorktreeCloseSafety};
use crate::models::{AgentNode, PendingWorktreeRemoval, SessionStatus};

/// Error type for agent node service operations
#[derive(Debug)]
pub enum AgentNodeError {
    Db(rusqlite::Error),
}

impl std::fmt::Display for AgentNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentNodeError::Db(e) => write!(f, "{}", e),
        }
    }
}

impl From<rusqlite::Error> for AgentNodeError {
    fn from(e: rusqlite::Error) -> Self {
        AgentNodeError::Db(e)
    }
}

/// Create a new agent node with auto-generated name, environment detection,
/// and provider resolution.
///
/// Pass `None` for `source_pr` / `source_pr_pinned_sha` /
/// `head_repo_owner` / `head_repo_clone_url` on non-PR spawns (issue-spawn,
/// handover, hand-spawn). Fork PRs call [`create_with_source_pr_fork`]
/// directly with the fork fields populated (issue #443). `source_pr_pinned_sha`
/// (issue #444) is the exact-pinning handle — `None` skips the drift check.
///
/// `source_pr` and `source_pr_pinned_sha` are **structurally** rejected here
/// (asserted at function entry) so a future caller that mistakenly passes
/// `Some(_)` for either — an importer, a resume-by-URL path, a migration
/// script — fails loudly at the boundary rather than silently writing a
/// `source_pr` onto a node that didn't actually come from a PR. The PR-spawn
/// entry point is [`create_with_source_pr_fork`], which is the only function
/// that should ever pass `Some(_)` for these fields. Pinning test lives in
/// `services::agent_node::tests` (issue #448).
#[allow(clippy::too_many_arguments)]
pub fn create(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    assert!(
        source_pr.is_none(),
        "services::agent_node::create refuses source_pr=Some(_); \
         the non-PR wrappers must persist source_pr=None so spawn_agent_inner's \
         'is this a PR-spawned node?' branch (node.source_pr.is_some()) stays a \
         reliable signal. Use create_with_source_pr_fork for PR spawns."
    );
    assert!(
        source_pr_pinned_sha.is_none(),
        "services::agent_node::create refuses source_pr_pinned_sha=Some(_); \
         use create_with_source_pr_fork for PR spawns that need SHA pinning."
    );
    // Single insert path; fork fields are None for the non-fork entry point.
    create_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree_override,
        name_override,
        None,
        None,
    )
}

/// Like [`create`], but also records the fork's owner login and clone URL
/// when the PR is from a fork (issue #443). `spawn_agent_inner` reads
/// these to register `fork-<owner>` as a remote and fetch the head ref.
/// For same-repo PRs, pass `None` for both fork fields and call [`create`]
/// instead — the spawn path then takes the #420 `origin/<head_ref>` branch.
#[allow(clippy::too_many_arguments)]
pub fn create_with_source_pr_fork(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    let mesh = db::get_mesh_by_id(mesh_id)?;
    let use_worktree = use_worktree_override.unwrap_or(mesh.use_worktree);

    // Caller may supply a pre-derived name (e.g. `slugify_issue_title` for the
    // GitHub-issue spawn path). Validation/fallback is the caller's job — by
    // the time we get here, the name is assumed to be a valid `SLUG_REGEX`
    // match, which is also a safe directory name.
    let session_name = match name_override {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => crate::session_naming::on_spawn(),
    };
    tracing::debug!(
        "agent_node::create: mesh_id={}, name={}, path={}, branch={}, provider={:?}, use_worktree={}, source_pr={:?}, source_pr_pinned_sha={:?}, head_repo_owner={:?}",
        mesh_id, session_name, path, branch, provider, use_worktree, source_pr, source_pr_pinned_sha, head_repo_owner
    );

    let worktree_db_name = if use_worktree {
        Some(session_name.as_str())
    } else {
        None
    };

    let resolved = env::resolve_agent_path(path, worktree_db_name);
    let env_type = resolved.env_type;
    // Store the harness/profile id verbatim (issue #535) — no premature parse
    // to the legacy `Provider` enum, which would flatten an unknown profile id
    // to Anthropic. Resolution happens at the spawn seam. An absent provider
    // defaults to "anthropic", matching the prior `Provider::Anthropic` default.
    let provider_id = provider.unwrap_or("anthropic");

    let node = db::create_agent_node(
        mesh_id,
        &session_name,
        path,
        branch,
        env_type,
        provider_id,
        worktree_db_name,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree,
        head_repo_owner,
        head_repo_clone_url,
    )?;

    Ok(node)
}

/// Like `create`, but sets the initial status to [`SessionStatus::Pending`]
/// so the frontend can distinguish "node row exists, stage-2 not yet
/// started" from "node row exists, agent is idle and ready to re-spawn".
///
/// This is the fast stage-1 of the two-stage issue-spawn flow. The caller
/// is expected to invoke `start_node_background` to do the slow work
/// (git fetch, worktree create, PTY spawn) and update the status to
/// `Running` on success or `Error` on failure. The source-pr / fork-repo
/// fields follow the same contract as [`create`]: `source_pr` and
/// `source_pr_pinned_sha` must be `None` here; the PR-spawn entry point
/// is [`create_pending_with_source_pr_fork`] (issue #448).
#[allow(clippy::too_many_arguments)]
pub fn create_pending(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    assert!(
        source_pr.is_none(),
        "services::agent_node::create_pending refuses source_pr=Some(_); \
         use create_pending_with_source_pr_fork for PR spawns."
    );
    assert!(
        source_pr_pinned_sha.is_none(),
        "services::agent_node::create_pending refuses source_pr_pinned_sha=Some(_); \
         use create_pending_with_source_pr_fork for PR spawns that need SHA pinning."
    );
    // Single insert path; fork fields are None for the non-fork entry point.
    create_pending_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        name_override,
        None,
        None,
    )
}

/// Like [`create_pending`], but also records the fork's owner login and
/// clone URL when the PR is from a fork (issue #443) and the PR's head
/// commit SHA for exact-pinning (issue #444). `commands::agent::create_pr_node`
/// calls this directly with the fork fields populated for fork PRs and
/// `None, None` for same-repo PRs.
#[allow(clippy::too_many_arguments)]
pub fn create_pending_with_source_pr_fork(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    let mut node = create_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        None,
        name_override,
        head_repo_owner,
        head_repo_clone_url,
    )?;
    // Two writes (insert + status update) is one extra ~1ms SQLite round
    // trip. Acceptable: this function is on the fast path, and the second
    // write is the whole point — without it the new node would look
    // identical to a freshly-closed, ready-to-resume idle node.
    db::update_agent_node_status(node.id, SessionStatus::Pending)?;
    node.status = SessionStatus::Pending;
    Ok(node)
}

/// Return whether closing the node can remove its worktree without risking work.
pub fn get_worktree_close_safety(session_id: i64) -> Result<WorktreeCloseSafety, AgentNodeError> {
    let node = db::get_agent_node_by_id(session_id)?;
    let Some(worktree_path) = env::node_worktree_path(&node).map(|r| r.host_path) else {
        return Ok(WorktreeCloseSafety {
            worktree_path: None,
            has_uncommitted: false,
            has_unpushed: false,
            is_detached: false,
        });
    };

    Ok(worktree::close_safety(&worktree_path))
}

/// Close an agent node (Phase 1 — fast and authoritative).
///
/// This kills the agent process tree and removes the node from the database
/// immediately, but deliberately does **not** delete the worktree directory:
/// that recursive delete is slow (a `node_modules`-heavy tree is tens of
/// thousands of files) and retry-prone on Windows, so blocking the close on it
/// is what makes the UI feel frozen. Instead, when a worktree should be removed
/// we record it in the durable `pending_worktree_removals` queue — atomically
/// with deleting the row — and let `process_pending_removals` reclaim the disk
/// in the background or on next launch. The kill stays synchronous here: it's
/// fast, and it's the one step we must finish before parting with the node so we
/// never leave a live agent running or fighting the eventual removal (#239).
pub fn delete(session_id: i64, remove_worktree: bool) -> Result<(), AgentNodeError> {
    let node = db::get_agent_node_by_id(session_id)?;

    let removal_path = if remove_worktree {
        crate::agent::process::PROCESS_REGISTRY.kill_session(session_id);
        crate::agent::process::PROCESS_REGISTRY.remove(&session_id);
        env::node_worktree_path(&node).map(|r| r.host_path)
    } else {
        None
    };

    crate::session_naming::cleanup(session_id);

    let removal = removal_path.as_deref().map(|p| (p, node.name.as_str()));
    db::delete_agent_node_enqueueing_removal(session_id, removal)?;
    Ok(())
}

/// Serializes drains. Each drain processes the *whole* queue, so two running at
/// once (e.g. closing two nodes in quick succession, or a close overlapping the
/// startup reconcile) would both try to remove the same just-enqueued worktree
/// and race on its `.removing` staging directory. Holding this for the drain's
/// duration means a second drain waits, then re-lists and finds the first
/// already dequeued — no wasted work, no spurious cleanup-failed warnings.
static DRAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Drain the pending worktree-removal queue: remove each queued worktree's
/// directory, dequeuing only the ones that succeed. Removals that still fail
/// (e.g. a handle is somehow held) stay queued for the next drain or next
/// launch, and are returned so the caller can warn the user. Safe to run from
/// the close path and from startup reconcile — `remove_one_worktree` is
/// idempotent (a missing directory counts as already removed).
pub fn process_pending_removals() -> Vec<(PendingWorktreeRemoval, String)> {
    let _guard = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let pending = match db::list_pending_worktree_removals() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("could not read pending worktree removals: {}", e);
            return Vec::new();
        }
    };

    process_removals(
        pending,
        crate::git::worktree::remove_one_worktree_and_branch,
        db::delete_pending_worktree_removal,
    )
}

/// Pure orchestration for the drain: for each removal, attempt the directory
/// remove; on success dequeue it; on failure keep it queued and collect it.
/// Dependencies are injected so the "dequeue only on success" invariant — the
/// thing that guarantees failed cleanups get retried rather than lost — is
/// testable without the global DB or a real filesystem.
fn process_removals(
    pending: Vec<PendingWorktreeRemoval>,
    remove: impl Fn(&str) -> Result<(), String>,
    dequeue: impl Fn(&str) -> db::SqlResult<()>,
) -> Vec<(PendingWorktreeRemoval, String)> {
    let mut failures = Vec::new();
    for removal in pending {
        match remove(&removal.worktree_path) {
            Ok(()) => {
                if let Err(e) = dequeue(&removal.worktree_path) {
                    tracing::error!(
                        "removed worktree {} but failed to dequeue it: {}",
                        removal.worktree_path, e
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "worktree removal for {} failed, will retry: {}",
                    removal.node_name, e
                );
                failures.push((removal, e));
            }
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let tmp =
                std::env::temp_dir().join(format!("buildmesh_agent_node_test_{}_{}", pid, id));
            Self(tmp)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sig() -> git2::Signature<'static> {
        git2::Signature::now("test", "test@example.com").unwrap()
    }

    fn init_repo(path: &Path) -> git2::Repository {
        fs::create_dir_all(path).unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(path, &opts).unwrap();

        fs::write(path.join("file.txt"), "initial").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let s = sig();
        repo.commit(Some("HEAD"), &s, &s, "initial commit", &tree, &[])
            .unwrap();
        drop(tree);

        repo
    }

    fn add_worktree(root_repo: &git2::Repository, root: &TempDir, name: &str) -> PathBuf {
        let head = root_repo.head().unwrap().peel_to_commit().unwrap();
        let branch = root_repo.branch(name, &head, false).unwrap();
        let reference = branch.into_reference();
        let worktree_path = root.path().join(".claude").join("worktrees").join(name);
        fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
        root_repo
            .worktree(
                name,
                &worktree_path,
                Some(git2::WorktreeAddOptions::new().reference(Some(&reference))),
            )
            .unwrap();
        worktree_path
    }

    #[test]
    fn drain_removes_real_worktree_and_dequeues_only_on_success() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let good = add_worktree(&root_repo, &root, "wt-good");

        let pending = vec![
            PendingWorktreeRemoval {
                worktree_path: good.to_string_lossy().to_string(),
                node_name: "wt-good".to_string(),
            },
            PendingWorktreeRemoval {
                worktree_path: "/nonexistent/wt-bad".to_string(),
                node_name: "wt-bad".to_string(),
            },
        ];

        let dequeued = std::cell::RefCell::new(Vec::<String>::new());
        let failures = process_removals(
            pending,
            |path| {
                // A missing parent repo can't be opened → genuine failure.
                if path.starts_with("/nonexistent") {
                    return Err("not a removable worktree".to_string());
                }
                crate::git::worktree::remove_one_worktree(path)
            },
            |path| {
                dequeued.borrow_mut().push(path.to_string());
                Ok(())
            },
        );

        assert!(!good.exists(), "the real worktree directory must be removed");
        assert_eq!(
            dequeued.into_inner(),
            vec![good.to_string_lossy().to_string()],
            "only the successful removal is dequeued"
        );
        assert_eq!(failures.len(), 1, "the failed removal is returned for warning");
        assert_eq!(failures[0].0.node_name, "wt-bad");
    }

    // -------------------------------------------------------------------
    // Issue #448 — pin the `source_pr = None` invariant on the issue-spawn
    // and hand-spawn paths. `services::agent_node::create` and
    // `create_pending` are the only entry points non-PR spawns go through
    // (issue-spawn, handover, generic mobile spawn, Tauri hand-spawn);
    // `spawn_agent_inner` uses `node.source_pr.is_some()` to decide whether
    // to take the worktree-adoption path. A future caller — resume-by-URL
    // (#37), an importer, a migration script — could silently write a
    // `source_pr` onto a node that didn't actually come from a PR, and the
    // user would see a confusing "could not fetch PR head ref" warning on
    // a node spawned from a regular issue.
    //
    // The wrappers now refuse `Some(_)` for `source_pr` and
    // `source_pr_pinned_sha` at function entry (PR-spawn fields). The PR-spawn
    // entry points (`create_with_source_pr_fork` /
    // `create_pending_with_source_pr_fork`) remain the only functions that
    // can write a non-`None` `source_pr` — those tests live with the PR-spawn
    // callers in `commands::agent_tests`.
    //
    // Two complementary tests cover the invariant:
    //
    //   * Positive (happy-path) tests exercise `create` / `create_pending`
    //     with `source_pr = None` and assert the persisted row reads back
    //     `source_pr = None`. These need a real DB row, so they call
    //     `ensure_db_init()` which lazily inits the global DB.
    //
    //   * Negative `#[should_panic]` tests call the wrappers with
    //     `source_pr = Some(_)`. The assertion fires before any DB call, so
    //     these don't need a DB at all — they catch a regression at the
    //     wrapper boundary with zero infrastructure.
    // -------------------------------------------------------------------

    use std::sync::Once;

    /// Lazily initialise the global DB exactly once per test process. The
    /// underlying `db::init` uses a process-global `OnceCell`, so calling
    /// it twice is an error; this guard makes the second-and-later call a
    /// no-op even if another test in the binary (e.g. `db::mesh_tests`)
    /// already won the race. The temp path is process-unique so a sibling
    /// test running with `--test-threads=1` and this one never collide on
    /// the SQLite file.
    fn ensure_db_init() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let temp_path = std::env::temp_dir().join(format!(
                "buildmesh_agent_node_invariant_{}.db",
                std::process::id()
            ));
            // `db::init` returns `Err` if another test already set the
            // global DB (e.g. `db::mesh_tests` running first). That's fine
            // for our tests — they only read the global DB and create their
            // own mesh rows with a unique path.
            let _ = crate::db::init(&temp_path);
        });
    }

    /// Create a fresh mesh in the global DB at a unique per-test path and
    /// return its id. Each call uses a monotonic counter so parallel tests
    /// can't collide on the `meshes.path` UNIQUE constraint.
    fn fresh_mesh() -> i64 {
        ensure_db_init();
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let path = format!("/tmp/buildmesh_invariant_test_{}", id);
        crate::db::create_mesh(&format!("invariant-{}", id), &path)
            .expect("fresh_mesh: create_mesh should succeed")
            .id
    }

    #[test]
    fn create_returns_node_with_source_pr_none() {
        // The wrapper contract: passing `source_pr = None` for an
        // issue-spawn / hand-spawn call persists `source_pr = None`.
        let mesh_id = fresh_mesh();

        let node = create(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create with source_pr=None must succeed");

        assert_eq!(
            node.source_pr, None,
            "issue-spawn wrapper must persist source_pr=None"
        );
        assert_eq!(
            node.source_pr_pinned_sha, None,
            "issue-spawn wrapper must persist source_pr_pinned_sha=None"
        );
        assert_eq!(
            node.source_issue, None,
            "hand-spawn wrapper must persist source_issue=None when not supplied"
        );
    }

    #[test]
    fn create_pending_returns_node_with_source_pr_none_and_pending_status() {
        // `create_pending` is the fast stage-1 of the two-stage issue-spawn
        // flow — it must also persist `source_pr = None` so stage-2's
        // `node.source_pr.is_some()` branch doesn't accidentally fire.
        let mesh_id = fresh_mesh();

        let node = create_pending(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
        )
        .expect("create_pending with source_pr=None must succeed");

        assert_eq!(
            node.source_pr, None,
            "create_pending must persist source_pr=None"
        );
        assert_eq!(
            node.source_pr_pinned_sha, None,
            "create_pending must persist source_pr_pinned_sha=None"
        );
        assert_eq!(
            node.status,
            SessionStatus::Pending,
            "create_pending must leave the row in Pending so the frontend \
             can distinguish stage-1-done from idle-and-ready-to-resume"
        );
    }

    #[test]
    fn create_with_source_issue_set_keeps_source_pr_independent() {
        // Optional step 4 from the issue: when `source_issue = Some(N)`,
        // `source_pr` must still come back as `None` — the two source
        // fields are independent columns on the row.
        let mesh_id = fresh_mesh();

        let node = create(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            Some(123),
            None,
            None,
            None,
            Some("gh123-test-slug"),
        )
        .expect("create with source_issue=Some(N), source_pr=None must succeed");

        assert_eq!(
            node.source_issue,
            Some(123),
            "source_issue must round-trip independently of source_pr"
        );
        assert_eq!(
            node.source_pr, None,
            "source_pr must remain None even when source_issue is set"
        );
        assert_eq!(node.source_pr_pinned_sha, None);
    }

    #[test]
    #[should_panic(expected = "source_pr=Some(_)")]
    fn create_rejects_source_pr_some() {
        // A future caller — resume-by-URL, importer, migration script —
        // tries to set `source_pr` on a node that didn't actually come
        // from a PR. The wrapper must refuse at the boundary rather than
        // silently persist it. The assertion fires before any DB call, so
        // we don't even need `ensure_db_init()` here — a missing DB is the
        // strongest possible failure signal for this regression.
        let _ = create(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            Some(42),
            None,
            None,
            None,
        );
    }

    #[test]
    #[should_panic(expected = "source_pr=Some(_)")]
    fn create_pending_rejects_source_pr_some() {
        let _ = create_pending(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            Some(42),
            None,
            None,
        );
    }

    #[test]
    #[should_panic(expected = "source_pr_pinned_sha=Some(_)")]
    fn create_rejects_source_pr_pinned_sha_some() {
        // Same boundary for the SHA-pinning field (#444) — a caller that
        // tries to pin the head SHA on a non-PR spawn gets the same
        // refused-at-entry treatment.
        let _ = create(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            None,
            Some("deadbeef"),
            None,
            None,
        );
    }
}
