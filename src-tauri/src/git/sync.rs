//! Mesh auto-sync: fetch + fast-forward the parent Mesh before a worktree spawn.
//!
//! `fetch_origin` is the best-effort, never-blocking auto-sync that runs before
//! a new Worktree Node is cut (ADR 0001). It used to live in `env`; it belongs
//! with the rest of Buildmesh's git access (ADR 0007).
//!
//! `do_sync` is the shared fetch+ff-pull algorithm (issue #274) used by both
//! the spawn-time auto-sync (`fetch_origin`, with a remote-derivation wrapper
//! around it) and the manual `git_sync` Tauri command in `commands/git.rs`
//! (which resolves the remote from the current branch's upstream and passes
//! it through). `commits_behind_upstream` is the shared "how far behind"
//! helper consumed by `do_sync` itself.
//!
//! Keeping the algorithm in one place closed the latent bug where
//! `git_sync` lacked `--no-rebase` (the #213 fix) and means future fixes
//! to the fetch/pull logic land in one file.

use crate::env::to_host_path;
use crate::git::primitives;
use crate::process_util::{git_command, run_command_with_timeout};
use std::time::Duration;

/// Outcome of a single `fetch_origin` invocation. The variants let the
/// caller (spawn.rs) decide whether to surface a warning toast without
/// having to reparse strings — every non-`Skipped*` non-`UpToDate` outcome
/// is something the user might want to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Parent repo has uncommitted changes; the pull is skipped per
    /// issue #213's "skip if dirty" criterion. This is silent from the
    /// user's perspective — the user clearly knows the working tree is
    /// dirty, and we don't want to nag them on every spawn.
    SkippedDirty,
    /// No `origin` remote is configured. A purely local Mesh is a
    /// valid setup; we don't surface a toast for that.
    SkippedNoRemote,
    /// Fetched and there were no new commits to pull — already up to
    /// date.
    UpToDate,
    /// Fetched and fast-forwarded `new_commits` commits onto the
    /// current branch. The Agent Node will start from the latest
    /// upstream HEAD.
    Synced { new_commits: u32 },
    /// Fetched `new_commits` commits but the fast-forward was rejected
    /// (local history has diverged from the remote). The Agent Node
    /// still spawns — from the local HEAD — but the user should be
    /// told the auto-sync was partial, so this is the case that
    /// surfaces a warning toast.
    FetchedButDiverged { new_commits: u32, reason: String },
}

/// Failure modes for `fetch_origin` that the caller should treat as
/// "auto-sync unavailable, proceed with local HEAD anyway". The
/// variants carry enough context to compose a user-readable toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The path exists but isn't a git repository (or can't be
    /// opened as one). Unusual — a Mesh without a repo is a
    /// misconfiguration — but we still proceed to spawn since the
    /// agent's behaviour with a non-repo path is a separate concern.
    RepoUnusable(String),
    /// `git fetch` itself failed (most commonly: no network, DNS
    /// failure, or `origin` was removed between the no-remote check
    /// and the fetch). Carries stderr for the toast.
    FetchFailed(String),
}

impl FetchOutcome {
    /// True iff the fetch actually advanced the local ref. Only the two
    /// variants that fetched new commits qualify — `UpToDate` and the
    /// `Skipped*` variants did not move anything. Used by the spawn path
    /// (issue #613 AC3) to decide whether to kick a freshness pass on the
    /// warm pool, and by the manual `git_sync` command for the same
    /// decision. Single-sourced here so the two sites stay in lockstep
    /// (issue #634).
    pub fn advanced_ref(&self) -> bool {
        matches!(self, Self::Synced { .. } | Self::FetchedButDiverged { .. })
    }
}

/// The shared outcome of a fetch+fast-forward pull sync, used by both
/// the spawn-time auto-sync (`fetch_origin`) and the manual `git_sync`
/// command. The two callers each map this to their own contract shape
/// (`FetchOutcome` / `FetchError` and `GitSyncResult` respectively) so
/// the wire types stay distinct even though the algorithm is unified
/// (issue #274).
///
/// All non-`Ok` cases — including the ones the callers treat as
/// "error" — are folded into the enum (no `Result` wrapper). The
/// callers decide what to do with each variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    /// Repo has uncommitted changes; the pull is skipped per
    /// issue #213's "skip if dirty" policy. Silent from the user's
    /// perspective (the user knows the working tree is dirty; we
    /// don't want to nag on every spawn or every Sync click).
    SkippedDirty,
    /// The named remote is not configured on the repo. A purely local
    /// Mesh is a valid state; we don't surface a warning for that.
    SkippedNoRemote,
    /// Fetched; no new commits to pull.
    UpToDate,
    /// Fetched and fast-forwarded `new_commits` commits onto the
    /// current branch.
    Synced { new_commits: u32 },
    /// Fetched `new_commits` commits but the fast-forward was rejected
    /// (local history has diverged from upstream). The spawn still
    /// proceeds from the local HEAD; callers surface a warning so the
    /// user can decide what to do.
    FetchedButDiverged { new_commits: u32, reason: String },
    /// `git fetch` itself failed (most commonly: no network, auth
    /// failure, or remote deleted between the has-remote check and
    /// the fetch). Carries stderr.
    FetchFailed { reason: String },
    /// `git pull --ff-only` (Step 6) timed out — distinct from
    /// [`FetchedButDiverged`] (pull RAN and was rejected) and from
    /// [`FetchFailed`] (the FETCH step failed, no commits arrived).
    /// A pull timeout means commits were fetched but the fast-forward
    /// couldn't apply in time — usually a half-open connection on the
    /// post-fetch socket. Carries the timeout error so the UI can
    /// render a "stuck pull" toast without falsely reporting "fetched
    /// N but diverged" (issue #762 review — pre-#762 the pull path
    /// only had spawn-failure / non-zero-exit; the timeout made the
    /// diverged variant misclassify a network hang).
    PullTimedOut { new_commits: u32, reason: String },
    /// The path isn't a usable git repository. Unusual — a Mesh
    /// without a repo is a misconfiguration — but spawn / sync
    /// proceed anyway since the worker's behaviour on a non-repo
    /// path is a separate concern.
    RepoUnusable { reason: String },
}

