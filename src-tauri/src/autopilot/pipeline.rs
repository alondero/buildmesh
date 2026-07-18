//! Autopilot wrap-up pipeline (issues #484 + #485, PRD #480 / ADR-0011).
//!
//! The per-node state machine that reacts to Node Turns for piloted nodes:
//!
//! ```text
//! implementing --(evaluator says COMPLETED)--> finishing(attempt 1)
//!   |                                             |
//!   |(BLOCKED: emit autopilot-blocked,            |-- verification green --> completed
//!   | stay implementing)                          |     (node status Completed, PR event)
//!   |(WORKING: nothing)                           |-- verification red, attempts < 3
//!                                                 |     --> inject correction, attempt+1
//!                                                 |-- verification red, attempts >= 3
//!                                                       --> failed (node status Error)
//! ```
//!
//! Verification (#485) is deterministic, not LLM-judged: the worktree must be
//! clean, the branch pushed (upstream exists, 0 ahead), and — unless the
//! mesh's policy is `none` — an open PR must exist for the branch. Per
//! ADR-0011 the *agent* runs tests/commits/pushes/creates the PR; Buildmesh
//! only checks the observable outcome and feeds a correction prompt back
//! into the PTY when it isn't there yet.
//!
//! Threading: `on_turn` is called from the attention webhook path, so all
//! real work (LLM classify, git inspection, GitHub round-trip) runs on a
//! dedicated worker thread; a per-node in-flight guard collapses re-entrant
//! turns while an evaluation is running.

use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

use super::evaluator::{self, Classification};
use super::finish;
use crate::db;
use crate::db::AutopilotRunState::{self as S, *};
use crate::models::SessionStatus;

/// Self-correction cap (PRD #480 story 13): total wrap-up prompt injections
/// (the initial `/finish` + corrections) before the node is failed.
pub const MAX_FINISH_ATTEMPTS: i32 = 3;

/// Nodes with an evaluation currently in flight (in-flight guard).
static EVALUATING: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));

// ---------------------------------------------------------------------------
// Pure decision cores (unit-tested without IO)
// ---------------------------------------------------------------------------

/// What to do after a turn while the node is `implementing`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TurnAction {
    InjectFinish,
    /// Surface "the agent needs a human" (UI badge/notification) but leave
    /// the pipeline state untouched — the user's reply resumes the loop.
    NotifyBlocked,
    Nothing,
}

pub(crate) fn decide_implementing(classification: Option<Classification>) -> TurnAction {
    match classification {
        Some(Classification::Completed) => TurnAction::InjectFinish,
        Some(Classification::Blocked) => TurnAction::NotifyBlocked,
        // WORKING and "couldn't tell" (safe degradation, #483 AC) both mean
        // "leave the agent alone".
        Some(Classification::Working) | None => TurnAction::Nothing,
    }
}

/// Observable wrap-up outcome for a `finishing` node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WrapupState {
    pub dirty: bool,
    /// Branch pushed with an up-to-date upstream (upstream exists, 0 ahead).
    pub pushed: bool,
    /// Open PR URL for the branch, if any.
    pub pr_url: Option<String>,
    /// The same PR's number — persisted to the ledger on completion so the
    /// merged-PR auto-close sweep can check it without re-deriving the branch.
    pub pr_number: Option<i64>,
    /// Does the mesh's policy require a PR (`action_on_success != "none"`)?
    pub pr_required: bool,
    /// The node's worktree could not be opened as a git repository — the
    /// dirty/pushed/PR fields are unknowable, and the correction must say so
    /// instead of fabricating "uncommitted changes" (2026-07-17 gh252 run:
    /// a broken worktree produced three invented reasons and sent the agent
    /// chasing state that was never wrong).
    pub repo_error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FinishOutcome {
    Complete,
    /// Inject a correction naming `reasons`, bumping attempts.
    Retry(Vec<String>),
    /// Attempts exhausted — fail the node, reporting `reasons`.
    Fail(Vec<String>),
}

