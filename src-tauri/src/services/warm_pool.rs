//! Pre-spawn Worktree Pool — the v21 tracer bullet (issue #609, PRD #608),
//! with per-mesh opt-in size wired in issue #611.
//!
//! Why this exists
//! ---------------
//! A cold worktree creation takes ~11s on a 400MB repo because Windows
//! Defender / NTFS search indexer / USN journal scan every freshly-written
//! file. ~97% of that time is the `git worktree add` checkout phase (not the
//! network `git fetch`). The pool keeps a detached-HEAD worktree already
//! checked out for every worktree-enabled mesh, so a manual spawn only has
//! to flip the worktree's mode (branched vs. detached) and start the agent —
//! no cold NTFS write cost, sub-500ms checkout in practice.
//!
//! Scope
//! -----
//! * Per-mesh target: `meshes.pre_spawn_pool_size` (schema v22, issue #611).
//!   `0` disables the pool for the mesh; `1..=5` is the target the worker
//!   fills to. Configured via the Worktrees Probe's ConfigurationCard
//!   (issue #611). Manual spawns adopt the preassigned slug as the node
//!   name (zero rename overhead). A successful claim triggers a background
//!   refill so the pool re-fills before the next spawn.
//! * Issue / PR spawns are NOT claimed — they would need a `git worktree
//!   move` rename (~50ms), and the user-facing benefit is dwarfed by the
//!   implementation cost. They're routed through the existing cold path.
//!   The PRD scopes this to a follow-up issue.
//! * Background refresh of stale SHAs (issue #608 §4) lands in issue #613:
//!   after a fetch advances a mesh's base ref, `on_fetch_completed` →
//!   `refresh_stale_warm_worktrees` `git reset --hard`s the pool's available
//!   worktrees onto the new commit so a claim never lands on a stale ref.
//! * Idle background maintenance (issue #613): `maintain_all_pools`, driven by
//!   `services::pool_worker`'s debounced worker thread, tops every mesh's pool
//!   back to target while the app is idle — the pool self-heals without
//!   waiting for the next claim or app restart.
//!
//! Module layout
//! -------------
//! `reconcile_on_startup` — called from `lib.rs::setup`, runs after
//! `db::init`. Prunes stale rows (their on-disk dir is gone), then for
//! each worktree-enabled mesh drains any excess (downsize) and fills up
//! to the per-mesh target.
//!
//! `maintain_all_pools` — the idle worker's per-tick body (issue #613): drains
//! + fills every worktree-enabled mesh to target. Serialized behind the fill
//! lock and gated on idleness by the caller (`services::pool_worker`).
//!
//! `refresh_stale_warm_worktrees` / `on_fetch_completed` — the post-fetch
//! ref-freshness pass (issue #613): hard-reset available pool worktrees onto a
//! freshly-fetched base SHA.
//!
//! `refill_after_claim` — kicked off by the spawn path immediately after a
//! successful claim so the pool is back at target by the next spawn. Runs on a
//! dedicated thread (it shells out to `git`) and behind the fill lock.
//!
//! `prewarm_one` — does the actual on-disk work for one mesh. Idempotent:
//! if a row already points at the directory, the call is a no-op. The
//! PR-style `git fetch` step is intentionally omitted for the v21 tracer
//! — the spawn path's existing `git::sync::fetch_origin` keeps things fresh
//! just before a claim lands.
//!
//! `drain_excess_warm_entries` — runs first in both reconcile paths so a
//! shrink (target 3 → 1) is observed even if the subsequent fill can't
//! catch up. Drops oldest rows in `filling` > `available` > `claimed`
//! priority, removing both DB row + on-disk directory. Idempotent: a no-op
//! when count ≤ target.

use crate::db::{self, WarmWorktreeStatus};
use crate::session_naming::on_spawn;
use rusqlite::Connection;
use std::path::Path;
use tauri::Emitter;

/// Event name broadcast on every state transition that changes a mesh's
/// pre-spawn pool *ready* count. Frontend `usePoolChanged` listens for
/// it and re-queries `get_mesh_pool_count` to refresh the Worktrees
/// Probe badge. Payload is `mesh_id: i64` so a future per-mesh listener
/// can short-circuit without a DB hit.
pub const POOL_COUNT_CHANGED_EVENT: &str = "pool-count-changed";

/// Broadcast `pool-count-changed` for `mesh_id`. Fire-and-forget — a
/// listener-drop (the frontend hasn't subscribed yet, or the window is
/// unfocused) must never fail a pool mutation, so the `let _` swallows
/// any emission error.
///
/// `maintain_all_pools` and `post_spawn_maintenance` deliberately do
/// NOT emit at end-of-pass — every state change inside them already
/// fires this, and an unconditional end-of-pass emit would wake the
/// listener every idle tick (~2s) for no reason.
pub fn emit_pool_changed(app: &tauri::AppHandle, mesh_id: i64) {
    let _ = app.emit(POOL_COUNT_CHANGED_EVENT, mesh_id);
}

/// A `filling` / `refreshing` row younger than this is assumed to belong to a
/// worker that is actively mid-checkout right now (a fresh `refill_after_claim`
/// fill takes seconds, not minutes), so the startup reconcile leaves it alone —
/// tearing it down would destroy in-flight work. Only older rows are treated as
/// crash-orphans from a prior session. Generous on purpose: no legitimate fill
/// approaches this, and a genuine orphan is always far older (the app was shut
/// down and relaunched in between), so a slightly-delayed cleanup of a truly
/// stuck row is a fine trade for never racing a live worker (issue #610 review).
pub const WARM_FILL_STALE_AFTER_MINUTES: i64 = 5;

/// Decide whether the spawn path should consult the pool for a given node.
///
/// Eligible == a **fresh worktree spawn** — one whose own worktree directory
/// is NOT already on disk (`existing_worktree_present == false`).
///
/// Manual, Issue, and PR spawns are all eligible. Manual spawns adopt the
/// pool's plain slug as the node name; Issue/PR spawns keep their
/// `gh{N}-`/`pr{N}-` name and `git worktree move` the pool directory to match
/// (issue #612). The spawn path branches on `node.source_issue` /
/// `node.source_pr` to pick which adoption mode to run; the gate itself only
/// asks "is there a worktree to create?".
///
/// Resume / handover / re-spawn paths re-enter `spawn_agent_inner` with a
/// `worktree_name` whose directory the original spawn already created
/// (`existing_worktree_present == true`); claiming a pool entry for one of
/// them would re-point the node at a *different* directory and abandon the
/// agent's existing work, so they're rejected. The cold path keys its own
/// "create the worktree?" decision off the same `!host_path.exists()` check,
/// so the two stay in lockstep: if the cold path would create a worktree, the
/// pool is allowed to satisfy that creation; if it would reuse one, the pool
/// stays out of the way.
///
/// `existing_worktree_present` is computed by the caller from
/// `env::resolve_agent_path(node.path, node.worktree_name)` — the path the
/// node resolves to WITHOUT a pool claim.
///
/// Returns `false` for any spawn the cold path must serve. The caller falls
/// back to a cold `create_git_worktree` on `false` — exactly what it would
/// do if the pool were empty.
pub fn should_claim_for_spawn(existing_worktree_present: bool) -> bool {
    !existing_worktree_present
}

/// What the spawn path needs to adopt a warm entry as a fresh node.
///
/// `path` and `preassigned_name` flow into `agent_nodes.path` and
/// `agent_nodes.worktree_name` respectively; `id` is the DB row id so the
/// spawn can `delete_warm_worktree(id)` after a successful claim (the row
/// outlives its purpose once the directory becomes a node's worktree).
/// `base_sha` is informational for the spawn-time freshness check.
#[derive(Debug, Clone)]
pub struct ClaimedWarmEntry {
    pub id: i64,
    pub path: String,
    pub preassigned_name: String,
    pub base_sha: Option<String>,
}

/// Try to claim a warm entry for `mesh_id`. Returns `None` when the pool is
/// empty / mid-fill — caller falls back to cold `create_git_worktree`.
///
/// After the atomic DB claim, the claimed row's own `path` is existence-
/// checked on disk to protect against claiming a row whose `git worktree
/// add` was rolled back by a crash between row insert and checkout
/// completion; a missing directory drops the row and reports `None`.
///
/// **Issue #653 cancel-at-claim**: a successful claim also drops any tombstone
/// in `pending_worktree_removals` at the same path. A previous close (or a
/// stale entry from a prior crash) may have queued this directory for
/// removal; the new claim supersedes that intent. The tombstone drop is
/// idempotent (`DELETE WHERE worktree_path = ?` on a missing row is 0 rows
/// affected, no error), so the `let _` swallow is safe. This is the
/// defense-in-depth half of the race fix — the deleter-side guard in
/// `services::agent_node::process_removals` independently skips any *new*
/// tombstone that lands at this path while the spawn is in flight.
///
/// Emits `pool-count-changed` SYNCHRONOUSLY after a successful claim — this
/// is the load-bearing emit for the spawn-failure gap: if the spawn dies
/// between claim and `post_spawn_maintenance`, the badge still sees the
/// count drop because this emit fired before the spawn could fail.
pub fn try_claim(
    app: &tauri::AppHandle,
    mesh_id: i64,
) -> Result<Option<ClaimedWarmEntry>, String> {
    let claimed = match db::claim_warm_entry_for_mesh(mesh_id) {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("warm_pool claim db error: {}", e)),
    };

    // Stale-row guard: a previous crash might have left a row marked
    // `available` whose on-disk directory is gone (the worker wrote the row
    // first then crashed mid-checkout, OR the user deleted the
    // `.claude/worktrees/<slug>` directory by hand). In that case the
    // claim succeeded at the DB layer but the directory isn't there — drop
    // BOTH the warm row AND any pending-removal tombstone (so the next
    // drain doesn't fire `remove_one_worktree` on a non-existent path and
    // generate a spurious failure log) and report `None` so the spawn
    // falls back to cold.
    if !Path::new(&claimed.path).exists() {
        tracing::warn!(
            "warm_pool: claimed row {} points at missing directory {}; dropping and falling back to cold",
            claimed.id,
            claimed.path
        );
        let _ = db::delete_warm_worktree(claimed.id);
        let _ = db::delete_pending_worktree_removal(&claimed.path);
        return Ok(None);
    }

    // Issue #653 cancel-at-claim (success path): any tombstone at this path
    // was queued by a prior close that's now obsolete — the new spawn's
    // intent supersedes it. Without this, a stale pending removal would
    // linger; if the user then closes the *spawning* node before the spawn
    // completes, a new tombstone would land on top and the deleter-side
    // guard in `process_removals` would skip+dequeue it (claim supersedes
    // intent), but this stale one would still be queued and the next drain
    // would see no claim (the spawn succeeded and dropped the row) and
    // delete the live agent's worktree. Cancelling now closes that gap.
    let _ = db::delete_pending_worktree_removal(&claimed.path);

    emit_pool_changed(app, mesh_id);

    Ok(Some(ClaimedWarmEntry {
        id: claimed.id,
        path: claimed.path,
        preassigned_name: claimed.preassigned_name,
        base_sha: claimed.base_sha,
    }))
}