impl SyncOutcome {
    /// Same predicate as [`FetchOutcome::advanced_ref`] — the two enums
    /// map 1:1 (`fetch_origin` translates `SyncOutcome → FetchOutcome` at
    /// `sync.rs:568-578`) but the manual `git_sync` command consumes
    /// `SyncOutcome` directly, so this lives on both to keep the predicate
    /// single-sourced (issue #634). Returns `false` for the two error-shaped
    /// variants (`FetchFailed`, `RepoUnusable`) — neither advanced the ref,
    /// and the caller can't usefully schedule a refresh pass on a failure.
    pub(crate) fn advanced_ref(&self) -> bool {
        matches!(self, Self::Synced { .. } | Self::FetchedButDiverged { .. })
    }
}

/// The shared fetch+ff-pull algorithm used by the spawn-time auto-sync
/// (`fetch_origin`, issue #213) and the manual `git_sync` Tauri
/// command (issue #274). Steps:
///
/// 1. **Open the repo** at `host_path`. On failure → `RepoUnusable`.
/// 2. **Dirty-check** (`unwrap_or(true)` = fail closed: an unreadable
///    status call is treated as dirty, not as a green light to pull
///    into a state we couldn't read). On dirty → `SkippedDirty`.
/// 3. **Has-remote check.** If `find_remote(remote)` fails → `SkippedNoRemote`.
/// 4. **`git fetch <remote> [<branch>]`.** On spawn failure or
///    non-zero exit → `FetchFailed` with stderr. The `branch` arg
///    narrows the fetch to a single refspec; pass `None` to fetch all
///    remote refs. Narrowing is the spawn-time latency fix from
///    `parse_branch_for_base_ref`; the manual `git_sync` passes `None`.
/// 5. **Count-behind** via [`commits_behind_upstream`]. Treats "no
///    upstream" as `UpToDate` (most common cause: a fresh local-only
///    branch that just gained a remote via `git remote add` but
///    hasn't been pushed).
/// 6. **`git pull --ff-only --no-rebase`.** The explicit `--no-rebase`
///    defeats a user's global `pull.rebase=true`, which would
///    otherwise turn this into a rebase and write conflict markers
///    on a diverged history (issue #213). The pull is the *only*
///    point in the algorithm that mutates the local branch, so the
///    `SkippedDirty` skip above exists to make sure this pull is
///    never asked to merge into a dirty tree.
///
/// **Steps 1-4 are factored into [`do_fetch_only`]** (issue #446) so
/// the PR-spawn fetch helper can reuse the open-repo + dirty-check +
/// has-remote + `git fetch` chain WITHOUT the `commits_behind` /
/// `git pull` tail — the PR-head fetch is a single-ref materialisation
/// and must not mutate the mesh's currently-checked-out branch as a
/// side effect of spawning a PR agent.
///
/// Caller responsibilities:
/// - Open the repo / resolve the remote / decide whether to narrow
///   the fetch. Both call sites have their own remote-resolution
///   logic (`fetch_origin` derives from `base_ref`; `git_sync` uses
///   the current branch's upstream).
/// - Map `SyncOutcome` to their contract shape. The shape difference
///   is load-bearing (the spawn path emits a `mesh-sync-warning`
///   event; the manual path returns a `GitSyncResult` to the
///   frontend), so we don't unify the wire types.
pub(crate) fn do_sync(
    host_path: &str,
    remote: &str,
    branch: Option<&str>,
    timeout: Duration,
) -> SyncOutcome {
    // Steps 1-4: open + dirty-check + has-remote + `git fetch`. The
    // helper returns the same `SyncOutcome` variants we would have
    // returned inline, so the early return is a literal rename of the
    // old Step 1-4 ladder.
    if let Err(outcome) = do_fetch_only(host_path, remote, branch, timeout) {
        return outcome;
    }

    // Re-open the repo for steps 5-6. The cost is a stat walk — we
    // paid it once in `do_fetch_only` and would pay it again here, but
    // threading the open repo through the `Err` channel would couple
    // the helper to its two consumers' needs.
    let repo = match git2::Repository::open(host_path) {
        Ok(r) => r,
        Err(e) => {
            return SyncOutcome::RepoUnusable {
                reason: e.to_string(),
            };
        }
    };

    // Step 5: count how many commits the local branch is behind. No
    // upstream → treat as "nothing to pull" rather than an error.
    // Other `commits_behind_upstream` errors (perm denied on .git/HEAD,
    // corrupt object db, refdb lock contention) are real git2 errors
    // and get a `warn!` so on-call can grep for them; the old
    // `git_sync` did the same (pre-#274 used `warn!` unconditionally
    // for this arm). The `info!` for the benign "no upstream" case is
    // preserved so the noise floor on a typical spawn doesn't change.
    let new_commits = match commits_behind_upstream(&repo) {
        Ok(n) => n,
        Err(e) => {
            if e.starts_with("no upstream") {
                tracing::info!(
                    "do_sync: no upstream to compare against ({}); treating as up-to-date",
                    e
                );
            } else {
                tracing::warn!(
                    "do_sync: failed to count commits behind upstream ({}); treating as up-to-date",
                    e
                );
            }
            return SyncOutcome::UpToDate;
        }
    };
    if new_commits == 0 {
        return SyncOutcome::UpToDate;
    }

    // Step 6: `git pull --ff-only --no-rebase`. The `--no-rebase` is
    // the load-bearing part of the issue-#213 fix: a user's global
    // `pull.rebase=true` would otherwise turn this into a rebase and
    // write conflict markers on a diverged history.
    //
    // **Timeout (issue #762):** `git pull` shares the half-open-connection
    // risk with `git fetch` (Step 4). Same `FETCH_TIMEOUT` bound.
    tracing::info!(
        "do_sync: running git pull --ff-only --no-rebase ({} new commit{} behind)",
        new_commits,
        if new_commits == 1 { "" } else { "s" }
    );
    let mut pull_builder = git_command();
    pull_builder
        .args(["pull", "--ff-only", "--no-rebase"])
        .current_dir(host_path);
    let pull_output = match run_command_with_timeout(pull_builder, "git pull", timeout) {
        Ok(o) => o,
        Err(e) => {
            // Timeout / spawn-failure / wait-failure on the pull — distinct
            // from `FetchedButDiverged` (pull ran and was rejected due to
            // real local divergence). The `advanced_ref()` predicate must
            // NOT fire here so the warm-pool doesn't see a phantom ref
            // advance (issue #762 review).
            return SyncOutcome::PullTimedOut {
                new_commits,
                reason: e,
            };
        }
    };
    if !pull_output.status.success() {
        let stderr = String::from_utf8_lossy(&pull_output.stderr);
        let trimmed = stderr.trim();
        tracing::warn!("do_sync: git pull --ff-only rejected: {}", trimmed);
        return SyncOutcome::FetchedButDiverged {
            new_commits,
            reason: if trimmed.is_empty() {
                "fast-forward rejected (likely local history diverged)".to_string()
            } else {
                trimmed.to_string()
            },
        };
    }

    SyncOutcome::Synced { new_commits }
}

