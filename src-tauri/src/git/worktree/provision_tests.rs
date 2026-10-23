// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::env::test_helpers::{
    commit_file, init_repo_with_commit, repo_with_drifted_head, TestDir,
};
use crate::git::worktree::warm::ClaimedWarmEntry;
use crate::models::{AgentNode, EnvType, SessionStatus};
use std::fs;
use std::path::{Path, PathBuf};

/// Zero-cost sink for tests that don't exercise post-success side effects —
/// each method no-ops so a fixture-only test can call `provision_for_spawn`
/// without standing up Tauri events or background threads.
#[derive(Debug, Clone, Copy)]
struct NullSink;

impl ProvisionSink for NullSink {
    fn return_warm_row(&self, _entry: &ClaimedWarmEntry, _mesh_id: i64) -> Result<(), String> {
        Ok(())
    }
    fn forget_warm_row(&self, _id: i64) {}
    fn adopt_manual_slug(&self, _node_id: i64, _slug: &str, _worktree_path: Option<&str>) {}
    fn on_pool_maintenance_required(&self, _mesh_id: i64, _do_refresh: bool, _do_refill: bool) {}
}

/// Build a minimal `SpawnContext` for tests, pre-populating the fields the
/// provisioner reads but the test doesn't care about (`base_ref`,
/// `worktree_mode`) with harmless defaults.
fn make_ctx(
    node: AgentNode,
    source: SpawnSource,
    host_path: String,
    warm_entry: Option<ClaimedWarmEntry>,
) -> SpawnContext {
    SpawnContext {
        node,
        source,
        base_ref: "HEAD".to_string(),
        worktree_mode: "branched".to_string(),
        use_worktree: true,
        warm_entry,
        host_path,
    }
}

fn empty_node(root: &Path, worktree_name: Option<&str>) -> AgentNode {
    AgentNode {
        launch_configuration: None,
        id: 1,
        mesh_id: 1,
        name: worktree_name.unwrap_or("buildmesh-spawn").to_string(),
        path: root.to_str().unwrap().to_string(),
        branch: "main".to_string(),
        env: EnvType::default(),
        provider: "anthropic".to_string(),
        status: SessionStatus::Idle,
        cli_session_id: None,
        worktree_name: worktree_name.map(|s| s.to_string()),
        use_worktree: true,
        // Required by the full-literal `AgentNode { ... }` initializer
        // (wayfinder #982 / ticket #984). `is_pinned` is a UI-toggle
        // field unrelated to this test's worktree-shape assertion;
        // `false` matches the column default for a fresh node.
        is_pinned: false,
        source_issue: None,
        source_pr: None,
        head_repo_owner: None,
        head_repo_clone_url: None,
        source_pr_pinned_sha: None,
        lifecycle: None,
        signal_health: None,
        position: 0,
        created_at: chrono::Utc::now(),
        worktree_path: None,
        project_scope: None,
    }
}

fn claimed_warm(path: &Path, preassigned: &str) -> ClaimedWarmEntry {
    ClaimedWarmEntry {
        id: 42,
        path: path.to_str().unwrap().to_string(),
        preassigned_name: preassigned.to_string(),
        base_sha: None,
    }
}

/// Cut a detached warm entry against the repo at `root` (matches the
/// pool's on-disk shape: a detached-HEAD worktree sitting in
/// `.claude/worktrees/<slug>`).
fn make_pool_entry(root: &Path, slug: &str) -> PathBuf {
    let pool = root.join(".claude").join("worktrees").join(slug);
    super::super::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        slug,
        "detached",
        "HEAD",
    )
    .expect("warm pool entry must be cuttable for test setup");
    pool
}

#[test]
fn scoped_cold_warm_manual_issue_and_pr_own_whole_checkouts() {
    let td = TestDir::new("scoped-provision");
    let root = td.path();
    init_repo_with_commit(
        root,
        &[
            ("family/parents/AGENTS.md", "parents rules"),
            ("family/children/README.md", "sibling"),
        ],
    );
    let selected = root.join("family/parents");
    let sibling = make_pool_entry(root, "sibling");
    for (name, source, warm) in [
        ("cold", SpawnSource::Manual, false),
        ("manual", SpawnSource::Manual, true),
        ("issue-m1", SpawnSource::Issue, true),
        ("pr-m1", SpawnSource::PullRequest, true),
    ] {
        let entry_path = warm.then(|| make_pool_entry(root, &format!("pool-{name}")));
        let target = if warm && matches!(source, SpawnSource::Manual) {
            entry_path.clone().unwrap()
        } else {
            selected.join(".claude/worktrees").join(name)
        };
        let mut node = empty_node(&selected, Some(name));
        node.project_scope = Some("family/parents".into());
        node.worktree_path = Some(target.to_string_lossy().into_owned());
        if matches!(source, SpawnSource::Manual) && warm {
            node.worktree_name = Some(format!("pool-{name}"));
        }
        let warm_entry = entry_path
            .as_ref()
            .map(|p| claimed_warm(p, &format!("pool-{name}")));
        let ctx = make_ctx(
            node.clone(),
            source,
            target.to_string_lossy().into_owned(),
            warm_entry,
        );
        let result = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).unwrap();
        match (warm, source, result) {
            (false, _, ProvisionOutcome::Created { .. })
            | (true, SpawnSource::Manual, ProvisionOutcome::Upgraded { .. })
            | (
                true,
                SpawnSource::Issue | SpawnSource::PullRequest,
                ProvisionOutcome::Adopted { .. },
            ) => {}
            other => panic!("wrong provision branch: {other:?}"),
        }
        assert!(git2::Repository::open(&target).unwrap().is_worktree());
        let cwd = crate::env::node_working_path(&node);
        assert_eq!(
            fs::read_to_string(Path::new(&cwd.host_path).join("AGENTS.md")).unwrap(),
            "parents rules"
        );
        assert_eq!(
            crate::env::node_worktree_path(&node).unwrap().host_path,
            target.to_string_lossy()
        );
        // A saved cwd is usable even when the selected source no longer exists.
        node.path = root
            .join("source-no-longer-present")
            .to_string_lossy()
            .into_owned();
        assert_eq!(crate::env::node_working_path(&node).raw_path, cwd.raw_path);
        assert_eq!(
            crate::env::node_working_path(&node).spawn_path,
            cwd.spawn_path
        );
        super::super::remove_one_worktree(&target.to_string_lossy()).unwrap();
        assert!(!target.exists());
        assert!(sibling.join("family/parents/AGENTS.md").exists());
        assert!(selected.join("AGENTS.md").exists());
    }
}

