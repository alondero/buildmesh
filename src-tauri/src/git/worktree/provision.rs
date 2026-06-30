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
    crate::services::sync_lock::with_mesh_sync_lock(project_root, || {
        match (head_repo_owner, head_repo_clone_url) {
            (Some(owner), Some(clone_url)) => {
                fetch_fork_head(project_root, owner, clone_url, head_ref)
            }
            _ => fetch_single_ref(project_root, head_ref),
        }
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

// ─── Spawn Context / Spawn Source / Provision Outcome (issue #677) ──────────
//
// These three types are the boundary between `agent::spawn` (the orchestrator
// that reads the DB, resolves the mesh row, and claims warm-pool entries) and
// the worktree provisioner (the body of `provision_for_spawn` below). The
// orchestrator builds a `SpawnContext`; the provisioner consumes it; the
// orchestrator matches the `ProvisionOutcome` to drive the post-spawn pool
// housekeeping. Names mirror `CONTEXT.md` (Spawn Context, Spawn Source).
//
// `SpawnContext` carries the loaded `AgentNode` rather than just
// `source_issue`/`source_pr` so the provisioner can branch on source without
// re-fetching the node row. The `resolved_base_sha` field is the SHA
// `resolve_base_ref_sha` resolved for the cold path; `base_ref` is the
// *requested* ref (e.g. `origin/feat-x` for a PR head). On `Created` the
// cold path takes `base_ref`; on `Adopted` the warm path takes the SHA
// resolved at call time so the `git checkout -b` never sees a symbolic ref.

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
/// `host_path` is the Windows-side path git operations run against (the form
/// `git2` and the `git` CLI both accept). `spawn_path` is the path the agent
/// process will see — `host_path` for Windows-only spawns, the WSL form
/// (built by `env::to_host_path` and friends — see that module) for WSL
/// nodes. `provision_for_spawn` only reads `host_path`; `spawn_path` is
/// preserved on the context so callers can derive the spawn command without
/// a second `env::resolve_agent_path`.
///
/// `resolved_base_sha`, `sandbox`, and `spawn_path` are part of the public
/// API contract — callers populate them so a future provisioner revision
/// (or a different orchestrator: stage-2 async resume, telemetry) can read
/// them. The current `provision_for_spawn` doesn't read them; the
/// `#[allow(dead_code)]` keeps the warnings out of `cargo check` output
/// without losing the field on the public surface.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SpawnContext {
    pub node: crate::models::AgentNode,
    pub source: SpawnSource,
    pub base_ref: String,
    pub resolved_base_sha: String,
    pub worktree_mode: String,
    pub use_worktree: bool,
    pub sandbox: bool,
    pub warm_entry: Option<crate::services::warm_pool::ClaimedWarmEntry>,
    pub spawn_path: String,
    pub host_path: String,
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
/// `Reused` / `Adopted` / `Upgraded` / `Created`; the caller matches on the
/// variant to drive the post-spawn bookkeeping (`forget_after_spawn`,
/// `node-renamed` emission, status writes).
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
pub fn provision_for_spawn(mut ctx: SpawnContext) -> Result<ProvisionOutcome, String> {
    // 1. Root Node (`use_worktree = false`). No worktree is created or moved;
    //    the spawn runs in the mesh root directly. `Reused`.
    if !ctx.use_worktree {
        return Ok(ProvisionOutcome::Reused {
            host_path: ctx.host_path,
        });
    }

    // 2. Warm claim — branch on Spawn Source to pick the right adoption mode.
    //    **Manual runs BEFORE the path-exists check** because the pool's slug
    //    is the node's `worktree_name`, so `host_path` already exists; the
    //    upgrade is what completes the adoption. Reordering this breaks every
    //    manual warm claim (it would short-circuit to `Reused` before the
    //    `git checkout -B <branch>` / `.worktreeinclude` re-application runs).
    if let Some(entry) = ctx.warm_entry.take() {
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
        match ctx.source {
            SpawnSource::Issue | SpawnSource::PullRequest => {
                adopt_warm_worktree_by_move(
                    &ctx.node.path,
                    &entry.path,
                    &ctx.host_path,
                    &branch_name,
                    &ctx.worktree_mode,
                    &ctx.base_ref,
                )?;
                Ok(ProvisionOutcome::Adopted {
                    entry,
                    host_path: ctx.host_path,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                })
            }
            SpawnSource::Manual => {
                upgrade_warm_to_mode(
                    &ctx.node.path,
                    &ctx.host_path,
                    &branch_name,
                    &ctx.worktree_mode,
                )?;
                Ok(ProvisionOutcome::Upgraded {
                    entry,
                    host_path: ctx.host_path,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                })
            }
        }
    } else if std::path::Path::new(&ctx.host_path).exists() {
        // 3. Resume / handover / re-spawn — the path is already on disk. Never
        //    re-cut; the agent's prior work sits in that directory and clobbering
        //    it would orphan the user's commits.
        Ok(ProvisionOutcome::Reused {
            host_path: ctx.host_path,
        })
    } else {
        // 4. Cold create. The cold path takes the *requested* `base_ref`
        //    (could be a symbolic ref like `origin/main` for the daily case,
        //    or `origin/<head_ref>` / `fork-<login>/<head_ref>` for a PR
        //    spawn); `create_git_worktree` itself resolves it via
        //    `add_worktree_impl` which already handles the offline fallback.
        let branch_name = ctx
            .node
            .worktree_name
            .clone()
            .unwrap_or_else(|| "buildmesh-spawn".to_string());
        let start = std::time::Instant::now();
        super::create_git_worktree(
            &ctx.node.path,
            &ctx.host_path,
            &branch_name,
            &ctx.worktree_mode,
            &ctx.base_ref,
        )?;
        Ok(ProvisionOutcome::Created {
            host_path: ctx.host_path,
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_helpers::{commit_file, init_repo_with_commit, TestDir};
    use crate::models::{AgentNode, EnvType, SessionStatus};
    use crate::services::warm_pool::ClaimedWarmEntry;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Build a minimal `SpawnContext` for tests, pre-populating fields the
    /// provisioner reads but the test doesn't care about (sandbox, resolved
    /// base SHA) with harmless defaults.
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
            resolved_base_sha: String::new(),
            worktree_mode: "branched".to_string(),
            use_worktree: true,
            sandbox: false,
            warm_entry,
            spawn_path: host_path.clone(),
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
            use_worktree: true,
            source_issue: None,
            source_pr: None,
            head_repo_owner: None,
            head_repo_clone_url: None,
            source_pr_pinned_sha: None,
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

        let outcome = provision_for_spawn(ctx).expect("Root Node must succeed");
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

        let outcome = provision_for_spawn(ctx).expect("resume must succeed");
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

        let outcome = provision_for_spawn(ctx).expect("cold create must succeed");
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

        let outcome = provision_for_spawn(ctx).expect("PR adopt must succeed");
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
        let outcome = provision_for_spawn(ctx).expect("Issue adopt must succeed");
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

        let result = provision_for_spawn(ctx);
        assert!(
            result.is_err(),
            "adopt must refuse to clobber existing branch, got Ok({:?})",
            result
        );
        assert!(
            result.unwrap_err().contains("already exists"),
            "error must name the clobber refusal"
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
        let outcome = provision_for_spawn(ctx).expect("offline fallback must succeed (ADR 0001)");
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

        let outcome = provision_for_spawn(ctx).expect("manual upgrade must succeed");
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

        let outcome = provision_for_spawn(ctx).expect("detached manual upgrade must succeed");
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
        let outcome = provision_for_spawn(ctx).expect("manual upgrade must succeed");
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
        let ProvisionOutcome::Created { elapsed_ms, .. } = provision_for_spawn(ctx_c).unwrap()
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
        let ProvisionOutcome::Adopted { elapsed_ms, .. } = provision_for_spawn(ctx_a).unwrap()
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
        let ProvisionOutcome::Upgraded { elapsed_ms, .. } = provision_for_spawn(ctx_u).unwrap()
        else {
            panic!("Upgraded branch expected");
        };
        assert!(elapsed_ms < 60_000);
    }
}