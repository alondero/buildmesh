//! Autopilot wrap-up pipeline (issues #484 + #485, PRD #480 / ADR-0011).
//!
//! The per-node state machine that reacts to Node Turns for piloted nodes:
//!
//! ```text
//! implementing --(evaluator says COMPLETED)--> finishing(attempt 1)
//!   |                                             |
//!   |(BLOCKED: emit autopilot-blocked,            |-- verification green + loop suffix
//!   | stay implementing)                          |     --> suffix_pending --(Node Turn)--> completed
//!   |(WORKING: nothing)                           |-- verification green, no suffix --> completed
//!                                                 |-- verification red, attempts < 3
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
//! dedicated worker thread; a per-node in-flight guard serialises re-entrant
//! turns while an evaluation is running (a turn arriving mid-evaluation is
//! queued, not dropped — a dropped *final* turn stalls the run, #874).
//!
//! Turn delivery is not guaranteed (the attention callback is a best-effort
//! HTTP hook), so the poller adds two fallbacks: the green-only re-drive for
//! stalled `finishing` rows, and [`watchdog_pass`], which synthesises the
//! evaluation a lost turn never delivered for any quiet piloted node.

use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::{hash_map::Entry, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

use super::evaluator::{self, Classification};

// ---------------------------------------------------------------------------
// Wire types — Tauri event payloads (issue #161)
// ---------------------------------------------------------------------------

/// Payload of the `autopilot-blocked` Tauri event. Emitted when the evaluator
/// classifies the agent's recent PTY tail as BLOCKED (the agent has asked
/// the user a question, or it's stalled needing human input). The frontend
/// surfaces it as a toast pointing the user at the node.
///
/// Generated to `src/types/generated/AutopilotBlockedPayload.ts`; the TS half
/// is imported by `src/App.tsx`. `issue` is `0` for hand-spawned autopilot
/// nodes that have no originating GitHub issue.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AutopilotBlockedPayload.ts")]
pub struct AutopilotBlockedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "i32")]
    pub issue: i64,
}

/// Payload of the `autopilot-finishing` Tauri event. Emitted when a task is
/// classified COMPLETED and the wrap-up prompt is injected into the node's
/// PTY. The frontend listener (`src/stores/agentNodeStore.ts`) uses it to
/// flip the header pill to the amber "wrap-up" treatment while the self-
/// correction loop runs.
///
/// Generated to `src/types/generated/AutopilotFinishingPayload.ts`; the TS
/// half is imported by `src/stores/agentNodeStore.ts`. `issue` is `None`
/// for hand-spawned nodes that have no originating GitHub issue — preserved
/// as a nullable wire field so the existing JSON shape (`"issue": null`) is
/// backwards-compatible with already-deployed listeners.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AutopilotFinishingPayload.ts")]
pub struct AutopilotFinishingPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "Option<i32>")]
    pub issue: Option<i64>,
}

/// Payload of the `autopilot-pr-created` Tauri event. Emitted after the
/// wrap-up verification (#485) is green — the worktree is clean, the branch
/// pushed, an open PR exists. The frontend marks the node's autopilot pill
/// as `completed` AND refetches the node list (status flips to `Completed`).
///
/// Generated to `src/types/generated/AutopilotPrCreatedPayload.ts`; the TS
/// half is imported by `src/App.tsx` and `src/stores/agentNodeStore.ts`.
/// `pr_url` is `None` when the mesh's `autopilot_action_on_success` policy
/// is `none` (no PR was opened).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AutopilotPrCreatedPayload.ts")]
pub struct AutopilotPrCreatedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "i32")]
    pub issue: i64,
    pub pr_url: Option<String>,
}

/// Payload of the `autopilot-finish-failed` Tauri event. Emitted after 3
/// failed wrap-up verification attempts (issue #485 self-correction loop
/// exhausted) — the node is marked `Error` and the user gets an error toast
/// with the reason list.
///
/// Generated to `src/types/generated/AutopilotFinishFailedPayload.ts`; the
/// TS half is imported by `src/App.tsx`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AutopilotFinishFailedPayload.ts")]
pub struct AutopilotFinishFailedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "i32")]
    pub issue: i64,
    pub reasons: Vec<String>,
}
use super::finish;
use crate::db;
use crate::db::AutopilotRunState::{self as S, *};

/// Self-correction cap (PRD #480 story 13): total wrap-up prompt injections
/// (the initial `/finish` + corrections) before the node is failed.
pub const MAX_FINISH_ATTEMPTS: i32 = 3;

/// Nodes with an evaluation currently in flight. The value records whether
/// another turn arrived *while* that evaluation was running — `true` means
/// the finishing evaluation must immediately re-run instead of dropping the
/// arrival (issue #874: a dropped final turn stalls the run forever).
static EVALUATING: Lazy<Mutex<HashMap<i64, bool>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Claim the per-node evaluation slot. `false` = an evaluation is already in
/// flight; the arrival has been queued and that evaluation will re-run.
pub(crate) fn try_begin_evaluation(node_id: i64) -> bool {
    match EVALUATING.lock().unwrap().entry(node_id) {
        Entry::Occupied(mut e) => {
            *e.get_mut() = true;
            false
        }
        Entry::Vacant(v) => {
            v.insert(false);
            true
        }
    }
}