/// Use-site guard for the claim → spawn window (issue #653). After
/// `try_claim` returns, the spawn path may wait seconds inside
/// `fetch_origin` and a `git worktree move`; another thread can delete the
/// directory in that gap (e.g. the close path enqueues a fresh tombstone
/// after the claim, or a user deletes the directory in Explorer). Re-check
/// at the use site to catch any disappearance that occurred AFTER the
/// claim's own existence check, drop the row, and signal `false` so the
/// spawn falls back to cold `create_git_worktree`.
///
/// Called from `agent/spawn.rs` immediately after the `try_claim` call.
/// Returns `true` if the directory is still on disk (the spawn may proceed
/// to use it). Returns `false` if the directory is gone — the row has been
/// dropped via `db::delete_warm_worktree` (best-effort) and the caller must
/// route the spawn through the cold-create path.
///
/// **The use-site guard is independent from the deleter-side guard in
/// `process_removals`**. The deleter-side guard prevents in-process races
/// (one thread deleting a path another is mid-spawn on). The use-site
/// guard catches everything else — user deletes, external processes,
/// stale pending removals that slipped past the cancel-at-claim, and the
/// microsecond TOCTOU window inside `try_claim` itself. Both fail closed.
pub fn recheck_after_claim(id: i64, path: &str) -> bool {
    recheck_after_claim_id(
        &db::delete_warm_worktree,
        &db::delete_pending_worktree_removal,
        id,
        path,
    )
}

/// `_inner`-style testable variant. Takes the row-delete and tombstone-cancel
/// functions as injected dependencies so a test can run against an in-memory
/// `Connection` without touching the global DB mutex.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn recheck_after_claim_inner(
    conn: &Connection,
    id: i64,
    path: &str,
) -> bool {
    let delete_row = |row_id: i64| db::delete_warm_worktree_inner(conn, row_id);
    let cancel_tombstone = |p: &str| db::delete_pending_worktree_removal_inner(conn, p);
    recheck_after_claim_id(&delete_row, &cancel_tombstone, id, path)
}

/// The shared body of `recheck_after_claim` and `_inner`. Pulled out so
/// the logic and warning message are identical across both call sites and
/// any future "drop the row AND tombstone" extension lands in one place.
fn recheck_after_claim_id(
    delete_row: impl Fn(i64) -> db::SqlResult<()>,
    cancel_tombstone: impl Fn(&str) -> db::SqlResult<()>,
    id: i64,
    path: &str,
) -> bool {
    if Path::new(path).exists() {
        return true;
    }
    tracing::warn!(
        "warm_pool: claimed row {}'s directory {} disappeared between claim and spawn use; dropping row and falling back to cold",
        id,
        path
    );
    // Best-effort row drop. `delete_warm_worktree_inner` returns `Ok(())`
    // even when the row was already deleted (0 rows affected is not an
    // error for `rusqlite::Connection::execute`), so this is idempotent
    // against the rare double-recheck race.
    //
    // The tombstone-cancel is also best-effort and is run defensively:
    // a stale tombstone in `pending_worktree_removals` at this path
    // (e.g. enqueued by a prior close that lost the race to the claim's
    // cancel-at-claim step) would otherwise sit queued until a future
    // `remove_one_worktree` ran on it — at which point the operation is
    // a no-op anyway because the directory is gone, so the cost of
    // leaving the tombstone is purely queue churn. Cancelling here keeps
    // the queue clean.
    if let Err(e) = delete_row(id) {
        tracing::warn!(
            "warm_pool: recheck_after_claim failed to drop row {} (non-fatal): {}",
            id,
            e
        );
    }
    let _ = cancel_tombstone(path);
    false
}

/// Drop a warm pool row after a successful spawn. The directory now lives
/// on as the node's worktree — the bookkeeping row's job is done.
///
/// Safe to call from the spawn path's success branch: a failure here just
/// leaves an orphan `claimed` row that the next startup reconcile GC's
/// (issue #639 gap 4). `reconcile_on_startup` calls
/// `db::delete_orphaned_claimed_warm_worktrees` as its first step — a
/// `claimed` row is unreachable by the claim filter (`status='available'`
/// is required) and exempt from the missing-dir scan (its directory may
/// back a live agent), so the explicit claimed-row sweep is what keeps the
/// pool inventory honest. The GC's contract is row-only by design (#642):
/// it must not touch the on-disk directory because the row's directory
/// may already belong to a live agent node, and a DB-only adoption check
/// can be fooled by a silent failure in the spawn's best-effort
/// `set_agent_node_worktree_name` UPDATE.
pub fn forget_after_spawn(id: i64) {
    if let Err(e) = db::delete_warm_worktree(id) {
        tracing::warn!(
            "warm_pool: failed to delete row {} after spawn (non-fatal): {}",
            id,
            e
        );
    }
}

/// Compose the directory-name slug for a warm entry. Used by the worker (and
/// by tests). It's a plain `session_naming::on_spawn` slug — deliberately NOT
/// prefixed — so a manual spawn can adopt it as BOTH the node's display name
/// and its worktree directory with zero rename (issue #609: directory name ==
/// node name). Pool entries are distinguished from live worktrees by the
/// `warm_worktrees` DB table, not by their on-disk name.
pub(crate) fn fresh_slug() -> String {
    on_spawn()
}

/// Compute the absolute path of the warm worktree for a mesh. Matches
/// `env::resolve_agent_path`'s layout so the spawn path can `resolve_agent_path`
/// the result without special-casing. Uses `std::path::Path::join` on the
/// already-host-converted mesh path so the joined separator matches the host
/// (a hand-rolled `format!("{}/.claude/worktrees/{}", …)` produces a mixed
/// separator string on Windows when the mesh path is `C:\…`).
pub(crate) fn warm_worktree_host_path(mesh_path: &str, slug: &str) -> String {
    use std::path::Path;
    let host_mesh = crate::env::to_host_path(mesh_path);
    // `Path::join` always uses the platform's native separator on the
    // appended segment, so the result is `C:\repo\m\.claude\worktrees\<slug>`
    // on Windows and `/repo/m/.claude/worktrees/<slug>` on POSIX.
    Path::new(&host_mesh)
        .join(".claude")
        .join("worktrees")
        .join(slug)
        .to_string_lossy()
        .into_owned()
}

/// What one call to [`prewarm_one`] did. Replaces the old `Result<bool, _>`
/// triple-overload that smushed "at target" and "transient count error"
/// into the same `Ok(false)` (issue #634): a transient
/// `count_available_warm_for_mesh` failure used to silently abort the
/// fill for that pass, because the caller couldn't tell it apart from
/// "we're done, stop the loop". Splitting into three variants lets the
/// caller take three distinct actions: continue the fill loop
/// (`Warmed`), break with no log noise (`AtTarget`), or break with a
/// WARN that explains why we stopped early (`CountFailed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrewarmOutcome {
    /// A new row + worktree were created. Continue the fill loop.
    Warmed,
    /// The mesh was already at target (or pool disabled). Stop the loop.
    AtTarget,
    /// A transient DB error in `count_available_warm_for_mesh` left us
    /// unable to tell where the pool stands. Stop the loop (don't spin
    /// on a broken DB) and surface the reason so the operator can see
    /// why this pass aborted.
    CountFailed(String),
}

/// Decide whether `prewarm_one` should early-exit before doing any work.
/// Extracted so the two early-exit branches (`AtTarget` and
/// `CountFailed`) are unit-testable as a pure function — the `Warmed`
/// case requires actual filesystem + git work and is covered by the
/// integration test in `tests/warm_pool_maintenance.rs`.
///
/// Returns `Some(outcome)` to early-exit (callers should `return` it
/// directly), or `None` when the call should proceed to cut a worktree.
/// Borrows `count_result` so the caller can unwrap the `Ok` arm without
/// paying for a clone.
fn prewarm_early_exit(target: i64, count_result: &Result<i64, String>) -> Option<PrewarmOutcome> {
    // Per-mesh target (issue #611). `0` is a hard no-op — the mesh has
    // opted out of the pool via the Worktrees Probe toggle. We still
    // want reconcile_on_startup and refill_after_claim to call us (so
    // the `filling`/`available` bookkeeping is consistent if the user
    // toggles back on), but the cheap early return avoids any
    // slug-generation + git-worktree-cut work for disabled meshes.
    if target <= 0 {
        return Some(PrewarmOutcome::AtTarget);
    }
    match count_result {
        Err(e) => Some(PrewarmOutcome::CountFailed(e.clone())),
        Ok(available) if *available >= target => Some(PrewarmOutcome::AtTarget),
        Ok(_) => None, // proceed to warm
    }
}