/// The fetch-only half of [`do_sync`] (issue #446): open the repo,
/// dirty-check, has-remote, `git fetch`. Returns the same
/// [`SyncOutcome`] non-success variants [`do_sync`] would have
/// returned at the same step, so [`do_sync`] can early-return without
/// re-implementing the mapping. Returns `Ok(())` on a clean fetch.
///
/// **Why this exists:** [`do_sync`]’s Step 6 (`git pull --ff-only
/// --no-rebase`) mutates the mesh's currently-checked-out branch. For
/// the PR-spawn fetch path ([`crate::git::worktree::provision::fetch_single_ref`]),
/// the only goal is to materialise a single ref into
/// `refs/remotes/origin/<head_ref>` so the worktree can be cut from
/// it — running `git pull` on the mesh's current branch is wasted
/// work (a few hundred ms of duplicate fast-forward on top of the
/// `fetch_origin` call a few lines earlier under the same per-Mesh
/// `sync_lock`), AND a behavioural surprise (the user's local `main`
/// advances as a side effect of spawning a PR agent). Factoring out
/// the fetch-only chain lets the PR-spawn path use the shared
/// algorithm without inheriting the pull tail.
///
/// **Note:** this helper intentionally does NOT pass a `--` separator
/// to `git fetch`. The fetch-only caller ([`fetch_single_ref`])
/// rejects `-`-prefixed refs up front for the same security reason
/// the old shell-out had `cmd.arg("--")` — see that fn's doc.
pub(crate) fn do_fetch_only(
    host_path: &str,
    remote: &str,
    branch: Option<&str>,
    timeout: Duration,
) -> Result<(), SyncOutcome> {
    // Step 1: open the repo. If the path isn't a git repo, the helper
    // bails with the same `RepoUnusable` variant `do_sync` would have
    // returned inline.
    let repo = match git2::Repository::open(host_path) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "do_fetch_only: path {} is not a usable git repo ({}); skipping",
                host_path,
                e
            );
            return Err(SyncOutcome::RepoUnusable {
                reason: e.to_string(),
            });
        }
    };

    // Step 2: skip if dirty. Fail-closed: a corrupt index or perm
    // denied on .git is treated as dirty rather than as a clean
    // green light to fetch into a state we couldn't read.
    //
    // We use `is_dirty_mirrors_git_status` here (not the strict `is_dirty`)
    // because the user's contract at the fetch gate is "is my tree dirty iff
    // `git status` says so". `git status` honours `submodule.<path>.ignore`
    // from `.git/config`, so a mesh with dirty submodules whose user has set
    // `submodule.X.ignore = dirty` (very common in N64-recomp-style projects
    // with build artifacts inside submodules) would otherwise be falsely
    // blocked here. The strict helper is correct for the data-safety gates
    // (close_safety, restore_to_base, prune badging) — see the helper's
    // docstring for the use-site split.
    if crate::git::primitives::is_dirty_mirrors_git_status(&repo).unwrap_or(true) {
        tracing::info!(
            "do_fetch_only: {} has uncommitted changes; skipping",
            host_path
        );
        return Err(SyncOutcome::SkippedDirty);
    }

    // Step 3: skip if the remote is missing. A Mesh may be a purely
    // local repo (not yet pushed) and that's a valid state.
    if repo.find_remote(remote).is_err() {
        tracing::info!(
            "do_fetch_only: {} has no '{}' remote; skipping",
            host_path,
            remote
        );
        return Err(SyncOutcome::SkippedNoRemote);
    }

    // Step 4: `git fetch <remote> [<branch>]`. Narrowing to a single
    // branch keeps cold-spawn latency down on repos with hundreds of
    // refs (the spawn-time call site always passes a branch; the
    // manual `git_sync` passes `None` for the all-refs default).
    //
    // **Timeout (issue #762):** `git fetch` can wedge indefinitely on a
    // half-open connection. `run_command_with_timeout` kills the child
    // and returns `Err` if it overruns `FETCH_TIMEOUT`. The wrapped
    // `FetchFailed { reason }` carries the timeout string to the toast
    // so the user can distinguish a network hang from a real fetch
    // failure.
    tracing::info!(
        "do_fetch_only: running git fetch {} {} in {}",
        remote,
        branch.unwrap_or("(all refs)"),
        host_path
    );
    let mut fetch_builder = git_command();
    fetch_builder.arg("fetch").arg(remote);
    if let Some(b) = branch {
        fetch_builder.arg(b);
    }
    fetch_builder.current_dir(host_path);
    let fetch_output = match run_command_with_timeout(
        fetch_builder,
        "git fetch",
        timeout,
    ) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("do_fetch_only: {}", e);
            return Err(SyncOutcome::FetchFailed { reason: e });
        }
    };
    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        let trimmed = stderr.trim();
        tracing::warn!(
            "do_fetch_only: git fetch {} {} failed: {}",
            remote,
            branch.unwrap_or(""),
            trimmed
        );
        return Err(SyncOutcome::FetchFailed {
            reason: trimmed.to_string(),
        });
    }

    Ok(())
}