/// Release the slot. `true` = a turn arrived mid-evaluation and the caller
/// must run another evaluation (the slot stays claimed for it).
pub(crate) fn end_evaluation_and_check_rerun(node_id: i64) -> bool {
    let mut evaluating = EVALUATING.lock().unwrap();
    match evaluating.get_mut(&node_id) {
        Some(pending) if *pending => {
            *pending = false;
            true
        }
        _ => {
            evaluating.remove(&node_id);
            false
        }
    }
}

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
    /// The branch currently checked out in the inspected worktree.
    pub branch: Option<String>,
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
    let reasons = wrapup_reasons(state);
    if reasons.is_empty() {
        FinishOutcome::Complete
    } else if attempts >= MAX_FINISH_ATTEMPTS {
        FinishOutcome::Fail(reasons)
    } else {
        FinishOutcome::Retry(reasons)
    }
}

/// Return the observable reasons a wrap-up is not ready to complete. Circuits
/// use the same verification contract as legacy issue Autopilot before their
/// `OpenPr` action: clean worktree, up-to-date pushed branch, and (when the
/// caller requires it) an existing PR.
pub(crate) fn wrapup_reasons(state: &WrapupState) -> Vec<String> {
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
    reasons
}

/// What a verified-green wrap-up does next. A suffix belongs only to the loop
/// run that was tagged at spawn time — the mesh may have changed mode while
/// this node was working, so current mesh mode is not a safe discriminator.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VerifiedWrapupAction<'a> {
    CompleteNow,
    InjectSuffix(&'a str),
}

pub(crate) fn decide_verified_wrapup<'a>(
    loop_iteration: Option<i64>,
    suffix_prompt: Option<&'a str>,
) -> VerifiedWrapupAction<'a> {
    match (loop_iteration, suffix_prompt) {
        (Some(_), Some(prompt)) if !prompt.trim().is_empty() => {
            VerifiedWrapupAction::InjectSuffix(prompt)
        }
        _ => VerifiedWrapupAction::CompleteNow,
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

/// Poll cadence for the submit watcher's readiness/acknowledgement checks.
const SUBMIT_POLL: Duration = Duration::from_millis(250);

/// How long to wait for the pasted prompt to echo back in PTY output before
/// concluding this provider renders no echo and moving on.
const PASTE_ECHO_DEADLINE: Duration = Duration::from_secs(3);

/// The TUI redraw after a paste must have been quiet this long before Enter
/// is sent (mirrors `launch::MIN_QUIET_MS`'s reasoning at a smaller scale —
/// the box is drawn and the CLI is waiting).
const PASTE_SETTLE_QUIET_MS: u128 = 1_000;

/// Upper bound on waiting for the post-paste redraw to settle.
const PASTE_SETTLE_DEADLINE: Duration = Duration::from_secs(15);

/// After an Enter keystroke, PTY output must appear within this window for
/// the submit to count as acknowledged.
const ENTER_ACK_WINDOW: Duration = Duration::from_secs(6);

/// Enter keystrokes attempted before the watcher gives up and surfaces the
/// node for human attention.
const MAX_ENTER_ATTEMPTS: u32 = 3;

/// The bytes staged into the PTY input box — WITHOUT the Enter keystroke.
/// Multi-line text is wrapped in bracketed-paste markers so the agent CLI
/// treats it as one pasted block instead of submitting at every newline.
///
/// The Enter is deliberately NOT part of this payload: ink-based TUIs
/// (Claude Code) batch stdin reads, and a `\r` arriving in the same read
/// burst as a bracketed paste is treated as part of the paste — the prompt
/// sits staged in the input box and is never submitted (issue #874, node
/// 2328: the correction was visibly pasted, the run stalled forever).
pub(crate) fn injection_payload(text: &str) -> String {
    if text.contains('\n') {
        format!("\x1b[200~{}\x1b[201~", text)
    } else {
        text.to_string()
    }
}

/// Has the node produced PTY output more recently than `ms_since_mark`
/// milliseconds ago? Pure core of both the paste-echo check and the
/// Enter-acknowledgement check.
pub(crate) fn output_seen_within(ms_since_output: Option<u128>, ms_since_mark: u128) -> bool {
    matches!(ms_since_output, Some(m) if m < ms_since_mark)
}

/// Write a (possibly multi-line) prompt into the node's PTY stdin, then
/// submit it from a background watcher: wait for the paste to echo and the
/// redraw to settle, send Enter as its own write, and verify output follows
/// (retrying Enter a bounded number of times). `Ok` means "staged and
/// submission scheduled" — a submit that never takes marks the node for
/// human attention instead of stalling silently.
///
/// Deliberately NOT routed through `coordinator::drive::AgentDriver`
/// (whose "no parallel write path" rule targets *Coordinator/scheduler*
/// callers): `send_prompt` is single-line (`{prompt}\n` — a newline mid-
/// template would submit fragments) and its idempotency ledger models
/// retried remote requests, which an in-process turn reaction doesn't
/// have. If `AgentDriver` grows multi-line paste support, converge on it.
pub(crate) fn write_prompt_to_pty(node_id: i64, text: &str, app: &AppHandle) -> Result<(), String> {
    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        return Err(format!("node {} has no live agent process", node_id));
    }
    crate::agent::process::PROCESS_REGISTRY
        .write_bytes(node_id, injection_payload(text).as_bytes())?;
    let app = app.clone();
    std::thread::spawn(move || submit_staged_prompt(node_id, &app));
    Ok(())
}

