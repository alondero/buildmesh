//! Worktree Node provisioning for the Agent Node spawn path.
//!
//! This module owns the four-branch decision that turns a Spawn Context into
//! an on-disk worktree state. The orchestrator (`agent::spawn::spawn_agent_inner`)
//! builds a [`SpawnContext`] from mesh-row + node-row + optional warm-pool
//! claim reads, hands it to [`provision_for_spawn`], and matches the
//! [`ProvisionOutcome`] to drive the post-spawn bookkeeping
//! (`forget_after_spawn`, name adoption, status writes).
//!
//! ## The four branches
//!
//! `provision_for_spawn` returns exactly one of:
//!
//! - **`Reused { host_path }`** — the spawn is a Root Node
//!   (`use_worktree = false`) OR the target path already exists on disk
//!   (resume / handover / re-spawn). No worktree is created or moved; the
//!   agent lands on whatever's there. Carries no timing field — the work
//!   done is a single `Path::exists()`.
//! - **`Adopted { entry, host_path, elapsed_ms }`** — an Issue or PR spawn
//!   claimed a warm pool entry (`ClaimedWarmEntry`). `git worktree move`
//!   rewrites the pool's plain-slug directory to the node's `gh{N}-` /
//!   `pr{N}-` target, then `git checkout -b <branch> <base_sha>` lands it
//!   on the resolved ref. `entry` is what the caller hands to
//!   `warm_pool::forget_after_spawn`.
//! - **`Upgraded { entry, host_path, elapsed_ms }`** — a Manual spawn
//!   claimed a warm pool entry. The pool's slug is the node's
//!   `worktree_name`, so the pool directory IS already at `host_path`;
//!   `git checkout -b <branch>` aligns the worktree's mode with the
//!   mesh's `worktree_mode`, and `.worktreeinclude` is re-applied so the
//!   agent sees the live source files, not the stale prewarm snapshot
//!   (issue #639 gap 1). `entry` is what the caller hands to
//!   `forget_after_spawn` AND uses as the source for the manual name
//!   adoption (overwrites the node's stage-1 throwaway slug).
//! - **`Created { host_path, elapsed_ms }`** — no warm entry (empty pool
//!   or non-warm-eligible spawn). The cold path's
//!   [`create_git_worktree`] runs `git worktree add -b <branch> <base_ref>`
//!   and re-applies `.worktreeinclude`.
//!
//! `elapsed_ms` is the wall-clock cost of the worktree-doing branch only;
//! `Reused` carries no timing field by design (the elapsed value would
//! just be `Path::exists()`'s negligible cost). The single timer covers
//! every spawn path — the `spawn_timing:` checkpoint log records it
//! uniformly.
//!
//! ## Vocabulary
//!
//! - **Spawn Context** — see [`SpawnContext`] and `CONTEXT.md` (Spawn
//!   Context): the boundary type between orchestrator and provisioner.
//! - **Spawn Source** — see [`SpawnSource`] and `CONTEXT.md` (Spawn
//!   Source): the runtime classification of an Agent Node spawn that
//!   picks between the two warm-pool adoption modes.
//!
//! ## Fetch helpers
//!
//! The four `fetch_single_ref` / `fork_remote_alias` / `fetch_fork_head` /
//! `read_origin_ref_sha` helpers below are the PR-spawn head-fetch
//! primitives (issues #420, #443, #444). They feed `provision_for_spawn`'s
//! `Adopted` branch but live at module scope so the orchestrator can run
//! them ahead of the provision call (the fetch is best-effort, ADR 0001
//! offline pattern — failure falls back to the mesh base, never errors).
//!
//! ## Rationale and history
//!
//! This module completes ADR 0007's consolidation of Worktree Node
//! ownership. The cold-path primitives (`create_git_worktree`,
//! `move_git_worktree`, `apply_worktree_include`)
//! live in the parent [`git::worktree`] module; the warm-path helpers and
//! the `provision_for_spawn` seam move out of `agent::spawn` and into
//! here. See:
//!
//! - `docs/adr/0007-extract-git-module.md` — original ADR
//! - its 2026-06-13 amendment — the `git worktree add` shell-out exception
//! - its 2026-06-27 amendment — this module's creation
//! - `CONTEXT.md` — Spawn Context / Spawn Source vocabulary

use tauri::Emitter as _;

// ─── Fetch helpers (PR-spawn head fetch + fork remote registration) ───────

/// Fetch a single ref from `origin` into the local repo. Used by the PR-spawn
/// path (#420) to materialise `origin/<head_ref>` so the worktree can be cut
/// from it. `head_ref` is the PR's source branch (e.g. `feat/420-pr-spawn`).
///
/// As of issue #446, this is a thin wrapper over
/// [`crate::git::sync::do_fetch_only`] — the fetch-only half of `do_sync`
/// (the shared fetch+ff-pull algorithm used by `fetch_origin` and the manual
/// `git_sync` command). Consolidating onto the shared algorithm means:
///   - Windows quoting, ref-with-special-chars, and lock-contention fixes
///     land in one place for both the auto-sync and the PR-head fetch.
///   - A dirty parent does NOT block the fetch (2026-07-17): a fetch never
///     touches the working tree, and the whole point here is materialising
///     `refs/remotes/origin/<head_ref>` — the old pre-fetch dirty-skip
///     silently cut the PR worktree from a stale ref (or fell back to
///     `base_ref`) whenever the mesh root had uncommitted changes.
///
/// **Why fetch-only, not full `do_sync`:** the PR-head fetch is a
/// single-ref materialisation; the goal is to populate
/// `refs/remotes/origin/<head_ref>` so the worktree can be cut from it.
/// `do_sync`'s Step 6 (`git pull --ff-only --no-rebase`) would mutate
/// the mesh's currently-checked-out branch (typically `main`) as a side
/// effect of spawning a PR agent — a behavioural surprise (the user's
/// `main` advances on every PR spawn) AND wasted work (a few hundred ms
/// of duplicate fast-forward on top of the `fetch_origin` call a few
/// lines earlier in `spawn_agent_inner`, under the same per-Mesh
/// `sync_lock`). `do_fetch_only` runs steps 1-3 (open, has-remote,
/// `git fetch`) and stops before the `commits_behind`, dirty-gate and
/// `git pull` tail.
///
/// Best-effort by design: the caller falls back to the mesh's `base_ref` on
/// `false` rather than failing the spawn (ADR 0001 offline pattern). The user
/// sees the agent spawn on the wrong commits in the rare offline / stale-ref
/// case, instead of a hard error every time the network blips. The
/// alternative (strict-error spawn) is brittle to the very first offline
/// session after a fresh install.
///
/// **Adversarial-ref guard:** `do_fetch_only` passes the branch as a single
/// argv entry without a `--` separator (it doesn't know it's running in the
/// spawn context, and the auto-sync + manual-sync callers both want git to
/// parse the ref as a refspec). So the wrapper rejects any `head_ref`
/// starting with `-` (e.g. `--upload-pack=evil`) up front — the same
/// hardening the old shell-out had via `cmd.arg("--")`. GitHub's branch-name
/// validation blocks this in practice, but the cost of the check is zero
/// and the upside is defending against a future refactor that lets a
/// hand-entered or imported ref flow through. Pinned by
/// `fetch_single_ref_rejects_adversarial_dash_ref`.
pub(crate) fn fetch_single_ref(project_root: &str, head_ref: &str) -> bool {
    // Adversarial-ref guard — see fn doc. Must run BEFORE we hand `head_ref`
    // to `do_fetch_only`, because the helper passes it as a plain argv entry
    // without a `--` separator.
    if head_ref.starts_with('-') {
        tracing::warn!(
            "fetch_single_ref: rejecting adversarial ref {} (starts with '-')",
            head_ref
        );
        return false;
    }

    let host_root = crate::env::to_host_path(project_root);
    // `do_fetch_only` returns the same `SyncOutcome` variants `do_sync` would,
    // but without the count-behind / pull tail. `Ok(())` covers every fetch-
    // succeeded case (UpToDate / Synced / FetchedButDiverged in `do_sync`'s
    // world); every `Err` variant maps to `false` per the wrapper contract.
    crate::git::sync::do_fetch_only(
        &host_root,
        "origin",
        Some(head_ref),
        crate::git::sync::SPAWN_FETCH_TIMEOUT,
    )
    .is_ok()
}