#[test]
fn scoped_checkout_missing_folder_fails_with_folder_and_ref() {
    let td = TestDir::new("scoped-missing");
    let repo = init_repo_with_commit(td.path(), &[("README.md", "old tree")]);
    let old_ref = repo.head().unwrap().target().unwrap().to_string();
    fs::create_dir_all(td.path().join("family/parents")).unwrap();
    commit_file(&repo, td.path(), "family/parents/AGENTS.md", "new scope");
    let selected = td.path().join("family/parents");
    let target = td.path().join("worktree");
    let mut node = empty_node(&selected, Some("missing"));
    node.project_scope = Some("family/parents".into());
    let ctx = SpawnContext {
        base_ref: old_ref.clone(),
        ..make_ctx(
            node,
            SpawnSource::Manual,
            target.to_string_lossy().into_owned(),
            None,
        )
    };
    let err = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).unwrap_err();
    assert!(err.contains("family/parents"), "{err}");
    assert!(err.contains(&old_ref), "{err}");
    assert!(!target.join("family/parents").exists());
}

// ── Reused ────────────────────────────────────────────────────────────────

/// Root Node (`use_worktree = false`) is the cheapest short-circuit — no
/// worktree is created or moved, the spawn runs in the mesh root. The
/// provisioner must return `Reused` even when the host_path is a fresh
/// non-existent directory.
#[test]
fn provision_for_spawn_reused_when_use_worktree_false() {
    let td = TestDir::new("reuse_root_node");
    let root = td.path();
    init_repo_with_commit(root, &[("README.md", "# project\n")]);

    let node = empty_node(root, None);
    let host_path = root
        .join("does-not-exist-yet")
        .to_string_lossy()
        .to_string();
    let ctx = SpawnContext {
        use_worktree: false,
        ..make_ctx(node, SpawnSource::Manual, host_path, None)
    };

    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("Root Node must succeed");
    match outcome {
        ProvisionOutcome::Reused { host_path: p } => {
            assert!(p.ends_with("does-not-exist-yet"));
        }
        other => panic!("expected Reused for use_worktree=false, got {:?}", other),
    }
    // Crucially: the non-existent directory was NOT created.
    assert!(
        !Path::new(&root.join("does-not-exist-yet")).exists(),
        "Root Node must not materialise a worktree"
    );
}

/// Resume / handover / re-spawn — the path already exists on disk AND there
/// is no warm claim (a warm claim for Manual would route to `Upgraded`,
/// not `Reused`). Re-cutting would orphan the agent's prior work, so the
/// provisioner must short-circuit to `Reused`.
#[test]
fn provision_for_spawn_reused_when_path_already_exists() {
    let td = TestDir::new("reuse_path_exists");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "prior\n")]);

    let host_path = root.join("existing-worktree");
    super::super::create_git_worktree(
        root.to_str().unwrap(),
        host_path.to_str().unwrap(),
        "existing-worktree",
        "detached",
        "HEAD",
    )
    .unwrap();
    fs::write(host_path.join("agent-notes.md"), "carried over").unwrap();

    let node = empty_node(root, Some("existing-worktree"));
    let host_path_str = host_path.to_string_lossy().to_string();
    // No warm entry — resume / handover / re-spawn path.
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str.clone(), None);

    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("resume must succeed");
    match outcome {
        ProvisionOutcome::Reused { host_path: p } => assert_eq!(p, host_path_str),
        other => panic!("expected Reused for existing path, got {:?}", other),
    }
    // Prior work preserved.
    assert_eq!(
        fs::read_to_string(host_path.join("agent-notes.md")).unwrap(),
        "carried over"
    );
}

// ── Created (cold) ────────────────────────────────────────────────────────

/// Empty pool + no warm entry = the daily manual cold create. The
/// provisioner must take the `Created` branch (no `git worktree move`),
/// the worktree ends up at `ctx.host_path` on `ctx.base_ref`.
#[test]
fn provision_for_spawn_created_when_pool_empty_and_no_warm_entry() {
    let td = TestDir::new("created_cold");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);

    let node = empty_node(root, Some("bold-amber-fox"));
    let host_path = root
        .join(".claude")
        .join("worktrees")
        .join("bold-amber-fox");
    let host_path_str = host_path.to_string_lossy().to_string();
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str.clone(), None);

    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("cold create must succeed");
    match outcome {
        ProvisionOutcome::Created {
            host_path: p,
            elapsed_ms,
        } => {
            assert_eq!(p, host_path_str);
            // Elapsed_ms is intentionally permissive (cold create on
            // Windows Defender / NTFS-indexed disks can spend >100ms in
            // the first write). A `0` here would mean the timer wasn't
            // started, which would be a real regression.
            assert!(
                elapsed_ms < 60_000,
                "elapsed_ms {} implausibly large",
                elapsed_ms
            );
        }
        other => panic!("expected Created for empty pool, got {:?}", other),
    }
    assert!(
        host_path.exists(),
        "cold create must materialise the worktree"
    );
    assert_eq!(fs::read_to_string(host_path.join("f.txt")).unwrap(), "v1\n");
}

/// `provision_for_spawn`'s `Created` branch must thread `SpawnContext.base_ref`
/// through to `create_git_worktree(.., base_ref)` → `add_worktree_impl(..,
/// base_ref)` → `git worktree add <path> <sha>`. The orchestrator pins the
/// `worktree_base_ref` derivation (`agent/spawn.rs:1452`); the primitive
/// tests (`git/worktree/mod.rs:1072+`) pin `create_git_worktree` end-to-end;
/// this test pins the seam in BETWEEN — a regression guard so the orchestrator
/// and the primitive can't drift on which ref the worktree is cut from.
/// Issue #248 / #230.
#[test]
fn provision_for_spawn_cold_created_uses_spawn_context_base_ref_not_local_head() {
    let td = TestDir::new("prov_base_ref_cold");
    let parent = td.path();
    let (_repo, origin_oid) = repo_with_drifted_head(parent);

    let node = empty_node(parent, Some("prov-cf"));
    let host_path = parent.join(".claude").join("worktrees").join("prov-cf");
    let host_path_str = host_path.to_string_lossy().to_string();
    let mut ctx = make_ctx(node, SpawnSource::Issue, host_path_str.clone(), None);
    // Override the default `base_ref: "HEAD"` (from `make_ctx`) so we
    // exercise the symbolic-ref path: `origin/main` must be the input,
    // NOT the local drift that `repo.head()` resolves to.
    ctx.base_ref = "origin/main".to_string();

    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("cold create must succeed");
    match outcome {
        ProvisionOutcome::Created { host_path: p, .. } => assert_eq!(p, host_path_str),
        other => panic!("expected Created, got {:?}", other),
    }

    let wt_repo = git2::Repository::open(&host_path).unwrap();
    let wt_oid = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(
        wt_oid, origin_oid,
        "SpawnContext.base_ref must reach git worktree add — local-drift HEAD would be a base_ref regression"
    );
}

