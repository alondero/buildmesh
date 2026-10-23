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
//! * Idle background maintenance (issue #613): `drain_and_fill_for_mesh`, driven
//!   per mesh by `services::pool_worker`'s debounced worker thread so each mesh's
//!   own idle gate can gate its own fill, tops that mesh's pool back to target
//!   while the app is idle — the pool self-heals without waiting for the next
//!   claim or app restart.
//! * A Mesh whose folder cannot be opened as a git working repository is not
//!   poolable: [`mesh_repo_unusable`] gates the two paths that create a
//!   worktree or fetch — the idle fill and the ADR 0020 background sync
//!   (issue #2348) — so such a mesh costs one cheap libgit2 open per pass
//!   instead of a doomed `git worktree add` / fetch and a WARN line each time.
//!   The drain's `git worktree remove` stays ungated on purpose: it drops the
//!   row before it touches the disk, so it converges in one pass and cannot
//!   repeat a warning.
//!
//! Module layout
//! -------------
//! `reconcile_on_startup` — called from `lib.rs::setup`, runs after
//! `db::init`. Prunes stale rows (their on-disk dir is gone), then for
//! each worktree-enabled mesh drains any excess (downsize) and fills up
//! to the per-mesh target.
//!
//! `drain_and_fill_for_mesh` — the idle worker's per-mesh body (issue #613):
//! drains and fills one worktree-enabled mesh to target. Serialized behind the
//! fill lock and gated on idleness by the caller (`services::pool_worker`), which
//! is why each mesh can gate its own fill (issue #634).
//!
//! `refresh_stale_warm_worktrees` / `on_fetch_completed` — the post-fetch
//! ref-freshness pass (issue #613): hard-reset available pool worktrees onto a
//! freshly-fetched base SHA.
//!
//! `refill_after_claim` — kicked off by the spawn path immediately after a
//! successful claim so the pool is back at target by the next spawn. Runs on a
//! dedicated thread (it shells out to `git`) and behind the fill lock.
//!
//! `warm_one_body` — does the actual on-disk work for one entry. Idempotent:
//! if a row already points at the directory, the call re-rolls the slug. The
//! PR-style `git fetch` step is intentionally omitted for the v21 tracer
//! — the spawn path's existing `git::sync::fetch_origin` keeps things fresh
//! just before a claim lands.
//!
//! `drain_excess_warm_entries` — runs first in both reconcile paths so a
//! shrink (target 3 → 1) is observed even if the subsequent fill can't
//! catch up. Drops oldest rows in `filling` > `available` > `claimed`
//! priority, removing both DB row + on-disk directory. Idempotent: a no-op
//! when count ≤ target.

use crate::db;
use crate::session_naming::on_spawn;
use once_cell::sync::Lazy;
#[cfg(test)]
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
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
/// The idle fill and `post_spawn_maintenance` deliberately do NOT emit at
/// end-of-pass — every state change inside them already fires this, and an
/// unconditional end-of-pass emit would wake the listener every idle tick
/// (~2s) for no reason.
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
/// The type lives with the consumer (`git::worktree::warm`) so `git/` can
/// name it without naming this module — that is the whole point of the
/// consumer-owned claim seam (issue #2533). This re-export keeps the pool's
/// own public surface intact for the service layer's callers.
pub use crate::git::worktree::warm::ClaimedWarmEntry;

/// The pool's implementation of the consumer-owned claim seam (issue #2533).
///
/// `git/worktree` reaches the pre-spawn pool through
/// [`crate::git::worktree::warm::WarmClaim`] — a narrow interface this module
/// implements — instead of importing the pool directly. The dependency
/// direction is therefore one way: `services/` → `git/`.
///
/// The claim also carries the two facts only the pool knows: the entry it
/// holds, and whether the pool's inventory is one short because of it.
pub struct PoolClaim {
    app: tauri::AppHandle,
    /// The claimed entry, while the claim is live. `None` when the pool had
    /// no entry to give (a cold spawn) or when the use-site recheck found the
    /// directory gone.
    entry: Option<ClaimedWarmEntry>,
    /// Whether this spawn took a row out of the pool's inventory. Stays true
    /// after the use-site recheck drops the row: the inventory is still one
    /// short, so the post-spawn refill must run.
    took_entry: bool,
}

impl PoolClaim {
    /// Wrap a claimed entry (or the absence of one) in the pool's claim
    /// handle. A `None` entry means the pool had no entry to give — the spawn
    /// is cold, and the claim still serves the pool-side operations the
    /// provisioner needs (the count emit, the post-spawn maintenance).
    pub fn new(app: tauri::AppHandle, entry: Option<ClaimedWarmEntry>) -> Self {
        let took_entry = entry.is_some();
        Self {
            app,
            entry,
            took_entry,
        }
    }