/// The alias used for a fork remote (issue #443). `fork-<login>` is
/// human-readable in `git remote -v` and stays distinct from any user-defined
/// remote name (a regular remote can't start with `fork-` because GitHub
/// logins are alphanumeric + `-` with no leading `-`, but a user could
/// still define one; the `fork-` prefix keeps our entries easy to spot in
/// the output and trivial to clean up if we ever need to).
pub(crate) fn fork_remote_alias(head_repo_owner: &str) -> String {
    format!("fork-{}", head_repo_owner)
}

/// Fetch a single ref from a fork's clone URL into the local repo. Used by
/// the PR-spawn path (issue #443, follow-up to #36 worktree adoption) when
/// the PR's head branch lives on a fork — `fetch_single_ref` only fetches
/// from `origin`, which the fork's head ref isn't on.
///
/// The function:
///   1. Registers the fork as a remote named `fork-<login>` (idempotent —
///      ignores the "remote already exists" error from `git remote add` and
///      updates the URL via `git remote set-url` if the existing URL drifted,
///      e.g. the user re-pointed the fork's origin on GitHub).
///   2. Runs `git fetch <alias> <head_ref>` to materialise the ref locally.
///   3. Returns `true` only when both steps succeed.
///
/// Best-effort by design (same contract as `fetch_single_ref`): the caller
/// falls back to the mesh's `base_ref` on `false` rather than failing the
/// spawn. The user sees the agent spawn on the wrong commits in the rare
/// offline / stale-ref / removed-fork case, instead of a hard error every
/// time the network blips.
pub(crate) fn fetch_fork_head(
    project_root: &str,
    head_repo_owner: &str,
    head_repo_clone_url: &str,
    head_ref: &str,
) -> bool {
    use crate::process_util::git_command;
    let host_root = crate::env::to_host_path(project_root);
    let alias = fork_remote_alias(head_repo_owner);
    tracing::info!(
        "fetch_fork_head: ensuring remote {} -> {} in {}",
        alias,
        head_repo_clone_url,
        host_root
    );

    // Step 1: `git remote add` is idempotent via the explicit existence check.
    // We use `git remote get-url` (read-only) to see if the remote already
    // exists; if it does, `set-url` keeps it in sync with the fork's current
    // clone URL. If it doesn't, `remote add` registers it. This avoids
    // parsing `git remote add`'s non-zero stderr for the "already exists"
    // signal — easier to read, and works on every git version.
    let mut get_url = git_command();
    get_url.arg("remote").arg("get-url").arg(&alias);
    get_url.current_dir(&host_root);
    let existing =
        match super::run_bounded_git(get_url, "git remote get-url", super::GIT_LOCAL_TIMEOUT) {
            Ok(o) if o.status.success() => {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            // A git that can't answer `remote get-url` inside 30s is wedged, so
            // don't spend another budget on `remote add` to reach the same
            // conclusion — fall back to the mesh base ref like any other failure
            // here (best-effort by design, see the fn doc).
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("fetch_fork_head: git remote get-url failed: {}", e);
                return false;
            }
        };

    let url_matches = existing.as_deref() == Some(head_repo_clone_url);
    if !url_matches {
        let mut cmd = git_command();
        let op_name = if existing.is_some() {
            cmd.arg("remote")
                .arg("set-url")
                .arg(&alias)
                .arg(head_repo_clone_url);
            tracing::info!("fetch_fork_head: updating remote {} URL", alias);
            "git remote set-url"
        } else {
            cmd.arg("remote")
                .arg("add")
                .arg(&alias)
                .arg(head_repo_clone_url);
            tracing::info!("fetch_fork_head: adding remote {}", alias);
            "git remote add"
        };
        cmd.current_dir(&host_root);
        let output = match super::run_bounded_git(cmd, op_name, super::GIT_LOCAL_TIMEOUT) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("fetch_fork_head: {} failed: {}", op_name, e);
                return false;
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                "fetch_fork_head: git remote config for {} failed: {}",
                alias,
                stderr.trim()
            );
            return false;
        }
    }

    // Step 2: fetch the head ref. `--` before `head_ref` defends against an
    // adversarial / malformed ref starting with `-` (same hardening as
    // `fetch_single_ref`).
    tracing::info!(
        "fetch_fork_head: running git fetch {} -- {} in {}",
        alias,
        head_ref,
        host_root
    );
    let mut cmd = git_command();
    cmd.arg("fetch").arg(&alias).arg("--").arg(head_ref);
    cmd.current_dir(&host_root);
    // Bounded like `fetch_single_ref`'s `do_fetch_only` call: this is a
    // network fetch on the spawn path, run under the per-Mesh sync lock —
    // a half-open connection must abort at the spawn budget, not wedge
    // the lock (and every later spawn on this mesh) indefinitely. The budget
    // is the network one, not the local tiers `GIT_TREE_TIMEOUT` /
    // `GIT_LOCAL_TIMEOUT` cover.
    let fetch_budget = crate::git::sync::SPAWN_FETCH_TIMEOUT;
    let output = match super::run_bounded_git(cmd, "git fetch (fork head)", fetch_budget) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("fetch_fork_head: {}", e);
            return false;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "fetch_fork_head: git fetch {} -- {} failed: {}",
            alias,
            head_ref,
            stderr.trim()
        );
        return false;
    }
    true
}

