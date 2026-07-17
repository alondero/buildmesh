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
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use crate::autopilot::{gate_trigger, AutopilotTrigger, GateDecision};
use crate::db;
use crate::models::{Mesh, SessionStatus, DEFAULT_AUTOPILOT_TRIGGER_LABEL};
use crate::services::github::{GitHubClient, Issue};

/// PRD #480 implementation decision: poll every 2 minutes.
pub const POLL_INTERVAL: Duration = Duration::from_secs(120);

/// Grace period before the first pass so startup (DB migration, HTTP bind,
/// pool reconcile) finishes before we compete for the DB mutex and network.
const STARTUP_DELAY: Duration = Duration::from_secs(20);

/// `(mesh_id, issue_number)` pairs whose author failed the collaborator gate.
/// Remembered for the app's lifetime so each gated trigger costs exactly one
/// permission fetch + one log line, not one per pass. Cleared on restart —
/// cheap to re-derive, and a permission granted meanwhile is picked up then.
static GATED_TRIGGERS: Lazy<Mutex<HashSet<(i64, i64)>>> =
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
    let meshes = match db::list_autopilot_enabled_meshes() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("autopilot: could not list enabled meshes: {}", e);
            return;
        }
    };
    for mesh in meshes {
        if let Err(e) = poll_mesh(app, &mesh) {
            tracing::warn!("autopilot: mesh {} ({}) pass failed: {}", mesh.id, mesh.name, e);
        }
    }
}

fn poll_mesh(app: &AppHandle, mesh: &Mesh) -> Result<(), String> {
    let active = db::count_active_autopilot_nodes(mesh.id).map_err(|e| e.to_string())?;
    let capacity = i64::from(mesh.autopilot_concurrency_limit) - active;
    // PRD story 6: no spare capacity → no GitHub round-trip at all.
    if capacity <= 0 {
        return Ok(());
    }

    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(mesh)?;
    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let label = mesh
        .autopilot_trigger_label
        .as_deref()
        .unwrap_or(DEFAULT_AUTOPILOT_TRIGGER_LABEL);
    let issues = client
        .list_open_issues_with_label(&owner, &repo, label)
        .map_err(|e| e.to_string())?;
    let known = db::list_known_autopilot_issue_numbers(mesh.id).map_err(|e| e.to_string())?;

    let planned = plan_spawns(&issues, &known, capacity as usize);
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
                if let Err(e) = spawn_autopilot_node(app, mesh, &owner, &repo, issue) {
                    tracing::warn!(
                        "autopilot: spawn for issue #{} on mesh {} failed: {}",
                        issue.number,
                        mesh.id,
                        e
                    );
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
    db::update_agent_node_status(node.id, SessionStatus::Pending).map_err(|e| e.to_string())?;
    // Ledger row BEFORE stage-2 starts, so `spawn_agent_inner`'s branched-mode
    // enforcement (which keys off `get_autopilot_run`) sees it.
    db::create_autopilot_run(node.id, mesh.id, issue.number).map_err(|e| e.to_string())?;

    // Start buffering the node's PTY output for the state evaluator (#483).
    crate::autopilot::evaluator::register(node.id);

    let _ = app.emit("node-created", serde_json::json!({ "id": node.id }));
    tracing::info!(
        "autopilot: created node {} for issue #{} on mesh {} (provider {})",
        node.id,
        issue.number,
        mesh.id,
        provider
    );

    crate::commands::agent::start_node_background(app.clone(), node.id, Some(prefill))
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
}