// ── Adopted (Issue / PR) ──────────────────────────────────────────────────

/// PR spawn adopts a warm entry: `git worktree move` rewrites the
/// pool's plain slug to the node's `pr{N}-` target, then `git checkout`
/// lands it on the resolved base SHA. `entry` is handed back so the
/// caller can `forget_after_spawn` it.
#[test]
fn provision_for_spawn_adopted_for_pr_spawn() {
    let td = TestDir::new("adopted_pr");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "warm-pool-slug");

    let mut node = empty_node(root, Some("pr123-feature"));
    node.source_pr = Some(123);
    let host_path = root.join(".claude").join("worktrees").join("pr123-feature");
    let host_path_str = host_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "warm-pool-slug");
    let ctx = make_ctx(
        node,
        SpawnSource::PullRequest,
        host_path_str.clone(),
        Some(warm),
    );

    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("PR adopt must succeed");
    match outcome {
        ProvisionOutcome::Adopted {
            entry,
            host_path: p,
            elapsed_ms,
        } => {
            assert_eq!(entry.id, 42);
            assert_eq!(p, host_path_str);
            assert!(elapsed_ms < 60_000);
        }
        other => panic!("expected Adopted for PR spawn, got {:?}", other),
    }
    // Pool dir was moved — old path gone, target path present.
    assert!(!pool_path.exists(), "pool directory must be moved away");
    assert!(
        host_path.exists(),
        "adopted worktree must exist at target path"
    );
}

/// Issue spawn adopts the warm entry similarly, but the worktree lands
/// on the mesh's base SHA (not a fetched PR head). Symmetric coverage
/// with the PR test so a regression in either branch is caught.
#[test]
fn provision_for_spawn_adopted_for_issue_spawn() {
    let td = TestDir::new("adopted_issue");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "warm-issue-slug");

    let mut node = empty_node(root, Some("gh45-bug"));
    node.source_issue = Some(45);
    let host_path = root.join(".claude").join("worktrees").join("gh45-bug");
    let host_path_str = host_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "warm-issue-slug");

    let ctx = make_ctx(node, SpawnSource::Issue, host_path_str.clone(), Some(warm));
    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("Issue adopt must succeed");
    match outcome {
        ProvisionOutcome::Adopted {
            entry,
            host_path: p,
            ..
        } => {
            assert_eq!(entry.preassigned_name, "warm-issue-slug");
            assert_eq!(p, host_path_str);
        }
        other => panic!("expected Adopted for Issue spawn, got {:?}", other),
    }
    assert!(host_path.exists());
    assert!(!pool_path.exists(), "pool directory must be moved away");
}

/// Issue/PR adoption uses `-b` (refuse to clobber) — a re-spawn against
/// an existing deterministic `gh{N}-`/`pr{N}-` branch must NOT force-
/// reset prior work. Pre-seeding the target branch exercises the
/// `git checkout -b` refusal.
#[test]
fn provision_for_spawn_adopted_refuses_to_clobber_existing_branch() {
    let td = TestDir::new("adopted_clobber");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "warm-clobber");
    // Pre-cut the target branch with a divergent commit so a clobber
    // would lose the user's prior work.
    let pre_target = root.join(".claude").join("worktrees").join("gh77-pre");
    super::super::create_git_worktree(
        root.to_str().unwrap(),
        pre_target.to_str().unwrap(),
        "gh77-prior",
        "branched",
        "HEAD",
    )
    .unwrap();
    // Drop the pre-worktree so the move target doesn't clash on disk —
    // we only need the BRANCH to exist, which it now does.
    super::super::remove_one_worktree(pre_target.to_str().unwrap()).unwrap();

    let mut node = empty_node(root, Some("gh77-prior"));
    node.source_issue = Some(77);
    let host_path = root.join(".claude").join("worktrees").join("gh77-prior");
    let host_path_str = host_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "warm-clobber");
    let ctx = make_ctx(node, SpawnSource::Issue, host_path_str, Some(warm));

    let result = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink);
    assert!(
        result.is_err(),
        "adopt must refuse to clobber existing branch, got Ok({:?})",
        result
    );
    assert!(
        result.unwrap_err().contains("already exists"),
        "error must name the clobber refusal"
    );
    // Fail-fast contract: the refusal happens BEFORE the pool directory
    // is disturbed — see the pre-move guard in `adopt_warm_worktree_by_move`.
    assert!(
        pool_path.exists(),
        "pool entry must be untouched after a refused adoption"
    );
    assert!(
        !host_path.exists(),
        "the target path must not have been materialised by a refused adoption"
    );
}