/// Run the PR-spawn head fetch inside `crate::git::sync_lock::with_mesh_sync_lock`
/// so concurrent PR-spawns (or a PR-spawn racing the manual `git_sync` from
/// #680) can't collide on `.git/FETCH_HEAD`, `.git/refs/remotes/<remote>/
/// <ref>.lock`, or — for the fork branch — the config files `git remote add/
/// set-url` write (issues #652, #680 follow-up #698).
///
/// The lock key is the mesh's **DB-stored path** (`project_root`), matching
/// the key `spawn_agent_inner` uses for the auto-sync `fetch_origin` call
/// two steps earlier. `sync_lock.rs` documents the key as the form stored on
/// `agent_nodes.path` — the host-native form for Windows repos, the WSL
/// form for WSL repos (the bare helpers internally re-map to the host
/// path via `env::to_host_path`, but the lock key stays as the DB-stored
/// string). Same-key contention serialises; different keys (different
/// meshes) never wait on each other.
///
/// Both branches of the underlying match share one helper so `spawn_agent_inner`
/// has a single, audit-able lock acquisition rather than two inline branches.
/// The closure that `with_mesh_sync_lock` runs owns nothing: it captures all
/// four arguments by reference, so the closure borrows are valid for the
/// duration of the fetch and the lock is released as soon as `fetch_single_ref`
/// or `fetch_fork_head` returns. Best-effort by design (ADR 0001) — the spawn
/// falls through to `base_ref` on `false` rather than hard-failing.
///
/// The two underlying helpers (`fetch_single_ref`, `fetch_fork_head`) stay
/// lock-free so the no-lock shape is testable in isolation (the issue #420 /
/// #443 unit tests in `agent::spawn` call them directly with unique tempdir
/// paths); `locked_fetch_pr_head` is the only caller that needs the
/// serialization guarantee.
pub(crate) fn locked_fetch_pr_head(
    project_root: &str,
    head_ref: &str,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> bool {
    // Bounded wait, matching `locked_fetch_origin`: a PR spawn must not
    // queue for minutes behind a wedged manual sync holding this mesh's
    // lock. On timeout we return `false`, which the spawn orchestrator
    // already handles as "head unfetchable" — it falls back to the mesh's
    // base_ref and raises the `pr_head_unfetchable` toast (ADR 0001).
    crate::git::sync_lock::try_with_mesh_sync_lock_timeout(
        project_root,
        crate::git::sync::SPAWN_SYNC_LOCK_TIMEOUT,
        || match (head_repo_owner, head_repo_clone_url) {
            (Some(owner), Some(clone_url)) => {
                fetch_fork_head(project_root, owner, clone_url, head_ref)
            }
            _ => fetch_single_ref(project_root, head_ref),
        },
    )
    .unwrap_or_else(|| {
        tracing::warn!(
            "locked_fetch_pr_head: gave up waiting {}s for the mesh sync lock on {}; \
             falling back to base_ref",
            crate::git::sync::SPAWN_SYNC_LOCK_TIMEOUT.as_secs(),
            project_root
        );
        false
    })
}

/// Read the local SHA at `refs/remotes/origin/<head_ref>` — the ref
/// `fetch_single_ref` populates via `git fetch origin -- <head_ref>`.
/// Returns `None` when the ref doesn't exist (a stale local cache, a
/// first-time fetch, or a non-git directory) so the spawn path can treat
/// the absence as "skip the drift check" rather than a hard error.
///
/// Issue #444 — exact-pinning: the spawn path compares this to the
/// `source_pr_pinned_sha` we stored at `create_pr_node` time and emits a
/// `pr_sha_drift` `mesh-sync-warning` if they differ. SHA comparison is
/// direct string equality: both `git rev-parse` and GitHub's API return
/// 40-char lowercase hex, so a `String::ne` check is sufficient (no need
/// to lowercase or trim).
///
/// `remote_ref` is the full remote-tracking ref (e.g. `origin/feat-x` for
/// same-repo PRs from #420, or `fork-alice/feat-x` for fork PRs from #443).
///
/// This is a thin wrapper over the crate's one SHA resolver: the
/// `ShaSource::OriginRef` source is resolved without a fallback — a missing
/// remote-tracking ref is `None` rather than a silent HEAD fallback (which would
/// have turned "skip the drift check" into a false "no drift").
pub(crate) fn read_origin_ref_sha(project_root: &str, remote_ref: &str) -> Option<String> {
    let source = crate::git::sha::ShaSource::OriginRef(remote_ref.to_string());
    crate::git::sha::resolve_sha_at(project_root, &source)
        .ok()
        .map(|oid| oid.to_string())
}

// ─── Worktree helpers (warm-pool adoption / upgrade / shared checkout) ─────

/// Upgrade a warm pool entry from detached HEAD to the mesh's configured
/// worktree mode (issue #609, PRD #608 §3). The pool cuts every entry as
/// `detached` so a future claim can adopt the directory without ever
/// touching the mesh's branch refs — the cost is one `git checkout -b
/// <branch>` (branched mode, ~50ms) or no-op (detached mode, ~5ms).
///
/// This is the entire warm-path "checkout" cost the tracer bullet buys: the
/// on-disk tree is already at the mesh's base SHA (the worker checked it
/// out); all the spawn has to do is flip the working ref. The 97% of cold-
/// spawn time that was Windows Defender / NTFS search indexer / USN journal
/// scanning freshly-written files is paid ONCE on app startup, not per
/// spawn.
///
/// Failure returns the manual claim to the pool and leaves the node in error;
/// the caller does not launch a detached fallback.
///
/// `command_no_window` already applies CREATE_NO_WINDOW on Windows
/// (`process_util::command_no_window`), so we don't need per-OS cfg
/// duplication here.
pub(crate) fn upgrade_warm_to_mode(
    project_root: &str,
    host_path: &str,
    branch_name: &str,
    mode: &str,
) -> Result<(), String> {
    let parent = crate::git::project::open(project_root).map_err(|e| e.to_string())?;
    super::super::trust::ensure_linked_worktree(&parent, std::path::Path::new(host_path))
        .map_err(|e| e.to_string())?;
    // `target_sha = None`: a Manual claim keeps the entry's own HEAD — the
    // pool already cut it at the mesh's base. The mode rules, the checkout
    // and the `.worktreeinclude` re-application all live in
    // `warm::prepare_claimed_worktree` (issue #2533), so the Manual and the
    // Issue/PR paths stay in step.
    super::warm::prepare_claimed_worktree(project_root, host_path, branch_name, mode, None)
}

/// Adopt a claimed warm-pool worktree for an Issue/PR spawn (issue #612): move
/// the pre-warmed plain-slug directory to the node's `gh{N}-`/`pr{N}-` target
/// path, then check that worktree out to `base_ref` on the node's branch (or
/// detached), then re-apply `.worktreeinclude` so the result matches a cold
/// spawn. Any step failing returns `Err` so the spawn path can clean up the
/// warm entry and fall back to a cold `git worktree add`.
///
/// The move is the cheap part (`git worktree move`, ~tens of ms); the checkout
/// only writes the diff between the pool's base SHA and `base_ref` — for an
/// Issue spawn `base_ref` IS the mesh base the pool already sits on (near-zero
/// writes), and for a PR spawn it's the freshly fetched PR head (just the PR's
/// changed files), versus a cold spawn re-writing the entire tree.
pub(crate) fn adopt_warm_worktree_by_move(
    project_root: &str,
    old_host_path: &str,
    new_host_path: &str,
    branch_name: &str,
    mode: &str,
    base_ref: &str,
) -> Result<(), String> {
    // Resolve `base_ref` to a concrete SHA up front (offline → HEAD fallback,
    // and never a symbolic ref to `git checkout`), mirroring what the cold
    // path's `add_worktree_impl` does via the crate's one SHA resolver.
    // Resolving BEFORE the move means a bad ref fails fast, before we
    // disturb the pool directory.
    let base_sha = crate::git::sha::resolve_sha_at(
        project_root,
        &crate::git::sha::ShaSource::BaseRef(base_ref.to_string()),
    )
    .map_err(|e| e.to_string())?
    .to_string();
    // Same fail-fast principle for the branch: the claimed worktree's
    // `git checkout -b` refuses an existing branch, but by then the pool
    // directory has already been moved — the spawn failure cleanup then
    // deletes a half-adopted worktree, which can strand a stale admin entry
    // (the phantom worktree from the 2026-07-17 gh252 duplicate-spawn
    // incident). Refuse BEFORE the move so a refused adoption leaves the
    // pool entry exactly where it was.
    if mode == "branched" {
        let host_root = crate::env::to_host_path(project_root);
        match crate::git::project::open(&host_root) {
            Ok(repo) => {
                if repo
                    .find_branch(branch_name, git2::BranchType::Local)
                    .is_ok()
                {
                    return Err(format!(
                        "a branch named '{}' already exists — refusing to adopt the warm worktree over it",
                        branch_name
                    ));
                }
            }
            // Fail open: the post-move `git checkout -b` still refuses an
            // existing branch, so an unopenable root degrades to the old
            // (later) refusal rather than blocking every adoption.
            Err(e) => tracing::warn!(
                "adopt_warm_worktree_by_move: could not open {} to pre-check branch '{}' ({}); \
                 relying on the post-move checkout refusal",
                host_root,
                branch_name,
                e
            ),
        }
    }
    super::move_git_worktree(project_root, old_host_path, new_host_path)?;
    // The mode rules, the checkout onto `base_sha` and the
    // `.worktreeinclude` re-application live in `warm::prepare_claimed_worktree`
    // (issue #2533) — the Manual path calls the same function with no target
    // SHA, so the two claim paths share one rule.
    super::warm::prepare_claimed_worktree(
        project_root,
        new_host_path,
        branch_name,
        mode,
        Some(&base_sha),
    )
}

// ─── Spawn Context / Spawn Source / Provision Outcome (issue #677) ──────────
//
// These three types are the boundary between `agent::spawn` (the orchestrator
// that reads the DB, resolves the mesh row, and claims warm-pool entries) and
// the worktree provisioner (the body of `provision_for_spawn` below). The
// orchestrator builds a `SpawnContext`; the provisioner consumes it; the
// orchestrator drives post-success bookkeeping through the `ProvisionSink`
// trait so it doesn't need to thread the entry back out or rebuild the cold
// context for the retry. Names mirror `CONTEXT.md` (Spawn Context, Spawn Source).
//
// `SpawnContext` carries the loaded `AgentNode` rather than just
// `source_issue`/`source_pr` so the provisioner can branch on source without
// re-fetching the node row. `base_ref` is the *requested* ref (e.g.
// `origin/feat-x` for a PR head); `create_git_worktree` (cold path) takes
// `base_ref` directly, while `adopt_warm_worktree_by_move` resolves it via
// the crate's one SHA resolver internally so its `git checkout -b` does not see a
// symbolic ref. The three speculative fields (`resolved_base_sha`, `sandbox`,
// `spawn_path`) this struct carried before the architecture-review deepening
// were dropped — see `CONTEXT.md` *Spawn Context* for the current contract.

/// How an **Agent Node** spawn was triggered. Replaces the `is_rename_spawn`
/// boolean at the provision seam: the two warm-pool adoption modes
/// (Issue/PR vs manual) and the cold fall-through are the three branches the
/// provisioner has to distinguish, and the boolean lost that information by
/// collapsing them to "true/false".
///
/// `Issue` and `PullRequest` adopt the warm entry via `git worktree move` to
/// the node's `gh{N}-`/`pr{N}-` name and `git checkout` it onto the resolved
/// base SHA. `Manual` keeps the pool's pre-assigned slug as the node's name
/// and `git checkout -b`s it onto the mesh's mode (issue #609).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnSource {
    Manual,
    Issue,
    PullRequest,
}