/// Cut a fresh warm worktree for `mesh` and insert a `warming → available`
/// row pair around it. Idempotent: if a row already points at the computed
/// path, the call is a no-op (so the startup reconcile is safe to re-run).
///
/// Returns [`PrewarmOutcome::Warmed`] when a new entry was warmed,
/// [`PrewarmOutcome::AtTarget`] when the mesh was already at target (or
/// the pool is disabled for this mesh), or
/// [`PrewarmOutcome::CountFailed`] with the DB error string when the
/// pre-flight `count_available_warm_for_mesh` query failed (issue #634).
/// Errors from `create_git_worktree` itself propagate as the outer
/// `Err(String)` so the worker can log + skip + try the next mesh on the
/// next reconcile pass.
///
/// Emits `pool-count-changed` after a successful fill (count +1) so a
/// long-running multi-row fill surfaces each row becoming available
/// immediately, instead of all-at-once at end-of-pass.
pub fn prewarm_one(
    app: &tauri::AppHandle,
    mesh: &db::WarmPoolMeshRow,
) -> Result<PrewarmOutcome, String> {
    // Per-mesh target (issue #611). `0` is a hard no-op — the mesh has
    // opted out of the pool via the Worktrees Probe toggle. We still
    // want reconcile_on_startup and refill_after_claim to call us (so
    // the `filling`/`available` bookkeeping is consistent if the user
    // toggles back on), but the cheap early return avoids any
    // slug-generation + git-worktree-cut work for disabled meshes.
    let target = mesh.pre_spawn_pool_size;
    let count_result = db::count_available_warm_for_mesh(mesh.id)
        .map_err(|e| format!("count_available_warm_for_mesh failed for mesh {}: {}", mesh.id, e));
    let available = match prewarm_early_exit(target, &count_result) {
        Some(outcome) => return Ok(outcome),
        None => count_result.expect("prewarm_early_exit returned None implies Ok(_); see its doc"),
    };

    // Pick a fresh slug. Slug collisions are vanishingly rare (~3.9B combos
    // in the seed pool; #608 follow-up adds the `preassigned_name` UNIQUE
    // guard for the per-mesh case), but if the directory already exists on
    // disk we re-roll up to a small bounded number of times before bailing.
    let mut last_err: Option<String> = None;
    for _ in 0..4 {
        let slug = fresh_slug();
        let host_path = warm_worktree_host_path(&mesh.path, &slug);
        if Path::new(&host_path).exists() {
            // Lost the slug lottery on a stale leftover; retry with a new
            // slug so we don't try to `git worktree add` into an existing
            // path (which would no-op or fail).
            continue;
        }

        // Insert the row as `filling` BEFORE cutting the worktree so a
        // crash mid-checkout leaves a recoverable ghost (startup reconcile
        // deletes rows whose `path` doesn't exist).
        let row_id = db::insert_warm_worktree(
            mesh.id,
            &host_path,
            &slug,
            None,
            WarmWorktreeStatus::Filling,
        )
        .map_err(|e| format!("warm_pool insert filling: {}", e))?;

        // Cut the on-disk worktree. We pin mode = "detached" so a future
        // claim can `git checkout -B <branch>` to upgrade to branched mode
        // (or stay detached if the mesh says so). Detached also avoids
        // touching the mesh's branch refs.
        match crate::git::worktree::create_git_worktree(
            &mesh.path,
            &host_path,
            &slug,
            "detached",
            &mesh.base_ref,
        ) {
            Ok(()) => {
                // Flip to available + stamp the base SHA the worker just
                // checked out at. We compute it from the new worktree's
                // HEAD; failures here are non-fatal — the row stays
                // `available` and the spawn-time freshness check falls
                // back to "no SHA to compare" semantics.
                let base_sha = read_warm_head_sha(&host_path).unwrap_or_default();
                let base_sha_opt = if base_sha.is_empty() {
                    None
                } else {
                    Some(base_sha.as_str())
                };
                if let Err(e) = db::mark_warm_worktree_available(row_id, base_sha_opt) {
                    tracing::warn!(
                        "warm_pool: failed to flip row {} to available: {}",
                        row_id,
                        e
                    );
                }
                tracing::info!(
                    "warm_pool: prewarmed {} for mesh {} ({} available)",
                    slug,
                    mesh.id,
                    available + 1,
                );
                // Surface the new row immediately — a long multi-row fill
                // gets to show "1/N ready" → "2/N ready" mid-tick instead
                // of all-at-once at end-of-pass.
                emit_pool_changed(app, mesh.id);
                return Ok(PrewarmOutcome::Warmed);
            }
            Err(e) => {
                // Roll back the filling row so the reconcile doesn't have
                // to garbage-collect it on the next pass.
                let _ = db::delete_warm_worktree(row_id);
                last_err = Some(e);
                continue;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| "warm_pool: exhausted slug retries".to_string()))
}

/// Read the SHA the warm worktree is checked out at. Empty string on any
/// failure (the row stays `available` with `base_sha = NULL` and the
/// spawn-time freshness check degrades to "no SHA to compare").
fn read_warm_head_sha(worktree_path: &str) -> Option<String> {
    let output = read_warm_head_sha_cmd(worktree_path).output().ok()?;
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

/// Build the `git rev-parse HEAD` command used by `read_warm_head_sha`.
/// Uses `command_no_window` so the child doesn't flash a console window on
/// Windows — `prewarm_one` calls this on the post-spawn thread, and an
/// un-flagged `git.exe` would briefly steal foreground focus from buildmesh's
/// WebView2, disrupting the user's terminal view (issue #665).
fn read_warm_head_sha_cmd(worktree_path: &str) -> std::process::Command {
    let mut cmd = crate::process_util::git_command();
    cmd.arg("-C")
        .arg(worktree_path)
        .arg("rev-parse")
        .arg("HEAD");
    cmd
}

/// Drain any excess, then fill a single mesh up to its `pre_spawn_pool_size`
/// target (issue #613). `prewarm_one` warms exactly ONE entry per call, so a
/// pool of target N starting from empty needs N calls — this loops it until
/// the mesh reports "at target" (`Ok(false)`) or a `git worktree add` fails.
///
/// Looks the mesh up by id (vs. `fill_mesh_to_target`'s in-hand row) so a
/// single-mesh caller like `update_mesh_pool_size` doesn't need to first
/// query `list_worktree_enabled_meshes_for_warm`. Silently no-ops when the
/// mesh is not worktree-enabled — the caller already wrote the column, so
/// the drain/fill aren't applicable.
///
/// All state changes inside (`drain_excess_warm_entries`, `prewarm_one`)
/// emit `pool-count-changed` themselves; no end-of-pass emit here.
///
/// Returns `true` iff the mesh's pool state actually changed (rows drained
/// OR warmed). The idle worker uses this as a real signal for its
/// adaptive-tick backoff — a static `pre_spawn_pool_size > 0` heuristic
/// (the pre-#634 broken version) never detected steady-state at-target
/// pools, so the backoff never triggered. See [`fill_mesh_to_target_with`]
/// for the rationale.
pub fn drain_and_fill_for_mesh(app: &tauri::AppHandle, mesh_id: i64) -> bool {
    let meshes = match db::list_worktree_enabled_meshes_for_warm() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                "warm_pool: drain_and_fill_for_mesh list failed for {}: {}",
                mesh_id,
                e
            );
            return false;
        }
    };
    let Some(mesh) = meshes.into_iter().find(|m| m.id == mesh_id) else {
        // Mesh is not worktree-enabled (or has been deleted) — nothing to drain/fill.
        return false;
    };
    fill_mesh_to_target(app, &mesh) > 0
}

/// Drain any excess, then fill a single mesh up to its `pre_spawn_pool_size`
/// target (issue #613). `prewarm_one` warms exactly ONE entry per call, so a
/// pool of target N starting from empty needs N calls — this loops it until
/// the mesh reports "at target" (`Ok(false)`) or a `git worktree add` fails.
///
/// The loop is bounded by `target` iterations as a belt-and-braces guard: each
/// successful `prewarm_one` raises the `available` count by one, so the
/// `available >= target` early-return inside `prewarm_one` is the real
/// terminator and the bound can never actually be hit unless a row vanishes
/// underneath us mid-loop (in which case stopping after `target` tries and
/// letting the next pass retry is exactly the right behaviour).
///
/// The "single sequential `git worktree add`" cadence the issue calls for
/// falls out naturally: this runs the cuts one after another on the calling
/// thread, and every caller is already serialized behind the fill lock.
///
/// `drain` and `prewarm` are passed as closures so the orchestration can be
/// unit-tested without `tauri::AppHandle` (which is hard to mock) — the
/// production caller passes the real `drain_excess_warm_entries` /
/// `prewarm_one` (which close over `app`); tests pass stubs that drive
/// the drain + warm loop with controlled responses. See the
/// `fill_mesh_to_target_*` tests at the bottom of this module
/// (issue #634).
fn fill_mesh_to_target_with(
    mesh: &db::WarmPoolMeshRow,
    drain: impl FnOnce(&db::WarmPoolMeshRow) -> Result<usize, String>,
    prewarm: impl Fn(&db::WarmPoolMeshRow) -> Result<PrewarmOutcome, String>,
) -> usize {
    let mut changes = 0usize;
    match drain(mesh) {
        Ok(n) => changes += n,
        Err(e) => {
            tracing::warn!(
                "warm_pool: drain failed for mesh {} ({}): {}",
                mesh.id,
                mesh.path,
                e
            );
        }
    }
    let target = mesh.pre_spawn_pool_size.max(0);
    for _ in 0..target {
        match prewarm(mesh) {
            Ok(PrewarmOutcome::Warmed) => {
                changes += 1;
                continue;
            }
            Ok(PrewarmOutcome::AtTarget) => break,
            Ok(PrewarmOutcome::CountFailed(reason)) => {
                // Issue #634: a transient DB error in
                // `count_available_warm_for_mesh` used to be silently
                // absorbed as `Ok(false)`, aborting the fill with no log
                // signal. Now we log WARN and break — the next idle tick
                // or post-spawn path will retry the count.
                tracing::warn!(
                    "warm_pool: fill aborted for mesh {} ({}): {}",
                    mesh.id,
                    mesh.path,
                    reason
                );
                break;
            }
            Err(e) => {
                tracing::warn!(
                    "warm_pool: fill failed for mesh {} ({}): {}",
                    mesh.id,
                    mesh.path,
                    e
                );
                break;
            }
        }
    }
    changes
}

/// Fill a single mesh up to target. Returns the number of state-changing
/// operations (drained + warmed) — a real signal the idle worker can use
/// for its adaptive-tick backoff. A return of `0` is the honest
/// "at-target / nothing to do" signal; `> 0` means the pool's available
/// count changed.
fn fill_mesh_to_target(app: &tauri::AppHandle, mesh: &db::WarmPoolMeshRow) -> usize {
    fill_mesh_to_target_with(
        mesh,
        |m| drain_excess_warm_entries(app, m),
        |m| prewarm_one(app, m),
    )
}

/// Maintain EVERY worktree-enabled mesh's pool to its per-mesh target (issue
/// #613 AC1). Driven by the idle background worker
/// (`services::pool_worker::start_background_worker`), which only calls this
/// after the app has been quiet for `IDLE_SILENCE` AND only under the fill
/// lock — so this body itself does no idle/serialization checking, it just
/// does the work.
///
/// Best-effort: a single mesh failing to fill is logged inside
/// `fill_mesh_to_target` and never stops the next mesh. Infallible from the
/// worker's perspective so the loop thread can never die on a transient git
/// error.
///
/// `app` is passed through so the inner `drain_excess_warm_entries` /
/// `prewarm_one` emits can reach the frontend. No end-of-pass emit here —
/// the inner calls already fire one per real state change, and a blanket
/// end-of-pass emit would wake the listener every idle tick (every ~2s)
/// even when nothing changed.
///
/// Kept as a public API (the integration test in
/// `tests/warm_pool_maintenance.rs` drives it directly to exercise the
/// all-meshes fan-out path) but no longer called by the production
/// worker — `services::pool_worker::start_background_worker` now calls
/// `drain_and_fill_for_mesh` per-mesh so each mesh's per-mesh idle gate
/// can gate ITS OWN fill (issue #634).
#[allow(dead_code)]
pub fn maintain_all_pools(app: &tauri::AppHandle) {
    let meshes = match db::list_worktree_enabled_meshes_for_warm() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("warm_pool: maintain list meshes failed: {}", e);
            return;
        }
    };
    for mesh in meshes {
        fill_mesh_to_target(app, &mesh);
    }
}

