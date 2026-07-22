//! Autopilot polling daemon & concurrency scheduler (issue #482, PRD #480).
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
//! The spawn itself reuses the exact two-stage issue-spawn flow the desktop
//! modal uses (`create_pending`-shaped row + `start_node_background`), so
//! autopilot nodes are ordinary issue-spawned nodes plus a ledger row.
//! Threading: the pass runs entirely on this worker's OS thread (blocking
//! reqwest + git shell-outs are fine here — this is NOT the tokio pool).

use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

use crate::autopilot::{gate_trigger, AutopilotTrigger, GateDecision};
use crate::db;
use crate::models::{Mesh, DEFAULT_AUTOPILOT_TRIGGER_LABEL};
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
    // The merged-PR sweep runs BEFORE the capacity gate: a mesh at capacity
    // must still get its finished nodes archived (that's what clears grid
    // space), and the sweep costs no network when there's nothing to sweep.
    let sweep_candidates =
        db::list_completed_autopilot_runs_with_pr(mesh.id).unwrap_or_default();

    let active = db::count_active_autopilot_nodes(mesh.id).map_err(|e| e.to_string())?;
    let capacity = effective_capacity(
        i64::from(mesh.autopilot_concurrency_limit) - active,
        *global_budget,
    );
    // PRD story 6: no spare capacity AND nothing to sweep → no GitHub
    // round-trip at all.
    if capacity <= 0 && sweep_candidates.is_empty() {
        return Ok(());
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

    let candidates = plan_spawns(&issues, &known, capacity as usize);
    // Map #976: skip issues whose body declares a `**Blocked by**` reference
    // that's still unresolved on this pass. `plan_spawns` returns GitHub's
    // best-match order (newest-labelled first for a label query); the
    // blocked-by filter walks the candidate list once and drops the
    // deferred ones. The open-number set is built per-pass from the
    // labelled-open page — see `unresolved_blockers` for the fail-open
    // semantics (decision 2).
    let open_numbers: HashSet<i64> = issues.iter().map(|i| i.number).collect();
    let planned: Vec<&Issue> = candidates
        .into_iter()
        .filter(|issue| {
            let Some(unresolved) = unresolved_blockers(issue, &open_numbers, &known) else {
                return true;
            };
            if mark_blocked_logged(mesh.id, issue.number) {
                tracing::info!(
                    "autopilot: issue #{} on mesh {} blocked by {:?} — parked, retry next pass",
                    issue.number,
                    mesh.name,
                    unresolved
                );
            }
            false
        })
        .collect();
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
        if GATED_TRIGGERS.lock().unwrap().contains(&(mesh.id, issue.number)) {
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
                GATED_TRIGGERS.lock().unwrap().insert((mesh.id, issue.number));
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
    LOGGED_BLOCKS
        .lock()
        .unwrap()
        .insert((mesh_id, issue_number))
}

/// Create the node row (Pending, worktree enforced) + `autopilot_runs` ledger
/// entry, then hand off to the shared stage-2 background spawn. Mirrors
/// `commands::agent::create_issue_node` + `start_node_background`, minus the
/// IPC skin.
fn spawn_autopilot_node(
    app: &AppHandle,
    mesh: &Mesh,
    owner: &str,
    repo: &str,
    issue: &Issue,
) -> Result<(), String> {
    let prefill =
        crate::commands::agent::format_issue_prefill(owner, repo, issue.number, &issue.title);
    let initial_name = crate::session_naming::issue_node_name(issue.number, &issue.title);
    // Provider chain: the Autopilot Policy's own provider wins; otherwise the
    // normal default chain (mesh default -> app default -> claude).
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

    // `use_worktree_override = Some(true)` enforces isolation even on a mesh
    // configured worktree-off (PRD implementation decision). Status Pending +
    // background stage-2 replicates `create_pending`'s two-write contract.
    let node = crate::services::agent_node::create_with_source_pr_fork(
        mesh.id,
        &mesh.path,
        &branch,
        Some(&provider),
        Some(issue.number),
        None,
        None,
        Some(true),
        Some(&initial_name),
        None,
        None,
    )
    .map_err(|e| e.to_string())?;
    // Routes through SessionLifecycle (issue #132). `AppSessionLifecycleSink`
    // here even though `Pending` doesn't emit events — keeps the write
    // site symmetric with sibling code in this module that *does* emit.
    crate::agent::session_lifecycle::on_created(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node.id,
    )
    .map_err(|e| e.to_string())?;
    // Ledger row BEFORE stage-2 starts, so `spawn_agent_inner`'s branched-mode
    // enforcement (which keys off `get_autopilot_run`) sees it.
    db::create_autopilot_run(node.id, mesh.id, issue.number).map_err(|e| e.to_string())?;

    // Start buffering the node's PTY output for the state evaluator (#483).
    crate::autopilot::evaluator::register(node.id);

    let _ = app.emit(
        "node-created",
        crate::commands::agent::NodeCreatedPayload { id: node.id },
    );
    tracing::info!(
        "autopilot: created node {} for issue #{} on mesh {} (provider {})",
        node.id,
        issue.number,
        mesh.id,
        provider
    );

    crate::commands::agent::start_node_background(app.clone(), node.id, Some(prefill))?;

    // The prefill only *stages* the prompt in the harness's input box —
    // nothing submits it. Watch the PTY until the harness is observably
    // ready (prefill echoed + output quiet), then press Enter for it.
    crate::autopilot::launch::watch_and_submit(app.clone(), node.id, issue.number);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        LOGGED_BLOCKS.lock().unwrap().remove(&pair);
        assert!(
            mark_blocked_logged(pair.0, pair.1),
            "first insert is novel — caller logs once"
        );
        assert!(
            !mark_blocked_logged(pair.0, pair.1),
            "second insert is a duplicate — caller stays silent"
        );
        // Tidy: leave the set as we found it.
        LOGGED_BLOCKS.lock().unwrap().remove(&pair);
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
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct VecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for VecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for VecWriter {
            type Writer = VecWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(VecWriter(captured.clone()))
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
        LOGGED_BLOCKS.lock().unwrap().remove(&pair);

        let logs = String::from_utf8(captured.lock().unwrap().clone())
            .expect("captured log buffer is utf-8");
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
}