impl SpawnSource {
    /// Derive a `SpawnSource` from the loaded [`AgentNode`]. The decision is
    /// `source_pr` first, then `source_issue`, then `Manual` — matching the
    /// priority the orchestrator itself uses (`is_rename_spawn` is set when
    /// either is `Some`, with PR taking precedence for fork-PR-spawned nodes
    /// that also carry an issue link in their body).
    pub fn from_node(node: &crate::models::AgentNode) -> Self {
        if node.source_pr.is_some() {
            SpawnSource::PullRequest
        } else if node.source_issue.is_some() {
            SpawnSource::Issue
        } else {
            SpawnSource::Manual
        }
    }
}

/// The fully resolved state an orchestrator hands to `provision_for_spawn`.
/// Mirrors `CONTEXT.md` *Spawn Context*: the orchestrator has finished
/// resolving (mesh row, node row, optional warm claim) and the provisioner
/// owns the next decision.
///
/// Every field is consumed by [`provision_for_spawn`]. The previous revision
/// carried three speculative fields (`resolved_base_sha`, `sandbox`,
/// `spawn_path`) that no provisioner read; those were dropped with the
/// architecture-review deepening that moved warm-failure recovery and the
/// post-success bookkeeping into this module (seam = `provision_for_spawn`'s
/// signature; old `#[allow(dead_code)]` retired).
///
/// `host_path` is the Windows-side path git operations run against (the form
/// `git2` and the `git` CLI both accept). The WSL-spelled path the spawned
/// agent will see stays in the orchestrator (consumed at command-build time
/// after `provision_for_spawn` returns) — moving it onto the context would
/// require duplicating env-shape logic the orchestrator already owns.
#[derive(Debug, Clone)]
pub struct SpawnContext {
    pub node: crate::models::AgentNode,
    pub source: SpawnSource,
    pub base_ref: String,
    pub worktree_mode: String,
    pub use_worktree: bool,
    pub warm_entry: Option<super::warm::ClaimedWarmEntry>,
    pub host_path: String,
}

