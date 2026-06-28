//! Worktree provision helpers for the Agent Node spawn path.
//!
//! These functions were moved from `agent::spawn` so the Worktree Node
//! lifecycle (the warm-pool adoption / upgrade / cold-create decision and the
//! PR-head fetch that feeds it) lives in one place alongside the cold-path
//! primitives in the parent `git::worktree` module. The orchestrator
//! (`agent::spawn::spawn_agent_inner`) now reaches these helpers across the
//! `git::worktree::provision` seam instead of holding them inline; the
//! long-term target is a single `provision_for_spawn(ctx: SpawnContext)`
//! entry point that encapsulates the 4-way decision (Reused / Adopted /
//! Upgraded / Created) and returns a typed `ProvisionOutcome`.
//!
//! See `docs/adr/0007-extract-git-module.md` (and its 2026-06-13 amendment)
//! for the rationale on why direct `git2` access is consolidated in
//! `git::worktree`.

// ─── Fetch helpers (PR-spawn head fetch + fork remote registration) ───────

/// Fetch a single ref from `origin` into the local repo. Used by the PR-spawn
/// path (#420) to materialise `origin/<head_ref>` so the worktree can be cut
/// from it. `head_ref` is the PR's source branch (e.g. `feat/420-pr-spawn`);
/// the function runs `git fetch origin <head_ref>` and returns `true` on a
/// clean exit, `false` on any failure.
///
/// Best-effort by design: the caller falls back to the mesh's `base_ref` on
/// `false` rather than failing the spawn (ADR 0001 offline pattern). The user
/// sees the agent spawn on the wrong commits in the rare offline / stale-ref
/// case, instead of a hard error every time the network blips. The
/// alternative (strict-error spawn) is brittle to the very first offline
/// session after a fresh install.
///
/// `--` separator before `head_ref` defends against an adversarial / malformed
/// ref starting with `-` (e.g. `--upload-pack=…`); `git fetch` would otherwise
/// treat it as a flag. GitHub's branch-name validation blocks this in
/// practice, but the cost of the separator is zero and the upside is hardening
/// against a future refactor that lets a hand-entered or imported ref flow
/// through.
pub(crate) fn fetch_single_ref(project_root: &str, head_ref: &str) -> bool {
    use crate::process_util::command_no_window;
    let host_root = crate::env::to_host_path(project_root);
    tracing::info!(
        "fetch_single_ref: running git fetch origin -- {} in {}",
        head_ref,
        host_root
    );
    let mut cmd = command_no_window("git");
    cmd.arg("fetch").arg("origin").arg("--").arg(head_ref);
    let output = match cmd.current_dir(&host_root).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("fetch_single_ref: failed to spawn git fetch: {}", e);
            return false;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "fetch_single_ref: git fetch origin -- {} failed: {}",
            head_ref,
            stderr.trim()
        );
        return false;
    }
    true
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
    use crate::process_util::command_no_window;
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
    let mut get_url = command_no_window("git");
    get_url.arg("remote").arg("get-url").arg(&alias);
    let existing = get_url
        .current_dir(&host_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let url_matches = existing.as_deref() == Some(head_repo_clone_url);
    if !url_matches {
        let mut cmd = command_no_window("git");
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
    let mut cmd = command_no_window("git");
    cmd.arg("fetch").arg(&alias).arg("--").arg(head_ref);
    let output = match cmd.current_dir(&host_root).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("fetch_fork_head: failed to spawn git fetch: {}", e);
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
    use crate::process_util::command_no_window;
    let host_root = crate::env::to_host_path(project_root);
    // Read the symbolic SHA in one shot — `git rev-parse` exits non-zero
    // (and produces no stdout) when the ref doesn't exist, so we don't
    // need a separate "is this a ref?" probe first.
    let mut cmd = command_no_window("git");
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
    use crate::process_util::command_no_window;
    let mut cmd = command_no_window("git");
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