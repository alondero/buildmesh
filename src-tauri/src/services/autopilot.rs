//! Autopilot polling daemon & concurrency scheduler (issue #482, PRD #480,
//! wayfinder #990 / tickets #991–#994).
//!
//! A long-lived background thread that, every [`POLL_INTERVAL`], walks every
//! mesh with `autopilot_enabled = 1` and ingests newly-labelled GitHub issues
//! as auto-spawned Agent Nodes:
//!
//! 1. **Capacity first, network second** (PRD story 6): a mesh whose active
//!    auto-spawned node count (`db::count_active_autopilot_nodes`) already
//!    meets `autopilot_concurrency_limit` is skipped *before* any GitHub
//!    round-trip, so no queued/stale node rows ever accumulate.
//! 2. **Ingest = the current open+labelled set**: the poller asks GitHub for
//!    issues that are open *and* carry the trigger label *right now*
//!    (`GitHubClient::list_open_issues_with_label`). Issues closed or
//!    untagged while the app was offline never appear, so startup state
//!    reconciliation falls out of the query shape (PRD story 8).
//! 3. **Dedupe against everything we already know**: any issue number that
//!    already has a node in this mesh — auto-spawned (the `autopilot_runs`
//!    ledger) or manually issue-spawned (`agent_nodes.source_issue`) — is
//!    never spawned twice, including after its node completed or errored.
//! 4. **Collaborator gate** (issue #499, ADR-0012 §5): before spawning, the
//!    issue author's push access is checked via `autopilot::gate_trigger`.
//!    Only `AutoRun` triggers spawn; `RequireApproval` triggers are parked in
//!    [`GATED_TRIGGERS`] (logged once, skipped on later passes without
//!    re-spending a rate-limited permission fetch). The approval UI is a
//!    later slice — until it lands, an external author's issue simply waits.
//! 5. **Enforced branched worktree** (PRD story 4): the node row is created
//!    with `use_worktree = true` regardless of the mesh's setting, and
//!    `spawn_agent_inner` forces `worktree_mode = "branched"` for any node
//!    with an `autopilot_runs` row — a detached-HEAD worktree could not push
//!    a branch for the wrap-up PR.
//!
//! ## Looping Autopilot mode (ticket #992)
//!
//! When `mesh.autopilot_mode == Looping`, `poll_mesh` dispatches to
//! [`poll_mesh_looping`] instead of the issue-driven path. The looping
//! path uses no GitHub traffic at all — it walks the `autopilot_runs`
//! ledger for loop-iteration rows (filtered by `loop_iteration IS NOT
//! NULL`, the v31 column) and runs [`evaluate_loop_continuation`], a pure
//! decision core that compares the mesh's loop config + derived history
//! against the current wall clock to decide whether to spawn the next
//! iteration. Two `use_worktree` semantics diverge between the modes:
//!
//! - **IssueDriven** (this file's existing spawn) — `use_worktree` is
//!   FORCED on (line ~482) because the wrap-up PR needs a real branch
//!   to push.
//! - **Looping** (ticket #992) — `use_worktree` RESPECTS the mesh's
//!   setting. Game-decompilation-style repos (the canonical non-worktree
//!   mesh) need to run on the root branch directly; a forced worktree
//!   would silently break them. The poller passes `None` for the
//!   `use_worktree_override` argument, letting
//!   `services::agent_node::create_with_source_pr_fork` fall through to
//!   `mesh.use_worktree` (the line `let use_worktree =
//!   use_worktree_override.unwrap_or(mesh.use_worktree);`).
//!
//! Threading: the pass runs entirely on this worker's OS thread (blocking
//! reqwest + git shell-outs are fine here — this is NOT the tokio pool).

use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

use crate::autopilot::{gate_trigger, AutopilotTrigger, GateDecision};
use crate::db;
use crate::db::AutopilotRunState;
use crate::models::{
    AutopilotMode, Mesh, DEFAULT_AUTOPILOT_TRIGGER_LABEL,
};
use crate::services::github::{parse_blocked_by, GitHubClient, Issue};

/// Payload of the `autopilot-node-closed` Tauri event. Emitted from the
/// merged-PR sweep when an autopilot-managed PR was merged and the node is
/// being archived (NOT deleted — the branch and scrollback stay in the
/// Archive tab). The frontend refetches the node list and shows a toast
/// explaining why the card vanished from the grid.
///
/// Generated to `src/types/generated/AutopilotNodeClosedPayload.ts`; the TS
/// half is imported by `src/stores/agentNodeStore.ts` and `src/App.tsx`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AutopilotNodeClosedPayload.ts")]
pub struct AutopilotNodeClosedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "i32")]
    pub pr_number: i64,
}

/// PRD #480 implementation decision: poll every 2 minutes.
pub const POLL_INTERVAL: Duration = Duration::from_secs(120);

/// Grace period before the first pass so startup (DB migration, HTTP bind,
/// pool reconcile) finishes before we compete for the DB mutex and network.
const STARTUP_DELAY: Duration = Duration::from_secs(20);

/// How long a `finishing` ledger row must sit untouched before the poller
/// re-verifies it (see `pipeline::redrive_stalled_finishing`). Short is safe:
/// the re-drive only *completes* observably-green runs, and a green
/// observation means the wrap-up work exists regardless of what the agent is
/// doing right now.
const FINISHING_REDRIVE_STALE_MINUTES: i64 = 5;

/// `(mesh_id, issue_number)` pairs whose author failed the collaborator gate.
/// Remembered for the app's lifetime so each gated trigger costs exactly one
/// permission fetch + one log line, not one per pass. Cleared on restart —
/// cheap to re-derive, and a permission granted meanwhile is picked up then.
static GATED_TRIGGERS: Lazy<Mutex<HashSet<(i64, i64)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// `(mesh_id, issue_number)` pairs whose blocked-by `info` log has already
/// been emitted this app lifetime (map #976, issue #489's mirror inside the
/// planner). Dedupes the LOG line, not the spawn-skip — the skip is
/// unconditional whenever an unresolved blocker exists; the set just keeps
/// the log from spamming once per 2-minute pass. Cleared on restart —
/// cheap to re-derive, and a blocker that's since resolved (the blocker
/// issue closed or its node archived) gets re-evaluated on the next pass.
static LOGGED_BLOCKS: Lazy<Mutex<HashSet<(i64, i64)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Lock one of the planner's app-lifetime tracking sets, recovering from
/// a poisoned mutex instead of panicking (issue #1224).
///
/// The planner holds two small `HashSet` mutexes — `GATED_TRIGGERS` and
/// `LOGGED_BLOCKS` — neither of which guards an invariant that a panic
/// mid-write would corrupt (the panic would have released the guard on
/// unwind, leaving the inner set in a consistent `HashSet` state).
/// `.unwrap()` on `PoisonError` permanently bricks the planner: the next
/// collaborator-gate pass or blocked-by dedupe would panic the polling
/// thread and the daemon silently stops scheduling work. The recover
/// shape matches `db::lock_db()`, `preferences::save`, and
/// `services::circuit_worker::lock_circuit_worker_static` (which is the
/// companion helper for the circuit-worker's `APPROVALS`/`WAKE` statics).
fn lock_planner_set<T>(mutex: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                "autopilot planner mutex was poisoned by a prior panic — recovering (issue #1224)"
            );
            poisoned.into_inner()
        }
    }
}

/// Start the Autopilot polling daemon. Called once from Tauri `setup`
/// (mirrors `services::pool_worker::start_background_worker`).
pub fn start_autopilot_worker(app: AppHandle) {
    std::thread::spawn(move || {
        // Hydrate the evaluator's piloted-node registry from the ledger so
        // runs that were mid-pipeline before a restart keep evaluating once
        // their node auto-resumes.
        match db::list_active_autopilot_node_ids() {
            Ok(ids) => {
                for id in ids {
                    crate::autopilot::evaluator::register(id);
                }
            }
            Err(e) => tracing::warn!("autopilot: piloted-node hydration failed: {}", e),
        }
        std::thread::sleep(STARTUP_DELAY);
        loop {
            run_poll_pass(&app);
            std::thread::sleep(POLL_INTERVAL);
        }
    });
}

/// One full pass over every autopilot-enabled mesh. Per-mesh failures are
/// logged and isolated — one mesh's bad remote must not starve the others.
fn run_poll_pass(app: &AppHandle) {
    // Re-drive stalled wrap-ups BEFORE the per-mesh loop: the pipeline is
    // turn-driven and a lost final turn strands a green, already-PR'd run in
    // `finishing` forever (node 2328, 2026-07-17). Completing it here frees
    // its concurrency slot for the capacity counts just below, in this same
    // pass. Runs across ALL meshes — not just autopilot-enabled ones — so
    // toggling a mesh's autopilot off can't strand its in-flight wrap-ups.
    // Conservative: the re-drive only completes observably-green runs.
    match db::list_stalled_finishing_autopilot_runs(FINISHING_REDRIVE_STALE_MINUTES) {
        Ok(stalled) if !stalled.is_empty() => {
            crate::autopilot::pipeline::redrive_stalled_finishing(app, &stalled)
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("autopilot: stalled-run listing failed: {}", e),
    }

    // Watchdog: synthesize the evaluation a lost turn never delivered (#874).
    // Covers what the green-only re-drive can't — a lost turn during
    // `implementing`, or a red `finishing` stall — gated on the node's PTY
    // output having been quiet long enough that the agent isn't mid-response.
    match db::list_active_autopilot_node_ids() {
        Ok(active) if !active.is_empty() => {
            crate::autopilot::pipeline::watchdog_pass(app, &active)
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("autopilot: active-run listing failed: {}", e),
    }

    let meshes = match db::list_autopilot_enabled_meshes() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("autopilot: could not list enabled meshes: {}", e);
            return;
        }
    };
    // App-wide pool budget (Settings → autopilot_pool_size): how many more
    // autopilot nodes may spawn THIS PASS across every mesh combined. `None`
    // = no global cap (the pre-setting behaviour). Computed once here and
    // decremented by `poll_mesh` per successful spawn, so the meshes earlier
    // in the loop can't be double-counted by the ones after them. A count
    // failure fails CLOSED (budget 0, retried next pass) — the setting
    // exists to protect the machine, so "unknown load" must not mean
    // "unlimited".
    let mut global_budget: Option<i64> = match crate::preferences::autopilot_pool_size() {
        None => None,
        Some(pool) => match db::count_active_autopilot_nodes_total() {
            Ok(total) => Some(i64::from(pool) - total),
            Err(e) => {
                tracing::warn!(
                    "autopilot: total active count failed, skipping spawns this pass: {}",
                    e
                );
                Some(0)
            }
        },
    };
    for mesh in meshes {
        if let Err(e) = poll_mesh(app, &mesh, &mut global_budget) {
            tracing::warn!("autopilot: mesh {} ({}) pass failed: {}", mesh.id, mesh.name, e);
        }
    }
}