/// Data-only inputs the caller supplies alongside [`SpawnContext`] at the
/// provisioner seam. These are *decision inputs* the provisioner reads to
/// decide whether to fire the maintenance path, *not* behavior — behavior
/// lives on the [`ProvisionSink`] trait passed to
/// [`provision_for_spawn`].
///
/// `ref_advanced_for_pool` is set by the orchestrator after the spawn-time
/// `fetch_origin` to record "the mesh's base ref moved this spawn" — when
/// true, the provisioner schedules a `git reset --hard` over any now-stale
/// warm entries under the single per-mesh fill-lock acquisition that also
/// runs the refill.
///
/// `pool_was_drained_by_this_spawn` is the dual: even when the use-site
/// recheck dropped a warm claim (issue #653), the pool's inventory is still
/// one short and the post-spawn refill must run. Tracking it here (not in
/// `SpawnContext`) keeps the worktree-data type pure.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProvisionHooks {
    pub ref_advanced_for_pool: bool,
    pub pool_was_drained_by_this_spawn: bool,
}

/// Side-effect surface for post-provision work. The provisioner is the only
/// caller; the methods map 1:1 to the two events that follow a successful
/// outcome — the Manual-only name adoption (Issue/PR keep their own
/// `gh{N}-`/`pr{N}-` name) and the optional pool-refresh/refill thread.
///
/// Trait rather than direct `AppHandle` so tests assert side effects without
/// standing up a Tauri test context. The semantic naming (`on_*`) names the
/// what-happened; the production impl builds the wire payload (`NodeRenamed`)
/// and the per-mesh maintenance thread from those arguments.
pub(crate) trait ProvisionSink: Sync {
    /// Return a refused manual upgrade to the pool without adopting its slug.
    fn return_warm_row(
        &self,
        entry: &super::warm::ClaimedWarmEntry,
        mesh_id: i64,
    ) -> Result<(), String>;
    // `Sync` is required because `&dyn ProvisionSink` crosses a
    // `spawn_blocking` boundary inside `provision_for_spawn`'s callers
    // (the orchestrator sends the sink across threads). The trade-off —
    // `NullSink` and `RecordingSink` add a small `Mutex` for interior
    // mutability — is cheaper than re-architecting the call site.

    /// Forget the warm-pool row after a successful adoption. Called on
    /// every `Adopted` and `Upgraded` outcome — the row's purpose is done
    /// when the directory has become the agent's worktree.
    fn forget_warm_row(&self, id: i64);

    /// Adopt the pool's pre-assigned slug onto a Manual node: persist it as
    /// the row's identity (`name` AND `worktree_name` AND `worktree_path`,
    /// one write — see `db::adopt_manual_pool_slug_with_path`) AND announce
    /// via `node-renamed` so the
    /// frontend's optimistic stage-1 row re-labels. Three consequences of one
    /// decision — kept as a single trait method because they must be
    /// atomic-ish: a partial application leaves either the on-disk name, the
    /// in-memory one, or the close path's removal target out of sync. The
    /// last of those is what leaked worktrees in #1080.
    ///
    /// Called for every Manual spawn that *claimed* a warm entry, whichever
    /// on-disk outcome it reached; Issue/PR spawns keep their own
    /// `gh{N}-`/`pr{N}-` identity per CONTEXT.md *Spawn Source*.
    ///
    /// Issue #1519: also persists the exact resolved `worktree_path` (raw
    /// form) so the close path derives its removal directory from the
    /// configured location, not the stage-1 throwaway. `None` preserves the
    /// legacy fallback (pre-#1519 rows).
    fn adopt_manual_slug(&self, node_id: i64, slug: &str, worktree_path: Option<&str>);

    /// Schedule the single post-spawn pool-maintenance task. Called only
    /// when `ref_advanced_for_pool || pool_was_drained_by_this_spawn`;
    /// refreshed and refilled under one fill-lock acquisition so the two
    /// jobs can't lose a race to each other (issue #613).
    fn on_pool_maintenance_required(&self, mesh_id: i64, do_refresh: bool, do_refill: bool);
}

/// Production sink: emits via `AppHandle::emit`, writes through the DB,
/// and spawns the maintenance thread via `std::thread::spawn`. Owns a clone
/// of the AppHandle so the provisioner's `&dyn ProvisionSink` parameter is
/// lifetime-free.
///
/// All DB-touching methods gate on `db::is_initialized()` — when the DB
/// is not initialised (e.g. a unit test that doesn't spin one up) the
/// sink is a no-op for the DB side of each method. This keeps the
/// provisioner itself free of `is_initialized` checks; if the seam needs
/// to add a new DB side effect in the future, the sink owns where the
/// gate lives.
pub(crate) struct AppHandleSink {
    pub(crate) app: tauri::AppHandle,
    /// The pool-agnostic claim seam (issue #2533). The sink reaches the
    /// pre-spawn pool through this consumer-owned interface rather than by
    /// naming the pool's module — that is what keeps the dependency direction
    /// one way (`services/` → `git/`).
    pub(crate) warm_claim: Box<dyn super::warm::WarmClaim>,
}

