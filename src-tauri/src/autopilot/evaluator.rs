//! LLM State Evaluator on PTY yield (issue #483, PRD #480 / ADR-0011 §3).
//!
//! Keeps a bounded tail of each *piloted* node's PTY output and, when the
//! pipeline asks, classifies the agent's most recent turn with a cheap LLM
//! call:
//!
//! - `COMPLETED` — the agent believes the task is done (pipeline reacts by
//!   injecting the wrap-up prompt, issue #484).
//! - `BLOCKED`  — the agent needs a human (clarification, credentials).
//! - `WORKING`  — mid-task yield (permission prompt, incremental question).
//!
//! ## Buffering
//! Only nodes registered via [`register`] are buffered (the Autopilot poller
//! and the manual `/finish` trigger register; ordinary nodes pay a single
//! `HashSet` lookup per PTY chunk and nothing else). The tail is capped at
//! [`MAX_TAIL_CHARS`] — the classifier only ever needs the recent turn.
//!
//! ## Safe degradation (#483 AC)
//! Every failure path — spawn error, timeout, unparseable output — returns
//! `None` from [`classify`], which the pipeline treats as `WORKING` (do
//! nothing). A broken classifier CLI can never corrupt a node's DB status.
//!
//! The LLM call mirrors `session_naming::summarize_and_rename_with`: the
//! Claude Code CLI in `--print` mode reading the prompt from stdin, with the
//! naming-backend env resolution reused so the evaluator routes through the
//! mesh's configured Autopilot provider (a cwrap-style side-channel), never
//! silently through an expensive default (the #824 lesson).

use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Upper bound for a node's buffered PTY tail. Enough for a full turn of
/// terminal output; the classifier prompt only sends [`CLASSIFY_TAIL_CHARS`].
const MAX_TAIL_CHARS: usize = 16_000;

/// How much *cleaned* tail is handed to the classifier.
const CLASSIFY_TAIL_CHARS: usize = 6_000;

/// One word out of the classifier, or `None` = "could not tell" (degrade to
/// no action).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    Completed,
    Blocked,
    Working,
}

/// Node ids whose output we buffer (auto-spawned + manual `/finish` nodes).
static PILOTED: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Circuit-owned piloted nodes. Circuits share the evaluator's PTY blackboard
/// and freshness clocks, but they do not have a legacy `autopilot_runs` row.
/// Keeping ownership explicit prevents the legacy pipeline from treating a
/// circuit node as stale and unregistering its evaluator state on the first
/// Node Turn.
static CIRCUIT_PILOTED: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Per-node PTY tail. Entries exist only for piloted nodes.
static TAILS: Lazy<Mutex<HashMap<i64, String>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// When each piloted node last produced PTY output. The launch watcher
/// (`autopilot::launch`) reads this to detect output quiescence — "the CLI
/// has finished drawing and is sitting idle at its input box".
static LAST_OUTPUT: Lazy<Mutex<HashMap<i64, std::time::Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// When each piloted node's turn evaluation last *started*. The poller
/// watchdog (`pipeline::watchdog_pass`) compares this against
/// [`LAST_OUTPUT`] to find yields no evaluation ever reacted to — the
/// lost-turn stall of issue #874. Stamped at evaluation start (not end) so
/// output produced *during* an evaluation still counts as unevaluated.
static LAST_EVAL: Lazy<Mutex<HashMap<i64, std::time::Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Where each piloted node's current turn started (raw char offset in TAILS).
/// Used by the circuits blackboard to capture strictly per-turn terminal output
/// rather than the entire process history.
static TURN_STARTS: Lazy<Mutex<HashMap<i64, usize>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Start buffering PTY output for a node. Idempotent.
pub fn register(node_id: i64) {
    PILOTED.lock().unwrap().insert(node_id);
}

/// Start buffering PTY output for a circuit-owned node. Circuit nodes share
/// the evaluator blackboard with legacy Autopilot, but are deliberately not
/// backed by an `autopilot_runs` ledger row.
pub fn register_circuit(node_id: i64) {
    PILOTED.lock().unwrap().insert(node_id);
    CIRCUIT_PILOTED.lock().unwrap().insert(node_id);
}

/// Is this node under Autopilot management (fast, in-memory)?
pub fn is_piloted(node_id: i64) -> bool {
    PILOTED.lock().unwrap().contains(&node_id)
}

/// Whether this node is owned by an Autopilot Circuit rather than the legacy
/// issue-driven pipeline.
pub fn is_circuit_piloted(node_id: i64) -> bool {
    CIRCUIT_PILOTED.lock().unwrap().contains(&node_id)
}