/// Run the startup reconcile pass (issue #610): prune crash-orphaned rows
/// (and their Git worktree metadata) + fill any mesh that is below the
/// per-mesh target.
///
/// Called from `lib.rs::setup` once, after `db::init`, on a background thread
/// so it never blocks the UI / window-creation path (issue #610 AC4).
/// Failures are logged and swallowed — a transient git error on one mesh must
/// not block the rest of the reconcile or the rest of app startup.
///
/// Emits `pool-count-changed` ONCE at end-of-pass so a probe opened during
/// the reconcile (the 0 → target transition) gets a final settle signal
/// even if no individual fill row tripped the inner emit (e.g. the mesh
/// was already at target from a previous session and the reconcile was a
/// no-op for it).
pub fn reconcile_on_startup(app: tauri::AppHandle) {
    // Step 1a: prune rows stuck in `claimed` after a previous crash (the
    // spawn path's `forget_after_spawn` failed after a successful spawn,
    // leaving the row alive with its directory present — the next claim
    // would skip it because the `status='available'` filter rejects it,
    // so without this prune the row leaks forever).
    match db::delete_orphaned_claimed_warm_worktrees() {
        Ok(n) if n > 0 => tracing::info!(
            "warm_pool: pruned {} orphan `claimed` row(s) from a prior crash",
            n
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!("warm_pool: claimed-row prune failed: {}", e),
    }

    // Step 1b: reconcile rows whose directory is gone (a crash / user-delete
    // between row-insert and the spawn-time claim) OR that a prior crash left
    // stuck `filling` / `refreshing` for longer than `WARM_FILL_STALE_AFTER`
    // (issue #610). These rows never became a live agent node's worktree —
    // `claimed` rows are handled by step 1a — so it's safe to tear down their
    // Git worktree metadata. The age guard in `list_warm_worktrees_to_reconcile`
    // keeps this pass from racing a worker that is filling a row right now.
    match db::list_warm_worktrees_to_reconcile(WARM_FILL_STALE_AFTER_MINUTES) {
        Ok(entries) => reconcile_warm_entries(
            entries,
            // Pool entries are detached (`prewarm_one` cuts them with mode
            // `detached`), so they own no branch — use the branch-preserving
            // remover. The branch-deleting variant would be a latent footgun
            // if a future `refreshing` worker ever checked out a real branch.
            crate::git::worktree::remove_one_worktree,
            db::delete_warm_worktree,
        ),
        Err(e) => tracing::warn!("warm_pool: stale-row scan failed: {}", e),
    }

    // Step 2: walk every worktree-enabled mesh and fill to target. The
    // worker is best-effort: a single mesh failing to warm does not stop
    // the next mesh.
    let meshes = match db::list_worktree_enabled_meshes_for_warm() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("warm_pool: list meshes failed: {}", e);
            return;
        }
    };
    for mesh in meshes {
        // Drain-before-fill and fill-to-target (issue #613): if the user
        // shrunk the pool size while the app was off, the downsize is
        // observed first (even if the subsequent fill can't catch up — e.g.
        // target=0, where the fill is a no-op and the drain is the only
        // useful work), then the pool is filled all the way up to the
        // per-mesh target rather than topped up by a single entry.
        fill_mesh_to_target(&app, &mesh);
        // End-of-mesh settle emit — fires AFTER `fill_mesh_to_target`
        // returns so the badge sees the final count for this mesh even
        // when the mesh was already at target (in which case no inner
        // emit fired). Carries the mesh_id per the `pool-count-changed`
        // IPC contract (`usePoolChanged` documents the payload as
        // `{ mesh_id: number }` — a single mesh-wide `app.emit((), ())`
        // would violate it).
        emit_pool_changed(&app, mesh.id);
    }
}

/// Tear down each reconcilable pool entry, then drop its bookkeeping row —
/// **only after** the on-disk teardown succeeds. Dependencies (`remove`,
/// `delete_row`) are injected so the two invariants this enforces are testable
/// without the global DB or a real filesystem:
///
///   1. **Teardown precedes row-delete, and the row survives a teardown
///      failure.** Deleting the row when `remove` errored (a locked / partial
///      worktree) would orphan the directory on disk forever — no later
///      reconcile would ever revisit a path with no row. Keeping the row lets
///      the next startup retry (issue #610 review).
///   2. **A row whose directory is already gone skips the teardown entirely**
///      and is dropped directly — there is nothing on disk to remove, and
///      calling `remove` would just re-derive the host path to confirm the
///      absence.
fn reconcile_warm_entries(
    entries: Vec<db::WarmReconcileEntry>,
    remove: impl Fn(&str) -> Result<(), String>,
    delete_row: impl Fn(i64) -> db::SqlResult<()>,
) {
    for entry in entries {
        if entry.dir_present {
            match remove(&entry.path) {
                Ok(()) => match delete_row(entry.id) {
                    Ok(()) => tracing::info!(
                        "warm_pool: reconciled stale row {} ({})",
                        entry.id,
                        entry.path
                    ),
                    Err(e) => tracing::warn!(
                        "warm_pool: removed worktree {} but failed to delete row {}: {}",
                        entry.path,
                        entry.id,
                        e
                    ),
                },
                Err(e) => tracing::warn!(
                    "warm_pool: failed to remove git worktree {}; keeping row {} to retry next startup: {}",
                    entry.path,
                    entry.id,
                    e
                ),
            }
        } else if let Err(e) = delete_row(entry.id) {
            tracing::warn!(
                "warm_pool: failed to delete ghost row {} (dir already gone {}): {}",
                entry.id,
                entry.path,
                e
            );
        } else {
            tracing::info!(
                "warm_pool: pruned ghost row {} (directory already gone: {})",
                entry.id,
                entry.path
            );
        }
    }
}

/// Resolve a worktree-enabled mesh by id and run `f` against it, all under the
/// global fill lock (issue #613 AC4). Returns without invoking `f` when the
/// lock is already held (another background pool mutation is in flight — the
/// winner does the mesh-global work for everyone, so skipping is correct) or
/// when the mesh has been disabled/deleted since the caller captured its id.
///
/// Collapses the identical "take the lock → list meshes → find by id →
/// None-guard" prologue that `refill_after_claim`, `on_fetch_completed`, and
/// `post_spawn_maintenance` would otherwise each copy (issue #613 review).
fn with_warm_mesh(mesh_id: i64, what: &str, f: impl FnOnce(&db::WarmPoolMeshRow)) {
    crate::services::pool_worker::try_with_fill_lock(|| {
        let mesh = match db::list_worktree_enabled_meshes_for_warm() {
            Ok(rows) => rows.into_iter().find(|m| m.id == mesh_id),
            Err(e) => {
                tracing::warn!("warm_pool: {} list failed for mesh {}: {}", what, mesh_id, e);
                return;
            }
        };
        let Some(mesh) = mesh else {
            // Mesh was disabled or deleted between the spawn and now.
            return;
        };
        f(&mesh);
    });
}

/// The spawn path's single post-spawn pool task (issue #613): under ONE
/// fill-lock acquisition, optionally refresh stale warm entries onto the new
/// base SHA (`do_refresh`, set when the spawn-time fetch advanced the ref),
/// then optionally refill the pool back to target (`do_refill`, set when this
/// spawn claimed an entry). Folding both into one locked section is what stops
/// refresh and refill from racing each other for the lock (issue #613 review).
///
/// `app` is passed through so the inner `drain_excess_warm_entries` /
/// `prewarm_one` emits can reach the frontend. The `try_claim` call that
/// precedes this in the spawn path has ALREADY emitted `pool-count-changed`
/// (count -1 on the claim); the refill emits here cover the +N to restore
/// to target. Together they tell the badge exactly what changed.
pub fn post_spawn_maintenance(
    mesh_id: i64,
    do_refresh: bool,
    do_refill: bool,
    app: &tauri::AppHandle,
) {
    with_warm_mesh(mesh_id, "post-spawn", |mesh| {
        if do_refresh {
            if let Err(e) = refresh_stale_warm_worktrees(mesh) {
                tracing::warn!(
                    "warm_pool: freshness pass failed for mesh {} ({}): {}",
                    mesh.id,
                    mesh.path,
                    e
                );
            }
        }
        if do_refill {
            fill_mesh_to_target(app, mesh);
        }
    });
}

/// Drop *droppable* warm entries until the mesh's pool is at or below the
/// per-mesh target. Pick order: `filling` rows first (worker mid-checkout —
/// cheapest to drop, will be GC'd on next reconcile anyway), then `available` /
/// `refreshing` ordered by `created_at ASC` (FIFO). For each drop the DB row
/// is removed first, then `git worktree remove --force` is attempted on
/// the directory; a dir-remove failure is logged at WARN but does not
/// fail the whole drain (the DB row is gone, so the orphan will be picked
/// up by the next `list_stale_warm_worktrees` scan).
///
/// **`claimed` rows are never counted or dropped** (issue #613 review): a
/// claimed entry is a worktree in transition to a live agent node — it is not
/// pool inventory, so it must not inflate the excess, and force-removing it
/// would delete a live agent's working tree out from under it. The idle worker
/// runs this drain every ~2s, which would otherwise make the claim →
/// `forget_after_spawn` window a frequent data-loss race. Both the droppable
/// count and the candidate scan filter `status != 'claimed'`.
///
/// Returns the number of entries dropped (0 when droppable ≤ target).
/// Idempotent: safe to call from `reconcile_on_startup`, `maintain_all_pools`,
/// and `refill_after_claim`.
///
/// Emits `pool-count-changed` IFF any rows were actually dropped — a no-op
/// drain (already at or below target) doesn't change the count and so
/// doesn't need to wake the listener.
pub fn drain_excess_warm_entries(
    app: &tauri::AppHandle,
    mesh: &db::WarmPoolMeshRow,
) -> Result<usize, String> {
    let target = mesh.pre_spawn_pool_size.max(0);
    let count = match db::count_droppable_warm_entries_for_mesh(mesh.id) {
        Ok(n) => n,
        Err(e) => {
            return Err(format!("drain count failed for mesh {}: {}", mesh.id, e));
        }
    };
    if count <= target {
        return Ok(0);
    }
    let excess = count - target;

    let candidates = db::list_oldest_warm_entries_for_mesh(mesh.id, excess)
        .map_err(|e| format!("drain candidate scan for mesh {}: {}", mesh.id, e))?;

    let mut dropped: usize = 0;
    for (id, path) in candidates {
        // Drop DB row first so the next reconcile can't re-add or re-claim
        // the same path while we wait on the directory removal.
        if let Err(e) = db::delete_warm_worktree(id) {
            tracing::warn!(
                "warm_pool: drain failed to delete row {} ({}): {}",
                id,
                path,
                e
            );
            continue;
        }
        if let Err(e) = crate::git::worktree::remove_one_worktree(&path) {
            // Don't fail the drain — the DB row is gone, so the orphan
            // directory will be cleaned up by the next
            // `list_stale_warm_worktrees` scan (which won't find a row,
            // but `process_pending_removals`-style cleanup will). Log
            // loudly so the user can intervene manually if it persists.
            tracing::warn!(
                "warm_pool: drain dropped DB row {} but failed to remove {}: {} (orphan will be GC'd on next reconcile)",
                id,
                path,
                e
            );
        }
        dropped += 1;
    }
    if dropped > 0 {
        tracing::info!(
            "warm_pool: drained {} excess entries for mesh {} (target={}, was {})",
            dropped,
            mesh.id,
            target,
            count
        );
        emit_pool_changed(app, mesh.id);
    }
    Ok(dropped)
}