/// The background half of [`write_prompt_to_pty`]: settle, Enter, verify.
fn submit_staged_prompt(node_id: i64, app: &AppHandle) {
    // Phase 1: wait for the paste to echo back (the TUI redrawing its input
    // box with the staged text). Providers that render no echo fall through
    // at the deadline.
    let wrote_at = Instant::now();
    while Instant::now() < wrote_at + PASTE_ECHO_DEADLINE {
        if output_seen_within(
            evaluator::millis_since_last_output(node_id),
            wrote_at.elapsed().as_millis(),
        ) {
            break;
        }
        std::thread::sleep(SUBMIT_POLL);
    }
    // Phase 2: wait for the redraw to go quiet — Enter must land at an idle
    // input box, not inside the paste-processing burst.
    let settle_deadline = Instant::now() + PASTE_SETTLE_DEADLINE;
    while Instant::now() < settle_deadline {
        match evaluator::millis_since_last_output(node_id) {
            Some(quiet) if quiet < PASTE_SETTLE_QUIET_MS => std::thread::sleep(SUBMIT_POLL),
            _ => break, // quiet (or no output tracked at all) — settled
        }
    }
    // Phase 3: submit and verify.
    match press_enter_until_output(node_id) {
        Ok(attempt) => tracing::info!(
            "autopilot inject({}): staged prompt submitted (Enter attempt {})",
            node_id,
            attempt
        ),
        Err(e) => {
            // Loud degrade: a staged-but-unsubmitted prompt is exactly the
            // silent stall of #874 — surface the node instead.
            tracing::warn!(
                "autopilot inject({}): staged prompt was never submitted ({}) — \
                 marking the node for human attention",
                node_id,
                e
            );
            crate::commands::attention::mark_attention(node_id, app);
        }
    }
}

/// Send Enter and wait for PTY output to acknowledge it, retrying up to
/// [`MAX_ENTER_ATTEMPTS`] times. Returns the attempt number that took.
/// Shared with the launch watcher — a swallowed Enter stalls a prefilled
/// launch the same way it stalls an injection.
pub(crate) fn press_enter_until_output(node_id: i64) -> Result<u32, String> {
    for attempt in 1..=MAX_ENTER_ATTEMPTS {
        crate::agent::process::PROCESS_REGISTRY.write_bytes(node_id, b"\r")?;
        let sent_at = Instant::now();
        while Instant::now() < sent_at + ENTER_ACK_WINDOW {
            std::thread::sleep(SUBMIT_POLL);
            if output_seen_within(
                evaluator::millis_since_last_output(node_id),
                sent_at.elapsed().as_millis(),
            ) {
                return Ok(attempt);
            }
        }
        tracing::warn!(
            "autopilot inject({}): Enter attempt {}/{} produced no output",
            node_id,
            attempt,
            MAX_ENTER_ATTEMPTS
        );
    }
    Err(format!(
        "no PTY output followed any of {} Enter keystrokes",
        MAX_ENTER_ATTEMPTS
    ))
}

/// After a backend-driven injection the agent is busy again — mirror the
/// coordinator drive's attention-clear so the UI doesn't keep a stale
/// "Needs attention" badge on a node Autopilot just drove. Private to
/// this module — every call site lives alongside it.
fn clear_attention_after_injection(node_id: i64, app: &AppHandle) {
    crate::attention_autoclear::disarm(node_id);
    // Routes through SessionLifecycle (issue #132); the desktop emit lives
    // in the sink. Mobile broadcast is a separate channel kept here.
    let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app };
    let _ = crate::agent::session_lifecycle::on_attention_cleared(&sink, node_id);
    crate::http::events::emit(crate::http::events::EventMsg::AttentionCleared {
        session_id: node_id,
    });
}

// ---------------------------------------------------------------------------
// Impure verification
// ---------------------------------------------------------------------------

/// Inspect the node's worktree + GitHub for the observable wrap-up state.
/// Blocking (libgit2 walk + one GitHub round-trip) — worker thread only.
pub(crate) fn observe_wrapup_state(
    node: &crate::models::AgentNode,
    pr_required: bool,
) -> Result<WrapupState, String> {
    let mesh_id = node.mesh_id;
    observe_wrapup_state_with(
        node,
        pr_required,
        observe_wrapup_git_state,
        move |branch| {
            let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
            let (owner, repo_name) = crate::commands::pr::resolve_github_owner_repo(&mesh)?;
            let client = crate::services::github::GitHubClient::new().map_err(|e| e.to_string())?;
            client
                .find_open_pr_for_branch(&owner, &repo_name, branch)
                .map_err(|e| {
                    format!(
                        "could not verify the pull request for {owner}/{repo_name} branch {branch}: {e}"
                    )
                })
        },
    )
}