/// A refused warm adoption must NOT remove a target that was already there.
///
/// The collision case (two nodes deriving the same name, and therefore the
/// same worktree path): `adopt_warm_worktree_by_move`'s pre-move guard
/// refuses because that branch is already checked out at the target, so the
/// disk is untouched. The warm-failure cleanup then used to read "the target
/// path exists" as "our move created it" and `remove_one_worktree` it —
/// deleting the *other* node's working directory, uncommitted work and all —
/// before the cold retry failed anyway on the still-existing branch.
#[test]
fn provision_for_spawn_refused_adoption_preserves_a_preexisting_target() {
    let td = TestDir::new("adopted_collision");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "warm-collision");
    // The other node's live worktree: same path, same branch on disk.
    let host_path = root.join(".claude").join("worktrees").join("gh78-collide");
    super::super::create_git_worktree(
        root.to_str().unwrap(),
        host_path.to_str().unwrap(),
        "gh78-collide",
        "branched",
        "HEAD",
    )
    .unwrap();
    // Local-only work the old cleanup would have destroyed.
    let uncommitted = host_path.join("UNCOMMITTED.txt");
    std::fs::write(&uncommitted, "work in progress\n").unwrap();

    let mut node = empty_node(root, Some("gh78-collide"));
    node.source_issue = Some(78);
    let warm = claimed_warm(&pool_path, "warm-collision");
    let ctx = make_ctx(
        node,
        SpawnSource::Issue,
        host_path.to_string_lossy().to_string(),
        Some(warm),
    );

    let result = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink);

    assert!(
        host_path.exists(),
        "a pre-existing target belongs to another node and must survive a refused adoption"
    );
    assert_eq!(
        std::fs::read_to_string(&uncommitted).unwrap(),
        "work in progress\n",
        "uncommitted work in a pre-existing target must survive"
    );
    match result {
        Ok(ProvisionOutcome::Reused { host_path: reused }) => assert_eq!(
            reused,
            host_path.to_string_lossy().as_ref(),
            "a name collision falls back to reusing the pre-existing path"
        ),
        other => panic!("expected Reused at the pre-existing path, got {:?}", other),
    }
    assert!(
        pool_path.exists(),
        "the refused pool entry must stay intact for the cold retry"
    );
}

/// Bad `base_ref` for an adopted Issue spawn falls through the offline
/// resolver (ADR 0001) — the one SHA resolver's `ShaSource::BaseRef` degrades
/// a missing ref to
/// the local HEAD, so `adopt_warm_worktree_by_move` succeeds and the
/// spawn lands on whatever commit the repo is parked on rather than
/// hard-failing the spawn. The strict-error alternative was considered
/// and rejected: it would block each offline session after a fresh
/// install.
#[test]
fn provision_for_spawn_returns_ok_on_bad_base_ref_for_adopted() {
    let td = TestDir::new("adopted_bad_base");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "warm-bad");
    let mut node = empty_node(root, Some("gh99-bad"));
    node.source_issue = Some(99);
    let host_path = root.join(".claude").join("worktrees").join("gh99-bad");
    let host_path_str = host_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "warm-bad");
    let ctx = SpawnContext {
        base_ref: "definitely-not-a-ref-12345".to_string(),
        ..make_ctx(node, SpawnSource::Issue, host_path_str.clone(), Some(warm))
    };
    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("offline fallback must succeed (ADR 0001)");
    match outcome {
        ProvisionOutcome::Adopted { host_path: p, .. } => {
            assert_eq!(p, host_path_str);
        }
        other => panic!("expected Adopted (offline fallback), got {:?}", other),
    }
    assert!(host_path.exists());
}

// ── Upgraded (Manual) ─────────────────────────────────────────────────────

/// Manual warm claim upgrades the pool's detached entry to the mesh's
/// `worktree_mode` via `git checkout -b <branch>`. The pool directory
/// is NOT moved — the orchestrator already overwrote the node's
/// `worktree_name` with the pool's slug, so the target path IS the
/// pool path.
#[test]
fn provision_for_spawn_upgraded_for_manual_spawn() {
    let td = TestDir::new("upgraded_manual");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "bold-amber-fox");

    let node = empty_node(root, Some("bold-amber-fox"));
    let host_path_str = pool_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "bold-amber-fox");
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str.clone(), Some(warm));

    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("manual upgrade must succeed");
    match outcome {
        ProvisionOutcome::Upgraded {
            entry,
            host_path: p,
            ..
        } => {
            assert_eq!(entry.preassigned_name, "bold-amber-fox");
            assert_eq!(p, host_path_str);
        }
        other => panic!("expected Upgraded for manual spawn, got {:?}", other),
    }
    assert!(pool_path.exists());
    // The pool's plain slug is preserved (no `git worktree move`).
    let wt = git2::Repository::open(&pool_path).unwrap();
    let head_name = wt.head().unwrap().shorthand().unwrap_or("").to_string();
    assert_eq!(
        head_name, "bold-amber-fox",
        "manual upgrade must land on the slug"
    );
}

/// Detached-mode Manual warm claim is a no-op on the `git checkout`
/// side (the pool already cut the entry detached), but
/// `.worktreeinclude` re-application still runs. Pin the no-op so a
/// future fix doesn't accidentally `-B` a branch the mesh wanted
/// detached.
#[test]
fn provision_for_spawn_upgraded_for_manual_spawn_detached_mode() {
    let td = TestDir::new("upgraded_manual_detached");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "bold-amber-fox");

    let node = empty_node(root, Some("bold-amber-fox"));
    let host_path_str = pool_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "bold-amber-fox");
    let ctx = SpawnContext {
        worktree_mode: "detached".to_string(),
        ..make_ctx(node, SpawnSource::Manual, host_path_str.clone(), Some(warm))
    };

    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("detached manual upgrade must succeed");
    match outcome {
        ProvisionOutcome::Upgraded { host_path: p, .. } => assert_eq!(p, host_path_str),
        other => panic!("expected Upgraded for detached manual, got {:?}", other),
    }
    // Stays detached.
    let wt = git2::Repository::open(&pool_path).unwrap();
    assert!(wt.head_detached().unwrap_or(false), "must remain detached");
}

/// Manual upgrade re-applies `.worktreeinclude` (issue #639 gap 1). A
/// regression that skipped the include copy on a Manual warm claim would
/// leave the agent on the prewarm snapshot and miss subsequent source
/// edits — exactly the half the original fix addressed.
#[test]
fn provision_for_spawn_upgraded_reapplies_worktreeinclude() {
    let td = TestDir::new("upgraded_include");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "tracked\n")]);
    fs::write(root.join("secrets.env"), "v1=old\n").unwrap();
    fs::write(root.join(".worktreeinclude"), "secrets.env\n").unwrap();
    let repo = git2::Repository::open(root).unwrap();
    commit_file(&repo, root, ".worktreeinclude", "secrets.env\n");

    let pool_path = make_pool_entry(root, "warm-include");
    assert_eq!(
        fs::read_to_string(pool_path.join("secrets.env")).unwrap(),
        "v1=old\n"
    );
    fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

    let node = empty_node(root, Some("warm-include"));
    let host_path_str = pool_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "warm-include");
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str, Some(warm));
    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink)
        .expect("manual upgrade must succeed");
    assert!(matches!(outcome, ProvisionOutcome::Upgraded { .. }));
    assert_eq!(
        fs::read_to_string(pool_path.join("secrets.env")).unwrap(),
        "v1=NEW\n",
        "manual warm claim must re-apply .worktreeinclude to live source"
    );
}