/// Wall-clock timeout for `git fetch` / `git pull` shell-outs in the
/// manual `git_sync` path. Sized for the all-refs manual fetch (a slow
/// refspec negotiation can take 7–36s on the project's 250-branch
/// remote); 5 minutes is generous headroom while still bounding
/// "stuck forever". The `gh auth token` and HTTP-client timeouts in
/// `services::github` are much shorter (30s) — `git fetch` is allowed
/// more headroom because the ref negotiation can dominate.
const MANUAL_FETCH_TIMEOUT: Duration = Duration::from_secs(300);

/// Wall-clock timeout for the spawn-time auto-sync (`fetch_origin`) and
/// for `git pull --ff-only` after a successful fetch. The spawn path has
/// a tight latency budget (`SpawnTimer` checkpoints, target ~2s cold
/// spawn) so 5 minutes is far too long — a wedged spawn-time fetch would
/// hold the per-Mesh `sync_lock` for the entire window with no UI
/// feedback. 30s matches the HTTP-client read timeout in
/// `services::github::HTTP_REQUEST_TIMEOUT` so a half-open connection
/// aborts at the same threshold from both directions (issue #762 review).
pub(crate) const SPAWN_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Back-compat alias — [`do_fetch_only`] and [`do_sync`] (which are
/// shared between the manual `git_sync` and the spawn-time
/// `fetch_origin` paths) historically used the manual-path 5-minute
/// cap. Renaming would touch every call site for no semantic gain, so we
/// alias instead. New call sites that need per-path timeouts should
/// pass [`MANUAL_FETCH_TIMEOUT`] or [`SPAWN_FETCH_TIMEOUT`] explicitly.
pub(crate) const FETCH_TIMEOUT: Duration = MANUAL_FETCH_TIMEOUT;

/// How long a *spawn-path* caller waits for the per-Mesh sync lock before
/// giving up and proceeding from local HEAD. The lock's other holders are
/// the manual `git_sync` (up to [`MANUAL_FETCH_TIMEOUT`] for the fetch plus
/// the same again for the pull), another spawn's auto-sync (up to
/// [`SPAWN_FETCH_TIMEOUT`] × 2), and the prune's `fetch --prune`. 45s
/// comfortably covers the legitimate case (a healthy predecessor sync on a
/// slow remote — measured 7–36s on a 250-branch repo) while capping the
/// pathological one (a wedged manual sync holding the lock for minutes) at
/// well under the old unbounded wait. On timeout the spawn emits the
/// existing `mesh-sync-warning` toast and continues — ADR 0001's
/// best-effort contract.
pub(crate) const SPAWN_SYNC_LOCK_TIMEOUT: Duration = Duration::from_secs(45);

/// Derive the remote name from a configured `base_ref` string, if the
/// string form names one. Pure string parsing — no repo, no I/O — so
/// it can be unit-tested without a checkout. See issue #276 for the
/// full rationale.
///
/// Returns `Some(remote)` for:
///   - `origin/main`                 → `Some("origin")`
///   - `upstream/feature/auth`       → `Some("upstream")`
///   - `refs/remotes/origin/main`    → `Some("origin")`
///   - `refs/remotes/upstream/feat/x`→ `Some("upstream")`
///
/// Returns `None` for:
///   - `HEAD` / `FETCH_HEAD` / empty / whitespace-only
///     (caller falls back to `origin`)
///   - `refs/heads/main`             (caller looks up `branch.<name>.remote`)
///   - bare `main` / `develop`       (caller looks up `branch.<name>.remote`)
///
/// **Limitation:** a plain nested branch name like `feature/auth`
/// **can't be disambiguated from a remote-qualified form** without a
/// repo, so the parser mirrors [`parse_local_branch`](
/// ../commands/git.rs )'s heuristic and treats the first segment
/// as the remote — `parse_remote_for_base_ref("feature/auth")`
/// returns `Some("feature")`. In practice this is harmless: a
/// `feature` remote almost never exists, so `find_remote("feature")`
/// fails and `fetch_origin` falls through to `SkippedNoRemote`. The
/// buildmesh DB default (`origin/main`) and the common short forms
/// (`main`, `develop`, `origin/develop`) all resolve correctly.
/// Callers that need to disambiguate should use the full
/// `refs/heads/feature/auth` form for the local case or
/// `refs/remotes/<remote>/feature/auth` for the remote case.
fn parse_remote_for_base_ref(base_ref: &str) -> Option<String> {
    let trimmed = base_ref.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("HEAD")
        || trimmed.eq_ignore_ascii_case("FETCH_HEAD")
    {
        return None;
    }
    // refs/remotes/<remote>/<branch...> — remote is the first segment.
    if let Some(after) = trimmed.strip_prefix("refs/remotes/") {
        return after
            .split_once('/')
            .map(|(remote, _)| remote.to_string())
            .filter(|r| is_valid_remote_segment(r));
    }
    // refs/heads/<branch...> never names a remote in the string.
    if trimmed.starts_with("refs/heads/") {
        return None;
    }
    // Short form: `<remote>/<branch>` if it has a slash and the
    // leading segment looks like a remote. The first-segment heuristic
    // matches `parse_local_branch`'s inverse so the two stay
    // symmetric.
    trimmed
        .split_once('/')
        .filter(|(remote, rest)| !rest.is_empty() && is_valid_remote_segment(remote))
        .map(|(remote, _)| remote.to_string())
}

fn is_valid_remote_segment(s: &str) -> bool {
    !s.is_empty()
        && !s.eq_ignore_ascii_case("HEAD")
        && !s.eq_ignore_ascii_case("FETCH_HEAD")
}

/// Derive the *branch* portion of a configured `base_ref`, so the fetch can
/// be narrowed to a single refspec (`git fetch <remote> <branch>`) instead of
/// the default "fetch every remote branch". Pure string parsing — the mirror
/// of [`parse_remote_for_base_ref`], which extracts the remote half.
///
/// Why this matters for latency: the default `git fetch <remote>` negotiates
/// *all* remote refs. Buildmesh's branched-worktree mode pushes a branch per
/// agent node, so that ref count grows without bound and every cold spawn pays
/// for it (measured at 7–36s on a repo with ~250 refs). The worktree is always
/// cut from `base_ref`, so fetching only that one branch keeps the worktree
/// exactly as fresh while skipping the all-refs negotiation.
///
/// Returns `Some(branch)` for:
///   - `origin/main`                  → `Some("main")`
///   - `upstream/feature/auth`        → `Some("feature/auth")`
///   - `refs/remotes/origin/main`     → `Some("main")`
///   - `refs/heads/main`              → `Some("main")`
///   - bare `main` / `develop`        → `Some("main")` / `Some("develop")`
///
/// Returns `None` (caller falls back to a bare, all-refs `git fetch`) for:
///   - `HEAD` / `FETCH_HEAD` / empty / whitespace-only
///
/// Like its remote-parsing sibling, a plain nested name such as `feature/auth`
/// is treated as `<remote>/<branch>` (→ `Some("auth")`); harmless because the
/// remote lookup for `feature` then fails and the whole sync skips before any
/// fetch runs.
fn parse_branch_for_base_ref(base_ref: &str) -> Option<String> {
    let trimmed = base_ref.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("HEAD")
        || trimmed.eq_ignore_ascii_case("FETCH_HEAD")
    {
        return None;
    }
    if let Some(after) = trimmed.strip_prefix("refs/remotes/") {
        // refs/remotes/<remote>/<branch...> — branch is everything past the remote.
        return after
            .split_once('/')
            .map(|(_, branch)| branch.to_string())
            .filter(|b| !b.is_empty());
    }
    if let Some(branch) = trimmed.strip_prefix("refs/heads/") {
        return (!branch.is_empty()).then(|| branch.to_string());
    }
    // Short `<remote>/<branch>` form: drop the leading remote segment.
    if let Some((remote, rest)) = trimmed.split_once('/') {
        if !rest.is_empty() && is_valid_remote_segment(remote) {
            return Some(rest.to_string());
        }
    }
    // Bare branch name (`main`, `develop`): the whole string is the branch.
    Some(trimmed.to_string())
}

/// Look up the upstream remote configured for the current branch.
/// Used as a fallback when `base_ref` doesn't name a remote in its
/// string form (e.g. `refs/heads/main` or a bare `main`).
///
/// Mirrors the `branch.<name>.remote` lookup git itself uses for
/// `branch.<name>.merge`; we derive the remote from the upstream
/// refname (which is `refs/remotes/<remote>/<branch>`) rather than
/// shelling out, to keep the path git2-only and avoid a Windows
/// CreateProcess round-trip.
///
/// `pub(crate)` so the manual `git_sync` Tauri command (issue #274)
/// can resolve the configured upstream the same way `git fetch` with
/// no arguments used to, without having to shell out to `git config`.
pub(crate) fn current_branch_upstream_remote(repo: &git2::Repository) -> Option<String> {
    let head = repo.head().ok()?;
    // Detached HEAD has no branch to look up upstream for. `is_branch`
    // returns false on detached HEAD.
    if !head.is_branch() {
        return None;
    }
    let branch_name = head.shorthand()?;
    let local_refname = format!("refs/heads/{}", branch_name);
    let upstream_refname = repo.branch_upstream_name(&local_refname).ok()?;
    let upstream_str = upstream_refname.as_str()?;
    // Format: refs/remotes/<remote>/<branch...>
    upstream_str
        .strip_prefix("refs/remotes/")
        .and_then(|s| s.split_once('/').map(|(r, _)| r.to_string()))
}

/// Fetch from the configured upstream and fast-forward the parent
/// **Mesh**'s current branch onto the remote tip.
///
/// This is the spawn-time auto-sync (issue #213, refined by #276, now
/// a thin wrapper over [`do_sync`] from issue #274). The wrapper's
/// job is purely the spawn-time-specific bit: derive the remote and
/// the fetch-narrowing branch from `base_ref`, then delegate the
/// shared algorithm to `do_sync` and translate `SyncOutcome` back to
/// the spawn-time `FetchOutcome` / `FetchError` contract.
///
/// The remote is derived from `base_ref` (issue #276), so a Mesh
/// configured with `base_ref = "upstream/main"` syncs against
/// `upstream` rather than the hardcoded `origin` that issue #213
/// used. The fall-back chain is:
/// 1. The string form of `base_ref` (e.g. `upstream/main` → `upstream`,
///    `refs/remotes/origin/main` → `origin`).
/// 2. The current branch's configured upstream
///    (`git config branch.<name>.remote`) — only reached when
///    `base_ref` is `refs/heads/<branch>` or a bare branch name.
/// 3. The literal string `"origin"` (preserves the issue #213
///    behaviour for `HEAD` / empty / detached cases).
///
/// The fetch is narrowed to a single branch (parsed from `base_ref`)
/// rather than the all-remote-refs default — on a repo with hundreds
/// of branches the all-refs negotiation dominates cold-spawn
/// latency. See [`parse_branch_for_base_ref`] for the narrow
/// rules and the call-out on the two deliberate behaviour deltas vs
/// an all-refs fetch.
///
/// **This function never blocks the spawn.** The caller (`spawn.rs`)
/// is expected to call it as a best-effort step and continue with
/// local-HEAD fallback on any non-`Ok(Synced|_UpToDate|_Skipped*)`
/// outcome.
pub fn fetch_origin(project_root: &str, base_ref: &str) -> Result<FetchOutcome, FetchError> {
    let host_root = to_host_path(project_root);

    // Open the repo up-front so we can resolve the remote via the
    // branch's configured upstream. If the path isn't a git repo, we
    // map straight to `FetchError::RepoUnusable` — the same outcome
    // `do_sync` would produce internally if we let it open the path,
    // but doing it here keeps the remote-resolution bookkeeping on
    // the caller's side.
    let repo = match git2::Repository::open(&host_root) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "fetch_origin: path {} is not a usable git repo ({}); skipping auto-sync",
                host_root,
                e
            );
            return Err(FetchError::RepoUnusable(e.to_string()));
        }
    };

    // Three-tier remote resolution (issue #276).
    let remote_name = parse_remote_for_base_ref(base_ref)
        .or_else(|| current_branch_upstream_remote(&repo))
        .unwrap_or_else(|| "origin".to_string());

    // Narrow the fetch to the branch the worktree will be cut from
    // (issue #276's latency fix). `None` means all-refs.
    let fetch_branch = parse_branch_for_base_ref(base_ref);

    // Delegate to the shared helper. The mapping from `SyncOutcome`
    // back to `FetchOutcome` / `FetchError` is 1:1 — the variants
    // were chosen so the spawn-time contract is a literal rename.
    //
    // Uses [`SPAWN_FETCH_TIMEOUT`] (30s) instead of the manual-path
    // 5-min cap — a hung spawn-time fetch holds the per-Mesh sync_lock
    // for the duration; 30s matches the HTTP read timeout in
    // `services::github` (issue #762 review).
    let outcome = do_sync(
        &host_root,
        &remote_name,
        fetch_branch.as_deref(),
        SPAWN_FETCH_TIMEOUT,
    );
    Ok(match outcome {
        SyncOutcome::SkippedDirty => FetchOutcome::SkippedDirty,
        SyncOutcome::SkippedNoRemote => FetchOutcome::SkippedNoRemote,
        SyncOutcome::UpToDate => FetchOutcome::UpToDate,
        SyncOutcome::Synced { new_commits } => FetchOutcome::Synced { new_commits },
        SyncOutcome::FetchedButDiverged { new_commits, reason } => {
            FetchOutcome::FetchedButDiverged { new_commits, reason }
        }
        SyncOutcome::PullTimedOut { new_commits, reason } => {
            // Treat the spawn-time pull timeout as a soft divergence:
            // commits ARE on the remote-tracking ref but the working tree
            // didn't fast-forward. The spawn proceeds from local HEAD
            // (same as a real divergence) and the caller surfaces a
            // warning toast.
            FetchOutcome::FetchedButDiverged { new_commits, reason }
        }
        SyncOutcome::FetchFailed { reason } => return Err(FetchError::FetchFailed(reason)),
        SyncOutcome::RepoUnusable { reason } => return Err(FetchError::RepoUnusable(reason)),
    })
}