pub(crate) fn decide_finishing(state: &WrapupState, attempts: i32) -> FinishOutcome {
    let mut reasons = Vec::new();
    if let Some(err) = &state.repo_error {
        // Unopenable repo: the git-state checks below would all be
        // fabrications. Report the one true failure so the agent repairs the
        // worktree the harness is actually looking at.
        reasons.push(err.clone());
    } else {
        if state.dirty {
            reasons.push("the worktree still has uncommitted changes".to_string());
        }
        if !state.pushed {
            reasons.push("the branch has not been pushed to origin (or has unpushed commits)".to_string());
        }
        if state.pr_required && state.pr_url.is_none() {
            reasons.push("no open pull request exists for the branch".to_string());
        }
    }
    if reasons.is_empty() {
        FinishOutcome::Complete
    } else if attempts >= MAX_FINISH_ATTEMPTS {
        FinishOutcome::Fail(reasons)
    } else {
        FinishOutcome::Retry(reasons)
    }
}

/// How much of the node's recent (cleaned) terminal output is echoed back
/// in a correction prompt (#485 AC: "captures the terminal output and
/// writes it back"). Kept short — the agent already holds its own context;
/// the tail is a pointer, not a transcript.
const CORRECTION_TAIL_CHARS: usize = 1_200;

/// The correction prompt written back into the PTY (#485 AC wording),
/// carrying the failure reasons plus the recent terminal tail.
pub(crate) fn correction_prompt(reasons: &[String], recent_output: &str) -> String {
    let tail = if recent_output.len() > CORRECTION_TAIL_CHARS {
        let mut start = recent_output.len() - CORRECTION_TAIL_CHARS;
        while !recent_output.is_char_boundary(start) {
            start += 1;
        }
        &recent_output[start..]
    } else {
        recent_output
    };
    let mut prompt = format!(
        "The automated wrap-up verification failed. Please fix this: {}. \
         Then complete the remaining wrap-up steps (commit, push, PR) and report the result.",
        reasons.join("; ")
    );
    if !tail.trim().is_empty() {
        prompt.push_str("\n\nRecent terminal output at the time of the check:\n");
        prompt.push_str(tail);
    }
    prompt
}

// ---------------------------------------------------------------------------
// PTY injection (#484)
// ---------------------------------------------------------------------------

/// Write a (possibly multi-line) prompt into the node's PTY stdin and submit
/// it. Multi-line text is wrapped in bracketed-paste markers so the agent
/// CLI treats it as one pasted block instead of submitting at every
/// newline; the trailing `\r` is the Enter keystroke (same as the desktop
/// input path).
///
/// Deliberately NOT routed through `coordinator::drive::AgentDriver`
/// (whose "no parallel write path" rule targets *Coordinator/scheduler*
/// callers): `send_prompt` is single-line (`{prompt}\n` — a newline mid-
/// template would submit fragments) and its idempotency ledger models
/// retried remote requests, which an in-process turn reaction doesn't
/// have. If `AgentDriver` grows multi-line paste support, converge on it.
pub(crate) fn write_prompt_to_pty(node_id: i64, text: &str) -> Result<(), String> {
    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        return Err(format!("node {} has no live agent process", node_id));
    }
    let payload = if text.contains('\n') {
        format!("\x1b[200~{}\x1b[201~\r", text)
    } else {
        format!("{}\r", text)
    };
    crate::agent::process::PROCESS_REGISTRY.write_bytes(node_id, payload.as_bytes())
}

/// After a backend-driven injection the agent is busy again — mirror the
/// coordinator drive's attention-clear so the UI doesn't keep a stale
/// "Needs attention" badge on a node Autopilot just drove. `pub(crate)` for
/// the manual `trigger_finish` command (PRD story 15), which injects the
/// same wrap-up prompt outside a turn evaluation.
pub(crate) fn clear_attention_after_injection(node_id: i64, app: &AppHandle) {
    crate::attention_autoclear::disarm(node_id);
    let _ = db::update_agent_node_status(node_id, SessionStatus::Running);
    let _ = app.emit(
        "attention-cleared",
        serde_json::json!({ "session_id": node_id }),
    );
    crate::http::events::emit(crate::http::events::EventMsg::AttentionCleared {
        session_id: node_id,
    });
}

// ---------------------------------------------------------------------------
// Impure verification
// ---------------------------------------------------------------------------