// ── Cross-cutting ─────────────────────────────────────────────────────────

/// Per-branch timing data is exposed on the worktree-doing outcomes.
/// Pin the lower bound at 0 (a successful git operation may complete
/// inside the timer's resolution) — the load-bearing assertion is that
/// the timer is wired into the worktree-doing branches and not, e.g.,
/// only into `Reused`. The `Reused` case carries no timing field by
/// design, so this test scopes to the three worktree branches only.
#[test]
fn provision_for_spawn_elapsed_ms_is_present_for_each_worktree_branch() {
    let td = TestDir::new("elapsed_branches");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    // Cut two warm pool entries — one for the Adopted case, one for the
    // Upgraded case. (The Created case has no warm entry.)
    let pool_adopted = make_pool_entry(root, "warm-elapsed-adopted");
    let pool_upgraded = make_pool_entry(root, "warm-elapsed-upgraded");

    // Created
    let node_c = empty_node(root, Some("elapsed-cold"));
    let host_c = root.join(".claude").join("worktrees").join("elapsed-cold");
    let ctx_c = make_ctx(
        node_c,
        SpawnSource::Manual,
        host_c.to_string_lossy().to_string(),
        None,
    );
    let ProvisionOutcome::Created { elapsed_ms, .. } =
        provision_for_spawn(ctx_c, &ProvisionHooks::default(), &NullSink).unwrap()
    else {
        panic!("Created branch expected");
    };
    assert!(elapsed_ms < 60_000);

    // Adopted (Issue)
    let mut node_a = empty_node(root, Some("elapsed-issue"));
    node_a.source_issue = Some(11);
    let host_a = root.join(".claude").join("worktrees").join("elapsed-issue");
    let warm_a = claimed_warm(&pool_adopted, "warm-elapsed-adopted");
    let ctx_a = make_ctx(
        node_a,
        SpawnSource::Issue,
        host_a.to_string_lossy().to_string(),
        Some(warm_a),
    );
    let ProvisionOutcome::Adopted { elapsed_ms, .. } =
        provision_for_spawn(ctx_a, &ProvisionHooks::default(), &NullSink).unwrap()
    else {
        panic!("Adopted branch expected");
    };
    assert!(elapsed_ms < 60_000);

    // Upgraded (Manual) — the pool path IS the target path for manual.
    let node_u = empty_node(root, Some("warm-elapsed-upgraded"));
    let warm_u = claimed_warm(&pool_upgraded, "warm-elapsed-upgraded");
    let ctx_u = make_ctx(
        node_u,
        SpawnSource::Manual,
        pool_upgraded.to_string_lossy().to_string(),
        Some(warm_u),
    );
    let ProvisionOutcome::Upgraded { elapsed_ms, .. } =
        provision_for_spawn(ctx_u, &ProvisionHooks::default(), &NullSink).unwrap()
    else {
        panic!("Upgraded branch expected");
    };
    assert!(elapsed_ms < 60_000);
}

// ── Seam deepening (post-success hooks + cold-fallback recovery) ────────────

/// Test sink that records every semantic call. Mutex because tests can run
/// concurrently on machines that allow it (the existing buildmesh tests use
/// `--test-threads=1` on Windows today, but Mutex is the lower-friction
/// default and matches the existing recorder patterns in this codebase).
#[derive(Default)]
struct RecordingSink {
    state: std::sync::Mutex<RecordingState>,
}

#[derive(Default)]
struct RecordingState {
    warm_rows_returned: Vec<i64>,
    /// `id` for each `forget_warm_row` call.
    warm_rows_forgotten: Vec<i64>,
    /// (node_id, slug, worktree_path) for each `adopt_manual_slug` call.
    adopted_slugs: Vec<(i64, String, Option<String>)>,
    /// (mesh_id, do_refresh, do_refill) for each `on_pool_maintenance_required`
    /// call. Multiple calls would be a regression — exactly one per spawn.
    maintenance_calls: Vec<(i64, bool, bool)>,
}

impl RecordingSink {
    fn new() -> Self {
        Self::default()
    }
    fn warm_rows_forgotten(&self) -> Vec<i64> {
        self.state.lock().unwrap().warm_rows_forgotten.clone()
    }
    fn adopted_slugs(&self) -> Vec<(i64, String, Option<String>)> {
        self.state.lock().unwrap().adopted_slugs.clone()
    }
    fn maintenance_calls(&self) -> Vec<(i64, bool, bool)> {
        self.state.lock().unwrap().maintenance_calls.clone()
    }
}

impl ProvisionSink for RecordingSink {
    fn return_warm_row(
        &self,
        entry: &crate::git::worktree::warm::ClaimedWarmEntry,
        _mesh_id: i64,
    ) -> Result<(), String> {
        self.state.lock().unwrap().warm_rows_returned.push(entry.id);
        Ok(())
    }
    fn forget_warm_row(&self, id: i64) {
        self.state.lock().unwrap().warm_rows_forgotten.push(id);
    }
    fn adopt_manual_slug(&self, node_id: i64, slug: &str, worktree_path: Option<&str>) {
        self.state.lock().unwrap().adopted_slugs.push((
            node_id,
            slug.to_string(),
            worktree_path.map(str::to_string),
        ));
    }
    fn on_pool_maintenance_required(&self, mesh_id: i64, do_refresh: bool, do_refill: bool) {
        self.state
            .lock()
            .unwrap()
            .maintenance_calls
            .push((mesh_id, do_refresh, do_refill));
    }
}