    /// The use-site guard for the claim → spawn window (issue #653) — see
    /// [`recheck_claim`], which owns the rule. A failed recheck leaves the
    /// claim with no entry, so the spawn falls back to a cold create.
    pub fn recheck(&mut self) {
        recheck_claim(&mut self.entry);
    }
}

/// The claim's use-site guard for the claim → spawn window (issue #653):
/// re-check that the claimed directory is still on disk.
///
/// When the directory is gone the claim holds no entry any more, so the
/// spawn must fall back to a cold create. The pool's inventory stays one
/// short either way, which is why `needs_refill` keeps answering "yes"
/// after a failed recheck.
///
/// This sits beside [`PoolClaim`] rather than inside it because it needs no
/// app handle: the guard is about the claimed row, and a free function is
/// what lets a test exercise it without a Tauri app (the crate has no
/// `tauri::AppHandle` test mock).
pub fn recheck_claim(entry: &mut Option<ClaimedWarmEntry>) {
    let Some(claimed) = entry.as_ref() else {
        return;
    };
    if !recheck_after_claim(claimed.id, &claimed.path) {
        // `recheck_after_claim` already logged the reason (claimed row N's
        // directory disappeared…), so don't duplicate that WARN here.
        *entry = None;
    }
}

impl crate::git::worktree::warm::WarmClaim for PoolClaim {
    fn entry(&self) -> Option<&ClaimedWarmEntry> {
        self.entry.as_ref()
    }

    fn needs_refill(&self) -> bool {
        self.took_entry
    }

    fn mark_forgotten(&self, id: i64) {
        forget_after_spawn(id);
    }

    fn emit_pool_changed(&self, mesh_id: i64) {
        emit_pool_changed(&self.app, mesh_id);
    }

    fn schedule_maintenance(&self, mesh_id: i64, do_refresh: bool, do_refill: bool) {
        let app = self.app.clone();
        std::thread::spawn(move || {
            post_spawn_maintenance(mesh_id, do_refresh, do_refill, &app);
        });
    }
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
pub fn try_claim(app: &tauri::AppHandle, mesh_id: i64) -> Result<Option<ClaimedWarmEntry>, String> {
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
        db::delete_warm_worktree,
        db::delete_pending_worktree_removal,
        id,
        path,
    )
}