impl ProvisionSink for AppHandleSink {
    fn return_warm_row(
        &self,
        entry: &super::warm::ClaimedWarmEntry,
        mesh_id: i64,
    ) -> Result<(), String> {
        crate::db::mark_warm_worktree_available(entry.id, entry.base_sha.as_deref())
            .map_err(|e| e.to_string())?;
        self.warm_claim.emit_pool_changed(mesh_id);
        Ok(())
    }
    fn forget_warm_row(&self, id: i64) {
        if !crate::db::is_initialized() {
            return;
        }
        self.warm_claim.mark_forgotten(id);
    }

    fn adopt_manual_slug(&self, node_id: i64, slug: &str, worktree_path: Option<&str>) {
        // DB write first; emit on success only. A failed DB write leaves
        // the row's identity stale, which the next manual rename (or
        // stage-2 reconcile) can correct — but a stale `node-renamed` emit
        // would also re-label the frontend's optimistic stage-1 row to a
        // value the DB doesn't carry, which is the harder case to undo.
        //
        // `adopt_manual_pool_slug_with_path` writes `name`, `worktree_name`
        // AND `worktree_path` together (issue #1519). Do NOT narrow this to
        // a name-only update: the close path derives its removal directory
        // from `worktree_name`/`worktree_path`, so a name-only adoption
        // silently leaks the worktree on every close (#1080).
        if crate::db::is_initialized() {
            if let Err(e) =
                crate::db::adopt_manual_pool_slug_with_path(node_id, slug, worktree_path)
            {
                tracing::warn!(
                    "AppHandleSink::adopt_manual_slug: DB write failed for node {} ({}); \
                     skipping the node-renamed emit so the frontend can't re-label to a value the DB doesn't carry",
                    node_id, e
                );
                return;
            }
        }
        let _ = self.app.emit(
            "node-renamed",
            crate::session_naming::NodeRenamedPayload {
                node_id,
                name: slug.to_string(),
            },
        );
    }

    fn on_pool_maintenance_required(&self, mesh_id: i64, do_refresh: bool, do_refill: bool) {
        // mesh_id==0 means "no mesh row resolved"; the orchestrator gates the
        // caller on this too, but the sink re-checks because the threshold
        // is cheap and silently skipping a no-mesh maintenance call is the
        // class of bug that ships a "my pool never refills" report.
        if mesh_id <= 0 || !(do_refresh || do_refill) {
            return;
        }
        // Gate on DB initialisation so unit tests (no DB) don't spawn a thread
        // that would immediately panic on the warm-mesh lookup.
        if !crate::db::is_initialized() {
            return;
        }
        // The pool owns the maintenance task (the refresh pass, the refill and
        // the thread that runs them). The seam schedules it; it does not
        // reimplement it.
        self.warm_claim
            .schedule_maintenance(mesh_id, do_refresh, do_refill);
    }
}

/// The four-way decision `provision_for_spawn` returns. Each variant names the
/// on-disk outcome, not the request — an Issue spawn that didn't get a warm
/// entry lands on `Created`, not `Adopted`.
///
/// `host_path` is carried on every variant so the caller can drive the rest
/// of the spawn (PTY open, sandbox dispatch) without re-deriving it. `entry`
/// is only present on the warm outcomes; the caller uses it to drive the
/// post-spawn `forget_after_spawn` and name-adoption bookkeeping. `elapsed_ms`
/// is the wall-clock cost of the worktree-doing branch; `Reused` is the
/// no-op short-circuit and carries no timing (the elapsed value would just be
/// `Path::exists()`'s negligible cost).
///
/// `host_path` and `elapsed_ms` are part of the public API contract — the
/// orchestrator currently matches on the variant name (not the payload), but
/// future callers (telemetry, post-spawn logging, unit tests asserting
/// per-branch timing) read them. Trimming the payload would change the seam the
/// next caller writes against, so the lint is allowed here with that reason
/// rather than the fields removed (issue #2450).
#[derive(Debug)]
#[allow(dead_code)]
pub enum ProvisionOutcome {
    /// No worktree was created or moved — the existing path already exists
    /// (resume / handover / re-spawn of an existing worktree) or the spawn
    /// is a Root Node (`use_worktree = false`).
    Reused { host_path: String },
    /// Issue/PR spawn adopted a warm entry via `git worktree move` +
    /// `git checkout -b <branch> <base_sha>`. `entry` is the claim the
    /// caller hands to `warm_pool::forget_after_spawn` after registration.
    Adopted {
        entry: super::warm::ClaimedWarmEntry,
        host_path: String,
        elapsed_ms: u64,
    },
    /// Manual spawn adopted a warm entry in place — the pool's pre-assigned
    /// slug is the node's name; `git checkout -b <branch>` aligned the
    /// worktree's mode with the mesh's `worktree_mode` and re-applied
    /// `.worktreeinclude`. `entry` is the claim the caller hands to
    /// `warm_pool::forget_after_spawn` AND uses as the source for the manual
    /// name adoption (the warm entry's `preassigned_name` overwrites the
    /// node's stage-1 throwaway slug).
    Upgraded {
        entry: super::warm::ClaimedWarmEntry,
        host_path: String,
        elapsed_ms: u64,
    },
    /// Cold create via `crate::git::worktree::create_git_worktree` — the
    /// pool was empty, or the spawn didn't qualify for a claim (resume,
    /// Root Node already covered by `Reused`, etc.).
    Created { host_path: String, elapsed_ms: u64 },
}