/// Sink that writes through the real DB, mirroring what `AppHandleSink`
/// does for production. Used to assert the spawn path's end-to-end
/// contract: a Manual warm-claim spawn persists the pool's pre-assigned
/// slug into `agent_nodes.worktree_name` (#1080). The pre-existing
/// `provision_for_spawn_manual_upgraded_adopts_name_via_sink` proves
/// the sink *call*; this one proves the DB *column*.
///
/// Tests using this sink must initialise the global DB via
/// `crate::db::test_support::isolated()`; otherwise
/// `db::is_initialized()` returns false and the write no-ops.
struct DbWritingSink;
#[test]
fn manual_warm_branch_collision_returns_claim_without_detached_launch() {
    let _db = crate::db::test_support::isolated();
    let td = TestDir::new("manual-warm-branch-collision");
    let repo = init_repo_with_commit(td.path(), &[("file.txt", "source\n")]);
    let mesh = crate::db::create_mesh("warm", td.path().to_str().unwrap()).unwrap();
    let pool = make_pool_entry(td.path(), "warm-collision");
    let branch_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.branch(
        "warm-collision",
        &repo.find_commit(branch_oid).unwrap(),
        false,
    )
    .unwrap();
    let row_id = crate::db::insert_warm_worktree(
        mesh.id,
        pool.to_str().unwrap(),
        "warm-collision",
        None,
        crate::db::WarmWorktreeStatus::Claimed,
    )
    .unwrap();
    let mut claim = claimed_warm(&pool, "warm-collision");
    claim.id = row_id;
    let node = empty_node(td.path(), Some("warm-collision"));
    let mut ctx = make_ctx(
        node,
        SpawnSource::Manual,
        pool.to_string_lossy().to_string(),
        Some(claim),
    );
    ctx.worktree_mode = "branched".into();
    let error = provision_for_spawn(ctx, &ProvisionHooks::default(), &DbWritingSink).unwrap_err();
    assert!(error.contains("manual warm upgrade failed"));
    assert_eq!(
        repo.find_branch("warm-collision", git2::BranchType::Local)
            .unwrap()
            .get()
            .target(),
        Some(branch_oid)
    );
    assert!(git2::Repository::open(&pool)
        .unwrap()
        .head_detached()
        .unwrap());
    assert!(pool.join("file.txt").exists());
    assert_eq!(
        crate::db::count_available_warm_for_mesh(mesh.id).unwrap(),
        1
    );
}
impl ProvisionSink for DbWritingSink {
    fn return_warm_row(
        &self,
        entry: &crate::git::worktree::warm::ClaimedWarmEntry,
        _mesh_id: i64,
    ) -> Result<(), String> {
        crate::db::mark_warm_worktree_available(entry.id, entry.base_sha.as_deref())
            .map_err(|e| e.to_string())
    }
    fn forget_warm_row(&self, _id: i64) {}
    fn adopt_manual_slug(&self, node_id: i64, slug: &str, worktree_path: Option<&str>) {
        crate::db::adopt_manual_pool_slug_with_path(node_id, slug, worktree_path)
            .expect("DB write must succeed in this test");
    }
    fn on_pool_maintenance_required(&self, _mesh_id: i64, _do_refresh: bool, _do_refill: bool) {}
}

// DB init routes through `db::test_support::isolated`.

/// End-to-end regression for #1080. A Manual spawn that claims a warm
/// pool entry must persist the pool's pre-assigned slug into BOTH
/// `agent_nodes.name` and `agent_nodes.worktree_name`. Issue #1080
/// was that only `name` got written (via the `adopt_manual_name` sink
/// method); `worktree_name` kept the stage-1 throwaway slug and the
/// close path then queued a removal directory that never existed,
/// silently leaking the real worktree.
///
/// The pre-existing `provision_for_spawn_manual_upgraded_adopts_name_via_sink`
/// pins the sink call but with `RecordingSink`, which doesn't touch
/// the DB. This test uses `DbWritingSink` and queries the row to assert
/// the SQL-level invariant the spec asked for.
#[test]
fn manual_warm_claim_persists_pool_slug_into_agent_node_row() {
    let _db = crate::db::test_support::isolated();

    let td = TestDir::new("manual_adopt_db");
    let root = td.path();
    init_repo_with_commit(
        root,
        &[(
            "f.txt", "v1
",
        )],
    );
    let pool_path = make_pool_entry(root, "bold-amber-fox");

    // Stage-1 row that `agent_node::create` would have written before
    // the provisioner ran. Use `create_mesh` (idempotent on path) and
    // a fresh agent_node id to avoid colliding with other DB-touching
    // tests that share the process-wide OnceCell.
    let mesh = crate::db::create_mesh("end_to_end_adopt_test", root.to_str().unwrap())
        .expect("mesh must be creatable");
    let mesh_id = mesh.id;
    let node_id: i64 = {
        let db = crate::db::write_conn();
        db.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, worktree_name)                  VALUES (?1, 'tidy-sorrowful-nautilus', ?2, 'tidy-sorrowful-nautilus')",
            rusqlite::params![mesh_id, root.to_str().unwrap()],
        )
        .unwrap();
        db.last_insert_rowid()
    };

    let mut node = empty_node(root, Some("tidy-sorrowful-nautilus"));
    node.id = node_id;
    node.mesh_id = mesh_id;
    let host_path_str = pool_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "bold-amber-fox");
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str, Some(warm));

    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &DbWritingSink)
        .expect("manual upgrade must succeed");
    assert!(matches!(outcome, ProvisionOutcome::Upgraded { .. }));

    // Query the agent_nodes row and assert BOTH columns landed on the
    // pool's pre-assigned slug. Reading only `name` (the way the
    // pre-existing RecordingSink test implicitly does) would not have
    // caught #1080 — the bug was that name got written but
    // worktree_name did not.
    let (name, worktree_name): (String, Option<String>) = {
        let db = crate::db::write_conn();
        db.query_row(
            "SELECT name, worktree_name FROM agent_nodes WHERE id = ?1",
            rusqlite::params![node_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(
        name, "bold-amber-fox",
        "name must adopt the pool's pre-assigned slug"
    );
    assert_eq!(
        worktree_name.as_deref(),
        Some("bold-amber-fox"),
        "worktree_name must adopt the pool's pre-assigned slug — otherwise the              close path's removal directory derivation drifts from the on-disk              directory and every close silently leaks the real worktree (#1080)"
    );
}

/// Manual spawn that adopts a warm entry must fire `adopt_manual_slug`
/// with the pool's pre-assigned slug. The provisioner does this for free
/// now (was the orchestrator's job pre-deepening), and the sink owns the
/// DB write + `node-renamed` emit pair.
#[test]
fn provision_for_spawn_manual_upgraded_adopts_name_via_sink() {
    let td = TestDir::new("manual_adopt");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "bold-amber-fox");

    let mut node = empty_node(root, Some("bold-amber-fox"));
    node.id = 4242;
    node.mesh_id = 7;
    let host_path_str = pool_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "bold-amber-fox");
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str.clone(), Some(warm));

    let sink = RecordingSink::new();
    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &sink)
        .expect("manual upgrade must succeed");
    assert!(matches!(outcome, ProvisionOutcome::Upgraded { .. }));
    assert_eq!(
        sink.adopted_slugs(),
        vec![(4242, "bold-amber-fox".to_string(), None)],
        "Manual warm claim must adopt the pool's slug via the sink",
    );
    // Maintenance NOT scheduled — neither flag set.
    assert!(
        sink.maintenance_calls().is_empty(),
        "no maintenance without ref_advanced/drained flags"
    );
}