fn poll_mesh(
    app: &AppHandle,
    mesh: &Mesh,
    global_budget: &mut Option<i64>,
) -> Result<(), String> {
    // Issue #1152 — revalidate compatibility at the scheduler boundary
    // (acceptance criteria 7: "Revalidate at the scheduler/worker boundary
    // before creating an Agent Node. Stale or externally modified enabled
    // state must not launch an incompatible harness"). A user can flip a
    // mesh's `use_worktree` to false, or change the default provider, or
    // change the explicit Autopilot selection, while the mesh is enabled;
    // the backend write paths auto-disable on the same turn (see
    // `update_mesh_use_worktree` / the harness-defaults writers), but the
    // re-check here is the second-and-final gate so an out-of-band write
    // (e.g. a stale UI snapshot, a direct DB edit, a third-party IPC
    // caller that bypassed the UI) cannot spawn an incompatible Agent
    // Node. We persist `autopilot_enabled = 0` through the existing
    // narrow write so the next pass skips this mesh entirely; running
    // Agent Nodes are NOT killed (the in-flight wrap-up runs complete on
    // their own — issue #1152 AC #11).
    let verdict = crate::autopilot::compatibility::compute_for_mesh(
        mesh.autopilot_provider.as_deref(),
        mesh.default_provider.as_deref(),
        crate::preferences::default_provider().as_deref(),
        mesh.use_worktree,
    );
    if !verdict.allowed {
        tracing::warn!(
            "autopilot: mesh {} ({}) is incompatible — disabling: harness={} reasons={:?}",
            mesh.id,
            mesh.name,
            verdict.resolved_spawn_option.as_deref().unwrap_or("<none>"),
            verdict.reasons,
        );
        if let Err(e) = db::set_mesh_autopilot_enabled(mesh.id, false) {
            tracing::warn!(
                "autopilot: failed to disable incompatible mesh {}: {}",
                mesh.id,
                e
            );
        }
        return Ok(());
    }
    // The merged-PR sweep runs BEFORE the capacity gate: a mesh at capacity
    // must still get its finished nodes archived (that's what clears grid
    // space), and the sweep costs no network when there's nothing to sweep.
    // Both modes share this sweep — a loop-mode node that ran the wrap-up
    // `finish.md` and opened a PR (issue #993's flow) ends up in the same
    // ledger state machine as an issue-driven run, so the close-sweep works
    // for both.
    let sweep_candidates =
        db::list_completed_autopilot_runs_with_pr(mesh.id).unwrap_or_default();

    let active = db::count_active_autopilot_nodes(mesh.id).map_err(|e| e.to_string())?;
    let capacity = effective_capacity(
        i64::from(mesh.autopilot_concurrency_limit) - active,
        *global_budget,
    );
    // PRD story 6: no spare capacity AND nothing to sweep → no GitHub
    // round-trip at all. Looping mode also short-circuits here when no
    // capacity AND no sweep candidates — the looping path has its own
    // pass-through return below when capacity > 0 but the looping
    // evaluator decides to skip.
    if capacity <= 0 && sweep_candidates.is_empty() {
        return Ok(());
    }

    // Mode dispatch (ticket #992). Looping mode never round-trips
    // GitHub — it walks the ledger + uses `loop_initial_prompt` directly.
    // Both modes still go through `close_merged_nodes` above (the
    // sweep is per-mesh and independent of the spawn path).
    if mesh.autopilot_mode == AutopilotMode::Looping {
        return poll_mesh_looping(app, mesh, capacity);
    }

    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(mesh)?;
    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    close_merged_nodes(app, &client, &owner, &repo, &sweep_candidates);
    if capacity <= 0 {
        return Ok(());
    }
    let label = mesh
        .autopilot_trigger_label
        .as_deref()
        .unwrap_or(DEFAULT_AUTOPILOT_TRIGGER_LABEL);
    let issues = client
        .list_open_issues_with_label(&owner, &repo, label)
        .map_err(|e| e.to_string())?;
    let known = db::list_known_autopilot_issue_numbers(mesh.id).map_err(|e| e.to_string())?;

    // Map #976: skip issues whose body declares a `**Blocked by**` reference
    // that's still unresolved on this pass. `plan_spawns_with_blockers`
    // runs the blocked-by filter BEFORE the dedup-and-take (the worked
    // example: alondero/pixelgrab dep-chain silent-stall under
    // `concurrency_limit = 1` — the head-of-list blocked issue would
    // otherwise starve the unblocked tail). The `LOGGED_BLOCKS`-gated
    // "blocked by … parked" log is emitted inside the helper for every
    // blocked issue, not just the head, so the operator sees the full
    // parked set on the first observation of a pass.
    let open_numbers: HashSet<i64> = issues.iter().map(|i| i.number).collect();
    let planned = plan_spawns_with_blockers(
        &issues,
        &known,
        &open_numbers,
        capacity as usize,
        mesh.id,
        &mesh.name,
    );
    if planned.is_empty() {
        return Ok(());
    }
    tracing::info!(
        "autopilot: mesh {} ({}): {} labelled issue(s), {} known, capacity {} -> spawning {}",
        mesh.id,
        mesh.name,
        issues.len(),
        known.len(),
        capacity,
        planned.len()
    );

    for issue in planned {
        if lock_planner_set(&GATED_TRIGGERS).contains(&(mesh.id, issue.number)) {
            continue;
        }
        let trigger = AutopilotTrigger::from_issue(&owner, &repo, issue);
        match gate_trigger(&client, &trigger) {
            Ok(GateDecision::AutoRun) => {
                match spawn_autopilot_node(app, mesh, &owner, &repo, issue) {
                    // A successful spawn consumes one app-wide pool slot so the
                    // meshes later in this pass see the reduced budget. Failed
                    // spawns don't consume — nothing is running.
                    Ok(()) => {
                        if let Some(budget) = global_budget.as_mut() {
                            *budget -= 1;
                        }
                    }
                    Err(e) => tracing::warn!(
                        "autopilot: spawn for issue #{} on mesh {} failed: {}",
                        issue.number,
                        mesh.id,
                        e
                    ),
                }
            }
            Ok(GateDecision::RequireApproval) => {
                // ADR-0012 §5: an author without push access never auto-runs.
                // Remember the pair so we don't re-fetch the permission (and
                // re-log) every 2 minutes.
                lock_planner_set(&GATED_TRIGGERS).insert((mesh.id, issue.number));
                tracing::info!(
                    "autopilot: issue #{} on mesh {} gated — author '{}' lacks push \
                     access; waiting for manual spawn/approval",
                    issue.number,
                    mesh.id,
                    trigger.author
                );
            }
            Err(e) => {
                // Transient (network / rate limit): leave the issue unplanned;
                // the next pass retries the permission fetch.
                tracing::warn!(
                    "autopilot: permission check for issue #{} on mesh {} failed: {}",
                    issue.number,
                    mesh.id,
                    e
                );
            }
        }
    }
    Ok(())
}

/// Looping-mode poller pass (ticket #992). Mirrors the issue-driven
/// `poll_mesh` shape — merged-PR sweep + active-count check + capacity
/// gate — but the spawn path is replaced by [`evaluate_loop_continuation`]
/// reading `autopilot_runs` rows tagged with `loop_iteration` (the v31
/// column). No GitHub round-trip: looping mode never fetches issues.
/// The capacity gate is re-purposed — for looping, `capacity <= 0`
/// means "no app-wide budget room", and `active` was already counted
/// by the caller (the polling worker only enters the spawning branch
/// when an active count of 0 is feasible — handled inside
/// `evaluate_loop_continuation` via [`LoopSkipReason::ActiveIterationInProgress`]).
fn poll_mesh_looping(
    app: &AppHandle,
    mesh: &Mesh,
    capacity: i64,
) -> Result<(), String> {
    let initial_prompt = match mesh.loop_initial_prompt.as_deref() {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        // "Loop not configured" — the docstring on
        // [`crate::models::Mesh::loop_initial_prompt`] says `None` means
        // "stay idle". Don't fabricate a prompt; log once via the
        // tracing info so a future state inspector can spot a misconfigured
        // looping mesh. Treated as a no-op (next pass re-checks).
        _ => {
            tracing::debug!(
                "autopilot: mesh {} ({}) is Looping but has no initial prompt; staying idle",
                mesh.id,
                mesh.name
            );
            return Ok(());
        }
    };

    let active = db::count_active_autopilot_nodes(mesh.id).map_err(|e| e.to_string())?;
    let raw_rows = db::list_loop_iterations(mesh.id).map_err(|e| e.to_string())?;
    let history = derive_loop_history(&raw_rows);
    let now = SystemTime::now();

    match evaluate_loop_continuation(mesh, &history, active, capacity, now) {
        LoopDecision::Spawn { iteration } => {
            tracing::info!(
                "autopilot: mesh {} ({}) Looping iteration {} starting (capacity {}, active {})",
                mesh.id,
                mesh.name,
                iteration,
                capacity,
                active
            );
            spawn_loop_node(app, mesh, iteration, &initial_prompt)?;
        }
        LoopDecision::Skip(reason) => {
            tracing::debug!(
                "autopilot: mesh {} ({}) Looping skip this pass: {:?}",
                mesh.id,
                mesh.name,
                reason
            );
        }
    }
    Ok(())
}