/// Ref-freshness pass for one mesh (issue #613 AC3). Resolves the mesh's base
/// ref to its current SHA (after a fetch has advanced the remote-tracking ref)
/// and hard-resets every `available` warm worktree that is parked on a stale
/// SHA onto the new one, so a subsequent claim lands on the latest commit
/// instead of yesterday's.
///
/// Called from `on_fetch_completed` (which serializes it behind the fill lock).
/// Returns the number of entries actually refreshed. Resolving the SHA can
/// fail (non-repo path, deleted ref) — that error propagates so the caller can
/// log it; a per-entry reset failure is non-fatal and handled inside
/// `refresh_warm_entries` (the entry is restored to `available` at its old SHA
/// so it stays claimable).
pub fn refresh_stale_warm_worktrees(mesh: &db::WarmPoolMeshRow) -> Result<usize, String> {
    // Pool disabled for this mesh: `list_worktree_enabled_meshes_for_warm`
    // returns it (it filters only on `use_worktree=1`), but there are no warm
    // entries to refresh. Bail before the `resolve_base_ref_sha` git2 repo-open
    // so a "pool off" mesh pays no freshness cost on every advancing fetch
    // (issue #613 review).
    if mesh.pre_spawn_pool_size <= 0 {
        return Ok(0);
    }
    let new_sha = crate::git::worktree::resolve_base_ref_sha(&mesh.path, &mesh.base_ref)?;
    let entries = db::list_available_warm_for_mesh(mesh.id)
        .map_err(|e| format!("warm_pool: freshness list for mesh {}: {}", mesh.id, e))?;
    Ok(refresh_warm_entries(
        &new_sha,
        entries,
        crate::git::worktree::reset_warm_worktree,
        db::mark_warm_worktree_refreshing,
        db::mark_warm_worktree_available,
    ))
}