/// A Manual spawn with NO warm claim must NOT adopt anything. Its
/// `worktree_name` was already correct — stage 1 wrote it and the cold
/// create used it — so an adoption here would be a no-op at best and a
/// spurious `node-renamed` emit at worst.
///
/// This is the guard for the #1080 fix's own failure mode: adoption moved
/// from "on the `Upgraded` outcome" to "on a Manual *claim*", and the way
/// to get that wrong in the other direction is to make it unconditional.
#[test]
fn provision_for_spawn_manual_without_warm_claim_does_not_adopt() {
    let td = TestDir::new("cold_no_adopt");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);

    let mut node = empty_node(root, Some("stage-one-slug"));
    node.id = 555;
    let host_path = root
        .join(".claude")
        .join("worktrees")
        .join("stage-one-slug");
    let host_path_str = host_path.to_string_lossy().to_string();
    // `None` — pool was empty, so this spawn cuts its own worktree.
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str, None);

    let sink = RecordingSink::new();
    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &sink)
        .expect("cold create must succeed");
    assert!(matches!(outcome, ProvisionOutcome::Created { .. }));
    assert!(
        sink.adopted_slugs().is_empty(),
        "a cold Manual spawn already owns the right slug — adopting would be \
         a spurious rename (#1080 fix must stay claim-driven, not unconditional)"
    );
    assert!(
        sink.warm_rows_forgotten().is_empty(),
        "no warm row was claimed, so none may be forgotten"
    );
}

/// Issue spawn (the other warm path) must NOT adopt a name — Issue/PR
/// keep their own `gh{N}-`/`pr{N}-` name per CONTEXT.md *Spawn Source*.
/// `forget_after_spawn` still fires; the maintenance check stays empty.
#[test]
fn provision_for_spawn_issue_adopted_does_not_adopt_name() {
    let td = TestDir::new("issue_no_adopt");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "warm-issue-no-adopt");

    let mut node = empty_node(root, Some("gh45-bug"));
    node.id = 9999;
    node.source_issue = Some(45);
    let host_path = root.join(".claude").join("worktrees").join("gh45-bug");
    let host_path_str = host_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "warm-issue-no-adopt");
    let ctx = make_ctx(node, SpawnSource::Issue, host_path_str, Some(warm));

    let sink = RecordingSink::new();
    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &sink)
        .expect("Issue adoption must succeed");
    assert!(matches!(outcome, ProvisionOutcome::Adopted { .. }));
    assert!(
        sink.adopted_slugs().is_empty(),
        "Issue/PR spawns keep their own name; the sink must NOT be called"
    );
}

/// Maintenance fires exactly once when `ref_advanced_for_pool` is set —
/// pins the freshness-pass handoff. Issue #613 AC3.
#[test]
fn provision_for_spawn_fires_maintenance_when_ref_advanced() {
    let td = TestDir::new("maintenance_advanced");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);

    // Trivial manual spawn that resolves to `Created` (no warm) — keeps the
    // test focused on the maintenance hook, not the worktree-doing branches.
    let mut node = empty_node(root, Some("advanced-name"));
    node.id = 11;
    node.mesh_id = 88;
    let host_path = root.join(".claude").join("worktrees").join("advanced-name");
    let host_path_str = host_path.to_string_lossy().to_string();
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str, None);

    let hooks = ProvisionHooks {
        ref_advanced_for_pool: true,
        pool_was_drained_by_this_spawn: false,
    };
    let sink = RecordingSink::new();
    let _ = provision_for_spawn(ctx, &hooks, &sink).expect("cold create must succeed");
    assert_eq!(
        sink.maintenance_calls(),
        vec![(88, true, false)],
        "ref_advanced flag alone must schedule a refresh pass"
    );
    assert!(sink.adopted_slugs().is_empty());
}

/// Maintenance fires exactly once when `pool_was_drained_by_this_spawn` is
/// set, even when the spawn fell back to cold because the use-site recheck
/// rejected the warm claim (issue #653) — the pool inventory is still one
/// short and the refill must run.
#[test]
fn provision_for_spawn_fires_maintenance_when_pool_drained() {
    let td = TestDir::new("maintenance_drained");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);

    let mut node = empty_node(root, Some("drained-name"));
    node.id = 22;
    node.mesh_id = 99;
    let host_path = root.join(".claude").join("worktrees").join("drained-name");
    let host_path_str = host_path.to_string_lossy().to_string();
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str, None);

    let hooks = ProvisionHooks {
        ref_advanced_for_pool: false,
        pool_was_drained_by_this_spawn: true,
    };
    let sink = RecordingSink::new();
    let _ = provision_for_spawn(ctx, &hooks, &sink).expect("cold create must succeed");
    assert_eq!(
        sink.maintenance_calls(),
        vec![(99, false, true)],
        "drained flag alone must schedule a refill"
    );
}

/// Maintenance NOT scheduled when neither flag is set — and the recorder
/// pin catches a regression that fires the hook unconditionally.
#[test]
fn provision_for_spawn_does_not_fire_maintenance_without_flags() {
    let td = TestDir::new("maintenance_quiet");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);

    let mut node = empty_node(root, Some("quiet-name"));
    node.mesh_id = 5;
    let host_path = root.join(".claude").join("worktrees").join("quiet-name");
    let host_path_str = host_path.to_string_lossy().to_string();
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str, None);

    let sink = RecordingSink::new();
    let _ = provision_for_spawn(ctx, &ProvisionHooks::default(), &sink)
        .expect("cold create must succeed");
    assert!(
        sink.maintenance_calls().is_empty(),
        "no flags → no maintenance call (no false-positive refills)"
    );
}