/// Stop buffering and drop all state for a node (close / pipeline terminal
/// state).
pub fn unregister(node_id: i64) {
    PILOTED.lock().unwrap().remove(&node_id);
    CIRCUIT_PILOTED.lock().unwrap().remove(&node_id);
    TAILS.lock().unwrap().remove(&node_id);
    LAST_OUTPUT.lock().unwrap().remove(&node_id);
    LAST_EVAL.lock().unwrap().remove(&node_id);
    TURN_STARTS.lock().unwrap().remove(&node_id);
}

/// Record the start of a fresh turn for this node (at the current tail position).
pub fn note_turn_start(node_id: i64) {
    let raw_len = TAILS.lock().unwrap().get(&node_id).map(|s| s.len()).unwrap_or(0);
    TURN_STARTS.lock().unwrap().insert(node_id, raw_len);
}

/// Milliseconds since the node last produced PTY output, or `None` if it has
/// produced none since registration (or isn't piloted).
pub fn millis_since_last_output(node_id: i64) -> Option<u128> {
    LAST_OUTPUT
        .lock()
        .unwrap()
        .get(&node_id)
        .map(|t| t.elapsed().as_millis())
}

/// Record that a turn evaluation for this node is starting now.
pub fn note_evaluation(node_id: i64) {
    LAST_EVAL
        .lock()
        .unwrap()
        .insert(node_id, std::time::Instant::now());
}

/// Milliseconds since the node's last turn evaluation started, or `None` if
/// none has run since registration.
pub fn millis_since_last_evaluation(node_id: i64) -> Option<u128> {
    LAST_EVAL
        .lock()
        .unwrap()
        .get(&node_id)
        .map(|t| t.elapsed().as_millis())
}

/// PTY reader hook — called for every output chunk (see `agent::spawn`'s
/// reader thread, next to `session_naming::on_output`). Non-piloted nodes
/// return after one set lookup.
pub fn on_output(node_id: i64, data: &str) {
    if !is_piloted(node_id) {
        return;
    }
    LAST_OUTPUT
        .lock()
        .unwrap()
        .insert(node_id, std::time::Instant::now());
    let mut tails = TAILS.lock().unwrap();
    let tail = tails.entry(node_id).or_default();
    tail.push_str(data);
    if tail.len() > MAX_TAIL_CHARS {
        let mut drain_to = tail.len() - MAX_TAIL_CHARS;
        while !tail.is_char_boundary(drain_to) {
            drain_to += 1;
        }
        tail.drain(..drain_to);
    }
    drop(tails);
    // Reactive gate evaluation (#1207): a circuit LlmTurnClassifier
    // waiting on this agent's turn yield must not sit out the 2s tick.
    // Cheap notify; redundant classifications are prevented by the
    // fresh-output guards in the circuit worker's observation pass.
    crate::services::circuit_worker::wake_circuit_worker();
}

/// The current cleaned (ANSI-stripped, tail-capped) buffer for a node.
pub fn cleaned_tail(node_id: i64) -> String {
    let raw = TAILS
        .lock()
        .unwrap()
        .get(&node_id)
        .cloned()
        .unwrap_or_default();
    let cleaned = crate::session_naming::ANSI_ESCAPE
        .replace_all(&raw, "")
        .to_string();
    if cleaned.len() > CLASSIFY_TAIL_CHARS {
        let mut start = cleaned.len() - CLASSIFY_TAIL_CHARS;
        while !cleaned.is_char_boundary(start) {
            start += 1;
        }
        cleaned[start..].to_string()
    } else {
        cleaned
    }
}

/// The cleaned output produced during the current turn (since [`note_turn_start`]).
/// If no turn start was recorded, returns the tail.
pub fn cleaned_turn_tail(node_id: i64) -> String {
    let raw = TAILS
        .lock()
        .unwrap()
        .get(&node_id)
        .cloned()
        .unwrap_or_default();
    let start_offset = TURN_STARTS
        .lock()
        .unwrap()
        .get(&node_id)
        .copied()
        .unwrap_or(0);
    let turn_raw = if start_offset < raw.len() {
        let mut slice_start = start_offset;
        while !raw.is_char_boundary(slice_start) && slice_start < raw.len() {
            slice_start += 1;
        }
        &raw[slice_start..]
    } else {
        &raw
    };
    let cleaned = crate::session_naming::ANSI_ESCAPE
        .replace_all(turn_raw, "")
        .to_string();
    if cleaned.len() > CLASSIFY_TAIL_CHARS {
        let mut start = cleaned.len() - CLASSIFY_TAIL_CHARS;
        while !cleaned.is_char_boundary(start) {
            start += 1;
        }
        cleaned[start..].to_string()
    } else {
        cleaned
    }
}