/// `_inner`-style testable variant. Takes the row-delete and tombstone-cancel
/// functions as injected dependencies so a test can run against an in-memory
/// `Connection` without touching the global DB writer. Compiled for tests
/// alone (issue #2450).
#[cfg(test)]
pub(crate) fn recheck_after_claim_inner(conn: &Connection, id: i64, path: &str) -> bool {
    let delete_row = |row_id: i64| db::delete_warm_worktree_inner(conn, row_id);
    let cancel_tombstone = |p: &str| db::delete_pending_worktree_removal_inner(conn, p);
    recheck_after_claim_id(delete_row, cancel_tombstone, id, path)
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

/// Compute the absolute host path for a warm entry inside an explicit
/// effective container dir (issue #1519). `effective_dir_raw` comes from
/// `env::effective_worktree_dir_raw`; host-converted once, then `slug`
/// joined via `Path::join` so the joined separator matches the host
/// (a hand-rolled `format!("{}/.claude/worktrees/{}", …)` produces a mixed
/// separator string on Windows when the mesh path is `C:\…`).
pub(crate) fn warm_worktree_host_path_in_dir(effective_dir_raw: &str, slug: &str) -> String {
    use std::path::Path;
    let host_dir = crate::env::to_host_path(effective_dir_raw);
    Path::new(&host_dir)
        .join(slug.trim())
        .to_string_lossy()
        .into_owned()
}

/// Effective container dir (raw form) for a warm-pool mesh row.
/// Precedence: Mesh override → application default → `.worktrees`.
/// Reads the app default via `preferences::worktree_directory()` so the
/// pool and cold creation agree without the caller threading prefs.
pub(crate) fn effective_dir_raw_for_mesh(mesh: &db::WarmPoolMeshRow) -> String {
    let app_dir = crate::preferences::worktree_directory();
    crate::env::effective_worktree_dir_raw(
        &mesh.path,
        mesh.worktree_directory.as_deref(),
        app_dir.as_deref(),
    )
}

/// Effective container dir (host form) for a warm-pool mesh row.
pub(crate) fn effective_host_dir_for_mesh(mesh: &db::WarmPoolMeshRow) -> String {
    crate::env::to_host_path(&effective_dir_raw_for_mesh(mesh))
}

/// Compute the absolute host path of the warm worktree for `mesh` + `slug`
/// under the mesh's current effective directory (issue #1519).
pub(crate) fn warm_worktree_host_path_for_mesh(mesh: &db::WarmPoolMeshRow, slug: &str) -> String {
    warm_worktree_host_path_in_dir(&effective_dir_raw_for_mesh(mesh), slug)
}

/// Normalize a host path for prefix comparison: backslashes → `/`,
/// trailing separators stripped, lowercased on Windows (case-insensitive
/// FS) to match `env::directories_match` semantics without pulling the
/// private normalizer.
fn normalize_host_for_prefix(p: &str) -> String {
    let mut n = p.replace('\\', "/");
    while n.len() > 1 && n.ends_with('/') {
        n.pop();
    }
    if cfg!(target_os = "windows") {
        n.to_ascii_lowercase()
    } else {
        n
    }
}

/// True when `warm_host_path` sits directly inside `effective_host_dir`
/// (i.e. `<effective>/<slug>`). Used to detect stale-location pool
/// inventory after a directory setting changes — entries outside the
/// current effective dir must be rebuilt, never reused.
pub(crate) fn warm_path_under_effective_dir(
    warm_host_path: &str,
    effective_host_dir: &str,
) -> bool {
    let warm = normalize_host_for_prefix(warm_host_path);
    let dir = normalize_host_for_prefix(effective_host_dir);
    warm.len() > dir.len()
        && warm.starts_with(&dir)
        && warm.as_bytes().get(dir.len()) == Some(&b'/')
}

/// Drain droppable warm entries whose on-disk path is NOT under the mesh's
/// current effective directory (issue #1519). Never touches `claimed` rows
/// (live spawns) or running Agent Nodes — only idle pool inventory.
/// Returns the number drained. Emits `pool-count-changed` iff anything
/// was dropped, mirroring `drain_excess_warm_entries`.
pub fn drain_stale_location_entries(
    app: &tauri::AppHandle,
    mesh: &db::WarmPoolMeshRow,
) -> Result<usize, String> {
    let effective_host = effective_host_dir_for_mesh(mesh);
    let all = db::list_all_droppable_warm_entries_for_mesh(mesh.id)
        .map_err(|e| format!("stale-location list for mesh {}: {}", mesh.id, e))?;
    let stale: Vec<(i64, String)> = all
        .into_iter()
        .filter(|(_, p)| !warm_path_under_effective_dir(p, &effective_host))
        .collect();
    if stale.is_empty() {
        return Ok(0);
    }
    let mut dropped = 0usize;
    for (id, path) in stale {
        if let Err(e) = db::delete_warm_worktree(id) {
            tracing::warn!(
                "warm_pool: stale-location drain failed to delete row {} ({}): {}",
                id,
                path,
                e
            );
            continue;
        }
        if let Err(e) = crate::git::worktree::remove_one_worktree(&path) {
            tracing::warn!(
                "warm_pool: stale-location drain dropped DB row {} but failed to remove {}: {} (orphan will be GC'd on next reconcile)",
                id,
                path,
                e
            );
        }
        dropped += 1;
    }
    if dropped > 0 {
        tracing::info!(
            "warm_pool: drained {} stale-location entries for mesh {} (effective dir changed)",
            dropped,
            mesh.id
        );
        emit_pool_changed(app, mesh.id);
    }
    Ok(dropped)
}

/// Pending directory-change rebuild work. `all` (from an application-default
/// change) subsumes per-mesh requests; per-mesh ids accumulate.
#[derive(Debug, Default)]
struct RebuildPending {
    all: bool,
    meshes: Vec<i64>,
}

/// Pure merge for [`rebuild_pools_for_worktree_dir_change`]'s queue.
/// `None` (rebuild everything) wins over any queued mesh id; mesh ids
/// accumulate deduplicated. Pulled out so rapid save-correct-save bursts
/// collapse deterministically without standing up threads in tests.
fn merge_rebuild_request(pending: &mut RebuildPending, mesh_id: Option<i64>) {
    match mesh_id {
        None => {
            pending.all = true;
            pending.meshes.clear();
        }
        Some(id) => {
            if !pending.all && !pending.meshes.contains(&id) {
                pending.meshes.push(id);
            }
        }
    }
}

static REBUILD_PENDING: once_cell::sync::Lazy<parking_lot::Mutex<RebuildPending>> =
    once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(RebuildPending::default()));