/// Inspect the node's worktree + GitHub for the observable wrap-up state.
/// Blocking (libgit2 walk + one GitHub round-trip) — worker thread only.
fn observe_wrapup_state(node: &crate::models::AgentNode, pr_required: bool) -> WrapupState {
    let host_path = crate::env::node_worktree_path(node)
        .map(|r| r.host_path)
        .unwrap_or_else(|| node.path.clone());

    let (dirty, branch, pushed, repo_error) = match git2::Repository::open(&host_path) {
        Ok(repo) => {
            let dirty = crate::git::primitives::is_dirty(&repo).unwrap_or(true);
            let branch = crate::git::primitives::head_branch_name(&repo);
            let pushed = branch
                .as_deref()
                .and_then(|b| {
                    let local = repo.find_branch(b, git2::BranchType::Local).ok()?;
                    let upstream = local.upstream().ok()?;
                    let local_oid = local.get().target()?;
                    let up_oid = upstream.get().target()?;
                    let (ahead, _behind) =
                        crate::git::primitives::ahead_behind(&repo, local_oid, up_oid).ok()?;
                    Some(ahead == 0)
                })
                .unwrap_or(false);
            (dirty, branch, pushed, None)
        }
        Err(e) => {
            tracing::warn!(
                "autopilot pipeline({}): could not open repo at {}: {}",
                node.id,
                host_path,
                e
            );
            // Name the exact path the harness inspects — without it the agent
            // has no way to know where the check is looking and starts
            // guessing (renaming branches, recreating worktrees elsewhere).
            let repo_error = format!(
                "the verification could not open the node's worktree at {} as a git repository ({}) — \
                 repair or recreate the worktree at that exact path and do your wrap-up (commit, push, PR) from it",
                host_path, e
            );
            (true, None, false, Some(repo_error))
        }
    };

    // PR lookup only makes sense once the branch exists remotely.
    let pr = if pushed {
        branch.as_deref().and_then(|b| {
            let mesh = db::get_mesh_by_id(node.mesh_id).ok()?;
            let (owner, repo_name) =
                crate::commands::pr::resolve_github_owner_repo(&mesh).ok()?;
            let client = crate::services::github::GitHubClient::new().ok()?;
            match client.find_open_pr_for_branch(&owner, &repo_name, b) {
                Ok(pr) => pr.map(|p| (p.number, p.html_url)),
                Err(e) => {
                    tracing::warn!(
                        "autopilot pipeline({}): PR lookup for {} failed: {}",
                        node.id,
                        b,
                        e
                    );
                    None
                }
            }
        })
    } else {
        None
    };
    let (pr_number, pr_url) = match pr {
        Some((n, url)) => (Some(n), Some(url)),
        None => (None, None),
    };

    WrapupState { dirty, pushed, pr_url, pr_number, pr_required, repo_error }
}

// ---------------------------------------------------------------------------
// Turn entry point
// ---------------------------------------------------------------------------

/// Node Turn hook — third consumer in `node_turn::publish`. Cheap for
/// non-piloted nodes (one in-memory set lookup).
pub fn on_turn(node_id: i64, app: &AppHandle) {
    if !evaluator::is_piloted(node_id) {
        return;
    }
    {
        let mut evaluating = EVALUATING.lock().unwrap();
        if !evaluating.insert(node_id) {
            tracing::debug!("autopilot pipeline({}): evaluation already in flight", node_id);
            return;
        }
    }
    let app = app.clone();
    std::thread::spawn(move || {
        run_turn_evaluation(node_id, &app);
        EVALUATING.lock().unwrap().remove(&node_id);
    });
}