/// Looping-mode poller's pure decision core (ticket #992). Given the
/// mesh's loop config + a derived [`LoopHistory`] + the current active
/// count + the remaining capacity + wall-clock now, decide whether to
/// spawn the next iteration and which iteration number it should be.
///
/// Pure — no DB, no I/O, no clock (caller passes `now`). Tested with
/// frozen `SystemTime` values to pin each branch deterministically.
/// Lives in `pub(crate)` so the integration test file can also exercise
/// it (mirrors `effective_capacity` / `plan_spawns` visibility).
pub(crate) fn evaluate_loop_continuation(
    mesh: &Mesh,
    history: &LoopHistory,
    active_count: i64,
    capacity: i64,
    now: SystemTime,
) -> LoopDecision {
    // Capacity gate: a negative or zero capacity (no app-wide pool
    // budget left) means we can't spawn this pass. We never kill a
    // running loop node — the same fail-soft contract as the
    // issue-driven path's `capacity <= 0` return. `PoolBudgetExhausted`
    // is its own variant so a probe / debug log can tell "interval"
    // apart from "budget" — issue #1263; the previous shape returned
    // `IntervalNotElapsed` and made pool-exhaustion look like a pacing
    // issue.
    if capacity <= 0 {
        return LoopDecision::Skip(LoopSkipReason::PoolBudgetExhausted);
    }
    // The "previous iteration still running" check. `active_count`
    // comes from `COUNT_ACTIVE_AUTOPILOT_SQL` which filters all non-terminal
    // states (`implementing`, `finishing`, and `suffix_pending`), so a row in
    // any terminal state (`completed`/`failed`/`merged`) is NOT counted —
    // exactly what we want, because a terminal iteration is "done" for loop
    // pacing purposes.
    if active_count > 0 {
        return LoopDecision::Skip(LoopSkipReason::ActiveIterationInProgress);
    }
    // Iteration cap. `loop_max_iterations == None` (or `0` post-validation)
    // means "continuous" — no cap. `Some(n>=1)` means stop after n
    // iterations; `iteration_count == n` is the first "exceeded" state.
    // The IPC boundary (`commands::mesh_properties::update_mesh_loop_config`)
    // already clamps to `n >= 1`, so the `>=` here is the same shape.
    if let Some(max) = mesh.loop_max_iterations {
        if history.iteration_count >= i64::from(max) {
            return LoopDecision::Skip(LoopSkipReason::MaxIterationsReached);
        }
    }
    // Consecutive-failure auto-pause. `loop_consecutive_failures == 0`
    // disables the threshold (the docstring on
    // [`crate::models::Mesh::loop_consecutive_failures`] pins this).
    // Strict-inequality: `n consecutive failures` is "the threshold was
    // hit" only when count `>= n`, so we compare `trailing_failures >=
    // threshold`. This matches the spec wording "auto-pause on N
    // consecutive failures".
    if mesh.loop_consecutive_failures > 0
        && history.trailing_failures >= i64::from(mesh.loop_consecutive_failures)
    {
        return LoopDecision::Skip(LoopSkipReason::ConsecutiveFailureThresholdReached);
    }
    // Interval pacing. `loop_interval_seconds == 0` means "no pause,
    // spawn as soon as the previous iteration finishes". `Some(n)`:
    // `now - last_spawn_at >= n seconds`. The first iteration has no
    // prior spawn timestamp, so we always pass this gate for
    // `iteration_count == 0`.
    if mesh.loop_interval_seconds > 0 {
        if let Some(last) = history.last_spawn_at {
            let required = Duration::from_secs(mesh.loop_interval_seconds as u64);
            if now.duration_since(last).unwrap_or(Duration::ZERO) < required {
                return LoopDecision::Skip(LoopSkipReason::IntervalNotElapsed);
            }
        }
    }
    LoopDecision::Spawn {
        iteration: history.iteration_count + 1,
    }
}

/// A snapshot of this mesh's loop iteration ledger, derived each pass
/// from [`db::list_loop_iterations`]. `pub(crate)` because the pure
/// decision core and its tests both live in this module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LoopHistory {
    /// How many iterations have been spawned for this mesh so far
    /// (including in-flight ones — `MAX(loop_iteration)`, not the
    /// terminal-state subset). `0` when no iterations exist yet.
    pub iteration_count: i64,
    /// The number of *consecutive terminal* iterations ending in
    /// [`AutopilotRunState::Failed`], walking from the highest
    /// iteration number downward and stopping at the first
    /// `Completed`/`Merged` (success). `0` when no iterations exist
    /// or the most recent terminal iteration was a success.
    pub trailing_failures: i64,
    /// `Some(t)` = wall-clock time of the most recent spawn (taken
    /// from `autopilot_runs.updated_at` of the highest-numbered row,
    /// which the wrap-up pipeline + this module both write through
    /// `db::set_autopilot_run_state`). `None` = no iterations yet
    /// (the first iteration is allowed to ignore the interval).
    pub last_spawn_at: Option<SystemTime>,
}

/// Outcome of [`evaluate_loop_continuation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopDecision {
    /// Spawn the next iteration; `iteration` is the 1-based iteration
    /// number the ledger row will carry (matches the poller's
    /// `MAX(loop_iteration) + 1` invariant).
    Spawn { iteration: i64 },
    /// Skip this pass; `reason` is the precise cause so the `tracing::debug!`
    /// in `poll_mesh_looping` can show it without ambiguity.
    Skip(LoopSkipReason),
}

/// Why a looping-mode pass skipped (no spawn this time). The variants
/// are mutually exclusive and ordered by the same priority
/// `evaluate_loop_continuation` evaluates them — the order is also
/// the "lowest-cost check first" heuristic, so the most common skip
/// (a previous iteration still running) wins without paying for the
/// SQL-derived fields' lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopSkipReason {
    /// `capacity <= 0` — app-wide pool budget exhausted (issue #1263
    /// made this its own variant; previously aliased under
    /// `IntervalNotElapsed`, which made pool-exhaustion look like a
    /// pacing issue in probe / debug logs).
    PoolBudgetExhausted,
    /// `loop_interval_seconds > 0` AND the gap since the last spawn is
    /// shorter than the configured pause.
    IntervalNotElapsed,
    /// `active_count > 0` — a previous loop iteration is still
    /// `implementing` or `finishing`.
    ActiveIterationInProgress,
    /// `iteration_count >= loop_max_iterations`.
    MaxIterationsReached,
    /// `trailing_failures >= loop_consecutive_failures` (the threshold
    /// is configured > 0 by the user).
    ConsecutiveFailureThresholdReached,
}

/// Convert the raw `autopilot_runs` rows into a [`LoopHistory`] snapshot.
/// Pure — `SystemTime` parsing is a no-fail best-effort: an
/// unparseable `updated_at` string degrades to `last_spawn_at = None`
/// (rather than failing the whole pass) so a future schema change in
/// the SQLite `datetime('now')` format can't take down the loop. The
/// pure function signature exposes `now` only via
/// `evaluate_loop_continuation`, which then makes the interval pacing
/// decision; `derive_loop_history` itself stays clock-free.
pub(crate) fn derive_loop_history(rows: &[db::LoopRunSnapshot]) -> LoopHistory {
    if rows.is_empty() {
        return LoopHistory::default();
    }
    // rows are already in ascending iteration order from
    // `db::list_loop_iterations`, so the highest iteration is the
    // last element. `last_spawn_at` reads from THAT row's `updated_at`
    // — the most recent state write (wrap-up completion, failed
    // terminal write, or initial INSERT).
    let iteration_count = rows.last().map(|(i, _, _)| *i).unwrap_or(0);
    let trailing_failures = rows
        .iter()
        .rev()
        .take_while(|(_, state, _)| matches!(state, AutopilotRunState::Failed))
        .count() as i64;
    let last_spawn_at = rows
        .last()
        .and_then(|(_, _, updated_at)| parse_sqlite_datetime(updated_at));
    LoopHistory {
        iteration_count,
        trailing_failures,
        last_spawn_at,
    }
}

/// Runtime status of a mesh's Looping Autopilot, surfaced to the Autopilot
/// Probe tab's status badge (ticket #994). Derived each fetch from the mesh's
/// `autopilot_enabled` flag + the loop-iteration ledger — there is no separate
/// runtime scheduler state to read, because the loop is DB-config-driven (see
/// the module docs). The pure [`derive_loop_status`] builds it so the mapping
/// is unit-testable without a DB or clock.
///
/// Generated to `src/types/generated/LoopStatus.ts`; the TS half maps it to
/// the tab's `LoopStatus` discriminated union (Active N / Idle / Stopped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "LoopStatus.ts")]
pub struct LoopStatusDto {
    /// The mesh's `autopilot_enabled` flag. `false` ⇒ the poller skips this
    /// mesh entirely (badge shows Stopped).
    pub enabled: bool,
    /// The 1-based iteration number of the currently-running loop node
    /// (`implementing`/`finishing`), or `None` when no iteration is live
    /// (badge shows Idle when `enabled`, Stopped when not).
    #[ts(as = "Option<i32>")]
    pub active_iteration: Option<i64>,
    /// The highest loop iteration number spawned so far (`0` when none) —
    /// informational count for the badge / tooltip.
    #[ts(as = "i32")]
    pub total_iterations: i64,
}

/// Pure derivation of [`LoopStatusDto`] from the enabled flag + the
/// loop-iteration ledger rows (ticket #994). No DB, no clock — mirrors the
/// [`derive_loop_history`] shape so it unit-tests deterministically. Both
/// derived fields are computed order-independently (via `max`, not by
/// assuming the caller's slice is sorted), so the pure function honours its
/// contract for any slice: `active_iteration` is the highest-numbered row
/// still in a non-terminal state (`Implementing`/`Finishing`) — a sequential
/// loop has at most one live iteration; `total_iterations` is the largest
/// iteration number seen (`0` when empty).
pub(crate) fn derive_loop_status(enabled: bool, rows: &[db::LoopRunSnapshot]) -> LoopStatusDto {
    let active_iteration = rows
        .iter()
        .filter(|(_, state, _)| {
            matches!(
                state,
                AutopilotRunState::Implementing | AutopilotRunState::Finishing
            )
        })
        .map(|(iteration, _, _)| *iteration)
        .max();
    let total_iterations = rows.iter().map(|(i, _, _)| *i).max().unwrap_or(0);
    LoopStatusDto {
        enabled,
        active_iteration,
        total_iterations,
    }
}

/// Best-effort parser for SQLite `datetime('now')` output
/// (`YYYY-MM-DD HH:MM:SS`). Returns `None` for unparseable input so
/// the poller can degrade gracefully instead of failing a pass.
/// Documented as best-effort precisely so a future schema change
/// here doesn't quietly take down the loop.
fn parse_sqlite_datetime(s: &str) -> Option<SystemTime> {
    // `YYYY-MM-DD HH:MM:SS` — split on the space so the two halves
    // parse independently and we don't depend on a specific
    // time-zone / fractional-second suffix.
    let mut parts = s.split(' ');
    let date = parts.next()?;
    let time = parts.next()?;
    let mut date_parts = date.split('-');
    let year: i32 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.parse().ok()?;
    let naive = chrono::NaiveDate::from_ymd_opt(year, month, day)?
        .and_hms_opt(hour, minute, second)?;
    let dt = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive, chrono::Utc);
    Some(SystemTime::from(dt))
}