static REBUILD_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Rebuild idle warm-pool inventory after a `worktree_directory` setting
/// changes (issue #1519). `mesh_id = Some(id)` rebuilds one Mesh (per-Mesh
/// override change); `None` rebuilds every worktree-enabled Mesh whose
/// effective dir could have moved (application-default change — only
/// meshes WITHOUT a per-Mesh override inherit it, but draining stale
/// locations is idempotent so rebuilding all is safe and simpler).
/// Never moves running Agent Nodes — only droppable (`!= claimed`) pool
/// rows are drained, then each affected mesh refills to target.
///
/// Serialized + debounced: rapid successive saves (type → correct → save)
/// merge into one pending request served by a single runner thread under
/// the blocking fill lock — never two threads issuing `git worktree add`
/// against the same repo concurrently (which fails on `.git/config.lock`
/// and orphans rows). Safe to call from IPC handlers: it only enqueues
/// and returns immediately.
pub fn rebuild_pools_for_worktree_dir_change(app: &tauri::AppHandle, mesh_id: Option<i64>) {
    merge_rebuild_request(&mut REBUILD_PENDING.lock(), mesh_id);
    if !REBUILD_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let app = app.clone();
        std::thread::spawn(move || rebuild_runner(app));
    }
}

/// Single runner for the [`rebuild_pools_for_worktree_dir_change`] queue.
/// Drains the merged request, runs it under the blocking fill lock (so no
/// idle-tick fill, freshness pass, or sibling rebuild can interleave git
/// mutations), and loops while newer requests arrived mid-pass. The
/// take-then-recheck shutdown Rochambeau guarantees no merged request is
/// ever lost while keeping at most one runner alive.
fn rebuild_runner(app: tauri::AppHandle) {
    use std::sync::atomic::Ordering;
    loop {
        let req = {
            let mut pending = REBUILD_PENDING.lock();
            if pending.all || !pending.meshes.is_empty() {
                std::mem::take(&mut *pending)
            } else {
                RebuildPending::default()
            }
        };
        if !req.all && req.meshes.is_empty() {
            REBUILD_RUNNING.store(false, Ordering::SeqCst);
            if !REBUILD_PENDING.lock().all && REBUILD_PENDING.lock().meshes.is_empty() {
                return;
            }
            if REBUILD_RUNNING
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            continue;
        }
        let wanted_all = req.all;
        let wanted_meshes = req.meshes;
        crate::services::pool_worker::with_fill_lock_blocking(|| {
            run_rebuild_pass(&app, wanted_all, &wanted_meshes);
        });
    }
}

/// One serialized rebuild pass over the requested meshes: ensure the new
/// container is git-ignored, drain stale-location entries, refill to
/// target. Drain-before-fill ordering mirrors `reconcile_on_startup`:
/// stale-location entries must go even when the count is at target (the
/// excess drain alone would no-op).
fn run_rebuild_pass(app: &tauri::AppHandle, all: bool, meshes_wanted: &[i64]) {
    let meshes = match db::list_worktree_enabled_meshes_for_warm() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                "warm_pool: rebuild after worktree-dir change list failed: {}",
                e
            );
            return;
        }
    };
    let targets: Vec<db::WarmPoolMeshRow> = if all {
        meshes
    } else {
        meshes
            .into_iter()
            .filter(|m| meshes_wanted.contains(&m.id))
            .collect()
    };
    for mesh in targets {
        crate::git::worktree::ensure_container_excluded(
            &mesh.path,
            &effective_host_dir_for_mesh(&mesh),
        );
        if let Err(e) = drain_stale_location_entries(app, &mesh) {
            tracing::warn!(
                "warm_pool: stale-location drain failed for mesh {} ({}): {}",
                mesh.id,
                mesh.path,
                e
            );
        }
        fill_mesh_to_target(app, &mesh);
        emit_pool_changed(app, mesh.id);
    }
}