fn run_turn_evaluation(node_id: i64, app: &AppHandle) {
    let run = match db::get_autopilot_run(node_id) {
        Ok(Some(run)) => run,
        Ok(None) => {
            // Piloted flag without a ledger row — stale registration.
            evaluator::unregister(node_id);
            return;
        }
        Err(e) => {
            tracing::warn!("autopilot pipeline({}): ledger read failed: {}", node_id, e);
            return;
        }
    };
    let (issue_number, state, attempts) = run;

    let node = match db::get_agent_node_by_id(node_id) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("autopilot pipeline({}): node read failed: {}", node_id, e);
            return;
        }
    };
    let mesh = db::get_mesh_by_id(node.mesh_id).ok();
    let action_on_success = mesh
        .as_ref()
        .and_then(|m| m.autopilot_action_on_success.clone())
        .unwrap_or_else(|| "draft_pr".to_string());

    match state {
        S::Implementing => {
            // Evaluator backend env: the mesh's Autopilot provider
            // side-channel (falls back to the built-in Anthropic haiku pin
            // when unset) — never the node's own possibly-expensive model
            // (the #824 lesson).
            let backend_env = crate::session_naming::naming_backend_env(
                mesh.as_ref()
                    .and_then(|m| m.autopilot_provider.as_deref())
                    .unwrap_or("anthropic"),
            );
            let classification = evaluator::classify(node_id, &backend_env);
            match decide_implementing(classification) {
                TurnAction::InjectFinish => {
                    let prompt =
                        finish::finish_prompt(Some(issue_number).filter(|n| *n > 0), Some(&action_on_success));
                    match write_prompt_to_pty(node_id, &prompt) {
                        Ok(()) => {
                            let _ = db::set_autopilot_run_state(node_id, Finishing, Some(1));
                            clear_attention_after_injection(node_id, app);
                            let _ = app.emit(
                                "autopilot-finishing",
                                serde_json::json!({ "node_id": node_id, "issue": issue_number }),
                            );
                            tracing::info!(
                                "autopilot pipeline({}): task classified COMPLETED — injected wrap-up prompt (attempt 1)",
                                node_id
                            );
                        }
                        Err(e) => tracing::warn!(
                            "autopilot pipeline({}): wrap-up injection failed: {}",
                            node_id,
                            e
                        ),
                    }
                }
                TurnAction::NotifyBlocked => {
                    // Status is already AwaitingInput via the attention path;
                    // this event is the "human help needed" escalation
                    // (PRD story 14) the frontend surfaces as a toast.
                    let _ = app.emit(
                        "autopilot-blocked",
                        serde_json::json!({ "node_id": node_id, "issue": issue_number }),
                    );
                    tracing::info!(
                        "autopilot pipeline({}): agent classified BLOCKED — human input requested",
                        node_id
                    );
                }
                TurnAction::Nothing => {}
            }
        }
        S::Finishing => {
            let observed = observe_wrapup_state(&node, action_on_success != "none");
            match decide_finishing(&observed, attempts) {
                FinishOutcome::Complete => {
                    // PR identity first, state second: the merged-PR sweep
                    // keys off `state = completed AND pr_number IS NOT NULL`,
                    // so this order can't yield a sweepable row without a PR.
                    if let (Some(n), Some(url)) = (observed.pr_number, observed.pr_url.as_deref()) {
                        let _ = db::set_autopilot_run_pr(node_id, n, url);
                    }
                    let _ = db::set_autopilot_run_state(node_id, Completed, None);
                    let _ = db::update_agent_node_status(node_id, SessionStatus::Completed);
                    let _ = app.emit(
                        "autopilot-pr-created",
                        serde_json::json!({
                            "node_id": node_id,
                            "issue": issue_number,
                            "pr_url": observed.pr_url,
                        }),
                    );
                    evaluator::unregister(node_id);
                    tracing::info!(
                        "autopilot pipeline({}): wrap-up verified (pr: {:?}) — node Completed",
                        node_id,
                        observed.pr_url
                    );
                }
                FinishOutcome::Retry(reasons) => {
                    let prompt = correction_prompt(&reasons, &evaluator::cleaned_tail(node_id));
                    match write_prompt_to_pty(node_id, &prompt) {
                        Ok(()) => {
                            let _ = db::set_autopilot_run_state(
                                node_id,
                                Finishing,
                                Some(attempts + 1),
                            );
                            clear_attention_after_injection(node_id, app);
                            tracing::info!(
                                "autopilot pipeline({}): verification red ({}) — correction attempt {}/{}",
                                node_id,
                                reasons.join("; "),
                                attempts + 1,
                                MAX_FINISH_ATTEMPTS
                            );
                        }
                        Err(e) => tracing::warn!(
                            "autopilot pipeline({}): correction injection failed: {}",
                            node_id,
                            e
                        ),
                    }
                }
                FinishOutcome::Fail(reasons) => {
                    let _ = db::set_autopilot_run_state(node_id, Failed, None);
                    let _ = db::update_agent_node_status(node_id, SessionStatus::Error);
                    let _ = app.emit(
                        "autopilot-finish-failed",
                        serde_json::json!({
                            "node_id": node_id,
                            "issue": issue_number,
                            "reasons": reasons,
                        }),
                    );
                    evaluator::unregister(node_id);
                    tracing::warn!(
                        "autopilot pipeline({}): wrap-up failed after {} attempts ({})",
                        node_id,
                        attempts,
                        reasons.join("; ")
                    );
                }
            }
        }
        // terminal — stale registration cleanup
        S::Completed | S::Failed | S::Merged => evaluator::unregister(node_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapup(dirty: bool, pushed: bool, pr: Option<&str>, pr_required: bool) -> WrapupState {
        WrapupState {
            dirty,
            pushed,
            pr_url: pr.map(str::to_string),
            pr_number: pr.map(|_| 1),
            pr_required,
            repo_error: None,
        }
    }

    // ── implementing-state decisions ────────────────────────────────────────

    #[test]
    fn completed_classification_injects_the_finish_prompt() {
        assert_eq!(
            decide_implementing(Some(Classification::Completed)),
            TurnAction::InjectFinish
        );
    }

    #[test]
    fn blocked_classification_notifies_without_state_change() {
        assert_eq!(
            decide_implementing(Some(Classification::Blocked)),
            TurnAction::NotifyBlocked
        );
    }

    #[test]
    fn working_and_failed_classification_do_nothing() {
        // `None` is the evaluator's safe-degradation path (#483 AC): a
        // broken/timed-out classifier must never drive the pipeline.
        assert_eq!(decide_implementing(Some(Classification::Working)), TurnAction::Nothing);
        assert_eq!(decide_implementing(None), TurnAction::Nothing);
    }

    // ── finishing-state decisions (#485) ────────────────────────────────────

    #[test]
    fn clean_pushed_with_pr_completes() {
        let s = wrapup(false, true, Some("https://github.com/x/y/pull/9"), true);
        assert_eq!(decide_finishing(&s, 1), FinishOutcome::Complete);
    }

    #[test]
    fn pr_not_required_completes_without_a_pr() {
        let s = wrapup(false, true, None, false);
        assert_eq!(decide_finishing(&s, 1), FinishOutcome::Complete);
    }

    #[test]
    fn dirty_worktree_retries_below_the_cap() {
        let s = wrapup(true, true, Some("url"), true);
        match decide_finishing(&s, 1) {
            FinishOutcome::Retry(reasons) => {
                assert!(reasons.iter().any(|r| r.contains("uncommitted")));
            }
            other => panic!("expected Retry, got {:?}", other),
        }
    }

    #[test]
    fn unpushed_and_missing_pr_are_both_reported() {
        let s = wrapup(false, false, None, true);
        match decide_finishing(&s, 2) {
            FinishOutcome::Retry(reasons) => {
                assert_eq!(reasons.len(), 2);
                assert!(reasons.iter().any(|r| r.contains("pushed")));
                assert!(reasons.iter().any(|r| r.contains("pull request")));
            }
            other => panic!("expected Retry, got {:?}", other),
        }
    }

    /// 2026-07-17 gh252/gh340 regression: when the node's worktree can't be
    /// opened, the old code degraded to `(dirty=true, pushed=false)` and the
    /// correction claimed "uncommitted changes / not pushed / no PR" — all
    /// fabricated. The agent was sent to fix state that was never wrong. An
    /// unopenable repo must surface as exactly one honest reason.
    #[test]
    fn unopenable_repo_reports_the_real_error_not_fabricated_reasons() {
        let mut s = wrapup(true, false, None, true);
        s.repo_error = Some(
            "the verification could not open the node's worktree at X ( ... )".to_string(),
        );
        match decide_finishing(&s, 1) {
            FinishOutcome::Retry(reasons) => {
                assert_eq!(reasons.len(), 1, "one honest reason, no fabrications");
                assert!(reasons[0].contains("could not open"));
                assert!(!reasons[0].contains("uncommitted"));
            }
            other => panic!("expected Retry, got {:?}", other),
        }
    }

    /// The impure half of the repo-error contract: an unopenable worktree
    /// must actually populate `repo_error` (and name the inspected path) —
    /// without this, a regression in `observe_wrapup_state`'s Err arm would
    /// pass every pure `decide_finishing` test above.
    #[test]
    fn observe_wrapup_state_populates_repo_error_for_an_unopenable_worktree() {
        let node = crate::models::AgentNode {
            id: 1,
            mesh_id: 1,
            name: "gh1-missing".to_string(),
            path: std::env::temp_dir()
                .join("bm-observe-missing-mesh")
                .to_string_lossy()
                .to_string(),
            branch: "main".to_string(),
            env: crate::models::EnvType::default(),
            provider: "anthropic".to_string(),
            status: SessionStatus::Running,
            cli_session_id: None,
            worktree_name: Some("gh1-missing".to_string()),
            use_worktree: true,
            source_issue: Some(1),
            source_pr: None,
            head_repo_owner: None,
            head_repo_clone_url: None,
            source_pr_pinned_sha: None,
            position: 0,
            created_at: chrono::Utc::now(),
        };
        // The worktree path doesn't exist, so the repo open must fail and the
        // pushed=false short-circuit keeps GitHub out of the picture.
        let state = observe_wrapup_state(&node, true);
        let err = state.repo_error.expect("unopenable worktree must set repo_error");
        assert!(
            err.contains("gh1-missing"),
            "reason must name the inspected path, got: {}",
            err
        );
        assert!(err.contains("could not open"));
    }

    /// The honest repo-error reason still respects the attempts cap.
    #[test]
    fn unopenable_repo_at_the_cap_fails_with_the_honest_reason() {
        let mut s = wrapup(true, false, None, true);
        s.repo_error = Some("could not open worktree".to_string());
        match decide_finishing(&s, MAX_FINISH_ATTEMPTS) {
            FinishOutcome::Fail(reasons) => {
                assert_eq!(reasons, vec!["could not open worktree".to_string()]);
            }
            other => panic!("expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn attempts_cap_fails_the_node() {
        // Attempt counter at the cap (3 injections done) + still red → Fail,
        // never a fourth injection (PRD story 13).
        let s = wrapup(true, false, None, true);
        assert!(matches!(
            decide_finishing(&s, MAX_FINISH_ATTEMPTS),
            FinishOutcome::Fail(_)
        ));
    }

    #[test]
    fn green_at_the_cap_still_completes() {
        // The cap gates *re-injection*, not success: a wrap-up that turns
        // green on the final attempt completes normally.
        let s = wrapup(false, true, Some("url"), true);
        assert_eq!(decide_finishing(&s, MAX_FINISH_ATTEMPTS), FinishOutcome::Complete);
    }

    #[test]
    fn correction_prompt_names_every_reason_and_echoes_the_terminal_tail() {
        let p = correction_prompt(
            &[
                "the worktree still has uncommitted changes".to_string(),
                "no open pull request exists for the branch".to_string(),
            ],
            "cargo test ... FAILED: assertion at foo.rs:42",
        );
        assert!(p.starts_with("The automated wrap-up verification failed. Please fix this:"));
        assert!(p.contains("uncommitted"));
        assert!(p.contains("pull request"));
        // #485 AC — the captured terminal output rides along.
        assert!(p.contains("assertion at foo.rs:42"));
    }

    #[test]
    fn correction_prompt_truncates_a_long_tail_and_skips_an_empty_one() {
        let long_tail = format!("{}END-MARKER", "y".repeat(5_000));
        let p = correction_prompt(&["r".to_string()], &long_tail);
        assert!(p.contains("END-MARKER"));
        assert!(p.len() < 2_000, "tail is capped, not embedded whole");

        let p_empty = correction_prompt(&["r".to_string()], "   ");
        assert!(!p_empty.contains("Recent terminal output"));
    }
}