/// The freshness orchestration, with its three side-effecting dependencies
/// (`reset`, `set_refreshing`, `set_available`) injected so the state-flip
/// choreography is unit-testable without a real DB or git — the same DI shape
/// as `reconcile_warm_entries`. The invariants it enforces:
///
///   1. **An entry already on `new_sha` is left untouched** — no reset, no
///      status churn (the common case after a fetch that only moved an
///      unrelated branch).
///   2. **A stale entry is flipped to `refreshing` BEFORE the reset** so a
///      concurrent claim can't grab a worktree mid-`git reset --hard` (the
///      claim filter only matches `available`). On success it is flipped back
///      to `available` stamped with `new_sha`.
///   3. **A reset failure restores the entry to `available` at its OLD SHA**
///      rather than leaving it stranded at `refreshing` (which would make it
///      permanently unclaimable) — and does not count it as refreshed.
///   4. **An entry is only counted as refreshed once it is back at
///      `available`.** If the post-reset flip-back DB write fails (a transient
///      SQLite error in the two-write window), the worktree is correct on disk
///      but stuck at `refreshing` and therefore not yet claimable — that is a
///      loud ERROR, not a success, and is recovered by the startup reconcile.
fn refresh_warm_entries(
    new_sha: &str,
    entries: Vec<db::WarmWorktree>,
    reset: impl Fn(&str, &str) -> Result<(), String>,
    set_refreshing: impl Fn(i64) -> db::SqlResult<()>,
    set_available: impl Fn(i64, Option<&str>) -> db::SqlResult<()>,
) -> usize {
    let mut refreshed = 0usize;
    for entry in entries {
        if entry.base_sha.as_deref() == Some(new_sha) {
            // Already fresh — nothing to do.
            continue;
        }
        // Park it at `refreshing` so a concurrent claim skips it while the
        // reset is in flight. If even this flip fails, skip the entry — better
        // to leave it claimable at its old SHA than to reset a row we couldn't
        // mark.
        if let Err(e) = set_refreshing(entry.id) {
            tracing::warn!(
                "warm_pool: failed to mark row {} refreshing (skipping): {}",
                entry.id,
                e
            );
            continue;
        }
        match reset(&entry.path, new_sha) {
            Ok(()) => match set_available(entry.id, Some(new_sha)) {
                Ok(()) => refreshed += 1,
                Err(e) => {
                    // Worktree is correctly reset on disk but the row is stuck
                    // at `refreshing` and so not yet claimable — do NOT count it
                    // as refreshed. The startup reconcile recovers it (the
                    // 5-minute age guard means a live worker mid-refresh is
                    // never touched). Loud because it leaves the pool one short
                    // until then.
                    tracing::error!(
                        "warm_pool: reset row {} onto {} but failed to flip back to available; \
                         row stranded at `refreshing` until startup reconcile: {}",
                        entry.id,
                        new_sha,
                        e
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    "warm_pool: failed to reset warm worktree {} onto {}: {}",
                    entry.path,
                    new_sha,
                    e
                );
                // Restore to `available` at the OLD SHA so the entry stays
                // claimable rather than being stranded at `refreshing`.
                if let Err(e2) = set_available(entry.id, entry.base_sha.as_deref()) {
                    tracing::warn!(
                        "warm_pool: failed to restore row {} to available after a failed reset: {}",
                        entry.id,
                        e2
                    );
                }
            }
        }
    }
    if refreshed > 0 {
        tracing::info!("warm_pool: refreshed {} stale warm worktree(s)", refreshed);
    }
    refreshed
}

/// Post-fetch hook (issue #613 AC3): after a fetch advances a mesh's base ref,
/// bring its warm pool's worktrees forward to the new SHA. Looks the mesh up
/// by id (so callers only need the `mesh_id` they already have) and runs the
/// freshness pass under the global fill lock — a refresh and a fill must never
/// hammer the same mesh's git/DB concurrently (issue #613 AC4). Best-effort
/// and infallible from the caller's side; intended to be invoked on a
/// background thread from the fetch paths so it never blocks a spawn or a
/// manual sync.
///
/// No `pool-count-changed` emit here: the refresh path flips entries
/// `available → refreshing → available`, so the net available count is
/// unchanged (and a probe opened during a refresh would see the same
/// count it had before the fetch started).
pub fn on_fetch_completed(mesh_id: i64) {
    with_warm_mesh(mesh_id, "freshness", |mesh| {
        if let Err(e) = refresh_stale_warm_worktrees(mesh) {
            tracing::warn!(
                "warm_pool: freshness pass failed for mesh {} ({}): {}",
                mesh.id,
                mesh.path,
                e
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ---- event name drift guard ----
    //
    // The `pool-count-changed` event is the IPC contract with the React
    // `usePoolChanged` hook. The TS side exports `POOL_COUNT_CHANGED_EVENT`
    // as a single source of truth — this test pins the Rust half so a
    // future rename here (e.g. `pool-changed`, `warm-pool-changed`) fails
    // loudly instead of silently breaking the badge's live updates.
    // Symmetric test in `tests/unit/usePoolChanged.test.tsx`.

    #[test]
    fn pool_count_changed_event_name_matches_frontend_constant() {
        assert_eq!(
            POOL_COUNT_CHANGED_EVENT, "pool-count-changed",
            "the event name must match `POOL_COUNT_CHANGED_EVENT` in src/hooks/usePoolChanged.ts — \
             rename in lockstep on both sides"
        );
    }

    // ---- reconcile_warm_entries (the teardown + row-delete orchestration) ----
    //
    // These pin the two invariants the issue #610 review flagged as untested:
    // teardown precedes row-delete, and the row survives a teardown failure so
    // a future startup can retry rather than orphaning the directory forever.

    fn entry(id: i64, dir_present: bool) -> db::WarmReconcileEntry {
        db::WarmReconcileEntry {
            id,
            path: format!("/repo/m/.claude/worktrees/slug-{}", id),
            dir_present,
        }
    }

    /// A live-directory entry: `remove` must be called BEFORE `delete_row`, and
    /// the row must be deleted once teardown succeeds. The shared log proves the
    /// ordering the doc comment promises (teardown-before-delete) — a refactor
    /// that flipped the two calls would orphan the directory on a delete failure.
    #[test]
    fn reconcile_removes_worktree_before_deleting_row() {
        let log: Mutex<Vec<String>> = Mutex::new(Vec::new());
        reconcile_warm_entries(
            vec![entry(7, true)],
            |path| {
                log.lock().unwrap().push(format!("remove:{}", path));
                Ok(())
            },
            |id| {
                log.lock().unwrap().push(format!("delete:{}", id));
                Ok(())
            },
        );
        let log = log.lock().unwrap();
        assert_eq!(
            *log,
            vec![
                "remove:/repo/m/.claude/worktrees/slug-7".to_string(),
                "delete:7".to_string()
            ],
            "git teardown must run before the row delete"
        );
    }

    /// Teardown failure ⇒ the row is KEPT (delete_row never called) so the next
    /// startup retries. Deleting it here would orphan the partial worktree
    /// directory forever — the exact leak the review caught.
    #[test]
    fn reconcile_keeps_row_when_teardown_fails() {
        let deleted: Mutex<Vec<i64>> = Mutex::new(Vec::new());
        reconcile_warm_entries(
            vec![entry(9, true)],
            |_path| Err("worktree locked".to_string()),
            |id| {
                deleted.lock().unwrap().push(id);
                Ok(())
            },
        );
        assert!(
            deleted.lock().unwrap().is_empty(),
            "a failed teardown must NOT delete the row (it would orphan the on-disk worktree)"
        );
    }

    /// A row whose directory is already gone skips teardown entirely and is
    /// dropped directly — there is nothing on disk to remove.
    #[test]
    fn reconcile_skips_teardown_for_missing_directory() {
        let removed: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let deleted: Mutex<Vec<i64>> = Mutex::new(Vec::new());
        reconcile_warm_entries(
            vec![entry(3, false)],
            |path| {
                removed.lock().unwrap().push(path.to_string());
                Ok(())
            },
            |id| {
                deleted.lock().unwrap().push(id);
                Ok(())
            },
        );
        assert!(
            removed.lock().unwrap().is_empty(),
            "a ghost row (directory already gone) must not invoke the git teardown"
        );
        assert_eq!(
            *deleted.lock().unwrap(),
            vec![3],
            "a ghost row must still have its bookkeeping row dropped"
        );
    }

    /// Pin the slug format: a plain `on_spawn` adj-adj-noun slug with NO
    /// prefix. The manual-spawn fast path adopts this slug as the node's
    /// display name, so a refactor that re-introduced a `pool-warm-` prefix
    /// (or otherwise made the slug un-adoptable as a user-facing name) must
    /// surface as a test failure, not a confusing node name in the UI.
    #[test]
    fn fresh_slug_is_a_plain_adoptable_slug() {
        let slug = fresh_slug();
        assert!(
            !slug.starts_with("pool-warm-"),
            "fresh slug must NOT carry an implementation-detail prefix (issue #609 full name adoption), got `{}`",
            slug
        );
        // Three-word hyphenated slug (matches `on_spawn`'s adj-adj-noun) so it
        // is a valid node name AND a valid git branch / directory name.
        assert!(
            slug.split('-').count() == 3,
            "slug must be three hyphenated words, got `{}`",
            slug
        );
    }

    /// `warm_worktree_host_path` mirrors `env::resolve_agent_path`'s layout
    /// exactly so the spawn-time resolution is a no-op on the claimed path.
    #[test]
    fn warm_worktree_host_path_matches_resolve_agent_path_layout() {
        let path = warm_worktree_host_path("/repo/my-mesh", "bold-amber-fox");
        // Either forward-slash (POSIX host) or backslash (Windows host) is
        // acceptable as long as the layout matches `resolve_agent_path`.
        let normalized = path.replace('\\', "/");
        assert_eq!(
            normalized, "/repo/my-mesh/.claude/worktrees/bold-amber-fox",
            "warm pool path must follow the same layout env::resolve_agent_path uses"
        );
    }

    // ---- should_claim_for_spawn (the activation gate) ----
    //
    // The gate keys solely off whether the node's CURRENT worktree directory is
    // already on disk — `false` ⇒ fresh spawn (claim), `true` ⇒ resume /
    // re-spawn reusing an existing worktree (don't claim). Issue/PR spawns are
    // now claim-eligible too (issue #612): the spawn path `git worktree move`s
    // the pool directory to their `gh{N}-`/`pr{N}-` name rather than excluding
    // them.

    #[test]
    fn claims_for_a_fresh_spawn_when_worktree_absent() {
        assert!(
            should_claim_for_spawn(false),
            "a fresh spawn (worktree dir not yet on disk) must be claim-eligible"
        );
    }

    #[test]
    fn does_not_claim_when_worktree_already_present() {
        // Resume / handover / re-spawn: the node's worktree already exists on
        // disk. Claiming would re-point it at a different directory and
        // abandon the agent's work.
        assert!(
            !should_claim_for_spawn(true),
            "a spawn reusing an existing on-disk worktree must NOT be claim-eligible"
        );
    }

    // ---- refresh_warm_entries (ref-freshness orchestration, issue #613 AC3) ----
    //
    // The three side effects (reset, set_refreshing, set_available) are injected
    // so the state-flip choreography is testable without git or a DB — the same
    // DI shape as reconcile_warm_entries above. A shared log records the call
    // order so the "refreshing-before-reset" invariant is provable.

    fn warm(id: i64, base_sha: Option<&str>) -> db::WarmWorktree {
        db::WarmWorktree {
            id,
            path: format!("/repo/m/.claude/worktrees/slug-{}", id),
            preassigned_name: format!("slug-{}", id),
            base_sha: base_sha.map(str::to_string),
        }
    }

    /// An entry already parked on the new SHA is left completely untouched — no
    /// reset, no status churn. This is the common case after a fetch that only
    /// advanced an unrelated ref, so it must be a true no-op (a needless
    /// `git reset --hard` would burn disk I/O on every fetch).
    #[test]
    fn refresh_skips_an_already_fresh_entry() {
        let log: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let n = refresh_warm_entries(
            "newsha",
            vec![warm(1, Some("newsha"))],
            |p, s| {
                log.lock().unwrap().push(format!("reset:{}:{}", p, s));
                Ok(())
            },
            |id| {
                log.lock().unwrap().push(format!("refreshing:{}", id));
                Ok(())
            },
            |id, sha| {
                log.lock().unwrap().push(format!("available:{}:{:?}", id, sha));
                Ok(())
            },
        );
        assert_eq!(n, 0, "an already-fresh entry counts as 0 refreshed");
        assert!(
            log.lock().unwrap().is_empty(),
            "a fresh entry must trigger no reset and no status flips"
        );
    }

    /// A stale entry is flipped to `refreshing` BEFORE the reset, then back to
    /// `available` stamped with the NEW sha. The shared log proves the ordering:
    /// flipping after the reset would let a concurrent claim grab a worktree
    /// mid-`git reset --hard`.
    #[test]
    fn refresh_resets_a_stale_entry_and_stamps_new_sha() {
        let log: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let n = refresh_warm_entries(
            "newsha",
            vec![warm(7, Some("oldsha"))],
            |p, s| {
                log.lock().unwrap().push(format!("reset:{}:{}", p, s));
                Ok(())
            },
            |id| {
                log.lock().unwrap().push(format!("refreshing:{}", id));
                Ok(())
            },
            |id, sha| {
                log.lock().unwrap().push(format!("available:{}:{:?}", id, sha));
                Ok(())
            },
        );
        assert_eq!(n, 1, "one stale entry refreshed");
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "refreshing:7".to_string(),
                "reset:/repo/m/.claude/worktrees/slug-7:newsha".to_string(),
                "available:7:Some(\"newsha\")".to_string(),
            ],
            "must mark refreshing, then reset, then flip back to available at the new sha"
        );
    }

    /// A reset failure restores the entry to `available` at its OLD sha (so it
    /// stays claimable) and does NOT count it as refreshed. Stranding it at
    /// `refreshing` would make it permanently unclaimable — the failure mode
    /// this test guards against.
    #[test]
    fn refresh_restores_old_sha_when_reset_fails() {
        let restored: Mutex<Vec<(i64, Option<String>)>> = Mutex::new(Vec::new());
        let n = refresh_warm_entries(
            "newsha",
            vec![warm(9, Some("oldsha"))],
            |_p, _s| Err("reset blew up".to_string()),
            |_id| Ok(()),
            |id, sha| {
                restored
                    .lock()
                    .unwrap()
                    .push((id, sha.map(str::to_string)));
                Ok(())
            },
        );
        assert_eq!(n, 0, "a failed reset must not count as refreshed");
        assert_eq!(
            *restored.lock().unwrap(),
            vec![(9, Some("oldsha".to_string()))],
            "a failed reset must restore the entry to available at its OLD sha"
        );
    }

    /// A successful reset whose flip-back to `available` fails must NOT be
    /// counted as refreshed (issue #613 review): the worktree is correct on
    /// disk but stuck at `refreshing` and so not yet claimable, so counting it
    /// would over-report the pool's healthy size. The startup reconcile
    /// recovers the stranded row later.
    #[test]
    fn refresh_does_not_count_when_flip_back_fails() {
        let n = refresh_warm_entries(
            "newsha",
            vec![warm(5, Some("oldsha"))],
            |_p, _s| Ok(()),       // reset succeeds
            |_id| Ok(()),          // mark refreshing succeeds
            |_id, _sha| Err(rusqlite::Error::InvalidQuery), // flip-back fails
        );
        assert_eq!(
            n, 0,
            "a reset whose flip-back to available failed must not count as refreshed (row stranded at refreshing)"
        );
    }

    /// An entry with NO recorded base SHA (`base_sha = NULL`, e.g. the
    /// `read_warm_head_sha` stamp failed at fill time) is treated as stale and
    /// reset — it can't be proven fresh, so bringing it onto the known-good new
    /// SHA is the safe choice.
    #[test]
    fn refresh_treats_null_base_sha_as_stale() {
        let reset_calls: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let n = refresh_warm_entries(
            "newsha",
            vec![warm(4, None)],
            |p, _s| {
                reset_calls.lock().unwrap().push(p.to_string());
                Ok(())
            },
            |_id| Ok(()),
            |_id, _sha| Ok(()),
        );
        assert_eq!(n, 1, "a NULL-sha entry must be refreshed");
        assert_eq!(
            *reset_calls.lock().unwrap(),
            vec!["/repo/m/.claude/worktrees/slug-4".to_string()],
            "a NULL-sha entry must be reset onto the new sha"
        );
    }

    // ---- recheck_after_claim (use-site guard, issue #653) ----
    //
    // The spawn path calls this immediately after `try_claim` to close the
    // TOCTOU window between the claim's own existence check and the spawn's
    // use of the path (which involves seconds of `fetch_origin` + git work).
    // The tests pin all three observable branches: present→true, missing→
    // false+row-dropped, and missing-row-idempotency (a delete during the
    // recheck must not panic the spawn).

    fn make_claimed_row(conn: &Connection, path: &str) -> i64 {
        // `insert_warm_worktree_inner` has a foreign key into `meshes`, so
        // the helper builds a v20-shape `meshes` table and inserts the parent
        // row before bringing the warm table forward. Mirrors
        // `db::warm_pool_tests::v20_schema_with_mesh(true)` — we replicate
        // it here because the `_inner` helpers we use don't ship their own
        // schema fixture (and the `db::warm_pool_tests` `v20_schema_with_mesh`
        // is private to that module).
        conn.execute_batch(
            "
            CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                build_command TEXT,
                run_command TEXT,
                model TEXT,
                effort TEXT,
                use_worktree INTEGER NOT NULL DEFAULT 1,
                worktree_mode TEXT,
                default_provider TEXT,
                base_ref TEXT NOT NULL DEFAULT 'origin/main',
                scratchpad TEXT NOT NULL DEFAULT '',
                sandbox INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO meshes (name, path) VALUES ('m', '/repo/m');
            ",
        )
        .unwrap();
        db::ensure_warm_worktables_table(conn).unwrap();
        db::insert_warm_worktree_inner(
            conn,
            1,
            path,
            "pool-warm-recheck",
            Some("aaaa"),
            db::WarmWorktreeStatus::Claimed,
        )
        .unwrap()
    }

    /// Happy path: the directory exists at recheck time. Returns `true`,
    /// row is untouched, the spawn proceeds to use the warm entry.
    #[test]
    fn recheck_after_claim_returns_true_when_directory_present() {
        let conn = Connection::open_in_memory().unwrap();
        db::ensure_warm_worktables_table(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let id = make_claimed_row(&conn, &path);

        assert!(
            recheck_after_claim_inner(&conn, id, &path),
            "directory exists → recheck must say true so the spawn proceeds"
        );
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 1, "row must survive when dir is present");
    }

    /// Race outcome: the directory vanished between `try_claim`'s check and
    /// the spawn's use. Returns `false` AND drops the row so the pool's
    /// bookkeeping stays consistent — without this, `claimed` rows would
    /// accumulate pointing at missing directories (the exact symptom in
    /// issue #653). The caller falls back to cold `create_git_worktree`.
    #[test]
    fn recheck_after_claim_returns_false_and_drops_row_when_dir_missing() {
        let conn = Connection::open_in_memory().unwrap();
        db::ensure_warm_worktables_table(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let id = make_claimed_row(&conn, &path);
        // Now delete the directory — simulate the race outcome.
        std::fs::remove_dir_all(&path).unwrap();

        assert!(
            !recheck_after_claim_inner(&conn, id, &path),
            "missing dir → recheck must say false so the spawn falls back to cold"
        );
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM warm_worktrees WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            remaining, 0,
            "the dropped directory must drop its bookkeeping row — issue #653's stranded-row symptom"
        );
    }

    /// Idempotency guard: if some other path (the deleter-side guard,
    /// startup reconcile, etc.) already dropped the row, the recheck's own
    /// DELETE on a non-existent id must not panic. `rusqlite::Connection::
    /// execute` returns `Ok(0)` for a missing row, so this is just a
    /// regression guard against a future refactor that changed the
    /// semantics.
    #[test]
    fn recheck_after_claim_is_idempotent_on_already_dropped_row() {
        let conn = Connection::open_in_memory().unwrap();
        db::ensure_warm_worktables_table(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let id = make_claimed_row(&conn, &path);
        std::fs::remove_dir_all(&path).unwrap();
        // First call drops the row (per the previous test).
        assert!(!recheck_after_claim_inner(&conn, id, &path));
        // Second call must not panic on the already-missing row.
        assert!(
            !recheck_after_claim_inner(&conn, id, &path),
            "idempotent recheck on a missing row + missing dir must still return false"
        );
    }

    // ---- prewarm_early_exit (issue #634) ----
    //
    // The pure decision helper that `prewarm_one` consults before doing
    // any work. Pinning all three early-exit branches in a table-driven
    // test catches the silent-collapse bug at the type level: a future
    // refactor that mapped `count_result = Err(_)` back to `AtTarget`
    // would break `prewarm_early_exit_logs_count_failures_loudly` and
    // surface immediately, instead of silently aborting fills in
    // production.

    /// target == 0 (mesh has opted out of the pool) is a no-op — return
    /// `AtTarget` regardless of count.
    #[test]
    fn prewarm_early_exit_returns_at_target_when_pool_disabled() {
        assert_eq!(
            prewarm_early_exit(0, &Ok(5)),
            Some(PrewarmOutcome::AtTarget),
            "target=0 must early-exit as AtTarget even if the count says otherwise"
        );
        assert_eq!(
            prewarm_early_exit(0, &Err("db down".into())),
            Some(PrewarmOutcome::AtTarget),
            "target=0 must early-exit as AtTarget even if the count failed (no fill work needed anyway)"
        );
    }

    /// count == target ⇒ already at target. Caller breaks the fill loop.
    /// Boundary: the `>=` (not `>`) means a mesh at target with one extra
    /// filling row also returns AtTarget — that's intentional, because
    /// `count_available_warm_for_mesh` excludes `filling` rows and the
    /// the fill loop is gated on `available >= target`, so the next tick
    /// can still see a fresh `available` row come in.
    #[test]
    fn prewarm_early_exit_returns_at_target_when_count_meets_target() {
        assert_eq!(
            prewarm_early_exit(2, &Ok(2)),
            Some(PrewarmOutcome::AtTarget),
            "count == target must early-exit (boundary case for >=)"
        );
        assert_eq!(
            prewarm_early_exit(2, &Ok(5)),
            Some(PrewarmOutcome::AtTarget),
            "count > target must early-exit (the common 'over target' case after a stale row)"
        );
    }

    /// count < target AND count succeeded ⇒ proceed to warm. Returns
    /// `None` (no early exit).
    #[test]
    fn prewarm_early_exit_returns_none_when_count_below_target() {
        assert_eq!(
            prewarm_early_exit(3, &Ok(1)),
            None,
            "1/3 available must proceed to warm (returns None to signal 'keep going')"
        );
        assert_eq!(
            prewarm_early_exit(3, &Ok(2)),
            None,
            "2/3 available must proceed to warm"
        );
    }

    /// Load-bearing regression for the issue #634 bug: a transient DB
    /// error in `count_available_warm_for_mesh` used to collapse to
    /// `Ok(false)` which the caller mistook for "at target" and silently
    /// aborted the fill. After the fix, it must surface as
    /// `CountFailed(reason)` so `fill_mesh_to_target` can log + stop.
    #[test]
    fn prewarm_early_exit_logs_count_failures_loudly() {
        assert_eq!(
            prewarm_early_exit(2, &Err("database is locked".into())),
            Some(PrewarmOutcome::CountFailed("database is locked".into())),
            "a transient DB error must surface as CountFailed, not AtTarget — the issue #634 bug"
        );
    }

    /// `CountFailed` variant carries the reason through unchanged so the
    /// WARN log line the caller emits includes the original error. Pin
    /// the contract that the reason is preserved verbatim.
    #[test]
    fn prewarm_early_exit_count_failed_preserves_reason_string() {
        let detailed = "count_available_warm_for_mesh failed for mesh 42: SQLITE_BUSY";
        assert_eq!(
            prewarm_early_exit(2, &Err(detailed.into())),
            Some(PrewarmOutcome::CountFailed(detailed.into())),
            "the reason string must flow through unchanged so the operator can see the original DB error"
        );
    }

    // ---- fill_mesh_to_target_with (orchestration, issue #634) ----
    //
    // The `drain` / `prewarm` closures are injected so the test can drive the
    // drain-then-fill-to-target contract without `tauri::AppHandle` (which is
    // hard to mock) or real git worktree creation (which is slow + requires a
    // real repo). The production `fill_mesh_to_target` wraps this with the
    // real `drain_excess_warm_entries` + `prewarm_one`.

    fn mesh_row(id: i64, target: i64) -> db::WarmPoolMeshRow {
        db::WarmPoolMeshRow {
            id,
            path: format!("/tmp/buildmesh_fill_test_{}", id),
            pre_spawn_pool_size: target,
            base_ref: "origin/main".to_string(),
        }
    }

    /// Happy path: drain runs once, then `prewarm` returns `Warmed` exactly
    /// `target` times. The `for _ in 0..target` loop bounds the calls at
    /// `target` even if every response is `Warmed` (the inner
    /// `prewarm_early_exit`'s "at target" check is the real terminator
    /// once the in-DB count catches up to `target`; this `target`
    /// outer bound is belt-and-braces).
    #[test]
    fn fill_mesh_to_target_with_warms_exactly_target_times() {
        let mesh = mesh_row(1, 3);
        let prewarm_calls: std::cell::RefCell<Vec<i64>> = std::cell::RefCell::new(Vec::new());
        let responses: std::cell::RefCell<Vec<_>> = std::cell::RefCell::new(vec![
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::AtTarget), // never reached — `for _ in 0..3` bounds the loop
        ]);
        let drain_calls = std::cell::RefCell::new(0);

        fill_mesh_to_target_with(
            &mesh,
            |_| {
                *drain_calls.borrow_mut() += 1;
                Ok(0)
            },
            |_m| {
                let mut responses = responses.borrow_mut();
                let next = responses.remove(0);
                let len = prewarm_calls.borrow().len();
                prewarm_calls.borrow_mut().push(len as i64);
                next
            },
        );

        assert_eq!(*drain_calls.borrow(), 1, "drain must run exactly once");
        assert_eq!(
            prewarm_calls.borrow().len(),
            3,
            "prewarm must run exactly `target` (3) times when every response is Warmed"
        );
        // Sanity: the leftover AtTarget response confirms we DIDN'T over-call.
        assert_eq!(
            responses.borrow().len(),
            1,
            "the AtTarget response was never consumed (the `for _ in 0..target` bound held)"
        );
    }

    /// Pin the issue #634 contract: `CountFailed` from `prewarm` aborts the
    /// fill loop (does NOT silently continue to exhaust `target` iterations
    /// with stale state). A transient DB error used to be collapsed to
    /// `Ok(false)` (AtTarget) — now it must surface as a real "stop" signal.
    #[test]
    fn fill_mesh_to_target_with_count_failed_aborts_loop() {
        let mesh = mesh_row(2, 5);
        let responses: std::cell::RefCell<Vec<_>> = std::cell::RefCell::new(vec![
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::CountFailed("transient db error".into())),
            Ok(PrewarmOutcome::Warmed), // must NOT be reached
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::Warmed),
        ]);
        let prewarm_call_count = std::cell::RefCell::new(0);

        fill_mesh_to_target_with(
            &mesh,
            |_| Ok(0),
            |_m| {
                *prewarm_call_count.borrow_mut() += 1;
                responses.borrow_mut().remove(0)
            },
        );

        assert_eq!(
            *prewarm_call_count.borrow(),
            2,
            "loop must stop on CountFailed, NOT exhaust `target` iterations (issue #634)"
        );
    }

    /// `prewarm_one`'s `Err` (a `git worktree add` failure after slug
    /// retries) also breaks the loop. Distinct from `CountFailed` (a DB
    /// query failure before any warm attempt).
    #[test]
    fn fill_mesh_to_target_with_prewarm_err_aborts_loop() {
        let mesh = mesh_row(3, 5);
        let responses: std::cell::RefCell<Vec<_>> = std::cell::RefCell::new(vec![
            Ok(PrewarmOutcome::Warmed),
            Err("git worktree add: locked".into()),
            Ok(PrewarmOutcome::Warmed), // must NOT be reached
        ]);
        let prewarm_call_count = std::cell::RefCell::new(0);

        fill_mesh_to_target_with(
            &mesh,
            |_| Ok(0),
            |_m| {
                *prewarm_call_count.borrow_mut() += 1;
                responses.borrow_mut().remove(0)
            },
        );

        assert_eq!(
            *prewarm_call_count.borrow(),
            2,
            "loop must stop on a git worktree add failure"
        );
    }

    /// `target == 0` ⇒ loop body never executes. Drain still runs once.
    /// This is the common case for a worktree-disabled mesh.
    #[test]
    fn fill_mesh_to_target_with_zero_target_runs_only_drain() {
        let mesh = mesh_row(4, 0);
        let drain_calls = std::cell::RefCell::new(0);
        let prewarm_call_count = std::cell::RefCell::new(0);

        fill_mesh_to_target_with(
            &mesh,
            |_| {
                *drain_calls.borrow_mut() += 1;
                Ok(0)
            },
            |_m| {
                *prewarm_call_count.borrow_mut() += 1;
                Ok(PrewarmOutcome::AtTarget)
            },
        );

        assert_eq!(*drain_calls.borrow(), 1);
        assert_eq!(
            *prewarm_call_count.borrow(),
            0,
            "a zero-target mesh must NOT call prewarm (drain is the only useful work)"
        );
    }

    /// Drain failure is logged at WARN and the loop PROCEEDS to warm (we
    /// don't refuse to fill just because the downsize scan errored — the
    /// pool is presumably already at or below target anyway, and a fill
    /// is at worst a single extra worktree).
    #[test]
    fn fill_mesh_to_target_with_drain_failure_still_attempts_fill() {
        let mesh = mesh_row(5, 2);
        let prewarm_call_count = std::cell::RefCell::new(0);

        fill_mesh_to_target_with(
            &mesh,
            |_| Err("drain DB error".into()),
            |_m| {
                *prewarm_call_count.borrow_mut() += 1;
                Ok(PrewarmOutcome::AtTarget)
            },
        );

        assert_eq!(
            *prewarm_call_count.borrow(),
            1,
            "a drain failure must NOT abort the fill — we still attempt to warm (drain err is logged + swallowed)"
        );
    }

    // ---- fill_mesh_to_target_with return value (issue #634 review) ----
    //
    // These tests pin the `usize` return contract that the idle worker
    // uses for its adaptive-tick backoff signal. A return of `0` is the
    // honest "nothing changed" signal; `> 0` means the pool's available
    // count changed (drained or warmed). The pre-#634 review caught that
    // the worker used a static `pre_spawn_pool_size > 0` heuristic
    // instead — that always returned true for any worktree-enabled mesh,
    // so the TICK_SLOW backoff never triggered in production.

    /// 3 Warmed responses + 1 drain that returned 0 → total changes = 3.
    #[test]
    fn fill_mesh_to_target_with_returns_count_of_warmed_entries() {
        let mesh = mesh_row(6, 3);
        let responses: std::cell::RefCell<Vec<_>> = std::cell::RefCell::new(vec![
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::AtTarget),
        ]);

        let changes = fill_mesh_to_target_with(
            &mesh,
            |_| Ok(0),
            |_m| responses.borrow_mut().remove(0),
        );
        assert_eq!(changes, 3, "3 Warmed responses = 3 changes");
    }

    /// Drain returns 2 (two excess rows removed) + 1 Warmed = 3 total.
    /// Pins that drain's count is added to the total — not just the warm
    /// loop's contributions.
    #[test]
    fn fill_mesh_to_target_with_aggregates_drain_and_warm_changes() {
        let mesh = mesh_row(7, 1);
        let responses: std::cell::RefCell<Vec<_>> = std::cell::RefCell::new(vec![
            Ok(PrewarmOutcome::Warmed),
            Ok(PrewarmOutcome::AtTarget),
        ]);

        let changes = fill_mesh_to_target_with(
            &mesh,
            |_| Ok(2), // drain removed 2 excess rows
            |_m| responses.borrow_mut().remove(0),
        );
        assert_eq!(changes, 3, "2 drain + 1 warm = 3 changes total");
    }

    /// Already-at-target mesh returns 0 — the honest "no work was
    /// needed" signal the idle worker uses to count a no-op pass toward
    /// its TICK_SLOW backoff.
    #[test]
    fn fill_mesh_to_target_with_returns_zero_when_at_target() {
        let mesh = mesh_row(8, 2);
        let prewarm_call_count = std::cell::RefCell::new(0);

        let changes = fill_mesh_to_target_with(
            &mesh,
            |_| Ok(0),
            |_m| {
                *prewarm_call_count.borrow_mut() += 1;
                Ok(PrewarmOutcome::AtTarget)
            },
        );

        assert_eq!(changes, 0, "at-target mesh must report 0 changes");
        assert_eq!(*prewarm_call_count.borrow(), 1);
    }

    /// Disabled mesh (target = 0) returns 0 because the loop body never
    /// executes; drain still ran once but reports 0 changes.
    #[test]
    fn fill_mesh_to_target_with_returns_zero_when_target_is_zero() {
        let mesh = mesh_row(9, 0);
        let changes = fill_mesh_to_target_with(&mesh, |_| Ok(0), |_| Ok(PrewarmOutcome::Warmed));
        assert_eq!(changes, 0);
    }

    // ---- End-to-end integration: drain-then-fill on a real temp git repo ----
    //
    // (Issue #634 sub-task #5.) The DB-layer drain/refresh logic is covered
    // exhaustively by `db::warm_pool_tests`; the loop orchestration is
    // covered by the `fill_mesh_to_target_with` tests above. What neither
    // of those covers is the **end-to-end drain-then-fill-to-target
    // contract** against a REAL git repo: real `git init` → real commit →
    // real `git worktree add` (via `create_git_worktree`) → real
    // directory on disk → real `git rev-parse HEAD` → real SHA stamped on
    // the row. These tests lock that contract in place.
    //
    // We can't call `fill_mesh_to_target` directly — it needs a
    // `tauri::AppHandle` for `emit_pool_changed`, and there's no test
    // mock for that. Instead we drive the exact same `_inner` DB
    // operations + `create_git_worktree` calls that `fill_mesh_to_target`
    // uses internally. That's the "real git + real DB" coverage the issue
    // asks for; the orchestration is unit-tested above.
    //
    // The Once-init'd DB pattern is borrowed from `commands::prune_tests`
    // — every test in this file (and the rest of the lib test binary)
    // shares one global DB.

    fn maintenance_db_path() -> std::path::PathBuf {
        use std::sync::OnceLock;
        static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let p = std::env::temp_dir().join(format!(
                "buildmesh_warm_pool_maintenance_{}.db",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&p);
            p
        })
        .clone()
    }

    fn ensure_maintenance_db() {
        if crate::db::is_initialized() {
            return;
        }
        std::sync::Once::new().call_once(|| {
            let _ = crate::db::init(&maintenance_db_path());
        });
    }

    /// Fresh temp dir with a real git repo: `git init` + an empty initial
    /// commit on `main`. Returns the temp dir + the repo path.
    fn fresh_git_repo() -> (tempfile::TempDir, String) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().to_str().unwrap().to_string();

        let run = |args: &[&str]| {
            let status = std::process::Command::new("git") // allow-inline-process-spawn: test helper
                .args(args)
                .current_dir(&path)
                .status()
                .expect("git invocation failed");
            assert!(status.success(), "git {:?} must succeed", args);
        };

        run(&["init", "-q"]);
        run(&["config", "user.email", "test@buildmesh.local"]);
        run(&["config", "user.name", "Buildmesh Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        run(&["checkout", "-q", "-b", "main"]);
        run(&["commit", "--allow-empty", "-q", "-m", "init"]);

        (tmp, path)
    }

    /// Insert a worktree-enabled mesh row pointing at `repo_path`.
    fn insert_mesh(conn: &rusqlite::Connection, repo_path: &str) -> i64 {
        conn.execute(
            "INSERT INTO meshes (name, path, use_worktree, pre_spawn_pool_size, base_ref) \
             VALUES ('m', ?1, 1, 0, 'origin/main')",
            rusqlite::params![repo_path],
        )
        .expect("insert mesh");
        conn.query_row::<i64, _, _>(
            "SELECT id FROM meshes WHERE path = ?1",
            rusqlite::params![repo_path],
            |row| row.get(0),
        )
        .expect("mesh id")
    }

    /// Cut a real git worktree via the production `create_git_worktree`
    /// helper, mirroring `prewarm_one`'s body. Returns the on-disk path.
    fn cut_real_warm_worktree(
        conn: &rusqlite::Connection,
        mesh_id: i64,
        mesh_path: &str,
    ) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let slug = format!("wt-{:x}", nanos);
        let host_path = warm_worktree_host_path(mesh_path, &slug);

        let row_id = db::insert_warm_worktree_inner(
            conn,
            mesh_id,
            &host_path,
            &slug,
            None,
            db::WarmWorktreeStatus::Filling,
        )
        .expect("insert filling row");

        crate::git::worktree::create_git_worktree(
            mesh_path,
            &host_path,
            &slug,
            "detached",
            "origin/main",
        )
        .unwrap_or_else(|e| panic!("create_git_worktree failed for {}: {}", slug, e));

        let sha = read_head_sha(&host_path);
        let sha_opt: Option<&str> = if sha.is_empty() { None } else { Some(&sha) };
        conn.execute(
            "UPDATE warm_worktrees SET status = 'available', base_sha = ?2 WHERE id = ?1",
            rusqlite::params![row_id, sha_opt],
        )
        .expect("flip to available");

        host_path
    }

    fn read_head_sha(worktree_path: &str) -> String {
        let output = std::process::Command::new("git") // allow-inline-process-spawn: test helper
            .args(["-C", worktree_path, "rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse");
        if !output.status.success() {
            return String::new();
        }
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Pin the drain-then-fill-to-target contract end-to-end against a real
    /// temp git repo + real `create_git_worktree` + real DB. Mirrors the
    /// issue #634 acceptance: drain removes the oldest excess (FIFO order),
    /// fill cuts real worktrees via the production helper, at-target is a
    /// no-op.
    ///
    /// Uses unique slug suffixes so multiple invocations in the same test
    /// don't collide on a stale directory.
    #[test]
    fn maintenance_drain_then_fill_end_to_end_on_real_repo() {
        ensure_maintenance_db();

        let (_tmp, repo_path) = fresh_git_repo();
        let db = crate::db::get();
        let conn = db.lock().unwrap();
        let mesh_id = insert_mesh(&conn, &repo_path);

        // Cut three warm worktrees in order — the first is the OLDEST.
        let path_oldest = cut_real_warm_worktree(&conn, mesh_id, &repo_path);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let path_middle = cut_real_warm_worktree(&conn, mesh_id, &repo_path);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let path_newest = cut_real_warm_worktree(&conn, mesh_id, &repo_path);
        assert!(std::path::Path::new(&path_oldest).exists());
        assert!(std::path::Path::new(&path_middle).exists());
        assert!(std::path::Path::new(&path_newest).exists());

        // Set target = 1 so the drain should drop 2 of 3 (FIFO).
        conn.execute(
            "UPDATE meshes SET pre_spawn_pool_size = 1 WHERE id = ?1",
            rusqlite::params![mesh_id],
        )
        .unwrap();

        // Drain phase — mirrors `drain_excess_warm_entries` body.
        let target: i64 = conn
            .query_row(
                "SELECT pre_spawn_pool_size FROM meshes WHERE id = ?1",
                rusqlite::params![mesh_id],
                |row| row.get(0),
            )
            .unwrap();
        let count = db::count_droppable_warm_entries_for_mesh_inner(&conn, mesh_id).unwrap();
        assert_eq!(count, 3, "precondition: 3 droppable rows");
        let excess = (count - target).max(0);
        let candidates =
            db::list_oldest_warm_entries_for_mesh_inner(&conn, mesh_id, excess).unwrap();
        assert_eq!(candidates.len(), 2);
        // FIFO: the OLDEST two are the drop candidates.
        assert_eq!(candidates[0].1, path_oldest);
        assert_eq!(candidates[1].1, path_middle);
        for (id, path) in &candidates {
            db::delete_warm_worktree_inner(&conn, *id).unwrap();
            let _ = crate::git::worktree::remove_one_worktree(path);
        }

        // Postcondition: 1 row left, only newest dir on disk.
        let remaining = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
        assert_eq!(remaining, 1);
        assert!(!std::path::Path::new(&path_oldest).exists());
        assert!(!std::path::Path::new(&path_middle).exists());
        assert!(std::path::Path::new(&path_newest).exists());

        // Fill phase — bump target to 2 and run the fill loop, which
        // pins the issue #634 contract: when count < target, we cut
        // exactly ONE worktree per iteration (mirrors `prewarm_one`).
        conn.execute(
            "UPDATE meshes SET pre_spawn_pool_size = 2 WHERE id = ?1",
            rusqlite::params![mesh_id],
        )
        .unwrap();
        let target: i64 = conn
            .query_row(
                "SELECT pre_spawn_pool_size FROM meshes WHERE id = ?1",
                rusqlite::params![mesh_id],
                |row| row.get(0),
            )
            .unwrap();

        let mut new_paths: Vec<String> = Vec::new();
        for _ in 0..target {
            let count = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
            if count >= target {
                break;
            }
            new_paths.push(cut_real_warm_worktree(&conn, mesh_id, &repo_path));
        }

        let post = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
        assert_eq!(post, 2, "fill must reach target");
        for p in &new_paths {
            assert!(std::path::Path::new(p).exists(), "new dir {} must exist", p);
        }

        // At-target no-op: running the fill loop again must not create
        // anything new (PrewarmOutcome::AtTarget early-exit).
        let mut new_paths_2 = 0;
        for _ in 0..target {
            let count = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
            if count >= target {
                break;
            }
            cut_real_warm_worktree(&conn, mesh_id, &repo_path);
            new_paths_2 += 1;
        }
        assert_eq!(new_paths_2, 0, "at-target fill loop must early-exit");
        let final_count = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
        assert_eq!(final_count, 2, "at-target no-op must leave count unchanged");
    }
}