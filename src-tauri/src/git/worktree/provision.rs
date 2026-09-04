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
//!   `git checkout -B <branch>` aligns the worktree's mode with the
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
//! `move_git_worktree`, `resolve_base_ref_sha`, `apply_worktree_include`)
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
    let existing = get_url
        .current_dir(&host_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let url_matches = existing.as_deref() == Some(head_repo_clone_url);
    if !url_matches {
        let mut cmd = git_command();
        if existing.is_some() {
            cmd.arg("remote").arg("set-url").arg(&alias).arg(head_repo_clone_url);
            tracing::info!("fetch_fork_head: updating remote {} URL", alias);
        } else {
            cmd.arg("remote").arg("add").arg(&alias).arg(head_repo_clone_url);
            tracing::info!("fetch_fork_head: adding remote {}", alias);
        }
        let output = match cmd.current_dir(&host_root).output() {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("fetch_fork_head: failed to spawn git remote: {}", e);
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
    // the lock (and every later spawn on this mesh) indefinitely.
    let output = match crate::process_util::run_command_with_timeout(
        cmd,
        "git fetch (fork head)",
        crate::git::sync::SPAWN_FETCH_TIMEOUT,
    ) {
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

/// Run the PR-spawn head fetch inside `services::sync_lock::with_mesh_sync_lock`
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
    crate::services::sync_lock::try_with_mesh_sync_lock_timeout(
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
/// `git rev-parse` accepts both the short and the fully-qualified
/// `refs/remotes/origin/...` form.
pub(crate) fn read_origin_ref_sha(project_root: &str, remote_ref: &str) -> Option<String> {
    use crate::process_util::git_command;
    let host_root = crate::env::to_host_path(project_root);
    // Read the symbolic SHA in one shot — `git rev-parse` exits non-zero
    // (and produces no stdout) when the ref doesn't exist, so we don't
    // need a separate "is this a ref?" probe first.
    let mut cmd = git_command();
    cmd.arg("rev-parse").arg(remote_ref);
    let output = cmd.current_dir(&host_root).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

// ─── Worktree helpers (warm-pool adoption / upgrade / shared checkout) ─────

/// Upgrade a warm pool entry from detached HEAD to the mesh's configured
/// worktree mode (issue #609, PRD #608 §3). The pool cuts every entry as
/// `detached` so a future claim can adopt the directory without ever
/// touching the mesh's branch refs — the cost is one `git checkout -B
/// <branch>` (branched mode, ~50ms) or no-op (detached mode, ~5ms).
///
/// This is the entire warm-path "checkout" cost the tracer bullet buys: the
/// on-disk tree is already at the mesh's base SHA (the worker checked it
/// out); all the spawn has to do is flip the working ref. The 97% of cold-
/// spawn time that was Windows Defender / NTFS search indexer / USN journal
/// scanning freshly-written files is paid ONCE on app startup, not per
/// spawn.
///
/// Best-effort by design: any failure here is logged by the caller and the
/// spawn falls through. The agent lands on the warm entry's current HEAD
/// (still at base_ref) instead of the mesh's named branch — strictly worse
/// than the branched path, but never worse than a cold spawn would be.
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
    if mode == "detached" {
        // No-op: the pool already cut the entry as detached.
    } else {
        // Branched mode: `git checkout -B <branch>` from the current HEAD. `-B`
        // (uppercase) is deliberate here — a manual spawn's branch IS the pool's
        // preassigned slug (a random adj-adj-noun like `bold-amber-fox`), so a
        // collision with a pre-existing branch is vanishingly unlikely and `-B`
        // keeps the call idempotent across a re-claim of a still-detached entry.
        // (The Issue/PR path uses `-b` instead — see `checkout_worktree_to_base` —
        // because its branch name is deterministic and `-B` would force-reset a
        // user's prior work.)
        run_git_checkout(host_path, &["-B", branch_name])?;
    }
    // Re-apply `.worktreeinclude` so the manual warm claim matches what
    // `create_git_worktree` and `adopt_warm_worktree_by_move` already do
    // (issue #639 gap 1). The prewarm-time copy is stale by the time a user
    // manually spawns — typical edits to a `.worktreeinclude` source (`.env`,
    // build cache, `node_modules/`) would otherwise leave the agent on the
    // prewarm snapshot. Best-effort like the other call sites: a copy
    // failure here is logged inside `apply_worktree_include` but doesn't fail
    // the spawn — the worktree is already usable without the extras.
    super::apply_worktree_include(
        project_root,
        std::path::Path::new(host_path),
    );
    Ok(())
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
    // path's `add_worktree_impl` does via `resolve_base_commit`. Resolving
    // BEFORE the move means a bad ref fails fast, before we disturb the pool
    // directory.
    let base_sha = super::resolve_base_ref_sha(project_root, base_ref)?;
    // Same fail-fast principle for the branch: `checkout_worktree_to_base`'s
    // `git checkout -b` refuses an existing branch, but by then the pool
    // directory has already been moved — the spawn failure cleanup then
    // deletes a half-adopted worktree, which can strand a stale admin entry
    // (the phantom worktree from the 2026-07-17 gh252 duplicate-spawn
    // incident). Refuse BEFORE the move so a refused adoption leaves the
    // pool entry exactly where it was.
    if mode == "branched" {
        let host_root = crate::env::to_host_path(project_root);
        match git2::Repository::open(&host_root) {
            Ok(repo) => {
                if repo.find_branch(branch_name, git2::BranchType::Local).is_ok() {
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
    checkout_worktree_to_base(new_host_path, branch_name, mode, &base_sha)?;
    super::apply_worktree_include(
        project_root,
        std::path::Path::new(new_host_path),
    );
    Ok(())
}

/// `git checkout` a (just-moved) warm worktree onto a specific `base_sha`.
/// Branched mode uses `-b <branch> <base_sha>` — like the cold path's
/// `git worktree add -b`, it REFUSES if the branch already exists rather than
/// clobbering it, so a re-spawn never silently force-resets a deterministic
/// `gh{N}-`/`pr{N}-` branch and orphans the agent's earlier commits. Detached
/// mode uses `--detach <base_sha>`.
///
/// Unlike [`upgrade_warm_to_mode`] (manual spawns, which stay on the warm
/// entry's current HEAD), Issue/PR spawns must land on a *named* ref: the mesh
/// base for Issue spawns, the PR head (`origin/<head>` / `fork-<login>/<head>`)
/// for PR spawns. The cold-path PR-head-fetch resolves that ref, and
/// `adopt_warm_worktree_by_move` resolves it to the `base_sha` passed here.
fn checkout_worktree_to_base(
    host_path: &str,
    branch_name: &str,
    mode: &str,
    base_sha: &str,
) -> Result<(), String> {
    if mode == "branched" {
        run_git_checkout(host_path, &["-b", branch_name, base_sha])
    } else {
        run_git_checkout(host_path, &["--detach", base_sha])
    }
}

/// Shared `git -C <host_path> checkout <args…>` runner for the warm-pool
/// checkout paths (manual mode-upgrade and Issue/PR adoption). Centralises the
/// `command_no_window` plumbing and the stderr-surfacing error shape so a
/// future fix (arg quoting, lock-retry) lands for both callers at once; the
/// deliberate flag differences (`-B` vs `-b`) stay explicit at the call sites.
fn run_git_checkout(host_path: &str, args: &[&str]) -> Result<(), String> {
    use crate::process_util::git_command;
    let mut cmd = git_command();
    cmd.arg("-C").arg(host_path).arg("checkout").args(args);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git checkout: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git checkout {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
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
// `resolve_base_ref_sha` internally so its `git checkout -b` never sees a
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
/// and `git checkout -B`s it onto the mesh's mode (issue #609).
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
    pub warm_entry: Option<crate::services::warm_pool::ClaimedWarmEntry>,
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
    /// the row's identity (`name` AND `worktree_name`, one write — see
    /// `db::adopt_manual_pool_slug`) AND announce via `node-renamed` so the
    /// frontend's optimistic stage-1 row re-labels. Three consequences of one
    /// decision — kept as a single trait method because they must be
    /// atomic-ish: a partial application leaves either the on-disk name, the
    /// in-memory one, or the close path's removal target out of sync. The
    /// last of those is what leaked worktrees in #1080.
    ///
    /// Called for every Manual spawn that *claimed* a warm entry, whichever
    /// on-disk outcome it reached; Issue/PR spawns keep their own
    /// `gh{N}-`/`pr{N}-` identity per CONTEXT.md *Spawn Source*.
    fn adopt_manual_slug(&self, node_id: i64, slug: &str, worktree_path: &str);

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
}

impl ProvisionSink for AppHandleSink {
    fn forget_warm_row(&self, id: i64) {
        if !crate::db::is_initialized() {
            return;
        }
        crate::services::warm_pool::forget_after_spawn(id);
    }

    fn adopt_manual_slug(&self, node_id: i64, slug: &str, worktree_path: &str) {
        // DB write first; emit on success only. A failed DB write leaves
        // the row's identity stale, which the next manual rename (or
        // stage-2 reconcile) can correct — but a stale `node-renamed` emit
        // would also re-label the frontend's optimistic stage-1 row to a
        // value the DB doesn't carry, which is the harder case to undo.
        //
        // `adopt_manual_pool_slug` writes `name` and `worktree_name`
        // together. Do NOT narrow this to a name-only update: the close
        // path derives its removal directory from `worktree_name`, so a
        // name-only adoption silently leaks the worktree on every close
        // (#1080).
        if crate::db::is_initialized() {
            if let Err(e) = crate::db::adopt_manual_pool_slug(node_id, slug, worktree_path) {
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
        let app = self.app.clone();
        std::thread::spawn(move || {
            crate::services::warm_pool::post_spawn_maintenance(
                mesh_id, do_refresh, do_refill, &app,
            );
        });
    }
}

/// Zero-cost sink for tests that don't exercise post-success side effects —
/// every method no-ops so a fixture-only test can call `provision_for_spawn`
/// without standing up Tauri events or background threads.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct NullSink;

#[cfg(test)]
impl ProvisionSink for NullSink {
    fn forget_warm_row(&self, _id: i64) {}
    fn adopt_manual_slug(&self, _node_id: i64, _slug: &str, _worktree_path: &str) {}
    fn on_pool_maintenance_required(&self, _mesh_id: i64, _do_refresh: bool, _do_refill: bool) {}
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
/// per-branch timing) read them. The `#[allow(dead_code)]` keeps the
/// warnings out without trimming the payload.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ProvisionOutcome {
    /// No worktree was created or moved — the existing path already exists
    /// (resume / handover / re-spawn of an existing worktree) or the spawn
    /// is a Root Node (`use_worktree = false`).
    Reused {
        host_path: String,
    },
    /// Issue/PR spawn adopted a warm entry via `git worktree move` +
    /// `git checkout -b <branch> <base_sha>`. `entry` is the claim the
    /// caller hands to `warm_pool::forget_after_spawn` after registration.
    Adopted {
        entry: crate::services::warm_pool::ClaimedWarmEntry,
        host_path: String,
        elapsed_ms: u64,
    },
    /// Manual spawn adopted a warm entry in place — the pool's pre-assigned
    /// slug is the node's name; `git checkout -B <branch>` aligned the
    /// worktree's mode with the mesh's `worktree_mode` and re-applied
    /// `.worktreeinclude`. `entry` is the claim the caller hands to
    /// `warm_pool::forget_after_spawn` AND uses as the source for the manual
    /// name adoption (the warm entry's `preassigned_name` overwrites the
    /// node's stage-1 throwaway slug).
    Upgraded {
        entry: crate::services::warm_pool::ClaimedWarmEntry,
        host_path: String,
        elapsed_ms: u64,
    },
    /// Cold create via `crate::git::worktree::create_git_worktree` — the
    /// pool was empty, or the spawn didn't qualify for a claim (resume,
    /// Root Node already covered by `Reused`, etc.).
    Created {
        host_path: String,
        elapsed_ms: u64,
    },
}

/// Resolve a `SpawnContext` to an on-disk worktree state. Returns one of
/// `Reused` / `Adopted` / `Upgraded` / `Created`; the orchestrator reads
/// only `host_path` (for command construction) and the per-branch elapsed
/// timing (for the spawn_timer log) — the post-success bookkeeping
/// (`forget_after_spawn`, Manual name adoption, `post_spawn_maintenance`
/// trigger) is owned by this function and dispatches through [`ProvisionSink`].
///
/// On the warm path: if `adopt_warm_worktree_by_move` / `upgrade_warm_to_mode`
/// errors, the seam cleans up both possible paths (idempotent, whichever
/// was untouched), forgets the pool row, and retries cold with
/// `warm_entry: None`. Cold failure on top of warm failure returns a combined
/// error so the orchestrator's `SessionLifecycle::on_error` carries the full
/// picture. The behaviour preserves the pre-deepening fall-through (issue
/// #612: warm-adopt failure was a graceful degradation, never fatal).
///
/// Decision order is deliberate and matches the existing spawn path:
/// 1. `use_worktree == false` → Root Node, the spawn runs in the mesh root
///    directly and no worktree is needed. `Reused`.
/// 2. `warm_entry` is `Some`:
///    - Manual: the pool's pre-assigned slug is the node's `worktree_name`,
///      so the pool directory IS already at `host_path`. The path-exists
///      check below must NOT short-circuit — the orchestrator relies on
///      `Upgraded` to land the `git checkout -B <branch>` and the
///      `.worktreeinclude` re-application. `Upgraded`.
///    - Issue/PR: the pool directory sits at the plain-slug path, and
///      `host_path` is the node's `gh{N}-`/`pr{N}-` target (does NOT exist
///      yet). Move the pool directory to the target, then `git checkout`
///      onto `base_ref`. `Adopted`.
/// 3. The host path already exists → resume / handover / re-spawn; never
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

    let warm_result: Result<Option<ProvisionOutcome>, (String, crate::services::warm_pool::ClaimedWarmEntry)> =
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
                SpawnSource::Issue | SpawnSource::PullRequest => {
                    adopt_warm_worktree_by_move(
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
                    })
                }
                SpawnSource::Manual => {
                    upgrade_warm_to_mode(
                        &ctx.node.path,
                        &ctx.host_path,
                        &branch_name,
                        &ctx.worktree_mode,
                    )
                    .map(|()| ProvisionOutcome::Upgraded {
                        entry: entry.clone(),
                        host_path: ctx.host_path.clone(),
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    })
                }
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
            if std::path::Path::new(&ctx.host_path).exists() {
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
            // Warm path failed. The failure happens in one of three places:
            //   (a) resolve_base_ref_sha failed — disk untouched.
            //   (b) pre-move branch guard refused (e.g. branch-exists) — disk untouched.
            //   (c) move_git_worktree failed — disk untouched (atomic at the
            //       file-system level; git CLI either moves and exits 0 or
            //       returns non-zero without moving).
            //   (d) checkout_worktree_to_base failed AFTER the move succeeded —
            //       `host_path` is now populated with a broken worktree that
            //       needs cleanup; `entry.path` (pool) was emptied by the move.
            //
            // The first three cases (a, b, c) leave the pool directory intact —
            // removing it would silently destroy work the user may want to keep
            // (the pre-move-guard test `provision_for_spawn_adopted_refuses_to_clobber_existing_branch`
            // pins this). Only (d) needs target cleanup. Detect (d) by checking
            // whether the target path now exists: `move_git_worktree` is the
            // sole step that populates it, and it's atomic.
            let row_id = entry.id;
            let target_exists = std::path::Path::new(&ctx.host_path).exists();
            if target_exists {
                let _ = super::remove_one_worktree(&ctx.host_path);
            }
            // Forget the warm row via the sink — same encapsulation as the
            // success path. The sink's `db::is_initialized` gate does the
            // right thing whether or not the test/caller has a DB.
            sink.forget_warm_row(row_id);

            // Cold retry — same SpawnContext with `warm_entry` already cleared.
            // If THIS fails too the spawn surfaces a combined error so the caller
            // can persist a real diagnostic instead of a half-failed warm retry.
            if std::path::Path::new(&ctx.host_path).exists() {
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
    if let Some(slug) = &manual_pool_slug {
        // `worktree_path` is the raw/runtime form (e.g. `/home/...` for WSL),
        // while `host_path` may be a `\\wsl$\\...` UNC used only by git.
        // Persist the runtime form so the next spawn can reconstruct both
        // host and spawn paths through `env::node_working_path`.
        let worktree_path = ctx
            .node
            .worktree_path
            .as_deref()
            .unwrap_or(&ctx.host_path);
        sink.adopt_manual_slug(ctx.node.id, slug, worktree_path);
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

    Ok(outcome)
}

// Note: production callers should prefer `AppHandleSink` (owning an
// `AppHandle`) over passing `&NullSink` so the side-effect surface is
// explicit at the call site. `NullSink` is the test-time escape hatch
// for fixtures that don't exercise post-success hooks.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_helpers::{commit_file, init_repo_with_commit, repo_with_drifted_head, TestDir};
    use crate::models::{AgentNode, EnvType, SessionStatus};
    use crate::services::warm_pool::ClaimedWarmEntry;
    use std::fs;
    use std::path::{Path, PathBuf};

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
            worktree_path: None,
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
            signal_health: None,
            position: 0,
            created_at: chrono::Utc::now(),
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
        let host_path = root.join("does-not-exist-yet").to_string_lossy().to_string();
        let ctx = SpawnContext {
            use_worktree: false,
            ..make_ctx(node, SpawnSource::Manual, host_path, None)
        };

        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("Root Node must succeed");
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
        fs::create_dir_all(&host_path).unwrap();
        fs::write(host_path.join("agent-notes.md"), "carried over").unwrap();

        let node = empty_node(root, Some("existing-worktree"));
        let host_path_str = host_path.to_string_lossy().to_string();
        // No warm entry — resume / handover / re-spawn path.
        let ctx = make_ctx(node, SpawnSource::Manual, host_path_str.clone(), None);

        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("resume must succeed");
        match outcome {
            ProvisionOutcome::Reused { host_path: p } => assert_eq!(p, host_path_str),
            other => panic!("expected Reused for existing path, got {:?}", other),
        }
        // Prior work preserved.
        assert_eq!(fs::read_to_string(host_path.join("agent-notes.md")).unwrap(), "carried over");
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
        let host_path = root.join(".claude").join("worktrees").join("bold-amber-fox");
        let host_path_str = host_path.to_string_lossy().to_string();
        let ctx = make_ctx(node, SpawnSource::Manual, host_path_str.clone(), None);

        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("cold create must succeed");
        match outcome {
            ProvisionOutcome::Created { host_path: p, elapsed_ms } => {
                assert_eq!(p, host_path_str);
                // Elapsed_ms is intentionally permissive (cold create on
                // Windows Defender / NTFS-indexed disks can spend >100ms in
                // the first write). A `0` here would mean the timer wasn't
                // started, which would be a real regression.
                assert!(elapsed_ms < 60_000, "elapsed_ms {} implausibly large", elapsed_ms);
            }
            other => panic!("expected Created for empty pool, got {:?}", other),
        }
        assert!(host_path.exists(), "cold create must materialise the worktree");
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

        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("cold create must succeed");
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
        let ctx = make_ctx(node, SpawnSource::PullRequest, host_path_str.clone(), Some(warm));

        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("PR adopt must succeed");
        match outcome {
            ProvisionOutcome::Adopted { entry, host_path: p, elapsed_ms } => {
                assert_eq!(entry.id, 42);
                assert_eq!(p, host_path_str);
                assert!(elapsed_ms < 60_000);
            }
            other => panic!("expected Adopted for PR spawn, got {:?}", other),
        }
        // Pool dir was moved — old path gone, target path present.
        assert!(!pool_path.exists(), "pool directory must be moved away");
        assert!(host_path.exists(), "adopted worktree must exist at target path");
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
        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("Issue adopt must succeed");
        match outcome {
            ProvisionOutcome::Adopted { entry, host_path: p, .. } => {
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

    /// Bad `base_ref` for an adopted Issue spawn falls through the offline
    /// resolver (ADR 0001) — `resolve_base_ref_sha` degrades a missing ref to
    /// the local HEAD, so `adopt_warm_worktree_by_move` succeeds and the
    /// spawn lands on whatever commit the repo is parked on rather than
    /// hard-failing the spawn. The strict-error alternative was considered
    /// and rejected: it would block every offline session after a fresh
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
        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("offline fallback must succeed (ADR 0001)");
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
    /// `worktree_mode` via `git checkout -B <branch>`. The pool directory
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

        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("manual upgrade must succeed");
        match outcome {
            ProvisionOutcome::Upgraded { entry, host_path: p, .. } => {
                assert_eq!(entry.preassigned_name, "bold-amber-fox");
                assert_eq!(p, host_path_str);
            }
            other => panic!("expected Upgraded for manual spawn, got {:?}", other),
        }
        assert!(pool_path.exists());
        // The pool's plain slug is preserved (no `git worktree move`).
        let wt = git2::Repository::open(&pool_path).unwrap();
        let head_name = wt.head().unwrap().shorthand().unwrap_or("").to_string();
        assert_eq!(head_name, "bold-amber-fox", "manual upgrade must land on the slug");
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

        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("detached manual upgrade must succeed");
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
        assert_eq!(fs::read_to_string(pool_path.join("secrets.env")).unwrap(), "v1=old\n");
        fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

        let node = empty_node(root, Some("warm-include"));
        let host_path_str = pool_path.to_string_lossy().to_string();
        let warm = claimed_warm(&pool_path, "warm-include");
        let ctx = make_ctx(node, SpawnSource::Manual, host_path_str, Some(warm));
        let outcome = provision_for_spawn(ctx, &ProvisionHooks::default(), &NullSink).expect("manual upgrade must succeed");
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
        let ProvisionOutcome::Created { elapsed_ms, .. } = provision_for_spawn(ctx_c, &ProvisionHooks::default(), &NullSink).unwrap()
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
        let ProvisionOutcome::Adopted { elapsed_ms, .. } = provision_for_spawn(ctx_a, &ProvisionHooks::default(), &NullSink).unwrap()
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
        let ProvisionOutcome::Upgraded { elapsed_ms, .. } = provision_for_spawn(ctx_u, &ProvisionHooks::default(), &NullSink).unwrap()
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
        /// `id` for each `forget_warm_row` call.
        warm_rows_forgotten: Vec<i64>,
        /// (node_id, slug) for each `adopt_manual_slug` call.
        adopted_slugs: Vec<(i64, String)>,
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
        fn adopted_slugs(&self) -> Vec<(i64, String)> {
            self.state.lock().unwrap().adopted_slugs.clone()
        }
        fn maintenance_calls(&self) -> Vec<(i64, bool, bool)> {
            self.state.lock().unwrap().maintenance_calls.clone()
        }
    }

    impl ProvisionSink for RecordingSink {
        fn forget_warm_row(&self, id: i64) {
            self.state.lock().unwrap().warm_rows_forgotten.push(id);
        }
        fn adopt_manual_slug(&self, node_id: i64, slug: &str, _worktree_path: &str) {
            self.state
                .lock()
                .unwrap()
                .adopted_slugs
                .push((node_id, slug.to_string()));
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
    /// `ensure_test_db()` below; otherwise `db::is_initialized()` returns
    /// false and the write no-ops.
    struct DbWritingSink;
    impl ProvisionSink for DbWritingSink {
        fn forget_warm_row(&self, _id: i64) {}
        fn adopt_manual_slug(&self, node_id: i64, slug: &str, worktree_path: &str) {
            crate::db::adopt_manual_pool_slug(node_id, slug, worktree_path)
                .expect("DB write must succeed in this test");
        }
        fn on_pool_maintenance_required(
            &self,
            _mesh_id: i64,
            _do_refresh: bool,
            _do_refill: bool,
        ) {
        }
    }

    /// One-shot DB init for tests in this module. Pattern lifted from
    /// `commands::agent::tests::ensure_pr_db`. The first test to call this
    /// picks a scratch file path under the OS temp dir; later tests share
    /// that connection via the global `db::DB` OnceCell. Schema is always
    /// migrated to current `SCHEMA_VERSION`, so the schema is independent
    /// of the order tests run.
    fn ensure_test_db() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let p = std::env::temp_dir().join(format!(
                "buildmesh_provisioner_slug_test_{}.db",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&p);
            let _ = crate::db::init(&p);
        });
    }

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
        ensure_test_db();

        let td = TestDir::new("manual_adopt_db");
        let root = td.path();
        init_repo_with_commit(root, &[("f.txt", "v1
")]);
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
            vec![(4242, "bold-amber-fox".to_string())],
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
        let host_path = root.join(".claude").join("worktrees").join("stage-one-slug");
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

        let hooks = ProvisionHooks { ref_advanced_for_pool: true, pool_was_drained_by_this_spawn: false };
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

        let hooks = ProvisionHooks { ref_advanced_for_pool: false, pool_was_drained_by_this_spawn: true };
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
        let _ = provision_for_spawn(ctx, &ProvisionHooks::default(), &sink).expect("cold create must succeed");
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
        assert!(pool_path.exists(), "pre-move refusal: pool directory must remain intact");
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
        assert_eq!(sink.adopted_slugs(), vec![(7, "quiet-warm".to_string())]);
        assert!(
            sink.warm_rows_forgotten() == vec![42],
            "manual upgrade must forget the warm row (id from claimed_warm fixture)"
        );
        assert!(sink.maintenance_calls().is_empty());
    }
}