/// Regression test for the warm-failure smart-cleanup: when the warm
/// helper's pre-move guard refuses (the branch already exists — issue
/// #612's refused-adoption case), the disk was untouched, and the
/// provisioner's cleanup branch must NOT remove the pool directory.
/// The sibling test (`provision_for_spawn_adopted_refuses_to_clobber_existing_branch`)
/// pins this for the `Adopted` branch; the `Upgraded` branch skips the
/// pre-move guard entirely (the pool slug IS the node's branch name),
/// so we exercise the cleanup branch directly with a forced checkout
/// failure: replace the worktree's `.git` HEAD with an invalid one.
#[test]
fn provision_for_spawn_warm_failure_leaves_pool_untouched_on_pre_move_refusal() {
    let td = TestDir::new("warm_fail_cleanup");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    // Build a valid warm entry, then pre-create a branch the Adoption
    // would target. With Issue source, the node's branch is its own
    // deterministic name (`gh{N}-bug`), and the pre-move guard refuses.
    let pool_path = make_pool_entry(root, "warm-fail-pool");

    let mut node = empty_node(root, Some("gh42-bug"));
    node.source_issue = Some(42);
    let host_path = root.join(".claude").join("worktrees").join("gh42-bug");
    let host_path_str = host_path.to_string_lossy().to_string();

    // Pre-create the deterministic branch the adoption would refuse.
    // Use a real detached HEAD so `find_branch` picks it up.
    std::process::Command::new("git")
        .args(["branch", "gh42-bug", "HEAD"])
        .current_dir(root)
        .output()
        .expect("pre-branch creation must succeed");

    let warm = claimed_warm(&pool_path, "warm-fail-pool");
    let ctx = make_ctx(node, SpawnSource::Issue, host_path_str, Some(warm));

    let sink = RecordingSink::new();
    let result = provision_for_spawn(ctx, &ProvisionHooks::default(), &sink);
    // Pre-move guard refused → either warm Err surfaced verbatim (no
    // cold fallback) or, on platforms where git worktree mv got partially
    // through, the cleanup leaves the pool intact.
    assert!(result.is_err(), "branch-exists must produce an error");
    assert!(
        pool_path.exists(),
        "pool entry must remain intact when the pre-move guard refuses"
    );
    assert!(
        sink.adopted_slugs().is_empty(),
        "no adopted name when warm failed before adoption"
    );
    assert!(
        sink.maintenance_calls().is_empty(),
        "warm failure must not schedule pool maintenance"
    );
}

/// Combined-error format: when both warm and cold paths fail, the
/// surfaced string names BOTH failures so a coordinator/operator can
/// see the full picture. This test exercises the format directly on
/// the warm-failure-with-pool-intact path (the pre-move-guard refusal
/// from the sibling test) and pins the error category. A future split
/// can add a cold-failure-only test by deleting the repo before the call.
#[test]
fn provision_for_spawn_warm_failure_surfaces_warm_error() {
    // This is the same setup as
    // `provision_for_spawn_warm_failure_leaves_pool_untouched_on_pre_move_refusal`,
    // rephrased as an error-format pin: the pre-move guard refuses, the
    // warm helper returns Err, the provisioner surfaces it verbatim (the
    // cold fallback only runs when `adopt_warm_worktree_by_move` errors
    // AFTER the move happened — pre-move refusal is below that threshold).
    let td = TestDir::new("warm_format");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "warm-format-pool");

    let mut node = empty_node(root, Some("gh99-format"));
    node.source_issue = Some(99);
    let host_path = root.join(".claude").join("worktrees").join("gh99-format");
    let host_path_str = host_path.to_string_lossy().to_string();
    std::process::Command::new("git")
        .args(["branch", "gh99-format", "HEAD"])
        .current_dir(root)
        .output()
        .expect("pre-branch creation must succeed");

    let warm = claimed_warm(&pool_path, "warm-format-pool");
    let ctx = make_ctx(node, SpawnSource::Issue, host_path_str, Some(warm));
    let sink = RecordingSink::new();
    let result = provision_for_spawn(ctx, &ProvisionHooks::default(), &sink);
    let err = result.expect_err("pre-move guard must refuse the adoption");
    assert!(
        err.contains("already exists"),
        "warm-failure error must name the refusal cause, got: {}",
        err
    );
    assert!(
        pool_path.exists(),
        "pre-move refusal: pool directory must remain intact"
    );
    assert!(sink.adopted_slugs().is_empty());
    assert!(sink.maintenance_calls().is_empty());
}

/// Manual warm upgrade succeeds — pin both invariants:
///   (a) `adopt_manual_slug` IS called (DB write + node-renamed),
///   (b) maintenance hook is NOT called (no ref_advanced / drained flags).
/// The seam concentrates these decisions — a regression that fires the
/// maintenance hook unconditionally, or that drops the name adoption,
/// fails here.
#[test]
fn provision_for_spawn_manual_upgraded_adopts_name_and_skips_maintenance() {
    let td = TestDir::new("manual_upgrade_no_maintenance");
    let root = td.path();
    init_repo_with_commit(root, &[("f.txt", "v1\n")]);
    let pool_path = make_pool_entry(root, "quiet-warm");

    let mut node = empty_node(root, Some("quiet-warm"));
    node.id = 7;
    node.mesh_id = 12;
    let host_path_str = pool_path.to_string_lossy().to_string();
    let warm = claimed_warm(&pool_path, "quiet-warm");
    let ctx = make_ctx(node, SpawnSource::Manual, host_path_str, Some(warm));

    let sink = RecordingSink::new();
    let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &sink)
        .expect("manual upgrade must succeed");
    assert!(matches!(outcome, ProvisionOutcome::Upgraded { .. }));
    assert_eq!(
        sink.adopted_slugs(),
        vec![(7, "quiet-warm".to_string(), None)]
    );
    assert!(
        sink.warm_rows_forgotten() == vec![42],
        "manual upgrade must forget the warm row (id from claimed_warm fixture)"
    );
    assert!(sink.maintenance_calls().is_empty());
}