/// Cut one fresh warm worktree for `mesh`, inserting a `filling → available`
/// row pair around it. Idempotent: if a row already points at the computed
/// path, the call re-rolls the slug.
///
/// Count-free by design (issue #1752): the per-mesh target / disabled-pool /
/// transient-count early-exits live in [`fill_mesh_to_target_with`], which
/// samples the available count **once** rather than re-querying it per entry.
/// The body therefore takes nothing it does not use — no running-count
/// parameter just to enrich a log line.
///
/// Errors from `create_git_worktree` (after slug retries) propagate as
/// `Err(String)` so the fill loop logs, stops, and lets the next pass retry.
///
/// Emits `pool-count-changed` after a successful fill (count +1) so a
/// long-running multi-row fill surfaces each row becoming available
/// immediately, instead of all-at-once at end-of-pass.
///
/// The `git worktree add` cadence stays strictly serial: every caller runs
/// behind the pool fill lock (issue #613 AC4), so two adds never target one
/// repo concurrently. Only the redundant counting was batched.
fn warm_one_body(app: &tauri::AppHandle, mesh: &db::WarmPoolMeshRow) -> Result<(), String> {
    let app = app.clone();
    let mesh_path = mesh.path.clone();
    let base_ref = mesh.base_ref.clone();
    let mesh_id = mesh.id;
    let deps = FillDeps {
        insert_marker: Box::new(move |path, slug| db::begin_warm_fill(mesh_id, path, slug)),
        create_worktree: Box::new(move |path, slug| {
            crate::git::worktree::create_git_worktree(&mesh_path, path, slug, "detached", &base_ref)
        }),
        mark_available: Box::new(|row_id, base_sha| {
            db::mark_warm_worktree_available(row_id, base_sha)
        }),
        delete_row: Box::new(db::delete_warm_worktree),
        head_sha: Box::new(|path| {
            // The crate's one SHA resolver with an explicit source: the
            // `LocalHead` source, read from the *worktree* directory (a
            // linked worktree's own HEAD, not the main repository's). `None`
            // on any failure — the row stays `available` with
            // `base_sha = NULL` and the spawn-time freshness check degrades
            // to "no SHA to compare".
            crate::git::sha::resolve_sha_at(path, &crate::git::sha::ShaSource::LocalHead)
                .ok()
                .map(|oid| oid.to_string())
        }),
        emit_changed: Box::new(move || emit_pool_changed(&app, mesh_id)),
    };
    warm_one_with(mesh, deps)
}

/// The fill's side-effecting dependencies, injected so the crash-recovery
/// path can be exercised without the global DB, a real git repo, or a Tauri
/// app handle (issue #2533).
///
/// The fill is three writes around one piece of disk work: the crash-recovery
/// marker, the checkout, and the flip to `available`. Injecting them makes
/// the window between the marker and the flip testable — a test can "kill"
/// the fill there and assert that the startup reconcile finds the row.
struct FillDeps {
    /// Write the crash-recovery marker row. Runs BEFORE the disk work — see
    /// [`db::begin_warm_fill`].
    insert_marker: InsertMarker,
    /// Cut the worktree on disk.
    create_worktree: CreateWorktree,
    /// Flip the marker row to `available` and stamp the SHA it landed on.
    mark_available: MarkAvailable,
    /// Drop the marker row (the checkout failed).
    delete_row: DeleteRow,
    /// Read the SHA the new worktree is checked out at.
    head_sha: HeadSha,
    /// Announce the new row to the UI.
    emit_changed: Box<dyn Fn()>,
}

/// Write the fill's crash-recovery marker: `(path, slug) -> row id`.
type InsertMarker = Box<dyn Fn(&str, &str) -> db::SqlResult<i64>>;

/// Cut the worktree on disk: `(path, slug) -> ()`.
type CreateWorktree = Box<dyn Fn(&str, &str) -> Result<(), String>>;

/// Flip the marker row to `available`: `(row id, base SHA) -> ()`.
type MarkAvailable = Box<dyn Fn(i64, Option<&str>) -> db::SqlResult<()>>;

/// Drop the marker row: `(row id) -> ()`.
type DeleteRow = Box<dyn Fn(i64) -> db::SqlResult<()>>;

/// Read the SHA a worktree is checked out at: `(path) -> SHA`.
type HeadSha = Box<dyn Fn(&str) -> Option<String>>;