/// How many commits the current branch is behind its upstream. Shared by
/// `fetch_origin` (auto-sync) and the manual `git_sync` command to decide
/// whether a `git pull --ff-only` would do anything. Returns `Err` when there's
/// no upstream to compare against — both callers treat that as "nothing to pull".
pub fn commits_behind_upstream(repo: &git2::Repository) -> Result<u32, String> {
    let head_oid = repo
        .head()
        .map_err(|e| format!("Failed to read HEAD: {}", e))?
        .peel_to_commit()
        .map_err(|e| format!("HEAD is not a commit: {}", e))?
        .id();

    let branch_name = repo
        .head()
        .map_err(|e| format!("Failed to read HEAD: {}", e))?
        .shorthand()
        .ok_or_else(|| "HEAD is not on a branch".to_string())?
        .to_string();
    let upstream_oid = primitives::upstream_oid_for_branch(repo, &branch_name)
        .ok_or_else(|| format!("no upstream configured for refs/heads/{}", branch_name))?;

    let (_ahead, behind) = primitives::ahead_behind(repo, head_oid, upstream_oid)
        .map_err(|e| format!("graph_ahead_behind failed: {}", e))?;
    Ok(behind)
}

// ── Per-Mesh sync_lock wrappers (issues #652, #680, #709 consolidation) ────
//
// All four per-Mesh `git fetch` / `git pull --ff-only` shell-out sites
// (manual `git_sync`, spawn-time auto-sync, PR-spawn head fetch, worktree
// prune) now route through a `locked_*` helper. Two of those helpers
// live here (`locked_do_sync`, `locked_fetch_origin`); the other two
// (`locked_fetch_pr_head` in `git::worktree::provision`,
// `locked_prune_remote_tracking` in `commands::prune`) live with the
// underlying shell-out they wrap, not with the lock primitive. Each
// helper is the SINGLE point that calls
// `services::sync_lock::with_mesh_sync_lock` for its underlying work,
// so the lock-acquisition shape is auditable in one place per site
// rather than scattered across `commands/git.rs`, `agent/spawn.rs`,
// `git/worktree/provision.rs`, and `commands/prune.rs`.
//
// The bare helpers (`do_sync`, `fetch_origin`, `fetch_single_ref`,
// `fetch_fork_head`, the prune shell-out) stay lock-free so their unit
// tests don't need to coordinate with the global lock map — the locked
// wrappers are tested separately (e.g.
// `git_sync_serializes_via_per_mesh_sync_lock_gh680`,
// `locked_fetch_pr_head_serializes_..._gh698`,
// `locked_prune_remote_tracking_serializes_..._gh709`).
//
// Lock-key choice: keyed on the mesh's DB-stored path (the form stored
// on `agent_nodes.path`) so the same row's contention map stays
// consistent — two callers for one Mesh row share one lock entry
// regardless of WSL vs host path remapping (the bare helpers internally
// re-map via `env::to_host_path`; the lock key stays as the DB string).
// The worktree prune is the exception — it keys on the worktree's own
// path (see `commands::prune::locked_prune_remote_tracking` for the
// rationale), so it uses a different lock entry than the mesh-scoped
// helpers here.