/// Spawn a loop-iteration node (ticket #992). Derives the loop-mode plan
/// facts (no `source_issue`, `use_worktree_override = None` so the Mesh
/// policy applies, ledger tagged with `loop_iteration`) and delegates to
/// [`crate::autopilot::node_launch::launch_autopilot_node`].
///
/// `use_worktree_override = None` is the load-bearing line — see the
/// module docblock's "Looping Autopilot mode" section.
/// `create_with_source_pr_fork`'s `unwrap_or(mesh.use_worktree)` fall-through
/// means a mesh with `use_worktree = false` (game decompilation, etc.)
/// spawns on the root branch; a mesh with `use_worktree = true` still
/// cuts a worktree.
///
/// The prefill is `loop_initial_prompt`; the watch-and-submit helper is
/// reused (it already does the two-phase paste/Enter split per #874 —
/// never glue `\r` onto a bracketed paste). One spawned node per loop
/// iteration; the merged-PR sweep is irrelevant for loop iterations (a
/// loop-mode mesh's `autopilot_action_on_success` is the user's choice,
/// but the wrap-up flow itself is unchanged).
fn spawn_loop_node(
    app: &AppHandle,
    mesh: &Mesh,
    iteration: i64,
    initial_prompt: &str,
) -> Result<(), String> {
    let provider = mesh.autopilot_provider.clone().unwrap_or_else(|| {
        crate::preferences::resolve_default_provider(
            None,
            mesh.default_provider.clone(),
            crate::preferences::default_provider(),
        )
    });
    // Same best-effort branch read as the issue-driven path: drift on
    // the parent mesh must not block the spawn. A loop iteration runs
    // off the mesh's base ref regardless of worktree mode.
    let branch = crate::commands::git::get_default_branch_blocking(mesh.path.clone())
        .unwrap_or_else(|_| "main".to_string());
    let initial_name = format!("loop-iter-{}", iteration);

    // `watcher_issue_number = 0` is preserved for the toast payload
    // only (wayfinder #1027 — the literal `issue #0` cannot appear in
    // any user-authored loop prefill, so the watcher's marker is
    // derived from the prefill text inside `launch_autopilot_node`).
    crate::autopilot::node_launch::launch_autopilot_node(
        app,
        crate::autopilot::node_launch::AutopilotNodeLaunchPlan {
            mesh: mesh.clone(),
            provider,
            branch,
            initial_name,
            intent: crate::agent::spawn::SpawnIntent::Loop {
                initial_prompt: initial_prompt.to_string(),
            },
            run: crate::autopilot::node_launch::AutopilotRunIdentity::Loop {
                iteration,
            },
            worktree_policy:
                crate::autopilot::node_launch::AutopilotWorktreePolicy::RespectMesh,
            watcher_issue_number: 0,
        },
    )
}

/// Merged-PR auto-close sweep: for each completed run whose wrap-up PR is
/// now merged on GitHub, kill the (idle) agent process and archive the node.
/// Archiving — not deleting — keeps the ledger row, so the issue stays
/// deduped even if it somehow re-appears labelled; the worktree and branch
/// stay on disk and surface in the Archive tab like any closed node.
/// Per-candidate failures are logged and skipped: the next pass retries.
fn close_merged_nodes(
    app: &AppHandle,
    client: &GitHubClient,
    owner: &str,
    repo: &str,
    candidates: &[(i64, i64)],
) {
    for &(node_id, pr_number) in candidates {
        match client.pull_request_merged(owner, repo, pr_number) {
            Ok(true) => {
                crate::agent::process::PROCESS_REGISTRY.kill_session(node_id);
                if let Err(e) = db::archive_agent_node(node_id) {
                    tracing::warn!("autopilot: archive of node {} failed: {}", node_id, e);
                    continue;
                }
                // Terminal ledger state so the sweep never re-checks this PR.
                let _ = db::set_autopilot_run_state(
                    node_id,
                    db::AutopilotRunState::Merged,
                    None,
                );
                crate::autopilot::evaluator::unregister(node_id);
                let _ = app.emit(
                    "autopilot-node-closed",
                    AutopilotNodeClosedPayload {
                        node_id,
                        pr_number,
                    },
                );
                tracing::info!(
                    "autopilot: PR #{} merged — node {} archived (slot freed)",
                    pr_number,
                    node_id
                );
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(
                "autopilot: merged check for PR #{} (node {}) failed: {}",
                pr_number,
                node_id,
                e
            ),
        }
    }
}

/// Pure capacity combinator: how many nodes a mesh may actually spawn this
/// pass, given its own spare per-mesh capacity and the remaining app-wide
/// pool budget (Settings → autopilot pool size). `None` budget = no global
/// cap. A negative budget (the pool was shrunk below the current active
/// count) clamps to 0 — we stop spawning but never kill running nodes.
pub(crate) fn effective_capacity(mesh_capacity: i64, global_budget: Option<i64>) -> i64 {
    match global_budget {
        Some(budget) => mesh_capacity.min(budget.max(0)),
        None => mesh_capacity,
    }
}

/// Pure scheduler core: which of the currently-labelled `issues` to spawn,
/// given the issue numbers this mesh already has nodes for and the spare
/// concurrency `capacity`. Keeps GitHub's returned order (best-match first —
/// effectively newest-labelled first for a label query).
///
/// Production callers go through [`plan_spawns_with_blockers`] (which
/// layers the **Blocked by** filter in front of the dedup-and-take to
/// avoid the head-of-list blocked silent-stall — see its docstring). This
/// primitive stays around for the four unit tests below, which pin the
/// dedup-then-take contract in isolation from the Blocker logic.
#[allow(dead_code)]
pub(crate) fn plan_spawns<'a>(
    issues: &'a [Issue],
    known_issue_numbers: &[i64],
    capacity: usize,
) -> Vec<&'a Issue> {
    issues
        .iter()
        .filter(|i| !known_issue_numbers.contains(&i.number))
        .take(capacity)
        .collect()
}

/// Issue-driven planner with the dependency-chain filter layered in.
///
/// **Why this helper exists.** A dep-chain mesh (worked example: the
/// alondero/pixelgrab Tracer 14–15 series, `concurrency_limit = 1`, 14
/// open issues with `ready-for-agent`) needs the planner to *filter
/// blocked-by BEFORE the capacity take*. The previous order — call
/// `plan_spawns` (dedup, take capacity) and then filter — meant
/// `capacity = 1` selected only the head-of-list issue (newest-labelled
/// first per GitHub's default order). When the head was blocked the
/// filter rejected it, `planned` was empty, and the poller wasted the
/// pass even though an unblocked issue sat further down the list. Every
/// 2-minute pass repeated the same waste; nothing ever spawned.
///
/// The fix is to sieve blocked-by FIRST, then dedup, then take. The
/// unblocked tail can win under capacity=1 because the `take` now
/// operates on the unblocked subset. The dedup-vs-take ordering inside
/// `plan_spawns` (`filter(!known).take(capacity)`) is preserved as a
/// sub-step — the dedup tests on `plan_spawns` stay green.
///
/// **Side effect.** The "blocked by [...] — parked, retry next pass"
/// info log is emitted inline, gated by [`mark_blocked_logged`] so a
/// repeated (mesh_id, issue_number) pair is silent on subsequent passes.
/// The log emission is intrinsically tied to the filter rejection (it's
/// the diagnostic that names the blocker chain), so splitting it from
/// the filter would create a confusing two-call API. The helper is
/// `pub(crate)` and the only production caller is [`poll_mesh`].
pub(crate) fn plan_spawns_with_blockers<'a>(
    issues: &'a [Issue],
    known_issue_numbers: &[i64],
    open_issue_numbers: &HashSet<i64>,
    capacity: usize,
    mesh_id: i64,
    mesh_name: &str,
) -> Vec<&'a Issue> {
    issues
        .iter()
        .filter(|issue| {
            match unresolved_blockers(issue, open_issue_numbers, known_issue_numbers) {
                None => true,
                Some(unresolved) => {
                    if mark_blocked_logged(mesh_id, issue.number) {
                        tracing::info!(
                            "autopilot: issue #{} on mesh {} blocked by {:?} — parked, retry next pass",
                            issue.number,
                            mesh_name,
                            unresolved
                        );
                    }
                    false
                }
            }
        })
        .filter(|issue| !known_issue_numbers.contains(&issue.number))
        .take(capacity)
        .collect()
}

/// Pure planner step (map #976): if `issue` declares a `**Blocked by**`
/// reference that's still unresolved on this pass, returns the unresolved
/// blocker list. Returns `None` when there are no unresolved blockers —
/// covers all three "not blocked" cases (no `**Blocked by**` section, the
/// `None` short-circuit, and "every blocker is in neither set") because
/// `parse_blocked_by` collapses the first two and the filter step collapses
/// the third. The body is parsed exactly once per call.
///
/// In-flight blockers count (issue #976 decision 1): a blocker present in
/// `known_issue_numbers` (the autopilot-managed dedupe set) keeps the
/// dependent blocked even if the blocker is no longer labelled.
///
/// Fail-open (decision 2): a blocker absent from BOTH sets is treated as
/// resolved. Cross-repo, off-label, paginated to page 2+, or simply
/// deleted blockers fall through this way by design — better to spawn the
/// dependent than to starve it forever.
pub(crate) fn unresolved_blockers(
    issue: &Issue,
    open_issue_numbers: &HashSet<i64>,
    known_issue_numbers: &[i64],
) -> Option<Vec<i64>> {
    let blockers = parse_blocked_by(&issue.body);
    let unresolved: Vec<i64> = blockers
        .into_iter()
        .filter(|b| open_issue_numbers.contains(b) || known_issue_numbers.contains(b))
        .collect();
    if unresolved.is_empty() {
        None
    } else {
        Some(unresolved)
    }
}

/// Record that the blocked-by `info` log line was emitted for
/// `(mesh_id, issue_number)`. Returns `true` if this is the first time
/// the pair has been recorded (caller should emit the log), `false` on
/// subsequent passes (caller should stay silent). Pure novelty wrapper
/// over `LOGGED_BLOCKS` — the deduped state is the log emission, NOT
/// the spawn-skip. Mirrors the `HashSet::insert` novelty signal used
/// by `GATED_TRIGGERS`.
pub(crate) fn mark_blocked_logged(mesh_id: i64, issue_number: i64) -> bool {
    lock_planner_set(&LOGGED_BLOCKS).insert((mesh_id, issue_number))
}