/// The fill's body, with its side effects injected (see [`FillDeps`]).
///
/// The ordering is the contract: the crash-recovery marker is written first,
/// the checkout happens second, and the flip to `available` happens third. A
/// process killed anywhere in that window leaves the `filling` marker behind
/// for the startup reconcile to find.
fn warm_one_with(mesh: &db::WarmPoolMeshRow, deps: FillDeps) -> Result<(), String> {
    // Pick a fresh slug. Slug collisions are vanishingly rare (~3.9B combos
    // in the seed pool; #608 follow-up adds the `preassigned_name` UNIQUE
    // guard for the per-mesh case), but if the directory already exists on
    // disk we re-roll up to a small bounded number of times before bailing.
    // Issue #1519: warm entries live under the mesh's effective worktree
    // dir (Mesh override → app default → `.worktrees`), not always
    // the legacy layout.
    let mut last_err: Option<String> = None;
    for _ in 0..4 {
        let slug = fresh_slug();
        let host_path = warm_worktree_host_path_for_mesh(mesh, &slug);
        if Path::new(&host_path).exists() {
            // Lost the slug lottery on a stale leftover; retry with a new
            // slug so we don't try to `git worktree add` into an existing
            // path (which would no-op or fail).
            continue;
        }

        // Write the crash-recovery marker BEFORE cutting the worktree: a
        // crash mid-checkout leaves a recoverable `filling` row for the
        // startup reconcile to find (issue #2533 — the marker is the fill's
        // explicit crash-recovery interface, not a convention).
        let row_id = (deps.insert_marker)(&host_path, &slug)
            .map_err(|e| format!("warm_pool insert filling: {}", e))?;

        // Cut the on-disk worktree. We pin mode = "detached" so a future
        // claim can `git checkout -B <branch>` to upgrade to branched mode
        // (or stay detached if the mesh says so). Detached also avoids
        // touching the mesh's branch refs.
        match (deps.create_worktree)(&host_path, &slug) {
            Ok(()) => {
                // Flip to available + stamp the base SHA the worker just
                // checked out at. We compute it from the new worktree's
                // HEAD; failures here are non-fatal — the row stays
                // `available` and the spawn-time freshness check falls
                // back to "no SHA to compare" semantics.
                let base_sha = (deps.head_sha)(&host_path).unwrap_or_default();
                let base_sha_opt = if base_sha.is_empty() {
                    None
                } else {
                    Some(base_sha.as_str())
                };
                if let Err(e) = (deps.mark_available)(row_id, base_sha_opt) {
                    tracing::warn!(
                        "warm_pool: failed to flip row {} to available: {}",
                        row_id,
                        e
                    );
                }
                tracing::info!(
                    "warm_pool: prewarmed {} for mesh {} (target {})",
                    slug,
                    mesh.id,
                    mesh.pre_spawn_pool_size,
                );
                // Surface the new row immediately — a long multi-row fill
                // gets to show "1/N ready" → "2/N ready" mid-tick instead
                // of all-at-once at end-of-pass.
                (deps.emit_changed)();
                return Ok(());
            }
            Err(e) => {
                // Roll back the filling row so the reconcile doesn't have
                // to garbage-collect it on the next pass.
                let _ = (deps.delete_row)(row_id);
                last_err = Some(e);
                continue;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| "warm_pool: exhausted slug retries".to_string()))
}

/// Drain any excess, then fill a single mesh up to its `pre_spawn_pool_size`
/// target (issue #613). Delegates the fill loop to [`fill_mesh_to_target`],
/// which cuts one entry at a time from a once-sampled available count
/// (issue #1752) until the mesh reaches target or a `git worktree add` fails.
///
/// Looks the mesh up by id (vs. `fill_mesh_to_target`'s in-hand row) so a
/// single-mesh caller like `update_mesh_pool_size` doesn't need to first
/// query `list_worktree_enabled_meshes_for_warm`. Silently no-ops when the
/// mesh is not worktree-enabled — the caller already wrote the column, so
/// the drain/fill aren't applicable.
///
/// All state changes inside (`drain_excess_warm_entries`, `warm_one_body`)
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

/// Meshes already reported as "not a git working repository" this app run
/// (issue #2348). Keyed by `(mesh_id, path)` — not id alone — so a mesh whose
/// folder is re-pointed gets its own line instead of being silently deduped
/// against the path that was already reported.
static NON_REPO_REPORTED: Lazy<Mutex<HashSet<(i64, String)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Claim the one-and-only report for this app run, returning `true` when the
/// caller is the first to see this `(mesh_id, path)`. The latch is the half of
/// issue #2348 that must stay quiet; the *probe* deliberately is not latched
/// (see [`mesh_repo_unusable`]).
fn claim_non_repo_report(mesh_id: i64, path: &str) -> bool {
    let key = (mesh_id, path.to_string());
    match NON_REPO_REPORTED.lock() {
        Ok(mut seen) => seen.insert(key),
        Err(poisoned) => poisoned.into_inner().insert(key),
    }
}

/// Issue #2348: can this Mesh's folder back a worktree at all?
///
/// Both pool paths that create a worktree or fetch consult this first — the
/// idle fill (`fill_mesh_to_target`) and the worker's background sync
/// (`services::pool_worker`). `create_git_worktree` resolves its
/// `project_root` through `git::project::resolve` before it touches anything
/// else, so every folder this probe rejects is a folder whose worktree add
/// cannot succeed either: a plain folder is libgit2's `NotFound (-3)`, which
/// is exactly the error the hub log was full of. Attempting it on every idle
/// tick cost a doomed `git worktree add` and a WARN line each time — 982 of
/// them in one day of hub log for a single Mesh (`F:\src\life\finance`),
/// drowning the diagnostics operators actually read.
///
/// The probe is that same `resolve`, re-run whenever work is due rather than
/// latched: a Mesh whose folder is re-mounted, moved or `git init`-ed
/// mid-session becomes poolable again on the next pass with no app restart.
/// Only the log is latched, via [`claim_non_repo_report`], which is what holds
/// the line count to one per Mesh per app run.
///
/// The wording is deliberately "cannot be opened as" rather than "is not":
/// `resolve` also rejects a missing or unreachable folder (an unplugged
/// drive) and a bare repository, and the reason travels with the one log line
/// so those stay diagnosable. None of them report success to a caller — the
/// pool work is skipped, not silently claimed.
///
/// **`refresh_stale_warm_worktrees` is deliberately NOT gated**: it runs only
/// from `on_fetch_completed` (which this gate now makes unreachable for an
/// unpoolable Mesh — the background sync is skipped, so no ref can advance) or
/// from `post_spawn_maintenance` after a spawn that already cut a worktree, and
/// it reports once per spawn rather than once per tick.
pub(crate) fn mesh_repo_unusable(mesh: &db::WarmPoolMeshRow) -> bool {
    let Err(reason) = crate::git::project::resolve(&mesh.path) else {
        return false;
    };
    if claim_non_repo_report(mesh.id, &mesh.path) {
        tracing::warn!(
            "warm_pool: mesh {} ({}) cannot be opened as a git working \
             repository; skipping pool fill and background sync (reported once \
             per app run): {}",
            mesh.id,
            mesh.path,
            reason
        );
    }
    true
}

/// Drain any excess, then fill a single mesh up to its `pre_spawn_pool_size`
/// target (issue #613). Each entry is cut one at a time; the loop runs until
/// the running `available` count reaches `target` or a `git worktree add`
/// fails.
///
/// The loop is bounded by the deficit rather than by `target`: `available`
/// starts at the once-sampled count and rises by one per successful cut, so a
/// mesh already partway to target only pays for the missing entries.
///
/// The "single sequential `git worktree add`" cadence the issue calls for
/// falls out naturally: this runs the cuts one after another on the calling
/// thread, and every caller is already serialized behind the fill lock.
///
/// `count`, `drain` and `warm_one` are passed as closures so the orchestration
/// can be unit-tested without `tauri::AppHandle` (which is hard to mock) — the
/// production caller passes the real `mesh_repo_unusable` /
/// `count_available_warm_for_mesh` / `drain_excess_warm_entries` /
/// `warm_one_body` (the last two close over `app`); tests pass stubs that
/// drive the drain + warm loop with controlled responses. `count` is `FnOnce`
/// because it is sampled exactly once, which is the point of issue #1752.
/// See the `fill_mesh_to_target_*` tests at the bottom of this module
/// (issue #634).
///
/// `repo_usable` is the issue #2348 precondition for the warm loop, and it is
/// consulted only when there is a deficit to fill: a mesh already at target (or
/// with the pool off) has no `git worktree add` that could fail, so it must not
/// pay for the probe — on an unreachable drive that open is the expensive call.
/// When it does answer "not a repository", the drain's changes are still
/// reported and no worktree is ever cut, which is what stops the retry-forever
/// WARN flood #2348 was filed for.
///
/// Trade-off of the single sample: the running total only knows about the cuts
/// this call makes, so a concurrent `try_claim` that drains an `available` row
/// mid-pass leaves this call a little short (it aims at the count it saw, not
/// the shrinking true count). That is bounded by the number of concurrent
/// claims and self-heals on the next pass (the idle worker re-checks within a
/// tick, and every claim schedules its own refill). The alternative — re-counting
/// per entry — was the redundant work #1752 removes for the common case where
/// nothing else touches the pool during a fill.
fn fill_mesh_to_target_with(
    mesh: &db::WarmPoolMeshRow,
    repo_usable: impl FnOnce() -> bool,
    count: impl FnOnce() -> Result<i64, String>,
    drain: impl FnOnce(&db::WarmPoolMeshRow) -> Result<usize, String>,
    warm_one: impl Fn(&db::WarmPoolMeshRow) -> Result<(), String>,
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
    if target <= 0 {
        // Pool disabled for this mesh — the drain above was the only useful
        // work, and we skip the count entirely (issue #1752).
        return changes;
    }
    // Issue #1752: one count per fill, not one per entry. A target-N fill used
    // to issue N `count_available_warm_for_mesh` queries (one per warm call).
    // We sample once and track the running total locally; the count still runs
    // AFTER the drain so a downsize is reflected.
    let mut available = match count() {
        Ok(n) => n,
        Err(reason) => {
            // Issue #634: a transient DB error used to be silently absorbed as
            // `Ok(false)`, aborting the fill with no log signal. Log WARN and
            // stop — the next idle tick or post-spawn path retries. The drain
            // already ran, so its changes are still reported.
            tracing::warn!(
                "warm_pool: fill aborted for mesh {} ({}): {}",
                mesh.id,
                mesh.path,
                reason
            );
            return changes;
        }
    };
    // Issue #2348: the one thing `warm_one` cannot do is cut a
    // worktree from a folder that is not a git working repository —
    // `create_git_worktree` fails there (libgit2 `NotFound (-3)` for a plain
    // folder) on every attempt, and each attempt used to log its own WARN.
    // Probe only when there is a deficit to fill, then stop: the mesh reports
    // no changes, so the idle worker backs off to TICK_SLOW instead of
    // retrying on every 2s tick. The drain above still runs — it is DB
    // bookkeeping plus `git worktree remove` on rows this mesh already owns,
    // it drops the row first, and it converges in one pass, so it cannot
    // produce a repeating flood.
    if available < target && !repo_usable() {
        return changes;
    }
    while available < target {
        match warm_one(mesh) {
            Ok(()) => {
                available += 1;
                changes += 1;
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
    // Issue #1519: drain stale-location entries first so a directory
    // setting changed while the app was off converges on next reconcile
    // even when the count is already at target (excess drain alone
    // would no-op). Never touches `claimed` rows or live nodes.
    if let Err(e) = drain_stale_location_entries(app, mesh) {
        tracing::warn!(
            "warm_pool: stale-location drain failed for mesh {} ({}): {}",
            mesh.id,
            mesh.path,
            e
        );
    }
    fill_mesh_to_target_with(
        mesh,
        // Issue #2348: a folder that isn't a git working repository can never
        // be filled. Consulted lazily (only when a fill is due) — see
        // `fill_mesh_to_target_with`.
        || !mesh_repo_unusable(mesh),
        // Sampled once per fill (issue #1752) — read after the drain so a
        // downsize is reflected.
        || {
            db::count_available_warm_for_mesh(mesh.id).map_err(|e| {
                format!(
                    "count_available_warm_for_mesh failed for mesh {}: {}",
                    mesh.id, e
                )
            })
        },
        |m| drain_excess_warm_entries(app, m),
        |m| warm_one_body(app, m),
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
/// `warm_one_body` emits can reach the frontend. No end-of-pass emit here —
/// the inner calls already fire one per real state change, and a blanket
/// end-of-pass emit would wake the listener every idle tick (every ~2s)
/// even when nothing changed.
///
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
            // Pool entries are detached (`warm_one_body` cuts them with mode
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
                tracing::warn!(
                    "warm_pool: {} list failed for mesh {}: {}",
                    what,
                    mesh_id,
                    e
                );
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
/// `warm_one_body` emits can reach the frontend. The `try_claim` call that
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
/// Idempotent: safe to call from `reconcile_on_startup`, the idle
/// `drain_and_fill_for_mesh` pass, and `refill_after_claim`.
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
    // entries to refresh. Bail before the one SHA resolver's git2 repo-open
    // so a "pool off" mesh pays no freshness cost on every advancing fetch
    // (issue #613 review).
    if mesh.pre_spawn_pool_size <= 0 {
        return Ok(0);
    }
    let new_sha = crate::git::sha::resolve_sha_at(
        &mesh.path,
        &crate::git::sha::ShaSource::BaseRef(mesh.base_ref.clone()),
    )
    .map_err(|e| e.to_string())?
    .to_string();
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
        // The freshness rule is owned by the consumer-side seam
        // (`git::worktree::warm::warm_entry_is_fresh`), so the pool and the
        // provisioner stay in agreement about what "still usable" means
        // (issue #2533).
        if crate::git::worktree::warm::warm_entry_is_fresh(entry.base_sha.as_deref(), new_sha) {
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
#[path = "warm_pool_tests.rs"]
mod tests;