/// Wrap `do_sync` (the fetch+ff-pull algorithm) in the per-Mesh sync lock.
///
/// Issue #680 — manual `git_sync` Tauri command's inline wrap consolidated
/// here so the lock-acquisition shape is identical to the spawn-time
/// auto-sync's `locked_fetch_origin` and the PR-spawn's
/// `locked_fetch_pr_head`. Keyed on `mesh_path` (the DB-stored canonical
/// form, NOT `host_path`).
pub(crate) fn locked_do_sync(
    mesh_path: &str,
    host_path: &str,
    remote: &str,
    branch: Option<&str>,
) -> SyncOutcome {
    crate::services::sync_lock::with_mesh_sync_lock(mesh_path, || {
        do_sync(host_path, remote, branch, MANUAL_FETCH_TIMEOUT)
    })
}

/// Wrap `fetch_origin` (the spawn-time auto-sync) in the per-Mesh sync lock
/// AND run it on a blocking thread. Issue #652 — spawn-time auto-sync's
/// inline `tokio::task::spawn_blocking` + `with_mesh_sync_lock` wrap
/// consolidated here so the lock-acquisition shape is identical to the
/// manual `git_sync`'s `locked_do_sync` and the PR-spawn's
/// `locked_fetch_pr_head`.
///
/// The blocking-thread boundary moves with the wrap: a `git fetch` shell-out
/// would otherwise pin a tokio worker for the duration of the network
/// round-trip, so the consolidation can't drop the `spawn_blocking`. The
/// `JoinError → FetchError::FetchFailed` mapping mirrors the prior inline
/// behaviour at `agent::spawn::spawn_agent_inner` (#652) so the spawn path's
/// error contract is unchanged.
pub async fn locked_fetch_origin(
    mesh_path: String,
    base_ref: String,
) -> Result<FetchOutcome, FetchError> {
    tokio::task::spawn_blocking(move || {
        // Bounded wait (not the unbounded `with_mesh_sync_lock`): the
        // spawn path must never queue for minutes behind a wedged manual
        // sync — see [`SPAWN_SYNC_LOCK_TIMEOUT`]. A `None` (lock still
        // held at the deadline) maps to `FetchFailed`, which the spawn
        // orchestrator already surfaces as a non-fatal `mesh-sync-warning`
        // toast and then proceeds from local HEAD (ADR 0001).
        crate::services::sync_lock::try_with_mesh_sync_lock_timeout(
            &mesh_path,
            SPAWN_SYNC_LOCK_TIMEOUT,
            || fetch_origin(&mesh_path, &base_ref),
        )
        .unwrap_or_else(|| {
            tracing::warn!(
                "locked_fetch_origin: gave up waiting {}s for the mesh sync lock on {}; \
                 spawning from local HEAD",
                SPAWN_SYNC_LOCK_TIMEOUT.as_secs(),
                mesh_path
            );
            Err(FetchError::FetchFailed(format!(
                "another sync for this mesh is still running (waited {}s)",
                SPAWN_SYNC_LOCK_TIMEOUT.as_secs()
            )))
        })
    })
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("locked_fetch_origin: sync task panicked: {}", e);
        Err(FetchError::FetchFailed(format!(
            "sync task panicked: {}",
            e
        )))
    })
}

