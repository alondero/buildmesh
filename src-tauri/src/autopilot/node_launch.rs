//! Deep module for the Autopilot Agent Node launch seam (issue #1178).
//!
//! Autopilot has two Agent Node launch paths — issue-driven
//! (`services::autopilot::spawn_autopilot_node`) and Looping
//! (`services::autopilot::spawn_loop_node`) — and each must perform the
//! same ordered mechanics:
//!
//! 1. create the pending Agent Node;
//! 2. write `Pending` status through `agent::session_lifecycle`;
//! 3. record the `autopilot_runs` ledger row **before** stage 2;
//! 4. register the node with `autopilot::evaluator`;
//! 5. emit `node-created`;
//! 6. start `spawn_with_intent` in the background;
//! 7. arrange `autopilot::launch::watch_and_submit` for the initial prompt.
//!
//! The policy facts legitimately differ between the two modes (GitHub
//! Issue vs. no source, forced Worktree vs. mesh policy, ledger
//! identity, watcher toast marker). The policy callers in
//! `services::autopilot` derive those facts and hand them to this
//! module through [`AutopilotNodeLaunchPlan`]. This module is the
//! **single production owner** of the ordered sequence above: no future
//! Autopilot mode can bypass a lifecycle write, a ledger write, the
//! evaluator registration, or the event emission through this seam,
//! because the seam is a single function and the steps are private.
//!
//! ## What the policy caller owns
//!
//! - Effective provider chain (Autopilot provider > mesh default > app default).
//! - Base branch resolution (`get_default_branch_blocking` with fallback).
//! - Initial node name (issue-derived slug, `loop-iter-{n}`).
//! - [`SpawnIntent`] — the canonical source of the prefill text (issue #1180).
//! - The literal that survives into the `autopilot-submitted` toast
//!   payload (`watcher_issue_number` — issue number for issue mode,
//!   `0` for loop mode).
//!
//! The launch watcher derives its readiness marker from
//! `intent.initial_prompt()` itself (wayfinder #1027), so callers do
//! not pass a separate prompt string — that would re-introduce a
//! second prompt-formatting rule (issue #1180 closed that).

use tauri::{AppHandle, Emitter};

use crate::agent::spawn::{SpawnIntent, SpawnRequest, TerminalSize};
use crate::commands::agent::NodeCreatedPayload;
use crate::db;
use crate::models::Mesh;

/// Identifies the *kind* of Autopilot run this launch is starting. The
/// ledger row shape differs by kind (issue-driven writes `issue_number`,
/// looping writes `loop_iteration`) and the issue-driven path needs a
/// `source_issue` set on the Agent Node so the rest of the system
/// (dedupe view, frontend badge) treats it as issue-spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutopilotRunIdentity {
    Issue { issue_number: i64 },
    Loop { iteration: i64 },
}

/// The worktree override policy. Issue-driven nodes force `use_worktree =
/// true` because the wrap-up PR needs a real branch to push; Looping
/// nodes respect the Mesh's setting because game-decompilation-style
/// non-worktree repos (the canonical non-worktree mesh) must run on
/// the root branch directly (ticket #992).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutopilotWorktreePolicy {
    /// `use_worktree_override = Some(true)`. The launch will spawn a
    /// Worktree Node regardless of `mesh.use_worktree`.
    ForceWorktreeBranch,
    /// `use_worktree_override = None`. The launch falls through to
    /// `mesh.use_worktree` inside `create_with_source_pr_fork`.
    RespectMesh,
}

/// Resolved plan handed to [`launch_autopilot_node`]. Every field is a
/// **policy fact** the caller has decided — the launch module does not
/// re-derive provider, branch, or name from the mesh.
///
/// `mesh` is taken by value because the launch module needs its `id`,
/// `path`, and `use_worktree`; cloning the row once at the call site is
/// cheaper than threading three borrows through every helper.
#[derive(Debug, Clone)]
pub(crate) struct AutopilotNodeLaunchPlan {
    pub mesh: Mesh,
    pub provider: String,
    pub branch: String,
    pub initial_name: String,
    pub intent: SpawnIntent,
    pub run: AutopilotRunIdentity,
    pub worktree_policy: AutopilotWorktreePolicy,
    /// Literal forwarded to `autopilot::launch::watch_and_submit` so the
    /// `autopilot-submitted` toast payload carries the originating issue
    /// number, or `0` for loop-mode nodes that have no GitHub source.
    pub watcher_issue_number: i64,
}