/// Parse the classifier's raw stdout into a [`Classification`].
///
/// Contract: the prompt asks for EXACTLY one word, but LLMs decorate — so we
/// scan the output's non-empty lines from the end and accept the first line
/// that contains exactly one of the three tokens (word-ish match, case
/// insensitive). Ambiguous or token-free output is `None` (degrade, never
/// guess). Pure function, unit-tested against mock transcripts below.
pub(crate) fn parse_classification(output: &str) -> Option<Classification> {
    for line in output.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_uppercase();
        let hits: Vec<Classification> = [
            ("COMPLETED", Classification::Completed),
            ("BLOCKED", Classification::Blocked),
            ("WORKING", Classification::Working),
        ]
        .iter()
        .filter(|(tok, _)| upper.contains(tok))
        .map(|(_, c)| *c)
        .collect();
        if hits.len() == 1 {
            return Some(hits[0]);
        }
        // A line mentioning several tokens (e.g. the model echoing the
        // instruction back) is ambiguous — keep scanning upward.
    }
    None
}

/// Build the classifier prompt for a cleaned PTY tail.
pub(crate) fn classify_prompt(tail: &str) -> String {
    format!(
        "The text below is the most recent terminal output of an AI coding agent \
         that has just yielded control back to the user. Classify the agent's state.\n\
         Answer with EXACTLY one word on a single line, nothing else:\n\
         COMPLETED - the agent states the implementation task is finished\n\
         BLOCKED - the agent is stuck and needs a human (a question only a person \
         can answer, missing credentials/API keys, or repeated failures it asked for help with)\n\
         WORKING - anything else (mid-task progress, a routine tool/permission \
         prompt, partial results)\n\n\
         Terminal output:\n{}",
        tail
    )
}

/// Run the LLM classification for a node's current tail. Blocking (spawns a
/// child process and waits up to 30s) — call from a worker thread, never
/// from the tokio pool. `backend_env` comes from
/// `session_naming::naming_backend_env(provider)` so the evaluator uses the
/// mesh's configured Autopilot provider side-channel.
///
/// Returns `None` on every failure path (spawn, timeout, exit status,
/// unparseable) — the caller treats that as `WORKING`.
pub fn classify(node_id: i64, backend_env: &[(String, String)]) -> Option<Classification> {
    let tail = cleaned_tail(node_id);
    if tail.trim().len() < 40 {
        // Nothing meaningful to classify yet (e.g. the first turn right
        // after spawn) — don't burn a call on it.
        return None;
    }
    let prompt = classify_prompt(&tail);

    classify_with_prompt(node_id, backend_env, &prompt)
}

/// Review completion alone is not approval. Unclear reports and backend
/// failures need attention rather than silently authorizing more work.
pub fn classify_review(node_id: i64, backend_env: &[(String, String)]) -> Option<Classification> {
    let prompt = format!(
        "Assess the final review report in this agent's terminal output. Ignore echoed prompts and tool progress. \
         Answer exactly one word: COMPLETED only if the reviewer explicitly approves the work with no remaining findings; \
         WORKING if the reviewer requests changes or reports unresolved actionable findings; \
         BLOCKED if the review is incomplete, ambiguous, or cannot be performed.\n\n{}",
        cleaned_turn_tail(node_id)
    );
    classify_with_prompt(node_id, backend_env, &prompt)
}