/// Combine local wrap-up observation with an optional, fallible PR lookup.
/// Keeping the external lookup behind this seam lets callers defer
/// infrastructure errors without changing the agent's correction budget.
fn observe_wrapup_state_with(
    node: &crate::models::AgentNode,
    pr_required: bool,
    observe_git: impl FnOnce(&crate::models::AgentNode) -> WrapupState,
    find: impl FnOnce(&str) -> Result<Option<crate::services::github::PullRequest>, String>,
) -> Result<WrapupState, String> {
    let mut state = observe_git(node);
    state.pr_required = pr_required;
    // A PR is a prerequisite only when the mesh policy requires one. The
    // optional path must not turn an unavailable GitHub service into a
    // finishing blocker merely because a PR lookup would be nice to have.
    if state.pushed && pr_required {
        let pr = if let Some(branch) = state.branch.as_deref() {
            find(branch)?
        } else {
            None
        };
        if let Some(pr) = pr {
            state.pr_number = Some(pr.number);
            state.pr_url = Some(pr.html_url);
        }
    }
    Ok(state)
}

/// Observe local wrap-up prerequisites and the actual checked-out branch.
/// Worktree directory names may outlive a branch rename by the agent.
pub(crate) fn observe_wrapup_git_state(
    node: &crate::models::AgentNode,
) -> WrapupState {
    // `node_working_path` resolves Worktree and Root Nodes alike (host path +
    // env), so the self-heal below covers both; on a Root Node the sanitize
    // is a no-op (`.git` is a directory, not a gitlink).
    let resolved = crate::env::node_working_path(node);
    let host_path = resolved.host_path.clone();

    // Self-heal before giving up: an MSYS-flavoured git leaves Git-Bash-style
    // `/f/...` paths in the worktree link files that the agent's CLI reads
    // fine but libgit2 reports as NotFound (the 2026-07-17 gh252 incident —
    // the agent had to run `git worktree repair` itself). Sanitize both link
    // sides and retry once, so a format-only mismatch never reaches the
    // repo_error path and never costs a correction attempt.
    let opened = git2::Repository::open(&host_path).or_else(|first_err| {
        tracing::info!(
            "autopilot pipeline({}): open failed ({}); sanitizing worktree links and retrying",
            node.id,
            first_err
        );
        if let Err(e) = crate::git::worktree::sanitize_git_worktree(&host_path, resolved.env_type)
        {
            tracing::warn!("autopilot pipeline({}): sanitize failed: {}", node.id, e);
        }
        git2::Repository::open(&host_path)
    });

    let (dirty, branch, pushed, repo_error) = match opened {
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

    WrapupState {
        dirty, pushed, branch, pr_url: None, pr_number: None, pr_required: false, repo_error,
    }
}

// ---------------------------------------------------------------------------
// Turn entry point
// ---------------------------------------------------------------------------

/// Node Turn hook — third consumer in `node_turn::publish`. Cheap for
/// non-piloted nodes (one in-memory set lookup).
pub fn on_turn(node_id: i64, app: &AppHandle) {
    // Circuit-owned nodes use this module's evaluator blackboard but have no
    // legacy `autopilot_runs` row. Let the circuit worker own their turn;
    // otherwise the lookup below would classify them as stale and unregister
    // the shared tail before the circuit observation pass can consume it.
    if evaluator::is_circuit_piloted(node_id) {
        return;
    }
    if !evaluator::is_piloted(node_id) {
        return;
    }
    if !try_begin_evaluation(node_id) {
        tracing::debug!(
            "autopilot pipeline({}): evaluation already in flight — turn queued for re-run",
            node_id
        );
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || loop {
        run_turn_evaluation(node_id, &app);
        if !end_evaluation_and_check_rerun(node_id) {
            break;
        }
        tracing::debug!(
            "autopilot pipeline({}): re-running evaluation for a turn that arrived mid-flight",
            node_id
        );
    });
}

fn run_turn_evaluation(node_id: i64, app: &AppHandle) {
    evaluator::note_evaluation(node_id);
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
    let (issue_number, state, attempts, loop_iteration, persisted_pr_url) = run;

    let node = match db::get_agent_node_by_id(node_id) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("autopilot pipeline({}): node read failed: {}", node_id, e);
            return;
        }
    };
    let mesh = db::get_mesh_by_id(node.mesh_id).ok();
    let action_on_success = crate::services::autopilot::configured_action_on_success(node.mesh_id);

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
                    match write_prompt_to_pty(node_id, &prompt, app) {
                        Ok(()) => {
                            let _ = db::set_autopilot_run_state(node_id, Finishing, Some(1));
                            clear_attention_after_injection(node_id, app);
                            let _ = app.emit(
                                "autopilot-finishing",
                                AutopilotFinishingPayload {
                                    node_id,
                                    issue: Some(issue_number),
                                },
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
                        AutopilotBlockedPayload {
                            node_id,
                            issue: issue_number,
                        },
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
            let observed = match observe_wrapup_state(&node, action_on_success != "none") {
                Ok(state) => state,
                Err(error) => {
                    // A GitHub outage is infrastructure state, not a fact
                    // the coding agent can repair. Leave the finishing
                    // attempt untouched and let a later redrive retry it.
                    tracing::warn!("autopilot pipeline({}): wrap-up observation deferred: {}", node_id, error);
                    return;
                }
            };
            match decide_finishing(&observed, attempts) {
                FinishOutcome::Complete => handle_verified_wrapup(
                    node_id,
                    issue_number,
                    attempts,
                    loop_iteration,
                    mesh.as_ref().and_then(|m| m.loop_suffix_prompt.as_deref()),
                    &observed,
                    app,
                ),
                FinishOutcome::Retry(reasons) => {
                    let prompt = correction_prompt(&reasons, &evaluator::cleaned_tail(node_id));
                    match write_prompt_to_pty(node_id, &prompt, app) {
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
                    // Routes through SessionLifecycle (issue #132). The
                    // `unless_in(Error, Archived)` guard lives inside
                    // `on_error`. Clone `app` because the sink borrows it
                    // and `app.emit` below needs to use it directly.
                    let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink {
                        app: &app.clone(),
                    };
                    let _ = crate::agent::session_lifecycle::on_error(&sink, node_id);
                    let _ = app.emit(
                        "autopilot-finish-failed",
                        AutopilotFinishFailedPayload {
                            node_id,
                            issue: issue_number,
                            reasons: reasons.clone(),
                        },
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
        S::SuffixPending => {
            // Reaching a Node Turn proves the suffix prompt's turn yielded.
            // It is already post-verification, so do not classify it or inject
            // `finish.md` a second time.
            complete_autopilot_run(node_id, issue_number, persisted_pr_url, app);
        }
        // terminal — stale registration cleanup
        S::Completed | S::Failed | S::Merged => evaluator::unregister(node_id),
    }
}

/// Route a verified-green wrap-up either to terminal completion or to the
/// optional second turn for a loop iteration. Shared by Node Turn evaluation
/// and the stalled-finishing re-drive so neither path can bypass the suffix.
fn handle_verified_wrapup(
    node_id: i64,
    issue_number: i64,
    attempts: i32,
    loop_iteration: Option<i64>,
    suffix_prompt: Option<&str>,
    observed: &WrapupState,
    app: &AppHandle,
) {
    // PR identity first: suffix completion happens on a later callback, after
    // this in-memory observation is gone, and the merged-PR sweep reads these
    // persisted columns once the run reaches `completed`.
    if let (Some(n), Some(url)) = (observed.pr_number, observed.pr_url.as_deref()) {
        if let Err(e) = db::set_autopilot_run_pr(node_id, n, url) {
            tracing::warn!(
                "autopilot pipeline({}): verified PR could not be persisted: {}",
                node_id,
                e
            );
            return;
        }
    }

    match decide_verified_wrapup(loop_iteration, suffix_prompt) {
        VerifiedWrapupAction::CompleteNow => {
            complete_autopilot_run(node_id, issue_number, observed.pr_url.clone(), app);
        }
        VerifiedWrapupAction::InjectSuffix(prompt) => {
            if let Err(e) = start_suffix_turn(node_id, attempts, prompt, app) {
                tracing::warn!(
                    "autopilot pipeline({}): suffix injection failed; left in finishing for retry: {}",
                    node_id,
                    e
                );
            }
        }
    }
}

/// Persist the non-terminal state before staging the prompt, so even a very
/// fast suffix turn cannot be mistaken for another `finishing` turn. If the
/// PTY write fails, roll back to `finishing`; the existing stale-run re-drive
/// can then retry without falsely completing the iteration.
fn start_suffix_turn(
    node_id: i64,
    attempts: i32,
    prompt: &str,
    app: &AppHandle,
) -> Result<(), String> {
    db::set_autopilot_run_state(node_id, SuffixPending, None).map_err(|e| e.to_string())?;
    if let Err(write_error) = write_prompt_to_pty(node_id, prompt, app) {
        if let Err(rollback_error) =
            db::set_autopilot_run_state(node_id, Finishing, Some(attempts))
        {
            tracing::warn!(
                "autopilot pipeline({}): suffix state rollback failed after PTY error: {}",
                node_id,
                rollback_error
            );
        }
        return Err(write_error);
    }

    clear_attention_after_injection(node_id, app);
    tracing::info!(
        "autopilot pipeline({}): wrap-up verified — suffix prompt injected; waiting for its Node Turn",
        node_id
    );
    Ok(())
}

/// Mark an Autopilot run Completed and publish the lifecycle/event effects.
/// For a loop suffix this is called by the suffix Node Turn; without a suffix
/// it is called immediately after deterministic wrap-up verification.
fn complete_autopilot_run(
    node_id: i64,
    issue_number: i64,
    pr_url: Option<String>,
    app: &AppHandle,
) {
    let _ = db::set_autopilot_run_state(node_id, Completed, None);
    // Routes through SessionLifecycle (issue #132).
    let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app };
    let _ = crate::agent::session_lifecycle::on_completed(&sink, node_id);
    let _ = app.emit(
        "autopilot-pr-created",
        AutopilotPrCreatedPayload {
            node_id,
            issue: issue_number,
            pr_url: pr_url.clone(),
        },
    );
    evaluator::unregister(node_id);
    tracing::info!(
        "autopilot pipeline({}): wrap-up lifecycle complete (pr: {:?}) — node Completed",
        node_id,
        pr_url
    );
}

// ---------------------------------------------------------------------------
// Poller re-drive (stalled `finishing` runs)
// ---------------------------------------------------------------------------

/// Re-verify stalled `finishing` runs without waiting for a Node Turn.
///
/// The wrap-up pipeline is otherwise purely turn-driven, and a turn is not a
/// guaranteed delivery: the in-flight guard drops turns that arrive during an
/// evaluation, and an attention callback can simply never fire. When the
/// *final* turn is the lost one, the run stalls in `finishing` forever with
/// its concurrency slot held (node 2328, 2026-07-17: agent had pushed and
/// PR'd, verification never re-ran).
///
/// Deliberately conservative: a green observation completes the run — the
/// work is observably done, no agent interaction needed. A red observation is
/// left for the turn-driven correction path and for [`watchdog_pass`], which
/// injects corrections only once the node's output has been quiet long
/// enough that the agent cannot still be typing. (A real turn arriving
/// *while* a re-drive holds the guard is queued and re-run as a full turn
/// evaluation, never dropped.)
/// Runs on the poller's worker thread (blocking git + GitHub round-trips are
/// fine there).
pub fn redrive_stalled_finishing(app: &AppHandle, candidates: &[i64]) {
    for &node_id in candidates {
        if !try_begin_evaluation(node_id) {
            continue; // a live turn evaluation owns this node right now
        }
        redrive_one(node_id, app);
        while end_evaluation_and_check_rerun(node_id) {
            // A real turn arrived while we re-drove — honour it in full.
            run_turn_evaluation(node_id, app);
        }
    }
}

/// How long a piloted node's PTY output must have been quiet before the
/// watchdog treats an unevaluated yield as a lost turn and synthesises the
/// evaluation itself. Long enough that an agent mid-response (thinking,
/// streaming pauses) is never interrupted; the poller only passes every
/// 2 minutes anyway.
pub(crate) const WATCHDOG_QUIET_MS: u128 = 180_000;

/// Should the watchdog synthesise a turn evaluation for this node?
/// True when the node has produced output no evaluation has reacted to
/// (output newer than the last evaluation start, or no evaluation ever ran)
/// AND that output has been quiet at least [`WATCHDOG_QUIET_MS`].
pub(crate) fn should_watchdog_evaluate(
    ms_since_output: Option<u128>,
    ms_since_eval: Option<u128>,
) -> bool {
    match ms_since_output {
        // No output observed since registration — nothing to react to (the
        // green-only re-drive still covers post-restart hydrated runs).
        None => false,
        Some(out) => out >= WATCHDOG_QUIET_MS && ms_since_eval.is_none_or(|ev| out < ev),
    }
}

/// Poller fallback for lost turns (issue #874): for every active piloted
/// node, if a yield went unevaluated (missed attention callback) and the
/// node has been output-quiet long enough, run the same evaluation a Node
/// Turn would have driven — classify in `implementing`, verify/correct in
/// `finishing`. This is what makes a lost turn recoverable in *every*
/// pipeline state, not just observably-green `finishing`.
pub fn watchdog_pass(app: &AppHandle, candidates: &[i64]) {
    for &node_id in candidates {
        if !evaluator::is_piloted(node_id) {
            continue;
        }
        if !should_watchdog_evaluate(
            evaluator::millis_since_last_output(node_id),
            evaluator::millis_since_last_evaluation(node_id),
        ) {
            continue;
        }
        if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
            // A dead agent can't receive an injection; the green re-drive
            // still completes observably-done work, everything else needs a
            // human (the node already shows as dead in the UI).
            tracing::debug!(
                "autopilot watchdog({}): unevaluated yield but no live process — skipping",
                node_id
            );
            continue;
        }
        if !try_begin_evaluation(node_id) {
            continue;
        }
        tracing::info!(
            "autopilot watchdog({}): quiet unevaluated yield — synthesizing the lost turn's evaluation",
            node_id
        );
        run_turn_evaluation(node_id, app);
        while end_evaluation_and_check_rerun(node_id) {
            run_turn_evaluation(node_id, app);
        }
    }
}

fn redrive_one(node_id: i64, app: &AppHandle) {
    // Read the ledger under the guard, not from the stalled listing: a turn
    // may have advanced the state (or the attempts count) in between.
    let (issue_number, attempts, loop_iteration) = match db::get_autopilot_run(node_id) {
        Ok(Some((issue, S::Finishing, attempts, loop_iteration, _))) => {
            (issue, attempts, loop_iteration)
        }
        _ => return,
    };
    let node = match db::get_agent_node_by_id(node_id) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("autopilot redrive({}): node read failed: {}", node_id, e);
            return;
        }
    };
    let mesh = db::get_mesh_by_id(node.mesh_id).ok();
    let action_on_success = crate::services::autopilot::configured_action_on_success(node.mesh_id);

    let observed = match observe_wrapup_state(&node, action_on_success != "none") {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!("autopilot redrive({}): wrap-up observation deferred: {}", node_id, error);
            return;
        }
    };
    match decide_finishing(&observed, attempts) {
        FinishOutcome::Complete => {
            tracing::info!(
                "autopilot redrive({}): stalled wrap-up is observably green — advancing",
                node_id
            );
            handle_verified_wrapup(
                node_id,
                issue_number,
                attempts,
                loop_iteration,
                mesh.as_ref().and_then(|m| m.loop_suffix_prompt.as_deref()),
                &observed,
                app,
            );
        }
        FinishOutcome::Retry(reasons) | FinishOutcome::Fail(reasons) => {
            tracing::info!(
                "autopilot redrive({}): still red ({}) — leaving for the turn-driven path",
                node_id,
                reasons.join("; ")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `SessionStatus` only appears inside this test module (used by the
    // node-fixture builder at line ~990); the previous module-level
    // `use` flagged an unused-import warning when compiling without
    // `--cfg test`. Scoped here so the prod build stays warning-free.
    use crate::models::SessionStatus;

    fn wrapup(dirty: bool, pushed: bool, pr: Option<&str>, pr_required: bool) -> WrapupState {
        WrapupState {
            dirty,
            pushed,
            branch: None,
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

    fn wrapup_test_node() -> crate::models::AgentNode {
        crate::models::AgentNode {
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
            // Required by the full-literal `AgentNode { ... }` initializer
            // (wayfinder #982 / ticket #984). `is_pinned` is a UI-toggle
            // field unrelated to this test's repo-open failure path;
            // `false` matches the column default for a fresh node.
            is_pinned: false,
            source_issue: Some(1),
            source_pr: None,
            head_repo_owner: None,
            head_repo_clone_url: None,
            source_pr_pinned_sha: None,
            signal_health: None,
            position: 0,
            created_at: chrono::Utc::now(),
            worktree_path: None,
        }
    }

    #[test]
    fn observe_wrapup_git_state_uses_actual_branch_without_github_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("test", "test@example.com").unwrap();
        let commit = repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[]).unwrap();
        repo.branch("renamed-implementation", &repo.find_commit(commit).unwrap(), false).unwrap();
        repo.set_head("refs/heads/renamed-implementation").unwrap();
        repo.remote("origin", "https://github.com/example/repo.git").unwrap();
        repo.reference("refs/remotes/origin/renamed-implementation", commit, true, "test").unwrap();
        repo.find_branch("renamed-implementation", git2::BranchType::Local).unwrap()
            .set_upstream(Some("origin/renamed-implementation")).unwrap();
        let worktree_path = dir.path().join("original-worktree-name");
        let branch_ref = repo.find_reference("refs/heads/renamed-implementation").unwrap();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&branch_ref));
        // Release the branch from the main checkout before attaching it.
        repo.set_head_detached(commit).unwrap();
        repo.worktree("original-worktree-name", &worktree_path, Some(&options)).unwrap();
        let mut node = wrapup_test_node();
        node.path = dir.path().to_string_lossy().into_owned();
        node.worktree_name = Some("original-worktree-name".into());
        node.worktree_path = Some(worktree_path.to_string_lossy().into_owned());
        let state = observe_wrapup_git_state(&node);
        assert_eq!(state.branch.as_deref(), Some("renamed-implementation"));
        assert_ne!(state.branch, node.worktree_name);
        assert!(state.pushed);
        assert!(wrapup_reasons(&state).is_empty());
        assert!(state.pr_url.is_none());
    }

    #[test]
    fn observe_wrapup_state_populates_repo_error_for_an_unopenable_worktree() {
        let node = wrapup_test_node();
        // The worktree path doesn't exist, so the repo open must fail and the
        // pushed=false short-circuit keeps GitHub out of the picture.
        let state = observe_wrapup_state(&node, true).unwrap();
        let err = state.repo_error.expect("unopenable worktree must set repo_error");
        assert!(
            err.contains("gh1-missing"),
            "reason must name the inspected path, got: {}",
            err
        );
        assert!(err.contains("could not open"));
    }

    fn synthetic_pushed_wrapup() -> WrapupState {
        WrapupState {
            dirty: false,
            pushed: true,
            branch: Some("feature/implementation".into()),
            pr_url: None,
            pr_number: None,
            pr_required: false,
            repo_error: None,
        }
    }

    #[test]
    fn optional_wrapup_does_not_block_on_github_lookup_errors() {
        let node = wrapup_test_node();
        let state = observe_wrapup_state_with(
            &node,
            false,
            |_| synthetic_pushed_wrapup(),
            |_| panic!("optional PR discovery must not be required"),
        )
        .unwrap();
        assert!(!state.pr_required);
        assert!(state.pr_number.is_none());
        assert!(state.pr_url.is_none());
    }

    #[test]
    fn required_wrapup_propagates_lookup_error_then_accepts_recovery() {
        let node = wrapup_test_node();
        let error = observe_wrapup_state_with(
            &node,
            true,
            |_| synthetic_pushed_wrapup(),
            |_| Err("GitHub unavailable".into()),
        )
        .unwrap_err();
        assert_eq!(error, "GitHub unavailable");

        let recovered = observe_wrapup_state_with(
            &node,
            true,
            |_| synthetic_pushed_wrapup(),
            |_| {
                Ok(Some(
                    serde_json::from_value(serde_json::json!({
                        "number": 42,
                        "html_url": "https://github.com/example/repo/pull/42"
                    }))
                    .unwrap(),
                ))
            },
        )
        .unwrap();
        assert_eq!(recovered.pr_number, Some(42));
        assert_eq!(
            recovered.pr_url.as_deref(),
            Some("https://github.com/example/repo/pull/42")
        );
        assert!(recovered.pr_required);
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

    // ── verified loop wrap-up decisions (#993) ───────────────────────────────

    #[test]
    fn loop_run_with_a_suffix_injects_a_second_turn() {
        assert_eq!(
            decide_verified_wrapup(Some(4), Some("review the result and report")),
            VerifiedWrapupAction::InjectSuffix("review the result and report")
        );
    }

    #[test]
    fn issue_runs_and_blank_suffixes_complete_immediately() {
        assert_eq!(
            decide_verified_wrapup(None, Some("configured on the mesh")),
            VerifiedWrapupAction::CompleteNow,
            "the ledger row, not the mesh's current mode, identifies a loop iteration"
        );
        for suffix in [None, Some(""), Some("   \n\t")] {
            assert_eq!(
                decide_verified_wrapup(Some(4), suffix),
                VerifiedWrapupAction::CompleteNow
            );
        }
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

    // ── prompt injection: paste and Enter are decoupled (#874) ─────────────

    /// The 2026-07-17 node-2328 stall: paste + `\r` in one atomic PTY write
    /// left the correction staged in the input box, never submitted — the
    /// TUI batched the Enter into the paste event. The staged payload must
    /// therefore never carry the Enter keystroke; Enter is a separate,
    /// settle-gated write.
    #[test]
    fn injection_payload_never_carries_the_enter_keystroke() {
        assert!(!injection_payload("line one\nline two").contains('\r'));
        assert!(!injection_payload("single line prompt").contains('\r'));
    }

    #[test]
    fn multiline_payload_is_bracketed_paste_wrapped() {
        let p = injection_payload("a\nb");
        assert!(p.starts_with("\x1b[200~"));
        assert!(p.ends_with("\x1b[201~"));
        assert!(p.contains("a\nb"));
    }

    #[test]
    fn single_line_payload_is_written_verbatim() {
        assert_eq!(injection_payload("do the thing"), "do the thing");
    }

    #[test]
    fn output_seen_within_only_counts_output_newer_than_the_mark() {
        // Output 200ms ago, mark set 500ms ago → the output followed the mark.
        assert!(output_seen_within(Some(200), 500));
        // Output 800ms ago predates a 500ms-old mark → not an acknowledgement.
        assert!(!output_seen_within(Some(800), 500));
        // No output tracked at all → never an acknowledgement.
        assert!(!output_seen_within(None, 10_000));
    }

    // ── in-flight guard: queue, don't drop (#874 candidate 3) ──────────────

    #[test]
    fn a_turn_arriving_mid_evaluation_is_queued_not_dropped() {
        let id = 920_001;
        assert!(try_begin_evaluation(id), "first claim wins the slot");
        assert!(!try_begin_evaluation(id), "second turn can't claim mid-flight");
        assert!(
            end_evaluation_and_check_rerun(id),
            "the queued turn demands a re-run"
        );
        assert!(
            !end_evaluation_and_check_rerun(id),
            "the re-run consumed the queued turn — slot released"
        );
        assert!(try_begin_evaluation(id), "slot is claimable again");
        assert!(
            !end_evaluation_and_check_rerun(id),
            "no queued turn → no re-run"
        );
    }

    // ── watchdog decision (#874: lost turns in any state) ──────────────────

    #[test]
    fn watchdog_evaluates_a_quiet_never_evaluated_node() {
        assert!(should_watchdog_evaluate(Some(WATCHDOG_QUIET_MS), None));
    }

    #[test]
    fn watchdog_evaluates_quiet_output_newer_than_the_last_evaluation() {
        // Output 4 minutes ago, last evaluation 10 minutes ago → the yield
        // after that evaluation was never reacted to (the lost turn).
        assert!(should_watchdog_evaluate(Some(240_000), Some(600_000)));
    }

    #[test]
    fn watchdog_skips_evaluated_streaming_and_silent_nodes() {
        // Last evaluation is newer than the last output → already reacted.
        assert!(!should_watchdog_evaluate(Some(240_000), Some(10_000)));
        // Recent output → the agent may still be mid-response; never inject.
        assert!(!should_watchdog_evaluate(Some(2_000), None));
        // No output since registration → nothing to react to.
        assert!(!should_watchdog_evaluate(None, None));
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