impl AutopilotNodeLaunchPlan {
    /// Map the worktree policy onto the `use_worktree_override` argument
    /// [`crate::services::agent_node::create_with_source_pr_fork`]
    /// expects: `Some(true)` for forced worktree, `None` to fall through
    /// to `mesh.use_worktree`. Extracted so the mapping is testable as a
    /// pure helper (no DB or AppHandle needed).
    pub(crate) fn use_worktree_override(&self) -> Option<bool> {
        match self.worktree_policy {
            AutopilotWorktreePolicy::ForceWorktreeBranch => Some(true),
            AutopilotWorktreePolicy::RespectMesh => None,
        }
    }

    /// The `source_issue` value the Agent Node row needs: `Some(number)`
    /// for issue-driven runs so the rest of the system treats this as an
    /// issue-spawned node, `None` for Looping runs which have no GitHub
    /// source. Pure helper, also testable in isolation.
    pub(crate) fn source_issue(&self) -> Option<i64> {
        match self.run {
            AutopilotRunIdentity::Issue { issue_number } => Some(issue_number),
            AutopilotRunIdentity::Loop { .. } => None,
        }
    }
}

/// Create an Agent Node, write its Autopilot ledger row, register it with
/// the LLM state evaluator, emit `node-created`, schedule the background
/// stage-2 spawn, and arm the launch watcher — in that exact order.
///
/// A failure before the Agent Node row exists (step 1) cannot leave any
/// ledger/evaluator state behind — those steps haven't run yet. A
/// failure after step 1 but before stage-2 completion is the same as
/// before #1178: the lifecycle machinery (autoclear safety net,
/// attention mark) handles a stuck `Pending` node.
///
/// Does not await stage 2 (the background `spawn_with_intent`), which
/// runs to completion independently on the tokio worker pool. A
/// stage-2 failure logs and drops — the node stays in `Pending` and the
/// user-facing lifecycle machinery handles it.
pub(crate) fn launch_autopilot_node(
    app: &AppHandle,
    plan: AutopilotNodeLaunchPlan,
) -> Result<(), String> {
    let use_worktree_override = plan.use_worktree_override();
    let source_issue = plan.source_issue();
    let mesh_id = plan.mesh.id;
    let provider = plan.provider.clone();

    // Step 1 — row creation and the `Pending` status write are
    // intentionally split: the row exists first, the lifecycle
    // transition follows, and a status-write failure can never strand a
    // partially-created row. Routing the status write through
    // SessionLifecycle (issue #132) keeps `agent_nodes.status` single-
    // writer across the whole codebase.
    let node = crate::services::agent_node::create_with_source_pr_fork(
        mesh_id,
        &plan.mesh.path,
        &plan.branch,
        Some(&plan.provider),
        source_issue,
        None, // source_pr
        None, // source_pr_pinned_sha
        use_worktree_override,
        Some(&plan.initial_name),
        None, // head_repo_owner
        None, // head_repo_clone_url
    )
    .map_err(|e| e.to_string())?;

    crate::agent::session_lifecycle::on_created(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node.id,
    )
    .map_err(|e| e.to_string())?;

    // Step 3 — ledger row BEFORE stage-2. `spawn_agent_inner` reads
    // `get_autopilot_run` to enforce branched-mode worktree policy; a
    // missing row would let an Autopilot-spawned node fall back to the
    // mesh's plain worktree mode. The helper differs by run identity:
    // issue-driven writes `issue_number`, looping writes `loop_iteration`
    // (with `issue_number = 0`).
    match plan.run {
        AutopilotRunIdentity::Issue { issue_number } => {
            db::create_autopilot_run(node.id, mesh_id, issue_number)
                .map_err(|e| e.to_string())?;
        }
        AutopilotRunIdentity::Loop { iteration } => {
            db::create_autopilot_loop_run(node.id, mesh_id, iteration)
                .map_err(|e| e.to_string())?;
        }
    }

    // Step 4 — `register` is idempotent, so a restart that replays the
    // launch re-registers cleanly.
    crate::autopilot::evaluator::register(node.id);

    let _ = app.emit(
        "node-created",
        NodeCreatedPayload { id: node.id },
    );
    match plan.run {
        AutopilotRunIdentity::Issue { issue_number } => {
            tracing::info!(
                "autopilot: created node {} for issue #{} on mesh {} (provider {})",
                node.id,
                issue_number,
                mesh_id,
                provider,
            );
        }
        AutopilotRunIdentity::Loop { iteration } => {
            tracing::info!(
                "autopilot: created loop node {} for mesh {} iteration {} (provider {})",
                node.id,
                mesh_id,
                iteration,
                provider,
            );
        }
    }

    // Step 6 — choose the startup delivery before moving the intent into
    // the background spawn. Supporting harnesses stage a prefill; other
    // harnesses start fresh and receive the prompt over the live PTY.
    let prefill = plan
        .intent
        .initial_prompt()
        .map(|p| p.into_string())
        .unwrap_or_default();
    let prompt_delivery =
        crate::autopilot::launch::initial_prompt_delivery(&plan.provider, &prefill);
    let app_for_spawn = app.clone();
    let intent_for_spawn = plan.intent.clone();
    let fallback_prompt = prefill.clone();
    let node_id_for_spawn = node.id;
    tauri::async_runtime::spawn(async move {
        if let Err(error) = crate::agent::spawn::spawn_with_intent(
            &app_for_spawn,
            SpawnRequest::new(node_id_for_spawn, intent_for_spawn, TerminalSize::default()),
        )
        .await
        {
            tracing::error!(
                "autopilot: node {} failed: {}",
                node_id_for_spawn,
                error
            );
            return;
        }
        if prompt_delivery == crate::autopilot::launch::InitialPromptDelivery::InjectAfterSpawn {
            if let Err(error) = crate::autopilot::pipeline::write_prompt_to_pty(
                node_id_for_spawn,
                &fallback_prompt,
                &app_for_spawn,
            ) {
                tracing::error!(
                    "autopilot: fallback prompt injection for node {} failed: {}",
                    node_id_for_spawn,
                    error
                );
                let _ = crate::agent::session_lifecycle::on_error(
                    &crate::agent::session_lifecycle::AppSessionLifecycleSink {
                        app: &app_for_spawn,
                    },
                    node_id_for_spawn,
                );
            }
        }
    });

    // Step 7 — for a staged prefill, `watch_and_submit` derives its
    // readiness marker from the prefill (wayfinder #1027); passing the
    // same `intent.initial_prompt()` text means loop-mode prefills still
    // match against the staged prefill instead of timing out. Failures
    // here are rare — the prefill only stages the prompt, the watcher
    // waits for harness readiness and presses Enter.
    if prompt_delivery == crate::autopilot::launch::InitialPromptDelivery::Prefill {
        crate::autopilot::launch::watch_and_submit(
            app.clone(),
            node.id,
            plan.watcher_issue_number,
            &prefill,
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(use_worktree: bool) -> Mesh {
        Mesh {
            id: 1,
            path: "/tmp/repo".to_string(),
            use_worktree,
            ..Default::default()
        }
    }

    // -- `use_worktree_override` mapping -----------------------------------
    //
    // Pins the contract that `spawn_autopilot_node`'s pre-#1178
    // `Some(true)` and `spawn_loop_node`'s pre-#1178 `None` survive the
    // refactor verbatim. A regression that flips either branch would
    // silently break the issue-driven wrap-up PR (needs a branch) or
    // silently break the game-decompilation non-worktree mesh.

    #[test]
    fn forced_worktree_policy_maps_to_some_true() {
        let plan = AutopilotNodeLaunchPlan {
            mesh: mesh(true),
            provider: "claude".into(),
            branch: "main".into(),
            initial_name: "x".into(),
            intent: SpawnIntent::Issue(crate::agent::spawn::IssueContext {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
                title: "t".into(),
            }),
            run: AutopilotRunIdentity::Issue { issue_number: 1 },
            worktree_policy: AutopilotWorktreePolicy::ForceWorktreeBranch,
            watcher_issue_number: 1,
        };
        assert_eq!(plan.use_worktree_override(), Some(true));
    }

    #[test]
    fn respect_mesh_policy_maps_to_none() {
        let plan = AutopilotNodeLaunchPlan {
            mesh: mesh(false),
            provider: "claude".into(),
            branch: "main".into(),
            initial_name: "x".into(),
            intent: SpawnIntent::Loop {
                initial_prompt: "p".into(),
            },
            run: AutopilotRunIdentity::Loop { iteration: 1 },
            worktree_policy: AutopilotWorktreePolicy::RespectMesh,
            watcher_issue_number: 0,
        };
        assert_eq!(plan.use_worktree_override(), None);
    }

    // -- `source_issue` mapping -------------------------------------------
    //
    // Pins the contract that issue-driven runs carry `source_issue =
    // Some(number)` (so the dedupe view + frontend badge treat them as
    // issue-spawned) while Looping runs carry `source_issue = None` (no
    // GitHub source exists).

    #[test]
    fn issue_run_carries_its_source_issue() {
        let plan = AutopilotNodeLaunchPlan {
            mesh: mesh(true),
            provider: "claude".into(),
            branch: "main".into(),
            initial_name: "x".into(),
            intent: SpawnIntent::Issue(crate::agent::spawn::IssueContext {
                owner: "o".into(),
                repo: "r".into(),
                number: 42,
                title: "t".into(),
            }),
            run: AutopilotRunIdentity::Issue { issue_number: 42 },
            worktree_policy: AutopilotWorktreePolicy::ForceWorktreeBranch,
            watcher_issue_number: 42,
        };
        assert_eq!(plan.source_issue(), Some(42));
    }

    #[test]
    fn loop_run_has_no_source_issue() {
        let plan = AutopilotNodeLaunchPlan {
            mesh: mesh(false),
            provider: "claude".into(),
            branch: "main".into(),
            initial_name: "loop-iter-3".into(),
            intent: SpawnIntent::Loop {
                initial_prompt: "iterate".into(),
            },
            run: AutopilotRunIdentity::Loop { iteration: 3 },
            worktree_policy: AutopilotWorktreePolicy::RespectMesh,
            watcher_issue_number: 0,
        };
        assert_eq!(plan.source_issue(), None);
    }

    // -- Step ordering invariants -----------------------------------------
    //
    // The 7-step orchestration has three ordering constraints whose
    // reordering would silently regress behaviour. We pin them via
    // source-level byte offsets (cheap smoke check): a regression that
    // renames a call site or moves it past another step trips the
    // assertion. These are belt-and-braces against a future refactor
    // — the *primary* defense is the behaviour-preserving pre-#1178
    // contract and the fact that no other code path writes the ledger
    // or registers the evaluator (audited).

    #[test]
    fn launch_writes_ledger_before_scheduling_stage_2() {
        let src = include_str!("node_launch.rs");
        let ledger_write = src
            .find("db::create_autopilot_run(")
            .or_else(|| src.find("db::create_autopilot_loop_run("))
            .expect("launch_autopilot_node must call a ledger helper");
        let stage_2_spawn = src
            .find("tauri::async_runtime::spawn")
            .expect("launch_autopilot_node must schedule stage 2");
        assert!(
            ledger_write < stage_2_spawn,
            "ledger row must be written before stage-2 spawn is scheduled \
             (issue #1178 invariant); ledger at byte {}, stage-2 at byte {}",
            ledger_write,
            stage_2_spawn,
        );
    }

    #[test]
    fn launch_writes_lifecycle_pending_before_ledger() {
        let src = include_str!("node_launch.rs");
        let lifecycle_write = src
            .find("session_lifecycle::on_created(")
            .expect("launch_autopilot_node must call session_lifecycle::on_created");
        let ledger_write = src
            .find("db::create_autopilot_run(")
            .or_else(|| src.find("db::create_autopilot_loop_run("))
            .expect("launch_autopilot_node must call a ledger helper");
        assert!(
            lifecycle_write < ledger_write,
            "Pending status must be written before the ledger row \
             (issue #132, #1178); lifecycle at byte {}, ledger at byte {}",
            lifecycle_write,
            ledger_write,
        );
    }

    #[test]
    fn launch_registers_evaluator_before_arming_watcher() {
        let src = include_str!("node_launch.rs");
        let evaluator_register = src
            .find("evaluator::register(node.id)")
            .expect("launch_autopilot_node must call evaluator::register");
        let watcher_arm = src
            .find("watch_and_submit(")
            .expect("launch_autopilot_node must call watch_and_submit");
        assert!(
            evaluator_register < watcher_arm,
            "evaluator::register must run before watch_and_submit \
             (launch watcher polls `is_piloted`); \
             evaluator at byte {}, watcher at byte {}",
            evaluator_register,
            watcher_arm,
        );
    }

    // -- Plan construction smoke test -------------------------------------
    //
    // Pins the field set so a future refactor that removes one of
    // these policy facts has to update this test alongside the call
    // sites. The mesh row carries the mesh identity; provider / branch
    // / initial_name / intent / run / worktree_policy / watcher_issue
    // are each derived by a policy caller.

    #[test]
    fn plan_struct_carries_the_expected_policy_facts() {
        let plan = AutopilotNodeLaunchPlan {
            mesh: mesh(true),
            provider: "claude".into(),
            branch: "main".into(),
            initial_name: "fix-auth".into(),
            intent: SpawnIntent::Issue(crate::agent::spawn::IssueContext {
                owner: "alondero".into(),
                repo: "buildmesh".into(),
                number: 1178,
                title: "Deepen the launch seam".into(),
            }),
            run: AutopilotRunIdentity::Issue { issue_number: 1178 },
            worktree_policy: AutopilotWorktreePolicy::ForceWorktreeBranch,
            watcher_issue_number: 1178,
        };
        assert_eq!(plan.mesh.id, 1);
        assert_eq!(plan.provider, "claude");
        assert_eq!(plan.branch, "main");
        assert_eq!(plan.initial_name, "fix-auth");
        assert_eq!(plan.watcher_issue_number, 1178);
        match plan.run {
            AutopilotRunIdentity::Issue { issue_number } => assert_eq!(issue_number, 1178),
            AutopilotRunIdentity::Loop { .. } => panic!("expected Issue run"),
        }
    }
}