pub(crate) fn classify_with_prompt(node_id: i64, backend_env: &[(String, String)], prompt: &str) -> Option<Classification> {

    let mut cmd = crate::process_util::command_no_window("claude");
    cmd.arg("--print");
    for k in crate::agent::provider::CLAUDE_BACKEND_ENV_VARS {
        cmd.env_remove(k);
    }
    for (k, v) in backend_env {
        cmd.env(k, v);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("autopilot evaluator({}): failed to spawn CLI: {}", node_id, e);
            return None;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if stdin.write_all(prompt.as_bytes()).is_err() {
            tracing::warn!("autopilot evaluator({}): stdin write failed", node_id);
        }
        // Drop closes the pipe so `--print` sees EOF.
    }

    // Bounded wait: poll the child for up to 30s, then kill. (std::process
    // has no wait_timeout; a 250ms poll on a dedicated worker thread is
    // fine — this mirrors gh688's leak lesson: never leave the child
    // running past our patience.)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    tracing::warn!(
                        "autopilot evaluator({}): classifier exited with {}",
                        node_id,
                        status
                    );
                    return None;
                }
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    tracing::warn!("autopilot evaluator({}): classifier timed out", node_id);
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) => {
                tracing::warn!("autopilot evaluator({}): wait failed: {}", node_id, e);
                let _ = child.kill();
                return None;
            }
        }
    }

    let output = {
        use std::io::Read;
        let mut s = String::new();
        match child.stdout.take() {
            Some(mut out) => {
                if out.read_to_string(&mut s).is_err() {
                    return None;
                }
                s
            }
            None => return None,
        }
    };
    let parsed = parse_classification(&output);
    tracing::info!(
        "autopilot evaluator({}): classified turn as {:?} (raw: {:?})",
        node_id,
        parsed,
        output.trim().chars().take(80).collect::<String>()
    );
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_classification against mock classifier outputs ───────────────

    #[test]
    fn parses_bare_single_word_answers() {
        assert_eq!(parse_classification("COMPLETED"), Some(Classification::Completed));
        assert_eq!(parse_classification("BLOCKED\n"), Some(Classification::Blocked));
        assert_eq!(parse_classification("  working  "), Some(Classification::Working));
    }

    #[test]
    fn parses_decorated_answer_on_last_line() {
        // Models often prepend reasoning despite instructions — the last
        // token-bearing line wins.
        let out = "Let me look at the transcript.\n\nThe agent finished.\nCOMPLETED\n";
        assert_eq!(parse_classification(out), Some(Classification::Completed));
    }

    #[test]
    fn ambiguous_line_is_skipped_in_favor_of_an_unambiguous_one() {
        // A line echoing the whole instruction (all three tokens) must not
        // be treated as an answer; the real answer above it wins.
        let out = "BLOCKED\nOptions were: COMPLETED, BLOCKED or WORKING.";
        assert_eq!(parse_classification(out), Some(Classification::Blocked));
    }

    #[test]
    fn tokenless_or_empty_output_degrades_to_none() {
        assert_eq!(parse_classification(""), None);
        assert_eq!(parse_classification("I am not sure what state this is."), None);
    }

    // ── buffering ───────────────────────────────────────────────────────────

    #[test]
    fn on_output_ignores_unregistered_nodes_and_buffers_registered_ones() {
        let id = 910_001;
        on_output(id, "dropped");
        assert_eq!(cleaned_tail(id), "", "unregistered node buffers nothing");

        register(id);
        on_output(id, "hello ");
        on_output(id, "\x1b[31mworld\x1b[0m");
        assert_eq!(cleaned_tail(id), "hello world", "ANSI codes are stripped");

        unregister(id);
        assert_eq!(cleaned_tail(id), "", "unregister drops the tail");
    }

    #[test]
    fn note_turn_start_isolates_current_turn_output() {
        let id = 910_004;
        register(id);
        on_output(id, "turn 1 boot output\n");
        assert_eq!(cleaned_tail(id), "turn 1 boot output\n");

        note_turn_start(id);
        on_output(id, "turn 2 work: fixed issue #1357");
        assert_eq!(
            cleaned_turn_tail(id),
            "turn 2 work: fixed issue #1357",
            "cleaned_turn_tail excludes prior turn output"
        );
        assert!(cleaned_tail(id).contains("turn 1 boot output"));

        unregister(id);
    }

    #[test]
    fn tail_is_capped_at_max_chars() {
        let id = 910_002;
        register(id);
        let chunk = "x".repeat(10_000);
        on_output(id, &chunk);
        on_output(id, &chunk);
        on_output(id, &chunk);
        let tails = TAILS.lock().unwrap();
        assert!(tails.get(&id).unwrap().len() <= MAX_TAIL_CHARS);
        drop(tails);
        unregister(id);
    }

    #[test]
    fn note_evaluation_is_tracked_and_unregister_clears_it() {
        let id = 910_003;
        register(id);
        assert_eq!(
            millis_since_last_evaluation(id),
            None,
            "no evaluation recorded yet"
        );
        note_evaluation(id);
        assert!(
            millis_since_last_evaluation(id).is_some(),
            "evaluation timestamp recorded"
        );
        unregister(id);
        assert_eq!(
            millis_since_last_evaluation(id),
            None,
            "unregister drops the evaluation timestamp"
        );
    }

    #[test]
    fn circuit_registration_is_visible_to_the_blackboard_but_not_legacy_pipeline() {
        let id = 910_005;
        register_circuit(id);
        assert!(is_piloted(id));
        assert!(is_circuit_piloted(id));
        on_output(id, "circuit output");
        assert_eq!(cleaned_tail(id), "circuit output");

        unregister(id);
        assert!(!is_piloted(id));
        assert!(!is_circuit_piloted(id));
        assert_eq!(cleaned_tail(id), "");
    }

    #[test]
    fn classify_prompt_embeds_the_tail_and_the_three_tokens() {
        let p = classify_prompt("cargo test ... 42 passed");
        assert!(p.contains("COMPLETED"));
        assert!(p.contains("BLOCKED"));
        assert!(p.contains("WORKING"));
        assert!(p.contains("42 passed"));
    }
}