#[cfg(test)]
#[path = "fetch_origin_tests.rs"]
mod fetch_origin_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // Issue #276 — parse_remote_for_base_ref (pure string parser).
    // The integration tests in fetch_origin_tests.rs exercise the
    // end-to-end fetch path; these cover the parser edge cases
    // (HEAD / empty / refs/heads / nested branch / whitespace)
    // without needing a git repo.
    // -----------------------------------------------------------------

    #[test]
    fn parse_remote_accepts_origin_slash_main() {
        // The buildmesh DB default.
        assert_eq!(
            parse_remote_for_base_ref("origin/main"),
            Some("origin".to_string())
        );
    }

    #[test]
    fn parse_remote_accepts_upstream_slash_branch() {
        // The headline case from issue #276: a Mesh that points at
        // a project-of-record upstream, not the user's personal fork.
        assert_eq!(
            parse_remote_for_base_ref("upstream/main"),
            Some("upstream".to_string())
        );
        assert_eq!(
            parse_remote_for_base_ref("upstream/develop"),
            Some("upstream".to_string())
        );
    }

    #[test]
    fn parse_remote_keeps_remote_for_nested_branch() {
        // "upstream/feature/auth" — the remote is `upstream`; the
        // branch (with its slashes) doesn't change the remote.
        assert_eq!(
            parse_remote_for_base_ref("upstream/feature/auth"),
            Some("upstream".to_string())
        );
        assert_eq!(
            parse_remote_for_base_ref("origin/release/v1.0"),
            Some("origin".to_string())
        );
    }

    #[test]
    fn parse_remote_accepts_refs_remotes_form() {
        assert_eq!(
            parse_remote_for_base_ref("refs/remotes/origin/main"),
            Some("origin".to_string())
        );
        assert_eq!(
            parse_remote_for_base_ref("refs/remotes/upstream/feature/auth"),
            Some("upstream".to_string())
        );
    }

    #[test]
    fn parse_remote_returns_none_for_refs_heads_form() {
        // `refs/heads/<branch>` never names a remote in the string;
        // the caller must look up `branch.<name>.remote`.
        assert_eq!(parse_remote_for_base_ref("refs/heads/main"), None);
        assert_eq!(
            parse_remote_for_base_ref("refs/heads/feature/auth"),
            None
        );
    }

    #[test]
    fn parse_remote_returns_none_for_bare_branch_name() {
        // A bare `main` / `develop` could be either a remote-qualified
        // ref (without the prefix) or a local branch. Without a repo
        // we can't disambiguate, so we return None and the caller
        // falls back to `branch.<current>.remote` or `origin`.
        assert_eq!(parse_remote_for_base_ref("main"), None);
        assert_eq!(parse_remote_for_base_ref("develop"), None);
    }

    #[test]
    fn parse_remote_treats_nested_branch_first_segment_as_remote() {
        // `feature/auth` is ambiguous: a remote `feature` with branch
        // `auth`, or a local branch `feature/auth`. We mirror
        // `parse_local_branch`'s heuristic and assume the first
        // segment is the remote. In practice this is harmless — a
        // `feature` remote almost never exists, so the caller falls
        // through to `SkippedNoRemote` and the spawn proceeds from
        // local HEAD.
        assert_eq!(
            parse_remote_for_base_ref("feature/auth"),
            Some("feature".to_string())
        );
    }

    #[test]
    fn parse_remote_rejects_head_and_empty() {
        assert_eq!(parse_remote_for_base_ref("HEAD"), None);
        assert_eq!(parse_remote_for_base_ref("head"), None);
        assert_eq!(parse_remote_for_base_ref("FETCH_HEAD"), None);
        assert_eq!(parse_remote_for_base_ref(""), None);
        assert_eq!(parse_remote_for_base_ref("   "), None);
    }

    #[test]
    fn parse_remote_trims_whitespace() {
        assert_eq!(
            parse_remote_for_base_ref("  upstream/main  "),
            Some("upstream".to_string())
        );
    }

    // parse_branch_for_base_ref — the branch half, used to narrow the fetch
    // refspec to a single branch (latency fix). Mirrors the remote-parser
    // cases above so the two stay symmetric.
    // -----------------------------------------------------------------

    #[test]
    fn parse_branch_accepts_short_remote_form() {
        // The buildmesh DB default and the upstream variant.
        assert_eq!(
            parse_branch_for_base_ref("origin/main"),
            Some("main".to_string())
        );
        assert_eq!(
            parse_branch_for_base_ref("upstream/develop"),
            Some("develop".to_string())
        );
    }

    #[test]
    fn parse_branch_keeps_slashes_in_nested_branch() {
        // "upstream/feature/auth" — remote is `upstream`, branch keeps its slashes.
        assert_eq!(
            parse_branch_for_base_ref("upstream/feature/auth"),
            Some("feature/auth".to_string())
        );
        assert_eq!(
            parse_branch_for_base_ref("origin/release/v1.0"),
            Some("release/v1.0".to_string())
        );
    }

    #[test]
    fn parse_branch_accepts_refs_forms() {
        assert_eq!(
            parse_branch_for_base_ref("refs/remotes/origin/main"),
            Some("main".to_string())
        );
        assert_eq!(
            parse_branch_for_base_ref("refs/remotes/upstream/feature/auth"),
            Some("feature/auth".to_string())
        );
        assert_eq!(
            parse_branch_for_base_ref("refs/heads/main"),
            Some("main".to_string())
        );
        assert_eq!(
            parse_branch_for_base_ref("refs/heads/feature/auth"),
            Some("feature/auth".to_string())
        );
    }

    #[test]
    fn parse_branch_accepts_bare_branch_name() {
        // A bare `main` is the branch itself; the remote comes from the
        // caller's fallback chain (branch.<current>.remote / origin).
        assert_eq!(parse_branch_for_base_ref("main"), Some("main".to_string()));
        assert_eq!(
            parse_branch_for_base_ref("develop"),
            Some("develop".to_string())
        );
    }

    #[test]
    fn parse_branch_returns_none_for_head_and_empty() {
        // No branch to narrow to → caller falls back to a bare all-refs fetch.
        assert_eq!(parse_branch_for_base_ref("HEAD"), None);
        assert_eq!(parse_branch_for_base_ref("FETCH_HEAD"), None);
        assert_eq!(parse_branch_for_base_ref(""), None);
        assert_eq!(parse_branch_for_base_ref("   "), None);
    }

    #[test]
    fn parse_branch_trims_whitespace() {
        assert_eq!(
            parse_branch_for_base_ref("  origin/main  "),
            Some("main".to_string())
        );
    }
}