/// Derive the issue-driven plan facts and delegate to
/// [`crate::autopilot::node_launch::launch_autopilot_node`]. The launch
/// module owns the ordered sequence (row create → lifecycle `Pending` →
/// ledger row → evaluator register → `node-created` emit → background
/// stage-2 → watcher arm), so a future Autopilot mode cannot bypass any
/// step through this seam (issue #1178).
///
/// Provider chain: the Autopilot Policy's own provider wins; otherwise the
/// normal default chain (mesh default -> app default -> claude).
fn spawn_autopilot_node(
    app: &AppHandle,
    mesh: &Mesh,
    owner: &str,
    repo: &str,
    issue: &Issue,
) -> Result<(), String> {
    // Issue #1180 — build the `SpawnIntent::Issue` once so the watcher's
    // prefill and the background launch's prefill both come from the
    // same `initial_prompt()` source. Three sites (desktop draft,
    // background launch, watcher) used to be able to silently drift if
    // anyone changed the wording in one but not the others; the launch
    // module derives the watcher's prefill from this same `intent`, so
    // we pass the intent (not a separately-formatted string).
    let intent = crate::agent::spawn::SpawnIntent::Issue(
        crate::agent::spawn::GitHubWorkContext {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number: issue.number,
            title: issue.title.clone(),
        },
    );
    let initial_name = crate::session_naming::issue_node_name(issue.number, &issue.title);
    let provider = mesh.autopilot_provider.clone().unwrap_or_else(|| {
        crate::preferences::resolve_default_provider(
            None,
            mesh.default_provider.clone(),
            crate::preferences::default_provider(),
        )
    });
    // PRD story 7: a drifted/unpushed mesh root must not block background
    // automation — `get_default_branch_blocking` only inspects refs, and the
    // spawn path's auto-sync is already best-effort (warns, never blocks).
    let branch = crate::commands::git::get_default_branch_blocking(mesh.path.clone())
        .unwrap_or_else(|_| "main".to_string());

    // Issue-driven: force `use_worktree = true` (PRD implementation
    // decision) — the wrap-up PR needs a real branch to push.
    crate::autopilot::node_launch::launch_autopilot_node(
        app,
        crate::autopilot::node_launch::AutopilotNodeLaunchPlan {
            mesh: mesh.clone(),
            provider,
            branch,
            initial_name,
            intent,
            run: crate::autopilot::node_launch::AutopilotRunIdentity::Issue {
                issue_number: issue.number,
            },
            worktree_policy:
                crate::autopilot::node_launch::AutopilotWorktreePolicy::ForceWorktreeBranch,
            // The literal that survives into the `autopilot-submitted`
            // toast payload — preserves the issue-driven wire shape.
            watcher_issue_number: issue.number,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Per-thread capture buffer for `tracing::info!` events emitted inside
    // `blocked_by_filter_emits_log_once_on_first_observation_silently_after`.
    // `thread_local!` (rather than a per-test `Arc<Mutex<Vec<u8>>>`) guarantees
    // events from other test threads can't bleed into this thread's buffer
    // under parallel `cargo test` — issue #1007. The buffer persists across
    // tests scheduled on the same OS thread, so the test drains it on entry.
    thread_local! {
        static INFO_BUFFER: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// `MakeWriter` that appends to the thread-local `INFO_BUFFER`. A unit
    /// struct because there's nothing to clone — the address lives in the
    /// `thread_local!`.
    struct ThreadLocalWriter;

    impl Write for ThreadLocalWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            INFO_BUFFER.with(|cell| cell.borrow_mut().extend_from_slice(buf));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLocalWriter {
        type Writer = ThreadLocalWriter;
        fn make_writer(&'a self) -> Self::Writer {
            ThreadLocalWriter
        }
    }

    /// Frozen wall-clock anchor for the loop-decision tests. `now`
    /// needs to be a fixed point so the duration comparisons stay
    /// deterministic; `SystemTime::UNIX_EPOCH` is fine because the
    /// decision core only cares about differences, not absolute
    /// values. Each test computes `now = last + Duration::from_secs(n)`
    /// relative to this anchor.
    const UNIX_EPOCH_PLACEHOLDER: SystemTime = SystemTime::UNIX_EPOCH;

    fn issue(number: i64) -> Issue {
        serde_json::from_str(&format!(
            r#"{{ "number": {}, "title": "task {}", "user": {{"login": "octocat"}} }}"#,
            number, number
        ))
        .expect("issue parses")
    }

    #[test]
    fn plan_spawns_respects_capacity() {
        let issues = vec![issue(1), issue(2), issue(3)];
        let planned = plan_spawns(&issues, &[], 2);
        assert_eq!(
            planned.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![1, 2],
            "only the first `capacity` issues are ingested"
        );
    }

    #[test]
    fn plan_spawns_skips_known_issues_before_taking_capacity() {
        // The dedupe filter must run BEFORE the capacity take: with capacity
        // 2 and issue 1 already known, issues 2 AND 3 spawn (not just 2).
        let issues = vec![issue(1), issue(2), issue(3)];
        let planned = plan_spawns(&issues, &[1], 2);
        assert_eq!(
            planned.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn plan_spawns_with_zero_capacity_spawns_nothing() {
        let issues = vec![issue(1)];
        assert!(plan_spawns(&issues, &[], 0).is_empty());
    }

    #[test]
    fn plan_spawns_ignores_issues_absent_from_the_current_set() {
        // Reconciliation contract (PRD story 8): the input IS the current
        // open+labelled set — an issue closed/untagged while offline is
        // simply not in it, so nothing plans it. An empty fetch plans nothing
        // even with capacity free.
        let planned = plan_spawns(&[], &[42], 5);
        assert!(planned.is_empty());
    }

    // -- plan_spawns_with_blockers: regression for the head-of-list
    //    silent-stall (worked example: alondero/pixelgrab dep chain). The
    //    helper composes `unresolved_blockers` (with the LOGGED_BLOCKS-gated
    //    info log) + the dedup-and-take, in that order. See the helper's
    //    docstring for the ordering contract.

    /// Mesh id reserved for the planner tests so `LOGGED_BLOCKS` (a global
    /// `Lazy<Mutex<HashSet>>`) never collides with another test's pair.
    /// Each test that drives the helper removes its own pairs at the end
    /// so the tests stay re-orderable.
    const PLANNER_TEST_MESH: i64 = i64::MAX - 211;

    fn reset_planner_logged_blocks(issue_numbers: &[i64]) {
        let mut guard = lock_planner_set(&LOGGED_BLOCKS);
        for n in issue_numbers {
            guard.remove(&(PLANNER_TEST_MESH, *n));
        }
    }

    /// The headline regression: capacity=1 with the head-of-list issue
    /// blocked by open dependencies and the tail issue clear. The
    /// previous shape (dedup-then-take + filter-after-take) wasted the
    /// pass; the new shape lets the tail issue win.
    #[test]
    fn plan_spawns_with_blockers_capacity_one_unblocked_tail_wins_over_blocked_head() {
        // Pixelgrab dep chain, distilled: #27 is the most-recent issue
        // (head-of-list per GitHub's newest-first default) and references
        // 9 open blockers; #14 references only #13, which is closed so
        // the unresolved set is empty under fail-open semantics.
        //
        // `open_numbers` must include the blockers of #27 — the labelled-open
        // page and the blockers-of-issue set are the same shape.
        let head_blocked = issue_with_blocked_by(27, &[
            "- #15", "- #18", "- #20", "- #21", "- #22", "- #23", "- #24", "- #25", "- #26",
        ]);
        let tail_unblocked = issue_with_blocked_by(14, &["- #13"]);
        let issues = vec![head_blocked, tail_unblocked];
        let open_numbers: HashSet<i64> = [
            27, 14, 15, 18, 20, 21, 22, 23, 24, 25, 26,
        ]
        .into_iter()
        .collect();
        let known: Vec<i64> = Vec::new();

        let planned = plan_spawns_with_blockers(
            &issues,
            &known,
            &open_numbers,
            1,
            PLANNER_TEST_MESH,
            "test_mesh",
        );

        assert_eq!(
            planned.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![14],
            "capacity=1 with head blocked must pick the unblocked tail (#14); \
             the previous shape yielded an empty plan and silently starved the mesh"
        );

        reset_planner_logged_blocks(&[27, 14, 15, 18, 20, 21, 22, 23, 24, 25, 26]);
    }

    /// The complement of the regression: when the head IS unblocked, the
    /// helper still picks it (the fix must not change the happy path).
    #[test]
    fn plan_spawns_with_blockers_capacity_one_picks_unblocked_head() {
        let head_unblocked = issue_with_blocked_by(27, &["- #13"]); // #13 closed
        let tail_unblocked = issue_with_blocked_by(14, &["- #13"]);
        let issues = vec![head_unblocked, tail_unblocked];
        let open_numbers: HashSet<i64> = [27, 14].into_iter().collect();
        let known: Vec<i64> = Vec::new();

        let planned = plan_spawns_with_blockers(
            &issues,
            &known,
            &open_numbers,
            1,
            PLANNER_TEST_MESH,
            "test_mesh",
        );

        assert_eq!(
            planned.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![27],
            "head-of-list unblocked wins when capacity=1 (existing happy-path contract)"
        );

        reset_planner_logged_blocks(&[27, 14]);
    }

    /// Dedup interacts with the new ordering: an already-spawned head
    /// (in `known`) must be skipped and the next unblocked issue picked,
    /// even when `capacity=1` would otherwise take the head.
    #[test]
    fn plan_spawns_with_blockers_skips_known_head_to_reach_unblocked_tail() {
        let head_unblocked = issue_with_blocked_by(27, &["- #13"]); // #13 closed
        let tail_unblocked = issue_with_blocked_by(14, &["- #13"]);
        let issues = vec![head_unblocked, tail_unblocked];
        let open_numbers: HashSet<i64> = [27, 14].into_iter().collect();
        let known: Vec<i64> = vec![27]; // #27 already spawned

        let planned = plan_spawns_with_blockers(
            &issues,
            &known,
            &open_numbers,
            1,
            PLANNER_TEST_MESH,
            "test_mesh",
        );

        assert_eq!(
            planned.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![14],
            "dedup must drop the head (in `known`) before the take so the unblocked tail \
             wins — mirrors `plan_spawns_skips_known_issues_before_taking_capacity` \
             for the new helper"
        );

        reset_planner_logged_blocks(&[27, 14]);
    }

    /// Capacity=N reconciles correctly: when capacity is large enough to
    /// enumerate the whole list, the new shape picks the unblocked
    /// subset in GitHub order. This is the case that worked correctly
    /// under the OLD shape (capacity=1 was the silent-stall trigger) and
    /// pins that the new shape preserves the broad-coverage behaviour.
    #[test]
    fn plan_spawns_with_blockers_capacity_large_takes_all_unblocked_in_order() {
        // #27 blocked by #15, #18 (both in open_numbers) → blocked.
        // #14 blocked by #13 (closed, not in open_numbers) → unblocked.
        // #15 has no blocker entry → unblocked.
        let blocked = issue_with_blocked_by(27, &["- #15", "- #18"]);
        let unblocked_a = issue_with_blocked_by(14, &["- #13"]);
        let unblocked_b = issue_with_blocked_by(15, &[]);
        let issues = vec![blocked, unblocked_a, unblocked_b];
        let open_numbers: HashSet<i64> = [27, 14, 15, 18].into_iter().collect();
        let known: Vec<i64> = Vec::new();

        let planned = plan_spawns_with_blockers(
            &issues,
            &known,
            &open_numbers,
            10,
            PLANNER_TEST_MESH,
            "test_mesh",
        );

        assert_eq!(
            planned.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![14, 15],
            "with capacity large enough, the blocked issue is dropped and the \
             unblocked ones taken in GitHub order"
        );

        reset_planner_logged_blocks(&[27, 14, 15, 18]);
    }

    /// All-blocked pass returns an empty plan (the poller's early-return
    /// path) and the helper must NOT panic. Pin the silent-stall symmetry.
    #[test]
    fn plan_spawns_with_blockers_all_blocked_returns_empty_plan() {
        let a = issue_with_blocked_by(7, &["- #8"]);
        let b = issue_with_blocked_by(8, &["- #7"]);
        let issues = vec![a, b];
        let open_numbers: HashSet<i64> = [7, 8].into_iter().collect();
        let known: Vec<i64> = Vec::new();

        let planned = plan_spawns_with_blockers(
            &issues,
            &known,
            &open_numbers,
            1,
            PLANNER_TEST_MESH,
            "test_mesh",
        );

        assert!(
            planned.is_empty(),
            "all-blocked set yields an empty plan and the poller ends the pass"
        );

        reset_planner_logged_blocks(&[7, 8]);
    }

    #[test]
    fn effective_capacity_without_global_cap_is_mesh_capacity() {
        // No pool size set → per-mesh limits are the only gate (the
        // pre-setting behaviour, so upgrades change nothing).
        assert_eq!(effective_capacity(3, None), 3);
    }

    #[test]
    fn effective_capacity_caps_at_remaining_global_budget() {
        assert_eq!(effective_capacity(3, Some(1)), 1);
        assert_eq!(effective_capacity(1, Some(3)), 1, "per-mesh limit still binds");
    }

    #[test]
    fn effective_capacity_clamps_negative_budget_to_zero() {
        // Pool shrunk below the current active count: stop spawning, but a
        // negative capacity must not flow onward (it would corrupt the
        // `capacity as usize` take in plan_spawns).
        assert_eq!(effective_capacity(2, Some(-3)), 0);
        assert_eq!(effective_capacity(2, Some(0)), 0, "pool size 0 pauses spawns");
    }

    // -- map #976: `**Blocked by**` planner filter -----------------------------

    fn body_with_blocked_by(listing: &str) -> Issue {
        // Setext-style heading — the format `parse_blocked_by` actually
        // recognises. `**Blocked by**` alone (without the `----------`
        // underline) is just inline-bold text and matches NEITHER of the
        // section-regex alternatives. Mirrors the real shape used by
        // alondero/buildmesh issues (see parse_blocked_by_setext_underline_*).
        serde_json::from_str(&format!(
            r#"{{ "number": 2, "title": "B", "user": {{"login": "octocat"}},
                  "body": "**Blocked by**\n----------\n\n{}\n" }}"#,
            listing
        ))
        .expect("issue parses")
    }

    /// Like `body_with_blocked_by` but with a caller-supplied issue
    /// number and a list of bullet lines (one per blocker). Each entry in
    /// `listing` MUST be a bullet-form line (e.g. `"- #15"`) — see
    /// `BLOCKED_BY_BARE_RE` for why bare `#NNN` references need a leading
    /// bullet marker.
    fn issue_with_blocked_by(number: i64, listing: &[&str]) -> Issue {
        let body = format!("**Blocked by**\n----------\n\n{}\n", listing.join("\n"));
        // `serde_json::json!` escapes the real Rust newlines from
        // `listing.join("\n")` at the JSON boundary.
        serde_json::from_value(serde_json::json!({
            "number": number,
            "title": format!("task {}", number),
            "user": {"login": "octocat"},
            "body": body,
        }))
        .expect("issue parses")
    }

    #[test]
    fn unresolved_blockers_returns_some_when_blocker_in_labelled_open_set() {
        // Issue B lists #1 as a blocker; #1 is in the labelled-open set
        // GitHub returned this pass. Spawning B now would race the spawn
        // of #1 in the same iteration.
        let issue = body_with_blocked_by("- #1");
        let open: HashSet<i64> = [1].into_iter().collect();
        let known: Vec<i64> = Vec::new();
        let unresolved = unresolved_blockers(&issue, &open, &known);
        assert_eq!(
            unresolved,
            Some(vec![1]),
            "the blocker in the labelled-open set is the unresolved chain"
        );
    }

    #[test]
    fn unresolved_blockers_returns_some_when_blocker_is_in_flight_known_set() {
        // Decision 1: an in-flight blocker counts. Issue A's spawn is
        // already recorded in `autopilot_runs` (so it lives in `known`);
        // B references A. A may no longer carry the trigger label, but
        // A's node is still running — B must wait.
        let issue = body_with_blocked_by("- #1");
        let open: HashSet<i64> = HashSet::new();
        let known: Vec<i64> = vec![1];
        let unresolved = unresolved_blockers(&issue, &open, &known);
        assert_eq!(
            unresolved,
            Some(vec![1]),
            "in-flight blocker (in known) must gate B"
        );
    }

    #[test]
    fn unresolved_blockers_returns_none_when_blocker_unknown_fail_open() {
        // Decision 2: a blocker absent from BOTH sets — cross-repo,
        // off-label, paginated to page 2+, or deleted — is treated as
        // resolved. Failing open is the safe default for the autonomous
        // loop: better to spawn the dependent than to starve it forever.
        let issue = body_with_blocked_by("- #99");
        let open: HashSet<i64> = [1].into_iter().collect();
        let known: Vec<i64> = Vec::new();
        assert!(
            unresolved_blockers(&issue, &open, &known).is_none(),
            "unknown blocker must fail open"
        );
    }

    #[test]
    fn unresolved_blockers_returns_none_when_body_has_no_blocked_by_section() {
        // Plain description, no `**Blocked by**` heading at all.
        let issue: Issue = serde_json::from_str(
            r#"{ "number": 1, "title": "task", "user": {"login": "octocat"},
                "body": "Just a description, no Blocked by.\n" }"#,
        )
        .unwrap();
        let open: HashSet<i64> = [1, 2, 3].into_iter().collect();
        let known: Vec<i64> = Vec::new();
        assert!(
            unresolved_blockers(&issue, &open, &known).is_none(),
            "body without a Blocked by section must not block"
        );
    }

    #[test]
    fn unresolved_blockers_returns_none_when_blocked_by_section_is_none() {
        // The `None` short-circuit in `parse_blocked_by` must propagate.
        // Issue may use `**Blocked by** None` to declare "no blockers" —
        // that's a positive signal to proceed, not a blocked state.
        let issue: Issue = serde_json::from_str(
            r#"{ "number": 1, "title": "task", "user": {"login": "octocat"},
                "body": "**Blocked by**\n----------\n\nNone\n" }"#,
        )
        .unwrap();
        let open: HashSet<i64> = [1, 2].into_iter().collect();
        let known: Vec<i64> = Vec::new();
        assert!(
            unresolved_blockers(&issue, &open, &known).is_none(),
            "the `None` short-circuit in parse_blocked_by must propagate"
        );
    }

    #[test]
    fn mark_blocked_logged_first_insert_is_novel_subsequent_is_duplicate() {
        // Pure novelty wrapper over `LOGGED_BLOCKS`. `HashSet::insert`
        // returns `true` iff the value was newly added. First call:
        // log-worthy. Second call: silent.
        //
        // Use a unique pair to avoid colliding with other tests'
        // state in the global set — `cargo test` runs in parallel and
        // the set is app-lifetime state.
        let pair = (i64::MAX - 7, i64::MAX - 13);
        lock_planner_set(&LOGGED_BLOCKS).remove(&pair);
        assert!(
            mark_blocked_logged(pair.0, pair.1),
            "first insert is novel — caller logs once"
        );
        assert!(
            !mark_blocked_logged(pair.0, pair.1),
            "second insert is a duplicate — caller stays silent"
        );
        // Tidy: leave the set as we found it.
        lock_planner_set(&LOGGED_BLOCKS).remove(&pair);
    }

    #[test]
    fn blocked_by_filter_emits_log_once_on_first_observation_silently_after() {
        // Acceptance criterion #6: first observation of a blocked
        // (mesh_id, issue_number) pair emits ONE `info` log line; any
        // subsequent observation of the same pair is silent. Captures
        // `tracing::info!` events via a custom `MakeWriter` and mirrors
        // the closure body in `poll_mesh` so the assertion reflects the
        // exact behaviour the planner will exhibit at runtime — a
        // regression in the `if mark_blocked_logged(...)` wiring would
        // fail this test.
        //
        // `ThreadLocalWriter` writes to a `thread_local!` buffer — issue
        // #1007 closed a parallel-test flake where the previous
        // `VecWriter(Arc<Mutex<Vec<u8>>>)` allowed events from other test
        // threads to bleed into this test's capture buffer.
        INFO_BUFFER.with(|cell| cell.borrow_mut().clear());

        let subscriber = tracing_subscriber::fmt()
            .with_writer(ThreadLocalWriter)
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_max_level(tracing::Level::INFO)
            .finish();

        let pair = (i64::MAX - 91, i64::MAX - 13);
        let issue = body_with_blocked_by("- #1");
        let open: HashSet<i64> = [1].into_iter().collect();
        let known: Vec<i64> = Vec::new();

        tracing::subscriber::with_default(subscriber, || {
            // First observation: should emit the log line.
            let unresolved = unresolved_blockers(&issue, &open, &known)
                .expect("issue must be blocked so the log path runs");
            if mark_blocked_logged(pair.0, pair.1) {
                tracing::info!(
                    "autopilot: issue #{} on mesh {} blocked by {:?} — parked, retry next pass",
                    pair.1,
                    "test_mesh",
                    unresolved
                );
            }

            // Second observation: same pair still blocked, but the log
            // must NOT fire.
            assert!(
                unresolved_blockers(&issue, &open, &known).is_some(),
                "issue must still be blocked"
            );
            // (assertion message carries the diagnostic — a regression
            // that flipped Some → None now halts here with a clear msg
            // instead of panicking deep in the poll loop.)
            if mark_blocked_logged(pair.0, pair.1) {
                tracing::info!("SHOULD NOT APPEAR");
            }

            // Third observation: explicit guard against log spam on
            // further passes.
            let _ = unresolved_blockers(&issue, &open, &known);
            if mark_blocked_logged(pair.0, pair.1) {
                tracing::info!("ALSO SHOULD NOT APPEAR");
            }
        });

        // Tidy: leave LOGGED_BLOCKS as we found it.
        lock_planner_set(&LOGGED_BLOCKS).remove(&pair);

        let logs = String::from_utf8(INFO_BUFFER.with(|cell| cell.borrow().clone()))
            .expect("captured log buffer is utf-8");
        // Tidy: don't leave our bytes around for the next test on this OS thread.
        INFO_BUFFER.with(|cell| cell.borrow_mut().clear());
        let autopilot_lines: Vec<&str> = logs
            .lines()
            .filter(|l| l.contains("autopilot:"))
            .collect();
        assert_eq!(
            autopilot_lines.len(),
            1,
            "exactly one `autopilot:` log line on first observation. Captured:\n{}",
            logs
        );
        assert!(
            autopilot_lines[0].contains("blocked by"),
            "first log line includes the blocker reason; got: {:?}",
            autopilot_lines[0]
        );
        assert!(
            !logs.contains("SHOULD NOT APPEAR"),
            "second observation must be silent"
        );
        assert!(
            !logs.contains("ALSO SHOULD NOT APPEAR"),
            "third observation must be silent"
        );
    }

    // -- Wayfinder #990 / ticket #992: Looping-mode decision core ----------
    //
    // These tests pin the pure decision function
    // (`evaluate_loop_continuation`) and its helpers. Each branch is
    // covered by an isolated, frozen-clock fixture so a future refactor
    // of the loop pacing logic can't quietly regress any of the four
    // skip reasons or the Spawn path.

    /// Test fixture: a Looping-mode mesh with sensible defaults so each
    /// test only has to override the field under test. `use_worktree`
    /// is `true` (default) — the `spawn_loop_node` worktree test
    /// checks the non-default path explicitly.
    fn looping_mesh() -> Mesh {
        Mesh {
            autopilot_mode: AutopilotMode::Looping,
            loop_initial_prompt: Some("iterate".to_string()),
            loop_suffix_prompt: None,
            loop_max_iterations: None,
            loop_interval_seconds: 0,
            loop_consecutive_failures: 0,
            use_worktree: true,
            ..Default::default()
        }
    }

    /// Empty history + `loop_interval_seconds = 0` + `loop_consecutive_failures = 0`
    /// + no max cap + `active_count = 0` + `capacity > 0` → Spawn iteration 1.
    ///
    /// The "first iteration is always allowed" baseline.
    #[test]
    fn evaluate_loop_first_iteration_spawns() {
        let mesh = looping_mesh();
        let history = LoopHistory::default();
        let now = UNIX_EPOCH_PLACEHOLDER;
        match evaluate_loop_continuation(&mesh, &history, 0, 1, now) {
            LoopDecision::Spawn { iteration } => assert_eq!(iteration, 1),
            other => panic!("first iteration must spawn, got {:?}", other),
        }
    }

    /// `iteration_count == max` → Skip (MaxIterationsReached). Uses a
    /// `Some(3)` cap and a history of 3 iterations; the FIRST boundary
    /// where the cap kicks in.
    #[test]
    fn evaluate_loop_max_iterations_reached() {
        let mut mesh = looping_mesh();
        mesh.loop_max_iterations = Some(3);
        let history = LoopHistory {
            iteration_count: 3,
            ..Default::default()
        };
        let now = UNIX_EPOCH_PLACEHOLDER;
        assert_eq!(
            evaluate_loop_continuation(&mesh, &history, 0, 1, now),
            LoopDecision::Skip(LoopSkipReason::MaxIterationsReached),
            "iteration_count == max must skip"
        );
    }

    /// `loop_max_iterations == None` is "continuous" — the cap check
    /// must never fire regardless of iteration_count. Regression pin
    /// against a future "treat None as 0" drift.
    #[test]
    fn evaluate_loop_no_max_iterations_means_continuous() {
        let mesh = looping_mesh(); // loop_max_iterations = None
        let history = LoopHistory {
            iteration_count: 10_000,
            ..Default::default()
        };
        let now = UNIX_EPOCH_PLACEHOLDER;
        assert!(
            matches!(
                evaluate_loop_continuation(&mesh, &history, 0, 1, now),
                LoopDecision::Spawn { .. }
            ),
            "no cap means no skip on iteration count"
        );
    }

    /// `trailing_failures >= loop_consecutive_failures` (threshold > 0)
    /// → Skip (ConsecutiveFailureThresholdReached). Uses threshold=2 and
    /// exactly 2 trailing failures so a `>` drift fails this test.
    #[test]
    fn evaluate_loop_consecutive_failure_threshold_reached() {
        let mut mesh = looping_mesh();
        mesh.loop_consecutive_failures = 2;
        let history = LoopHistory {
            iteration_count: 5,
            trailing_failures: 2,
            last_spawn_at: None,
        };
        let now = UNIX_EPOCH_PLACEHOLDER;
        assert_eq!(
            evaluate_loop_continuation(&mesh, &history, 0, 1, now),
            LoopDecision::Skip(LoopSkipReason::ConsecutiveFailureThresholdReached),
            "trailing_failures >= threshold must skip"
        );
    }

    /// `loop_consecutive_failures == 0` is the documented "feature off"
    /// state — even if every iteration failed, the threshold check
    /// must NOT fire. The docstring on
    /// `models::Mesh::loop_consecutive_failures` pins this.
    #[test]
    fn evaluate_loop_consecutive_failure_threshold_zero_disables_check() {
        let mut mesh = looping_mesh();
        mesh.loop_consecutive_failures = 0;
        let history = LoopHistory {
            iteration_count: 7,
            trailing_failures: 7, // every iteration failed
            last_spawn_at: None,
        };
        let now = UNIX_EPOCH_PLACEHOLDER;
        assert!(
            matches!(
                evaluate_loop_continuation(&mesh, &history, 0, 1, now),
                LoopDecision::Spawn { .. }
            ),
            "threshold == 0 must disable the check even with all failures"
        );
    }

    /// `active_count > 0` → Skip (ActiveIterationInProgress). A previous
    /// iteration still `implementing` or `finishing` must block the
    /// next one (looping is strictly sequential).
    #[test]
    fn evaluate_loop_active_iteration_in_progress() {
        let mesh = looping_mesh();
        let history = LoopHistory::default();
        let now = UNIX_EPOCH_PLACEHOLDER;
        assert_eq!(
            evaluate_loop_continuation(&mesh, &history, 1, 1, now),
            LoopDecision::Skip(LoopSkipReason::ActiveIterationInProgress),
        );
    }

    /// Interval pacing — `loop_interval_seconds > 0` and the gap since
    /// the last spawn is shorter than the configured pause → Skip.
    #[test]
    fn evaluate_loop_interval_not_elapsed() {
        let mut mesh = looping_mesh();
        mesh.loop_interval_seconds = 60;
        let last_spawn = UNIX_EPOCH_PLACEHOLDER;
        let now = last_spawn + Duration::from_secs(30);
        let history = LoopHistory {
            iteration_count: 1,
            trailing_failures: 0,
            last_spawn_at: Some(last_spawn),
        };
        assert_eq!(
            evaluate_loop_continuation(&mesh, &history, 0, 1, now),
            LoopDecision::Skip(LoopSkipReason::IntervalNotElapsed),
            "30s gap < 60s interval must skip"
        );
    }

    /// Interval pacing — gap >= interval passes the gate. Regression
    /// pin for an off-by-one in the duration comparison.
    #[test]
    fn evaluate_loop_interval_elapsed_allows_spawn() {
        let mut mesh = looping_mesh();
        mesh.loop_interval_seconds = 60;
        let last_spawn = UNIX_EPOCH_PLACEHOLDER;
        let now = last_spawn + Duration::from_secs(60);
        let history = LoopHistory {
            iteration_count: 1,
            trailing_failures: 0,
            last_spawn_at: Some(last_spawn),
        };
        assert!(
            matches!(
                evaluate_loop_continuation(&mesh, &history, 0, 1, now),
                LoopDecision::Spawn { iteration: 2 }
            ),
            "60s gap == 60s interval must spawn"
        );
    }

    /// Interval pacing — `loop_interval_seconds == 0` disables the
    /// check (the docstring on `loop_interval_seconds` calls out "no
    /// pause; spawn as soon as the previous iteration finished").
    #[test]
    fn evaluate_loop_interval_zero_disables_check() {
        let mut mesh = looping_mesh();
        mesh.loop_interval_seconds = 0;
        let history = LoopHistory {
            iteration_count: 1,
            trailing_failures: 0,
            last_spawn_at: Some(UNIX_EPOCH_PLACEHOLDER),
        };
        let now = UNIX_EPOCH_PLACEHOLDER + Duration::from_millis(1);
        assert!(
            matches!(
                evaluate_loop_continuation(&mesh, &history, 0, 1, now),
                LoopDecision::Spawn { .. }
            ),
            "interval == 0 must disable the check"
        );
    }

    /// `capacity <= 0` → Skip `PoolBudgetExhausted`. The pool-budget variant
    /// is its own reason (issue #1263) — a probe reading "interval not
    /// elapsed" for a zero-capacity mesh would point the operator at
    /// pacing config when the real knob is `autopilot_pool_size`.
    /// Capacity is the FIRST gate checked, so a negative pool budget wins
    /// over every other skip reason.
    #[test]
    fn evaluate_loop_capacity_zero_skips() {
        let mesh = looping_mesh();
        let history = LoopHistory::default();
        let now = UNIX_EPOCH_PLACEHOLDER;
        assert_eq!(
            evaluate_loop_continuation(&mesh, &history, 0, 0, now),
            LoopDecision::Skip(LoopSkipReason::PoolBudgetExhausted),
            "capacity == 0 must skip with the budget reason"
        );
        assert_eq!(
            evaluate_loop_continuation(&mesh, &history, 0, -3, now),
            LoopDecision::Skip(LoopSkipReason::PoolBudgetExhausted),
            "negative capacity must skip with the budget reason (not crash on capacity <= 0)"
        );
    }

    /// Regression pin for the mislabel that issue #1263 fixed: when
    /// `capacity == 0` and ALL OTHER FIELDS would otherwise allow a
    /// spawn (no active iteration, no interval gating, no max-iter cap,
    /// no failure threshold), the skip reason MUST be the budget reason —
    /// not `IntervalNotElapsed`. This pins the priority ordering too:
    /// capacity-first wins over interval pacing.
    #[test]
    fn evaluate_loop_capacity_zero_returns_budget_reason_not_interval() {
        // Mesh with a 60s interval so a naive reading "no spawn happened"
        // might guess the interval gate fired.
        let mut mesh = looping_mesh();
        mesh.loop_interval_seconds = 60;
        let history = LoopHistory {
            iteration_count: 0,
            trailing_failures: 0,
            last_spawn_at: Some(UNIX_EPOCH_PLACEHOLDER),
        };
        let now = UNIX_EPOCH_PLACEHOLDER + Duration::from_secs(1); // well under interval
        assert_eq!(
            evaluate_loop_continuation(&mesh, &history, 0, 0, now),
            LoopDecision::Skip(LoopSkipReason::PoolBudgetExhausted),
            "capacity=0 must report PoolBudgetExhausted, never IntervalNotElapsed, \
             even when the interval gate would also fire"
        );
    }

    /// Iteration counter increment — Spawn always returns
    /// `iteration_count + 1`. Regression pin for a future
    /// off-by-one in the spawn assignment.
    #[test]
    fn evaluate_loop_spawn_iteration_is_count_plus_one() {
        let mesh = looping_mesh();
        for count in [1, 2, 5, 99] {
            let history = LoopHistory {
                iteration_count: count,
                ..Default::default()
            };
            match evaluate_loop_continuation(&mesh, &history, 0, 1, UNIX_EPOCH_PLACEHOLDER) {
                LoopDecision::Spawn { iteration } => {
                    assert_eq!(iteration, count + 1, "iteration should be count+1")
                }
                other => panic!("expected Spawn at count={}, got {:?}", count, other),
            }
        }
    }

    // -- derive_loop_history ---------------------------------------------------

    /// Empty rows → all fields at their defaults. This is the "first
    /// pass on a freshly-configured Looping mesh" baseline.
    #[test]
    fn derive_loop_history_empty_rows() {
        let h = derive_loop_history(&[]);
        assert_eq!(h, LoopHistory::default());
    }

    /// Single completed iteration → `iteration_count = 1`,
    /// `trailing_failures = 0`, `last_spawn_at` parses from the
    /// `updated_at` column.
    #[test]
    fn derive_loop_history_single_completed() {
        let rows: Vec<db::LoopRunSnapshot> = vec![(
            1,
            AutopilotRunState::Completed,
            "2026-07-22 10:00:00".to_string(),
        )];
        let h = derive_loop_history(&rows);
        assert_eq!(h.iteration_count, 1);
        assert_eq!(h.trailing_failures, 0, "completed is not a failure");
        assert!(h.last_spawn_at.is_some(), "last_spawn_at parses the column");
    }

    /// Two completed then one failed → `trailing_failures = 1`
    /// (only the last iteration counts as "trailing"). Two completed
    /// earlier resets the trailing counter even though those rows are
    /// terminal failures-of-nothing.
    #[test]
    fn derive_loop_history_trailing_failures_stops_at_completed() {
        let rows: Vec<db::LoopRunSnapshot> = vec![
            (1, AutopilotRunState::Completed, "2026-07-22 10:00:00".to_string()),
            (2, AutopilotRunState::Completed, "2026-07-22 10:05:00".to_string()),
            (3, AutopilotRunState::Failed, "2026-07-22 10:10:00".to_string()),
        ];
        let h = derive_loop_history(&rows);
        assert_eq!(h.iteration_count, 3);
        assert_eq!(
            h.trailing_failures, 1,
            "only the most-recent failed iteration counts as trailing"
        );
    }

    /// Three consecutive failures → `trailing_failures = 3`. Pins the
    /// inclusive walk (the failed rows, not just the last one).
    #[test]
    fn derive_loop_history_three_consecutive_failures() {
        let rows: Vec<db::LoopRunSnapshot> = vec![
            (1, AutopilotRunState::Failed, "2026-07-22 10:00:00".to_string()),
            (2, AutopilotRunState::Failed, "2026-07-22 10:05:00".to_string()),
            (3, AutopilotRunState::Failed, "2026-07-22 10:10:00".to_string()),
        ];
        let h = derive_loop_history(&rows);
        assert_eq!(h.trailing_failures, 3);
    }

    /// `Merged` is a terminal success state (the merged-PR sweep sets
    /// it). It must reset the trailing counter, just like `Completed`.
    #[test]
    fn derive_loop_history_merged_resets_trailing_failures() {
        let rows: Vec<db::LoopRunSnapshot> = vec![
            (1, AutopilotRunState::Failed, "2026-07-22 10:00:00".to_string()),
            (2, AutopilotRunState::Failed, "2026-07-22 10:05:00".to_string()),
            (
                3,
                AutopilotRunState::Merged,
                "2026-07-22 10:10:00".to_string(),
            ),
        ];
        let h = derive_loop_history(&rows);
        assert_eq!(
            h.trailing_failures, 0,
            "Merged is a terminal success — resets the trailing-failure count"
        );
    }

    // -- derive_loop_status (ticket #994) ------------------------------------

    /// Disabled mesh, no iterations → Stopped: `enabled = false`, no active
    /// iteration, zero total. The tab maps this to the `Stopped` badge.
    #[test]
    fn derive_loop_status_disabled_no_rows() {
        let s = derive_loop_status(false, &[]);
        assert!(!s.enabled);
        assert_eq!(s.active_iteration, None);
        assert_eq!(s.total_iterations, 0);
    }

    /// Enabled with only terminal rows → Idle: no live iteration, but the
    /// total reflects the ledger. The tab maps `enabled && active == None`
    /// to the `Idle` badge (loop on, between iterations).
    #[test]
    fn derive_loop_status_enabled_all_terminal_is_idle() {
        let rows: Vec<db::LoopRunSnapshot> = vec![
            (1, AutopilotRunState::Completed, "2026-07-22 10:00:00".to_string()),
            (2, AutopilotRunState::Merged, "2026-07-22 10:05:00".to_string()),
        ];
        let s = derive_loop_status(true, &rows);
        assert!(s.enabled);
        assert_eq!(
            s.active_iteration, None,
            "completed/merged are terminal — no iteration is live"
        );
        assert_eq!(s.total_iterations, 2);
    }

    /// A live `Implementing` row → Active N: the newest non-terminal
    /// iteration is surfaced as `active_iteration`. `Finishing` counts too.
    #[test]
    fn derive_loop_status_active_iteration_from_live_row() {
        let rows: Vec<db::LoopRunSnapshot> = vec![
            (1, AutopilotRunState::Completed, "2026-07-22 10:00:00".to_string()),
            (2, AutopilotRunState::Implementing, "2026-07-22 10:05:00".to_string()),
        ];
        let s = derive_loop_status(true, &rows);
        assert_eq!(s.active_iteration, Some(2), "the implementing row is live");
        assert_eq!(s.total_iterations, 2);

        // `Finishing` is also a live (non-terminal) state.
        let finishing: Vec<db::LoopRunSnapshot> = vec![(
            3,
            AutopilotRunState::Finishing,
            "2026-07-22 10:10:00".to_string(),
        )];
        assert_eq!(derive_loop_status(true, &finishing).active_iteration, Some(3));
    }

    /// `enabled` is reported verbatim even while an iteration is live — the
    /// flag and the run state are independent projections (a Stop during a
    /// running iteration reads `enabled = false` with an active iteration
    /// that finishes on its own).
    #[test]
    fn derive_loop_status_reports_enabled_flag_verbatim() {
        let rows: Vec<db::LoopRunSnapshot> = vec![(
            1,
            AutopilotRunState::Implementing,
            "2026-07-22 10:00:00".to_string(),
        )];
        assert!(!derive_loop_status(false, &rows).enabled);
        assert_eq!(
            derive_loop_status(false, &rows).active_iteration,
            Some(1),
            "a running iteration is still live even after the mesh is disabled"
        );
    }

    /// The pure function must not assume the caller sorted the slice: both
    /// derived fields use `max`, not the last element. An out-of-order slice
    /// still yields the highest iteration number (`total`) and the
    /// highest-numbered live row (`active`).
    #[test]
    fn derive_loop_status_is_order_independent() {
        let rows: Vec<db::LoopRunSnapshot> = vec![
            (3, AutopilotRunState::Completed, "2026-07-22 10:10:00".to_string()),
            (1, AutopilotRunState::Completed, "2026-07-22 10:00:00".to_string()),
            (2, AutopilotRunState::Implementing, "2026-07-22 10:05:00".to_string()),
        ];
        let s = derive_loop_status(true, &rows);
        assert_eq!(s.total_iterations, 3, "max iteration, not the last slice element");
        assert_eq!(
            s.active_iteration,
            Some(2),
            "the live row's iteration, regardless of slice position"
        );
    }

    // -- parse_sqlite_datetime ------------------------------------------------

    #[test]
    fn parse_sqlite_datetime_well_formed_string() {
        let t = parse_sqlite_datetime("2026-07-22 10:30:45")
            .expect("valid datetime string parses");
        // Pin via the `chrono::DateTime` round-trip rather than a hard-coded
        // unix timestamp so a chrono version bump doesn't break the test.
        let expected: SystemTime = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
            chrono::NaiveDate::from_ymd_opt(2026, 7, 22)
                .unwrap()
                .and_hms_opt(10, 30, 45)
                .unwrap(),
            chrono::Utc,
        )
        .into();
        assert_eq!(t, expected);
    }

    #[test]
    fn parse_sqlite_datetime_unparseable_returns_none() {
        assert!(parse_sqlite_datetime("not a datetime").is_none());
        assert!(parse_sqlite_datetime("2026/07/22 10:30:45").is_none()); // wrong date sep
        assert!(parse_sqlite_datetime("").is_none());
    }

    // -- spawn_loop_node: marker signature pin (wayfinder #1027) --------------
    //
    // Behaviour is pinned in `autopilot::launch::tests`; here we just
    // confirm the cross-module helper still exists with the expected
    // path/signature, so a future rename drops this test out of the
    // build before any call site silently references a non-existent
    // function.
    #[test]
    fn spawn_loop_node_signature_uses_prefill_via_marker_hint() {
        let _marker = crate::autopilot::launch::marker_hint_for_prefill(
            "Iterate on the failing test cases",
        );
    }

    // ---- Poison-recovery regression (issue #1224) ----
    //
    // Each planner static used to be locked with `.unwrap()` — a single
    // panic while holding the guard permanently poisoned the mutex and
    // every subsequent call re-panicked with "poisoned lock". The
    // recovery helper `lock_planner_set` calls `into_inner()` instead
    // so the polling thread keeps scheduling work after any one-off
    // failure. These tests are the regression pin: poison each static
    // inside `catch_unwind`, then re-lock and prove normal insert/contains
    // still works.
    fn poison_planner_static<T>(mutex: &'static Mutex<T>) {
        let _guard = mutex.lock().expect("first lock must succeed (test setup)");
        // `catch_unwind` keeps the test binary alive; the panic payload
        // is the assertion that the inner code path did panic.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("intentional planner-static poison for issue #1224 regression test");
        }));
        assert!(result.is_err(), "test fixture must panic to poison the mutex");
    }

    #[test]
    fn gated_triggers_recovers_from_poison() {
        poison_planner_static(&GATED_TRIGGERS);
        // After the panic, the OLD `.lock().unwrap()` form would now
        // return `Err(Poisoned)`. The recovery helper must hand back
        // a usable guard, and the underlying HashSet must still accept
        // inserts.
        let pair = (i64::MAX - 101, i64::MAX - 103);
        {
            let mut guard = lock_planner_set(&GATED_TRIGGERS);
            assert!(
                guard.insert(pair),
                "post-poison insert must report novelty (issue #1224)"
            );
        }
        // Tidy so the pair does not pollute sibling tests.
        lock_planner_set(&GATED_TRIGGERS).remove(&pair);
    }

    #[test]
    fn logged_blocks_recovers_from_poison() {
        poison_planner_static(&LOGGED_BLOCKS);
        // The dedup wrapper `mark_blocked_logged` reads through the
        // helper — it is the path production calls take, so the
        // regression test goes through it instead of duplicating the
        // lock site.
        let pair = (i64::MAX - 151, i64::MAX - 157);
        assert!(
            mark_blocked_logged(pair.0, pair.1),
            "first insert after poison must succeed and report novelty"
        );
        assert!(
            !mark_blocked_logged(pair.0, pair.1),
            "second insert must report duplicate (set survived the panic)"
        );
        lock_planner_set(&LOGGED_BLOCKS).remove(&pair);
    }
}