/// Resolve a `SpawnContext` to an on-disk worktree state. Returns one of
/// `Reused` / `Adopted` / `Upgraded` / `Created`; the orchestrator reads
/// only `host_path` (for command construction) and the per-branch elapsed
/// timing (for the spawn_timer log) — the post-success bookkeeping
/// (`forget_after_spawn`, Manual name adoption, `post_spawn_maintenance`
/// trigger) is owned by this function and dispatches through [`ProvisionSink`].
///
/// Issue/PR warm failures use the ownership-aware cold fallback. A refused
/// manual upgrade returns its claim to the pool and reports an error instead
/// of launching detached or resetting an existing branch.
///
/// Decision order is deliberate and matches the existing spawn path:
/// 1. `use_worktree == false` → Root Node, the spawn runs in the mesh root
///    directly and no worktree is needed. `Reused`.
/// 2. `warm_entry` is `Some`:
///    - Manual: the pool's pre-assigned slug is the node's `worktree_name`,
///      so the pool directory IS already at `host_path`. The path-exists
///      check below must NOT short-circuit — the orchestrator relies on
///      `Upgraded` to land the `git checkout -b <branch>` and the
///      `.worktreeinclude` re-application. `Upgraded`.
///    - Issue/PR: the pool directory sits at the plain-slug path, and
///      `host_path` is the node's `gh{N}-`/`pr{N}-` target (does NOT exist
///      yet). Move the pool directory to the target, then `git checkout`
///      onto `base_ref`. `Adopted`.
/// 3. The host path is a materialized linked checkout → resume / handover / re-spawn; never
///    re-create. `Reused`.
/// 4. No warm entry: cold `create_git_worktree`. `Created`.
pub fn provision_for_spawn(
    mut ctx: SpawnContext,
    hooks: &ProvisionHooks,
    sink: &dyn ProvisionSink,
) -> Result<ProvisionOutcome, String> {
    // 1. Root Node short-circuit (`use_worktree = false`). No worktree is created
    //    or moved; the spawn runs in the mesh root directly. `Reused`. Runs
    //    BEFORE the warm/cold branches so a Root Node with a stale warm claim
    //    (extremely rare but possible if `use_worktree` flipped between claim
    //    and provision) doesn't accidentally adopt the entry.
    if !ctx.use_worktree {
        return Ok(ProvisionOutcome::Reused {
            host_path: ctx.host_path,
        });
    }

    // ---- Warm path: capture the entry so a failure can clean up both
    //      possible paths before the cold retry (idempotent — whichever
    //      was untouched no-ops). The original entry is moved into the
    //      warm branch and either consumed (warm Ok → forget + name) or
    //      restored for the cleanup block below (warm Err).
    // Snapshot the target BEFORE this call touches the disk. The warm-failure
    // cleanup below must only remove a target that THIS call created, and
    // "the target exists now" is not enough to prove that: a node whose name
    // collides with an existing node resolves to that node's worktree path, so
    // the path can pre-exist for reasons that have nothing to do with the move
    // (see `session_naming::disambiguate_node_name`). Removing it in that case
    // deletes a live agent's working directory — uncommitted work included.
    let target_preexisted = std::path::Path::new(&ctx.host_path).exists();
    let warm_entry_for_cleanup = ctx.warm_entry.take();

    // Drive adoption from the CLAIM, not the outcome. A Manual warm claim
    // resolves to `Upgraded` on the happy path, but a warm failure falls
    // back to a cold create at the same `ctx.host_path` — which is still
    // the pool-slug directory, because the orchestrator resolved
    // `host_path` from the claimed slug before calling us. Keying
    // adoption off `Upgraded` alone would leave the cold-fallback spawn's
    // row pointing at the stage-1 throwaway slug — the same silent
    // worktree leak as #1080, just via a rarer path.
    //
    // Scope: this extends beyond issue #1080's literal "next to the
    // existing adoption site" wording — the issue named the `Upgraded`
    // outcome, not the claim. The extension is intentional: the
    // cold-fallback is the same bug class. Tracking issue for the
    // scope line lives in #1080's PR description.
    let manual_pool_slug: Option<String> = match (&ctx.source, &warm_entry_for_cleanup) {
        (SpawnSource::Manual, Some(entry)) => Some(entry.preassigned_name.clone()),
        _ => None,
    };

    let warm_result: Result<Option<ProvisionOutcome>, (String, super::warm::ClaimedWarmEntry)> =
        if let Some(entry) = warm_entry_for_cleanup {
            // Manual spawns adopt the pool's pre-assigned slug (it's the node's
            // `worktree_name` by the time this fn runs — see orchestrator's
            // pre-call rename); Issue/PR spawns keep the node's own deterministic
            // `gh{N}-`/`pr{N}-` name.
            let branch_name = ctx
                .node
                .worktree_name
                .clone()
                .unwrap_or_else(|| "buildmesh-spawn".to_string());
            let start = std::time::Instant::now();
            // `entry.clone()` borrows from `entry` so the original stays in scope
            // for the warm-failure cleanup branch below — only one of
            // `outcome_result = Ok(...)` or `Err((e, entry))` runs, but Rust's
            // borrow checker tracks moves statically, so we must clone (entry
            // holds 4 short strings per `ClaimedWarmEntry` — cheap).
            let outcome_result: Result<ProvisionOutcome, String> = match ctx.source {
                SpawnSource::Issue | SpawnSource::PullRequest => adopt_warm_worktree_by_move(
                    &ctx.node.path,
                    &entry.path,
                    &ctx.host_path,
                    &branch_name,
                    &ctx.worktree_mode,
                    &ctx.base_ref,
                )
                .map(|()| ProvisionOutcome::Adopted {
                    entry: entry.clone(),
                    host_path: ctx.host_path.clone(),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                }),
                SpawnSource::Manual => upgrade_warm_to_mode(
                    &ctx.node.path,
                    &ctx.host_path,
                    &branch_name,
                    &ctx.worktree_mode,
                )
                .map(|()| ProvisionOutcome::Upgraded {
                    entry: entry.clone(),
                    host_path: ctx.host_path.clone(),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                }),
            };
            match outcome_result {
                Ok(o) => Ok(Some(o)),
                Err(e) => Err((e, entry)),
            }
        } else {
            Ok(None) // no warm entry — drive the cold-path branches below
        };

    let outcome = match warm_result {
        Ok(Some(o)) => Ok(o),
        Ok(None) => {
            // No warm entry. Two cold branches: path-exists (resume) or genuine create.
            if crate::git::project::is_materialized_checkout(&ctx.host_path) {
                Ok(ProvisionOutcome::Reused {
                    host_path: ctx.host_path.clone(),
                })
            } else {
                let branch_name = ctx
                    .node
                    .worktree_name
                    .clone()
                    .unwrap_or_else(|| "buildmesh-spawn".to_string());
                let start = std::time::Instant::now();
                let create_result = super::create_git_worktree(
                    &ctx.node.path,
                    &ctx.host_path,
                    &branch_name,
                    &ctx.worktree_mode,
                    &ctx.base_ref,
                )
                .map(|()| ProvisionOutcome::Created {
                    host_path: ctx.host_path.clone(),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
                match create_result {
                    Ok(o) => Ok(o),
                    Err(e) => Err(format!("provision_for_spawn: cold create failed: {}", e)),
                }
            }
        }
        Err((warm_err, entry)) => {
            // A refused manual branch creation must not become a successful
            // detached launch via the materialized-path fallback.
            if ctx.source == SpawnSource::Manual {
                sink.return_warm_row(&entry, ctx.node.mesh_id)
                    .map_err(|error| format!("{warm_err}; cannot return pool claim: {error}"))?;
                return Err(format!(
                    "provision_for_spawn: manual warm upgrade failed: {warm_err}"
                ));
            }
            // Warm path failed. The failure happens in one of three places:
            //   (a) the base-SHA resolution failed — disk untouched.
            //   (b) pre-move branch guard refused (e.g. branch-exists) — disk untouched.
            //   (c) move_git_worktree failed — disk untouched (atomic at the
            //       file-system level; git CLI either moves and exits 0 or
            //       returns non-zero without moving).
            //   (d) the claim's checkout failed AFTER the move succeeded —
            //       `host_path` is now populated with a broken worktree that
            //       needs cleanup; `entry.path` (pool) was emptied by the move.
            //
            // The first three cases (a, b, c) leave the pool directory intact —
            // removing it would silently destroy work the user may want to keep
            // (the pre-move-guard test `provision_for_spawn_adopted_refuses_to_clobber_existing_branch`
            // pins this). Only (d) needs target cleanup, and only when the
            // target is one THIS call materialised: `move_git_worktree` is the
            // sole step that populates it and it is atomic, but a pre-existing
            // target means another node already owns that path (a name
            // collision — see `target_preexisted` above). Deleting it would
            // remove that node's working directory, so the `Reused` branch
            // below hands the path back instead.
            let row_id = entry.id;
            let target_exists = std::path::Path::new(&ctx.host_path).exists();
            if target_exists && !target_preexisted {
                let _ = super::remove_one_worktree(&ctx.host_path);
            }
            // Forget the warm row via the sink — same encapsulation as the
            // success path. The sink's `db::is_initialized` gate does the
            // right thing whether or not the test/caller has a DB.
            sink.forget_warm_row(row_id);

            // Cold retry — same SpawnContext with `warm_entry` already cleared.
            // If THIS fails too the spawn surfaces a combined error so the caller
            // can persist a real diagnostic instead of a half-failed warm retry.
            if crate::git::project::is_materialized_checkout(&ctx.host_path) {
                // Race: another thread cut the worktree between our warm failure
                // and now. Treat as a resume rather than a create.
                Ok(ProvisionOutcome::Reused {
                    host_path: ctx.host_path.clone(),
                })
            } else {
                let branch_name = ctx
                    .node
                    .worktree_name
                    .clone()
                    .unwrap_or_else(|| "buildmesh-spawn".to_string());
                let start = std::time::Instant::now();
                match super::create_git_worktree(
                    &ctx.node.path,
                    &ctx.host_path,
                    &branch_name,
                    &ctx.worktree_mode,
                    &ctx.base_ref,
                ) {
                    Ok(()) => Ok(ProvisionOutcome::Created {
                        host_path: ctx.host_path.clone(),
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    }),
                    Err(cold_err) => Err(format!(
                        "provision_for_spawn: warm failed ({}) and cold fallback failed ({})",
                        warm_err, cold_err
                    )),
                }
            }
        }
    }?;

    // ---- Post-success bookkeeping (issue #609, #613, #653, #1080 semantics).
    //
    // The provisioner owns this fan-out now (deepening). On `Reused` and
    // `Created` no warm row exists to forget, so only the warm outcomes
    // (`Adopted` / `Upgraded`) call `sink.forget_warm_row`.
    //
    // Slug adoption is deliberately NOT part of this match — it is driven by
    // `manual_pool_slug` (the claim) below, because a Manual claim that fell
    // back to a cold create still runs in the pool-slug directory and still
    // needs its row reconciled (#1080).
    //
    // The provisioner is therefore DB-free — the sink encapsulates every
    // `db::is_initialized` gate. Adding new side effects on a future
    // post-success expansion is a sink-method change, not a provisioner
    // change.
    match &outcome {
        ProvisionOutcome::Adopted { entry, .. } => {
            sink.forget_warm_row(entry.id);
        }
        ProvisionOutcome::Upgraded { entry, .. } => {
            sink.forget_warm_row(entry.id);
        }
        ProvisionOutcome::Reused { .. } | ProvisionOutcome::Created { .. } => {
            // No warm entry was adopted here — nothing to forget. (A warm
            // failure already forgot its row in the `Err` branch above.)
        }
    }

    // Reconcile the row's identity with the directory the agent will actually
    // run in. Issue/PR spawns keep their own `gh{N}-`/`pr{N}-` identity — the
    // pool directory was moved to match them instead — so `manual_pool_slug`
    // is `None` for those and this is a no-op.
    // Issue #1519: also persist the in-memory `worktree_path` (raw form,
    // aligned in `prepare_context` to the claimed entry) so the DB row
    // agrees with the on-disk directory even under a custom container.
    if let Some(slug) = &manual_pool_slug {
        sink.adopt_manual_slug(ctx.node.id, slug, ctx.node.worktree_path.as_deref());
    }

    let do_refresh = hooks.ref_advanced_for_pool;
    let do_refill = hooks.pool_was_drained_by_this_spawn;
    // Only fire the maintenance hook when there's actual work to do — the
    // sink implementations use this as the no-op gate (the recorder reads
    // `maintenance_calls()` directly, so an extra no-op call would pollute
    // the test assertions about "no maintenance scheduled without flags").
    if do_refresh || do_refill {
        sink.on_pool_maintenance_required(ctx.node.mesh_id, do_refresh, do_refill);
    }

    if let Some(scope) = ctx.node.project_scope.as_deref() {
        crate::env::validate_project_scope(scope)?;
        let cwd = std::path::Path::new(&ctx.host_path).join(scope);
        if !cwd.is_dir() {
            return Err(format!("Project folder '{scope}' is missing from checkout at {} (ref '{}'); restore this folder or create a new node from a ref containing it", ctx.host_path, ctx.base_ref));
        }
    }

    Ok(outcome)
}

// Note: production callers should prefer `AppHandleSink` (owning an
// `AppHandle`) over passing `&NullSink` so the side-effect surface is
// explicit at the call site. `NullSink` is the test-time escape hatch
// for fixtures that don't exercise post-success hooks.

#[cfg(test)]
#[path = "provision_tests.rs"]
mod tests;
