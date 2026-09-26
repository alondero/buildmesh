//! Native hook receipt. The attention route owns transport/session validation;
//! this adapter preserves only lifecycle/ownership fields and the final report.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct NativeHook {
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub event: String,
    pub child_id: Option<String>,
    pub active_work: Option<BTreeSet<String>>,
    pub final_report: Option<String>,
    /// Antigravity `Stop` with `fullyIdle: false`: the turn yielded while
    /// harness-owned background work is still in flight (issue #1901).
    /// Never set by the Claude/Codex parsers below.
    #[serde(default)]
    pub background_busy: bool,
    /// Antigravity `executionNum`: an opaque 0-based step counter, not a
    /// turn identity token. Retained so consecutive settled turns from one
    /// session hash to distinct receipt source ids instead of colliding
    /// into the history deduplicator (issue #1901 review). Never set by
    /// the Claude/Codex parsers below.
    #[serde(default)]
    pub execution_num: Option<i64>,
    /// Antigravity `terminationReason` (e.g. `model_stop`,
    /// `NO_TOOL_CALL`): opaque triage telemetry, never a decision input.
    /// Never set by the Claude/Codex parsers below.
    #[serde(default)]
    pub termination_reason: Option<String>,
    #[serde(default)]
    pub human_fact: Option<crate::autopilot::circuit::observation::ObservedWorkFact>,
    #[serde(default)]
    pub provider: Option<String>,
}

impl NativeHook {
    pub(crate) fn parse(provider: &str, body: &[u8]) -> Option<Self> {
        if provider == "agy" {
            return parse_agy(body);
        }
        if !matches!(provider, "claude" | "claude_code" | "anthropic" | "codex") {
            return None;
        }
        let value: serde_json::Value = serde_json::from_slice(body).ok()?;
        let event = value.get("hook_event_name").or_else(|| value.get("hookEventName")).or_else(|| value.get("hookName"))?.as_str()?;
        let human_fact = human_fact(&value);
        // Codex's PermissionRequest contract has no stable request id. Keep
        // the event as an uncorrelated permission wait instead of dropping
        // it or inventing a correlation token.
        let uncorrelated_codex_permission = provider == "codex"
            && event == "PermissionRequest"
            && human_fact.is_none();
        if provider == "codex" && human_fact.is_none() && !uncorrelated_codex_permission { return None; }
        if human_fact.is_none() && !matches!(
            event,
            "UserPromptSubmit"
                | "Stop"
                | "SubagentStart"
                | "SubagentStop"
                | "PermissionRequest"
                | "StopFailure"
        ) {
            return None;
        }
        let token = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let active_work = (|| {
            let tasks = value.get("background_tasks")?.as_array()?;
            let crons = value.get("session_crons")?.as_array()?;
            let mut active = BTreeSet::new();
            for (prefix, items) in [("task", tasks), ("cron", crons)] {
                for item in items {
                    let id = item.get("id")?.as_str()?.trim();
                    if id.is_empty() {
                        return None;
                    }
                    active.insert(format!("{prefix}:{id}"));
                }
            }
            Some(active)
        })();
        Some(Self {
            session_id: token("session_id").or_else(|| token("sessionId")).or_else(|| token("sessionID")).or_else(|| token("conversationId")).or_else(|| token("conversation_id")).or_else(|| token("taskId")),
            turn_id: token("prompt_id").or_else(|| token("promptId")).or_else(|| token("turn_id")),
            event: event.into(),
            child_id: token("agent_id"),
            human_fact,
            background_busy: false,
            execution_num: None,
            termination_reason: None,
            provider: Some(provider.into()),
            active_work,
            final_report: if event == "Stop" {
                token("last_assistant_message")
                    .map(|text| crate::secret_scrubber::SecretScrubber::scrub(&text))
            } else {
                None
            },
        })
    }
}

/// Antigravity (`agy`) `Stop` receipts (issue #1901).
///
/// Validated against agy 1.2.11 on Windows interactive sessions (`-p`
/// print mode emits no `Stop` hook, so hook evidence covers only
/// Buildmesh-launched PTY sessions): the provisioned `.agents/hooks.json`
/// `Stop` hook pipes `{conversationId, executionNum, fullyIdle,
/// terminationReason, error, workspacePaths, transcriptPath,
/// artifactDirectoryPath, modelName}` (1.2.x also sends
/// `hookEventName: "Stop"`) to the attention route. What the shape
/// provably lacks decides the adapter:
/// - No per-turn token (`executionNum` is an opaque 0-based step counter,
///   not identity) → `turn_id` stays `None`; receipts are session-fenced
///   but never turn-fenced, hence never authoritative. The counter is still
///   retained as `execution_num` so consecutive turns hash to distinct
///   receipt source ids instead of colliding in history deduplication.
/// - No child/background registry → `active_work` stays `None`; every
///   `Stop` carries `OwnershipUnavailable` so a settled foreground turn
///   can never verify owned work.
/// - No inline report text → `final_report` stays `None`.
/// - Subagent `Stop`s carry their own `conversationId`, so they fence as
///   a different session downstream and can never complete the parent.
///
/// Only `Stop` is accepted; under `--dangerously-skip-permissions` AGY
/// installs no `PreToolUse` gate, so no other event is validated.
fn parse_agy(body: &[u8]) -> Option<NativeHook> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let named = value
        .get("hook_event_name")
        .or_else(|| value.get("hookEventName"))
        .or_else(|| value.get("hookName"))
        .and_then(|name| name.as_str());
    // Canonicalize through the same UUID gate the attention route uses
    // before writing `cli_session_id`: downstream identity comparison is
    // exact, so an uppercase or mixed-case `conversationId` must fence as
    // the same session (issue #1901 review). A non-UUID value is malformed.
    let conversation: Option<String> = value
        .get("conversationId")
        .or_else(|| value.get("conversation_id"))
        .and_then(|id| id.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .and_then(|id| crate::http::request::parse_session_id_for_provider("agy", id));
    // AGY always ships its conversation id on `Stop`; a `Stop` without one
    // is malformed, never turn evidence for whichever node it reached.
    let conversation = conversation?;
    let event = match named {
        Some(name) if name.eq_ignore_ascii_case("stop") => "Stop",
        Some(_) => return None,
        None => {
            // Shape-keyed `Stop`: pre-`hookEventName` payloads carry no
            // event key, so require the AGY `Stop` markers alongside the
            // conversation id rather than treating any JSON as a turn end.
            let marked = [
                "fullyIdle",
                "fully_idle",
                "terminationReason",
                "termination_reason",
                "transcriptPath",
                "transcript_path",
                "executionNum",
                "execution_num",
            ]
            .iter()
            .any(|key| value.get(key).is_some());
            if marked {
                "Stop"
            } else {
                return None;
            }
        }
    };
    let background_busy = value
        .get("fullyIdle")
        .or_else(|| value.get("fully_idle"))
        .and_then(|idle| idle.as_bool())
        == Some(false);
    let execution_num = value
        .get("executionNum")
        .or_else(|| value.get("execution_num"))
        .and_then(|num| num.as_i64());
    let termination_reason = value
        .get("terminationReason")
        .or_else(|| value.get("termination_reason"))
        .and_then(|reason| reason.as_str())
        .filter(|reason| !reason.trim().is_empty())
        .map(str::to_owned);
    Some(NativeHook {
        session_id: Some(conversation),
        turn_id: None,
        event: event.into(),
        child_id: None,
        active_work: None,
        final_report: None,
        human_fact: None,
        background_busy,
        execution_num,
        termination_reason,
        provider: Some("agy".into()),
    })
}

/// Stable deduplication identity for a hook receipt: the hook payload plus
/// the session incarnation it arrived under. Callers must produce the id
/// through this helper so persistence, replay, and tests agree on what
/// counts as the same delivery (issue #1901 review).
pub(crate) fn receipt_source_id(
    hook: &NativeHook,
    session_incarnation: &Option<String>,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(hook, session_incarnation)).map_err(|e| e.to_string())?
        )
    ))
}

// This is the Claude/Codex hook request contract already accepted by the
// attention route. Tool names identify request kind, never request identity.
fn human_fact(value: &serde_json::Value) -> Option<crate::autopilot::circuit::observation::ObservedWorkFact> {
    use crate::autopilot::circuit::observation::{HumanWaitKind as Kind, ObservedWorkFact as Fact};
    let event = value.get("hook_event_name").or_else(|| value.get("hookEventName")).or_else(|| value.get("hookName"))?.as_str()?;
    let request_id = ["request_id", "tool_use_id", "toolUseId", "requestId", "requestID", "elicitation_id", "toolCallId", "tool_call_id", "callId", "call_id", "permissionID", "permission_id"]
        .iter().find_map(|key| value.get(key).and_then(|v| v.as_str()).filter(|v| !v.trim().is_empty()))?;
    let tool = value.get("tool_name").or_else(|| value.get("toolName")).and_then(|v| v.as_str());
    let wait_kind = match tool {
        Some("AskUserQuestion" | "request_user_input" | "ask_user_question") => Kind::Question,
        Some("ExitPlanMode") => Kind::ReviewApproval,
        _ => Kind::Permission,
    };
    match event {
        "PermissionRequest" => Some(Fact::HumanWaitRequested { wait_kind: Kind::Permission, request_id: request_id.into() }),
        "PreToolUse" if matches!(tool, Some("AskUserQuestion" | "request_user_input" | "ask_user_question" | "ExitPlanMode")) =>
            Some(Fact::HumanWaitRequested { wait_kind, request_id: request_id.into() }),
        "PostToolUse" => Some(Fact::ToolResponse { wait_kind, request_id: request_id.into() }),
        "PostToolUseFailure" => Some(Fact::ToolFailed { wait_kind, request_id: request_id.into() }),
        "PermissionResult" => Some(Fact::HumanResponse { wait_kind: Kind::Permission, request_id: request_id.into() }),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NativeReceipt {
    pub agent_node_id: i64,
    pub input_stamp: Option<String>,
    pub session_incarnation: Option<String>,
    pub source_id: String,
    pub received_at_ms: i64,
    pub turn_fenced: bool,
    #[serde(default)]
    pub submission_correlated: bool,
    pub hook: NativeHook,
}

pub(crate) fn receive(
    agent_node_id: i64,
    hook: NativeHook,
    turn_fenced: bool,
) -> Result<(), String> {
    let input_stamp = crate::agent::process::PROCESS_REGISTRY.input_stamp(agent_node_id);
    let session_incarnation = crate::db::agent_turn_stamp(agent_node_id)
        .map_err(|e| e.to_string())?
        .and_then(|stamp| {
            stamp
                .split_once(':')
                .map(|(generation, _)| generation.to_owned())
        });
    let source_id = receipt_source_id(&hook, &session_incarnation)?;
    let receipt = NativeReceipt {
        agent_node_id,
        input_stamp,
        session_incarnation,
        source_id,
        received_at_ms: chrono::Utc::now().timestamp_millis(),
        turn_fenced,
        // Native turn IDs do not acknowledge a Buildmesh submission. Arrival
        // time cannot establish that association after delayed hook delivery.
        submission_correlated: false,
        hook,
    };
    crate::db::circuit::evidence::receive_native_hook(&receipt)?;
    super::wake_circuit_worker();
    Ok(())
}

pub(super) fn pending(view: &super::RunView) -> Result<Vec<super::CircuitEvent>, String> {
    let after = view
        .context
        .get("observer.receipt_cursor")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut events = Vec::new();
    for entry in crate::db::circuit::evidence::native_hook_receipts(view.run_id, after)
        .map_err(|e| e.to_string())?
    {
        events.push(resolve_receipt(view.run_id, entry, |agent_node_id| {
            let node = crate::db::get_agent_node_by_id(agent_node_id)?;
            let input = crate::agent::process::PROCESS_REGISTRY.input_stamp(node.id);
            let incarnation = crate::db::agent_turn_stamp(node.id)?.and_then(|stamp| {
                stamp
                    .split_once(':')
                    .map(|(generation, _)| generation.to_owned())
            });
            Ok((node.cli_session_id, incarnation, input))
        })?);
    }
    Ok(events)
}

fn resolve_receipt(
    run_id: i64,
    entry: crate::db::circuit::evidence::CircuitHistoryEntry,
    lookup: impl FnOnce(i64) -> rusqlite::Result<(Option<String>, Option<String>, Option<String>)>,
) -> Result<super::CircuitEvent, String> {
    let receipt: NativeReceipt = match serde_json::from_str(&entry.detail) {
        Ok(receipt) => receipt,
        Err(_) => {
            return Ok(rejected_receipt(
                run_id,
                entry,
                0,
                "malformed_native_receipt",
            ))
        }
    };
    match lookup(receipt.agent_node_id) {
        Ok((session, incarnation, input)) => Ok(normalize(
            run_id,
            entry,
            receipt,
            session,
            incarnation,
            input,
        )),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(rejected_receipt(
            run_id,
            entry,
            receipt.agent_node_id,
            "native_receipt_agent_deleted",
        )),
        // A transient database failure must leave the receipt available for retry.
        Err(error) => Err(error.to_string()),
    }
}

fn rejected_receipt(
    run_id: i64,
    entry: crate::db::circuit::evidence::CircuitHistoryEntry,
    agent_node_id: i64,
    source: &str,
) -> super::CircuitEvent {
    use crate::autopilot::circuit::observation::{
        CircuitObservation, ObservationIdentity, ObservedWorkFact,
    };
    let expected = ObservationIdentity {
        run_id,
        step_id: entry.node_id.unwrap_or_default(),
        attempt: entry.attempt.unwrap_or_default(),
        agent_node_id,
        session_incarnation: None,
        session_id: None,
        turn_id: None,
        report_revision: None,
    };
    super::CircuitEvent::ObservationBatch {
        receipt_id: entry.id,
        observations: vec![CircuitObservation {
            identity: expected.clone(),
            source: source.into(),
            source_id: Some(entry.id.to_string()),
            observed_at_ms: 0,
            authoritative: false,
            fact: ObservedWorkFact::Unavailable,
        }],
        expected,
        stale: true,
        input_guard: None,
    }
}

fn normalize(
    run_id: i64,
    entry: crate::db::circuit::evidence::CircuitHistoryEntry,
    receipt: NativeReceipt,
    current_session: Option<String>,
    current_incarnation: Option<String>,
    current_input: Option<String>,
) -> super::CircuitEvent {
    use crate::autopilot::circuit::observation::{
        CircuitObservation, ObservationIdentity, ObservedWorkFact as Fact,
    };
    use sha2::{Digest, Sha256};
    let child_terminal = receipt.hook.event == "SubagentStop" && receipt.hook.child_id.is_some();
    let stale = !child_terminal
        && current_input
            .as_ref()
            .zip(receipt.input_stamp.as_ref())
            .is_some_and(|(a, b)| a != b);
    let identity = ObservationIdentity {
        run_id,
        step_id: entry.node_id.unwrap_or_default(),
        attempt: entry.attempt.unwrap_or_default(),
        agent_node_id: receipt.agent_node_id,
        session_incarnation: receipt.session_incarnation,
        session_id: receipt.hook.session_id.clone(),
        turn_id: receipt.hook.turn_id.clone(),
        report_revision: None,
    };
    let mut expected = identity.clone();
    expected.session_id = current_session;
    expected.session_incarnation = current_incarnation;
    let human_fact = receipt.hook.human_fact.clone();
    let authoritative = ((receipt.turn_fenced && (receipt.submission_correlated || human_fact.is_some())) || child_terminal)
        && current_input.is_some()
        && (receipt.input_stamp.is_some() || human_fact.is_some())
        && expected.session_id.is_some()
        && expected.session_incarnation.is_some()
        && !stale;
    let mut facts = Vec::new();
    if let Some(text) = receipt.hook.final_report {
        let revision = format!("{:x}", Sha256::digest(text.as_bytes()));
        facts.push(Fact::AssistantReport { text, revision });
    }
    if let Some(fact) = human_fact { facts.push(fact); }
    match receipt.hook.event.as_str() {
        "UserPromptSubmit" => facts.push(Fact::Working),
        // `background_busy` is only ever set by the AGY parser above; the
        // harness signalled the turn yielded while owned background work is
        // still in flight, so this is yield evidence, not a settled turn.
        "Stop" => facts.push(if receipt.hook.background_busy {
            Fact::Yielded
        } else {
            Fact::ForegroundTerminated
        }),
        "PermissionRequest" if receipt.hook.human_fact.is_none() => facts.push(Fact::PermissionRequested),
        "StopFailure" => facts.push(Fact::Unavailable),
        "SubagentStart" => {
            if let Some(id) = receipt.hook.child_id {
                facts.push(Fact::OwnedStarted {
                    work_id: format!("task:{id}"),
                });
            }
        }
        "SubagentStop" => {
            if let Some(id) = receipt.hook.child_id {
                facts.push(Fact::OwnedTerminated {
                    work_id: format!("task:{id}"),
                });
            }
        }
        _ => {}
    }
    // Child termination is scoped to the owned child, not to the current
    // foreground turn. Its parent registry can describe an older turn.
    if receipt.hook.event == "Stop" {
        if let Some(active_work) = receipt.hook.active_work {
            facts.push(Fact::OwnershipSnapshot {
                active_work: active_work.into_iter().collect(),
            });
        } else if receipt.hook.provider.as_deref() == Some("agy") {
            // Validated absence (issue #1901): Antigravity exposes no
            // child/background registry, so a settled foreground turn must
            // park the step Unverified instead of implying no owned work.
            facts.push(Fact::OwnershipUnavailable {
                reason: "Antigravity exposes no child/background registry; a settled foreground turn cannot verify owned work. Inspect the agent and Recheck evidence.".into(),
            });
        } else {
            facts.push(Fact::Unavailable);
        }
    }
    let observations = facts
        .into_iter()
        .enumerate()
        .map(|(index, fact)| CircuitObservation {
            identity: identity.clone(),
            source: if receipt.hook.human_fact.is_some() {
                format!("{}_request_hook", receipt.hook.provider.as_deref().unwrap_or("claude"))
            } else if receipt.hook.provider.as_deref() == Some("codex") {
                "codex_native_hook".into()
            } else if receipt.hook.provider.as_deref() == Some("agy") {
                "agy_native_hook".into()
            } else {
                "claude_native_hook".into()
            },
            source_id: Some(format!("{}:{index}", receipt.source_id)),
            observed_at_ms: receipt.received_at_ms,
            authoritative,
            fact,
        })
        .collect();
    super::CircuitEvent::ObservationBatch {
        input_guard: authoritative.then(|| {
            crate::autopilot::circuit::stepper::ObservationInputFence {
                transcript_guard: None, report_guard: None,
                agent_node_id: identity.agent_node_id,
                input_stamp: current_input.expect("authoritative input stamp"),
                observed_at_ms: if child_terminal {
                    chrono::Utc::now().timestamp_millis()
                } else {
                    receipt.received_at_ms
                },
                session_id: expected.session_id.clone().expect("authoritative session"),
                session_incarnation: expected
                    .session_incarnation
                    .clone()
                    .expect("authoritative incarnation"),
            }
        }),
        receipt_id: entry.id,
        expected,
        observations,
        stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_native_human_requests_use_exact_ids_and_never_infer_responses_from_activity() {
        use crate::autopilot::circuit::observation::{HumanWaitKind as Kind, ObservedWorkFact as Fact};
        let request = br#"{"hook_event_name":"PermissionRequest","session_id":"session","turn_id":"turn","tool_use_id":"tool-1","tool_name":"Bash"}"#;
        let reply = br#"{"hook_event_name":"PostToolUse","session_id":"session","turn_id":"turn","tool_use_id":"tool-1","tool_name":"Bash"}"#;
        for provider in ["claude", "codex"] {
            assert_eq!(NativeHook::parse(provider, request).unwrap().human_fact,
                Some(Fact::HumanWaitRequested { wait_kind: Kind::Permission, request_id: "tool-1".into() }));
            assert_eq!(NativeHook::parse(provider, reply).unwrap().human_fact,
                Some(Fact::ToolResponse { wait_kind: Kind::Permission, request_id: "tool-1".into() }));
            for event in ["Stop", "UserPromptSubmit"] {
                let payload = serde_json::json!({"hook_event_name":event,"tool_use_id":"tool-1"});
                assert!(NativeHook::parse(provider, &serde_json::to_vec(&payload).unwrap()).is_none_or(|hook| hook.human_fact.is_none()));
            }
        }
        assert!(NativeHook::parse("opencode", request).is_none(), "no other harness parser fallback");
        assert!(NativeHook::parse("codex", br#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#).is_none(), "tool name alone is not a request identity");
        let question = NativeHook::parse("codex", br#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_use_id":"q-1"}"#).unwrap();
        assert!(matches!(question.human_fact, Some(Fact::HumanWaitRequested { wait_kind: Kind::Question, .. })));
    }

    #[test]
    fn codex_permission_without_request_id_is_retained_as_unresolved_permission() {
        use crate::autopilot::circuit::observation::{
            CircuitObservation, HumanWaitKind, ObservationDisposition, ObservedWorkFact as Fact,
            WorkEvidence,
        };

        // Codex PermissionRequest's documented payload has no tool_use_id.
        let hook = NativeHook::parse(
            "codex",
            br#"{"hook_event_name":"PermissionRequest","session_id":"session","turn_id":"turn","tool_name":"Bash","tool_input":{"command":"git status"}}"#,
        )
        .expect("retain the no-ID permission event");
        assert!(hook.human_fact.is_none(), "do not fabricate a request ID");
        let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("input-1".into()),
            session_incarnation: Some("1".into()),
            source_id: "permission-without-id".into(),
            received_at_ms: 10,
            turn_fenced: true,
            submission_correlated: false,
            hook,
        };
        let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 1,
            node_id: Some("work".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: String::new(),
            observed_at: String::new(),
        };
        let super::super::CircuitEvent::ObservationBatch { expected, observations, stale, .. } = normalize(
            42, entry, receipt, Some("session".into()), Some("1".into()), Some("input-1".into()),
        ) else { panic!("native batch") };
        assert!(!stale);
        assert_eq!(observations.len(), 1);
        assert!(!observations[0].authoritative, "missing submission correlation cannot prove a current turn fact");
        assert_eq!(observations[0].source, "codex_native_hook");
        assert_eq!(observations[0].fact, Fact::PermissionRequested);

        let mut evidence = WorkEvidence::default();
        assert_eq!(evidence.observe(&expected, &observations[0]), ObservationDisposition::ReducedConfidence);
        assert_eq!(evidence.human_waits.len(), 1);
        assert_eq!(evidence.human_waits[0].wait_kind, HumanWaitKind::Permission);
        assert!(evidence.human_waits[0].request_id.is_none());
        assert!(evidence.has_human_wait());
        assert!(!evidence.completion_verified());

        // An unrelated identified tool result and an aggregate status update
        // cannot answer this uncorrelated wait.
        let reply = NativeHook::parse(
            "codex",
            br#"{"hook_event_name":"PostToolUse","session_id":"session","turn_id":"turn","tool_use_id":"different-tool","tool_name":"Bash"}"#,
        ).unwrap();
        let reply_receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("input-1".into()),
            session_incarnation: Some("1".into()),
            source_id: "unrelated-tool-result".into(),
            received_at_ms: 11,
            turn_fenced: true,
            submission_correlated: false,
            hook: reply,
        };
        let reply_entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 2, node_id: Some("work".into()), attempt: Some(1), kind: "native_hook_received".into(),
            detail: String::new(), observed_at: String::new(),
        };
        let super::super::CircuitEvent::ObservationBatch { observations: reply_observations, .. } = normalize(
            42, reply_entry, reply_receipt, Some("session".into()), Some("1".into()), Some("input-1".into()),
        ) else { panic!("native batch") };
        assert_eq!(evidence.observe(&expected, &reply_observations[0]), ObservationDisposition::Rejected);

        let mut stale_identity = expected.clone();
        stale_identity.session_id = Some("different-session".into());
        let stale_response = CircuitObservation {
            identity: stale_identity, source: "codex_request_hook".into(), source_id: Some("stale-response".into()),
            observed_at_ms: 12, authoritative: true,
            fact: Fact::ToolResponse { wait_kind: HumanWaitKind::Permission, request_id: "different-tool".into() },
        };
        assert_eq!(evidence.observe(&expected, &stale_response), ObservationDisposition::Rejected);
        evidence = serde_json::from_str(&serde_json::to_string(&evidence).unwrap()).unwrap();
        assert_eq!(evidence.human_waits.len(), 1, "restart preserves the typed wait without inventing identity");

        let projection = CircuitObservation {
            identity: expected.clone(), source: "agent_status_projection".into(), source_id: Some("awaiting".into()),
            observed_at_ms: 12, authoritative: false, fact: Fact::NeedsInput,
        };
        evidence.observe(&expected, &projection);
        let ready_projection = CircuitObservation {
            identity: expected.clone(), source: "agent_status_projection".into(), source_id: Some("ready".into()),
            observed_at_ms: 13, authoritative: false, fact: Fact::Yielded,
        };
        evidence.observe(&expected, &ready_projection);
        let working_projection = CircuitObservation {
            identity: expected.clone(), source: "agent_status_projection".into(), source_id: Some("working".into()),
            observed_at_ms: 14, authoritative: false, fact: Fact::Working,
        };
        evidence.observe(&expected, &working_projection);
        assert_eq!(evidence.human_waits.len(), 1, "status cannot invent another request or replace the permission request");
        assert!(evidence.human_waits.iter().any(|wait| wait.wait_kind == HumanWaitKind::Permission && wait.request_id.is_none()));
        assert!(evidence.has_human_wait(), "status projections cannot clear the permission wait");
        assert!(!evidence.completion_verified());
    }

    #[test]
    fn circuit_request_receipts_preserve_attention_correlation_aliases() {
        use crate::autopilot::circuit::observation::{HumanWaitKind, ObservedWorkFact};
        for session in ["session_id", "sessionId", "sessionID", "conversationId", "conversation_id", "taskId"] {
            for turn in ["turn_id", "prompt_id", "promptId"] {
                for request in ["request_id", "tool_use_id", "toolUseId", "requestId", "requestID", "elicitation_id", "toolCallId", "tool_call_id", "callId", "call_id", "permissionID", "permission_id"] {
                    let mut payload = serde_json::json!({"hookName":"PostToolUseFailure","toolName":"request_user_input"});
                    payload[session] = "session".into();
                    payload[turn] = "turn".into();
                    payload[request] = "request".into();
                    let hook = NativeHook::parse("codex",&serde_json::to_vec(&payload).unwrap()).unwrap();
                    assert_eq!(hook.session_id.as_deref(),Some("session"));
                    assert_eq!(hook.turn_id.as_deref(),Some("turn"));
                    assert_eq!(hook.human_fact,Some(ObservedWorkFact::ToolFailed {wait_kind:HumanWaitKind::Question,request_id:"request".into()}));
                }
            }
        }
    }

    #[test]
    fn circuit_native_request_projection_and_reply_reconcile_in_both_arrival_orders() {
        use crate::autopilot::circuit::observation::{CircuitObservation, ObservedWorkFact as Fact, WorkEvidence};
        for (provider, tool) in [("claude", "AskUserQuestion"), ("codex", "request_user_input"), ("codex", "ask_user_question")] {
            for projection_first in [false, true] {
                let native = |event: &str, index: i64| {
                    let payload = serde_json::json!({"hook_event_name":event,"session_id":"session","turn_id":"turn","tool_use_id":"request","tool_name":tool});
                    let hook = NativeHook::parse(provider, &serde_json::to_vec(&payload).unwrap()).unwrap();
                    let receipt = NativeReceipt { agent_node_id:9,input_stamp:None,session_incarnation:Some("1".into()),
                        source_id:format!("{event}:{index}"),received_at_ms:index,turn_fenced:true,submission_correlated:false,hook };
                    let entry = crate::db::circuit::evidence::CircuitHistoryEntry {id:index,node_id:Some("work".into()),attempt:Some(1),kind:"native_hook_received".into(),detail:String::new(),observed_at:String::new()};
                    let super::super::CircuitEvent::ObservationBatch { expected, observations, stale, .. } = normalize(42,entry,receipt,Some("session".into()),Some("1".into()),Some("input".into())) else { panic!("native batch") };
                    assert!(!stale);
                    assert!(observations.iter().all(|item| item.authoritative));
                    (expected, observations)
                };
                let (identity, permission) = native("PermissionRequest",2);
                let (_, question) = native("PreToolUse",3);
                let mut projection_identity = identity.clone();
                projection_identity.turn_id = None;
                let projection = CircuitObservation {identity:projection_identity.clone(),source:"agent_status_projection".into(),source_id:Some("awaiting".into()),observed_at_ms:1,authoritative:false,fact:Fact::NeedsInput};
                let mut state = WorkEvidence::default();
                if projection_first { state.observe(&projection_identity,&projection); }
                for fact in permission.iter().chain(&question) { state.observe(&identity,fact); }
                if !projection_first { state.observe(&projection_identity,&projection); }
                assert_eq!(state.human_waits.len(),2,"projection must not create another obligation");
                state = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
                let (_, replies) = native("PermissionResult",4);
                for fact in &replies { state.observe(&identity,fact); }
                assert!(state.has_human_wait(),"permission answer does not answer the question");
                let waiting = state.clone();
                for reply_event in ["PostToolUse", "PostToolUseFailure"] {
                    state = waiting.clone();
                    let (_, replies) = native(reply_event,5);
                    for fact in &replies { state.observe(&identity,fact); }
                    assert!(!state.has_human_wait(),"exact tool outcome resolves the question and its permission");
                    assert!(!state.completion_verified(),"failed or answered requests never establish completion or approval");
                }
                let mut late_projection = projection.clone();
                late_projection.source_id = Some("not-seen-before-reply".into());
                state.observe(&projection_identity,&late_projection);
                assert!(!state.has_human_wait(),"late status snapshot cannot recreate a resolved native request");
                late_projection.source_id = Some("new-status-transition".into());
                late_projection.observed_at_ms = 7;
                state.observe(&projection_identity,&late_projection);
                assert!(!state.has_human_wait(),"a newer status projection is still not a native request");
                assert!(!state.completion_verified());
            }
        }
    }

    #[test]
    fn malformed_and_deleted_receipts_are_consumed_without_starving_later_receipts() {
        use crate::autopilot::circuit::observation::ObservationDisposition;
        use crate::autopilot::circuit::{
            context::CircuitContext,
            model::CircuitGraph,
            stepper::{advance, RunState, RunView},
        };
        let mut run = RunView {
            run_id: 42,
            state: RunState::Running,
            graph: CircuitGraph::walking_skeleton(""),
            context: CircuitContext::default(),
            steps: vec![],
        };
        let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: None,
            session_incarnation: None,
            source_id: "receipt".into(),
            received_at_ms: 5,
            turn_fenced: false,
            submission_correlated: false,
            hook: NativeHook::parse("claude", br#"{"hook_event_name":"Stop"}"#).unwrap(),
        };
        for (id, detail, deleted) in [
            (1, "malformed".to_owned(), false),
            (2, serde_json::to_string(&receipt).unwrap(), true),
            (3, serde_json::to_string(&receipt).unwrap(), false),
        ] {
            let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
                id,
                node_id: Some("work".into()),
                attempt: Some(1),
                kind: "native_hook_received".into(),
                detail,
                observed_at: String::new(),
            };
            let event = resolve_receipt(42, entry, |_| {
                if deleted {
                    Err(rusqlite::Error::QueryReturnedNoRows)
                } else {
                    Ok((None, None, None))
                }
            })
            .unwrap();
            let transition = advance(&mut run, &event);
            assert_eq!(
                run.context.get("observer.receipt_cursor"),
                Some(id.to_string().as_str())
            );
            assert!(!transition.observations.is_empty());
            assert!(transition
                .observations
                .iter()
                .all(|r| r.disposition == ObservationDisposition::Rejected));
            assert!(transition.effects.is_empty());
            assert_eq!(
                transition.observations[0].observation.source,
                match id {
                    1 => "malformed_native_receipt",
                    2 => "native_receipt_agent_deleted",
                    _ => "claude_native_hook",
                }
            );
        }
    }

    #[test]
    fn normalization_retains_identity_and_rejects_input_that_overtook_the_hook() {
        use crate::autopilot::circuit::observation::ObservedWorkFact as Fact;
        let receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("1:2".into()), session_incarnation: Some("1000".into()),
            source_id: "native-event".into(), received_at_ms: 5, turn_fenced: true, submission_correlated: true,
            hook: NativeHook::parse("claude", br#"{"hook_event_name":"Stop","session_id":"session","prompt_id":"prompt","background_tasks":[],"session_crons":[],"last_assistant_message":"Final report"}"#).unwrap() };
        for (input, stale, authoritative) in [
            (Some("1:2"), false, true),
            (Some("1:3"), true, false),
            (None, false, false),
        ] {
            let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
                id: 10,
                node_id: Some("work".into()),
                attempt: Some(2),
                kind: "native_hook_received".into(),
                detail: String::new(),
                observed_at: String::new(),
            };
            let event = normalize(
                42,
                entry,
                receipt.clone(),
                Some("session".into()),
                Some("1000".into()),
                input.map(str::to_owned),
            );
            let super::super::CircuitEvent::ObservationBatch {
                expected,
                observations,
                stale: actual_stale,
                receipt_id,
                input_guard,
            } = event
            else {
                panic!("native batch expected")
            };
            assert_eq!(actual_stale, stale);
            assert_eq!(input_guard.is_some(), authoritative);
            assert_eq!(receipt_id, 10);
            assert_eq!(expected.attempt, 2);
            assert_eq!(expected.agent_node_id, 9);
            assert_eq!(expected.turn_id.as_deref(), Some("prompt"));
            assert_eq!(observations.len(), 3);
            assert!(observations
                .iter()
                .all(|o| o.authoritative == authoritative && o.observed_at_ms == 5));
            assert!(
                matches!(&observations[0].fact, Fact::AssistantReport { text, revision } if text == "Final report" && !revision.is_empty())
            );
            assert!(
                matches!(&observations[2].fact, Fact::OwnershipSnapshot { active_work } if active_work.is_empty())
            );
        }
    }

    #[test]
    fn native_registry_absence_and_malformed_items_cannot_claim_zero_owned_work() {
        for registry in [
            serde_json::json!({}),
            serde_json::json!({"background_tasks":[]}),
            serde_json::json!({"background_tasks":[{}],"session_crons":[]}),
            serde_json::json!({"background_tasks":[],"session_crons":null}),
        ] {
            let mut value = registry;
            value["hook_event_name"] = "Stop".into();
            let hook = NativeHook::parse("claude", &serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(hook.active_work, None);
        }
        let hook = NativeHook::parse("claude", br#"{"hook_event_name":"Stop","prompt_id":"prompt-1","session_id":"session-1","background_tasks":[],"session_crons":[]}"#).unwrap();
        assert_eq!(hook.active_work, Some(BTreeSet::new()));
        assert_eq!(hook.turn_id.as_deref(), Some("prompt-1"));
    }

    #[test]
    fn opencode_plugin_events_never_enter_the_native_hook_path() {
        // Issue #1899: OpenCode's plugin wire (`session.idle`,
        // `permission.asked`, `question.asked`, capture-only
        // `session.created`, plus `session.busy` / reply events) is
        // attention-route input only. It must never parse as a native
        // Circuit hook receipt — under its own provider id or under a
        // sibling harness id — so malformed OpenCode data cannot alter
        // another harness's lifecycle.
        let bodies = [
            br#"{"hook_event_name":"session.idle","sessionID":"ses_fc52ccfb9ffek1jl23ZwpRuSP7"}"#.as_slice(),
            br#"{"hook_event_name":"permission.asked","sessionID":"ses_fc52ccfb9ffek1jl23ZwpRuSP7","request_id":"req-1","tool_name":"Bash"}"#.as_slice(),
            br#"{"hook_event_name":"question.asked","sessionID":"ses_fc52ccfb9ffek1jl23ZwpRuSP7","request_id":"q-1"}"#.as_slice(),
            br#"{"hook_event_name":"session.created","sessionID":"ses_fc52ccfb9ffek1jl23ZwpRuSP7"}"#.as_slice(),
            br#"{"hook_event_name":"session.busy","sessionID":"ses_fc52ccfb9ffek1jl23ZwpRuSP7"}"#.as_slice(),
            br#"{"hook_event_name":"question.replied","sessionID":"ses_fc52ccfb9ffek1jl23ZwpRuSP7","request_id":"q-1"}"#.as_slice(),
            br#"{"hook_event_name":"permission.replied","sessionID":"ses_fc52ccfb9ffek1jl23ZwpRuSP7","request_id":"req-1"}"#.as_slice(),
            br#"{"hook_event_name":"session.error","sessionID":"ses_fc52ccfb9ffek1jl23ZwpRuSP7"}"#.as_slice(),
            br#"not json at all"#.as_slice(),
            br#"{}"#.as_slice(),
        ];
        for body in bodies {
            for provider in ["opencode", "claude", "claude_code", "anthropic", "codex"] {
                assert!(
                    NativeHook::parse(provider, body).is_none(),
                    "opencode-shaped payload must not parse as a native hook for {provider}"
                );
            }
        }
        // The gate itself stays provider-scoped: unknown harness ids never
        // enter the native path either.
        assert!(NativeHook::parse("terminal", br#"{"hook_event_name":"Stop"}"#).is_none());
    }

    #[test]
    fn native_registry_preserves_all_task_types_and_scheduled_wakeups() {
        let hook = NativeHook::parse("anthropic", br#"{"hook_event_name":"Stop","background_tasks":[{"id":"a","type":"subagent"},{"id":"b","type":"future-task-type"}],"session_crons":[{"id":"a"}]}"#).unwrap();
        assert_eq!(
            hook.active_work.unwrap(),
            BTreeSet::from(["task:a".into(), "task:b".into(), "cron:a".into()])
        );
        assert!(NativeHook::parse(
            "codex",
            br#"{"hook_event_name":"Stop","background_tasks":[],"session_crons":[]}"#
        )
        .is_none());
        assert!(NativeHook::parse("claude", br#"{"hook_event_name":"TaskCompleted"}"#).is_none());
    }

    #[test]
    fn agy_stop_shapes_parse_with_session_identity_and_no_turn_token() {
        // Issue #1901: pre-`hookEventName` shape from the attention-route
        // fixtures (`conversationId`, `executionNum`, `fullyIdle`,
        // `terminationReason`, …).
        let hook = NativeHook::parse("agy", br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":1,"terminationReason":"model_stop","error":"","fullyIdle":true,"workspacePaths":["F:\\src\\repo"],"transcriptPath":"C:\\x\\transcript.jsonl","artifactDirectoryPath":"C:\\x","modelName":"gemini-3.7-flash"}"#).unwrap();
        assert_eq!(hook.event, "Stop");
        assert_eq!(hook.session_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(hook.turn_id, None, "executionNum is telemetry, not a turn fence");
        assert!(!hook.background_busy);
        assert_eq!(hook.execution_num, Some(1), "the counter is retained for receipt disambiguation");
        assert_eq!(hook.termination_reason.as_deref(), Some("model_stop"));
        assert_eq!(hook.active_work, None, "no child/background registry exists");
        assert_eq!(hook.final_report, None, "Stop carries no inline report");
        assert_eq!(hook.human_fact, None);
        assert_eq!(hook.provider.as_deref(), Some("agy"));
        // 1.2.x shape carrying `hookEventName` parses the same way.
        let named = NativeHook::parse("agy", br#"{"hookEventName":"Stop","conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":0,"fullyIdle":true,"terminationReason":"NO_TOOL_CALL"}"#).unwrap();
        assert_eq!(named.event, "Stop");
        assert_eq!(named.session_id, hook.session_id);
        assert_eq!(named.execution_num, Some(0));
        assert_eq!(named.termination_reason.as_deref(), Some("NO_TOOL_CALL"));
        // `fullyIdle: false` is the harness's background-busy signal.
        let busy = NativeHook::parse("agy", br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","fullyIdle":false}"#).unwrap();
        assert!(busy.background_busy);
        assert_eq!(busy.session_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(busy.execution_num, None, "absent counter stays absent");
        // snake_case spellings parse identically.
        let snake = NativeHook::parse("agy", br#"{"hook_event_name":"stop","conversation_id":"550e8400-e29b-41d4-a716-446655440000","fully_idle":false}"#).unwrap();
        assert_eq!(snake, busy);
        // Session comparison downstream is exact, so the conversation id is
        // canonicalized to the lowercase UUID the attention route stores.
        let upper = NativeHook::parse("agy", br#"{"conversationId":"550E8400-E29B-41D4-A716-446655440000","fullyIdle":true}"#).unwrap();
        assert_eq!(upper.session_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert!(NativeHook::parse("agy", br#"{"conversationId":"not-a-uuid","fullyIdle":true}"#).is_none());
        // Serde round-trip keeps the receipt (durable history rows persist it).
        assert_eq!(serde_json::from_str::<NativeHook>(&serde_json::to_string(&hook).unwrap()).unwrap(), hook);
    }

    #[test]
    fn agy_rejects_unvalidated_events_malformed_bodies_and_foreign_harnesses() {
        for body in [
            "{}",
            r#"{"conversationId":""}"#,
            r#"{"conversationId":"abc"}"#,
            r#"{"hookEventName":"Stop"}"#,
            r#"{"hookEventName":"PreToolUse","conversationId":"abc","toolCall":{"name":"Bash"}}"#,
            r#"{"hookEventName":"PostToolUse","conversationId":"abc"}"#,
            r#"{"hookEventName":"TaskCompleted","conversationId":"abc"}"#,
            r#"{"hook_event_name":"Stop","session_id":"session"}"#,
            "not json",
        ] {
            assert!(NativeHook::parse("agy", body.as_bytes()).is_none(), "{body}");
        }
        // Provider scoping: agy bytes never parse under a harness id that
        // owns no AGY adapter, and Claude request bytes are not AGY evidence.
        let stop = br#"{"conversationId":"abc","fullyIdle":true}"#;
        for provider in ["opencode", "terminal", "cursor", "grok", "mcode"] {
            assert!(NativeHook::parse(provider, stop).is_none(), "{provider}");
        }
        assert!(NativeHook::parse("agy", br#"{"hook_event_name":"PermissionRequest","session_id":"s","tool_use_id":"t","tool_name":"Bash"}"#).is_none());
    }

    #[test]
    fn agy_settled_stop_records_fenced_evidence_but_never_completion() {
        use crate::autopilot::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        let hook = NativeHook::parse("agy", br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":3,"fullyIdle":true,"terminationReason":"model_stop"}"#).unwrap();
        let receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("input-1".into()), session_incarnation: Some("7".into()),
            source_id: "agy-stop".into(), received_at_ms: 10, turn_fenced: false, submission_correlated: false, hook };
        let entry = crate::db::circuit::evidence::CircuitHistoryEntry { id: 1, node_id: Some("work".into()),
            attempt: Some(1), kind: "native_hook_received".into(), detail: String::new(), observed_at: String::new() };
        let super::super::CircuitEvent::ObservationBatch { expected, observations, stale, input_guard, .. } =
            normalize(42, entry, receipt, Some("550e8400-e29b-41d4-a716-446655440000".into()), Some("7".into()), Some("input-1".into()))
        else { panic!("native batch") };
        assert!(!stale);
        assert!(input_guard.is_none(), "no turn token means no authoritative guard");
        assert_eq!(observations.len(), 2);
        assert!(observations.iter().all(|o| !o.authoritative));
        assert!(observations.iter().all(|o| o.source == "agy_native_hook"));
        assert!(matches!(&observations[0].fact, Fact::ForegroundTerminated));
        assert!(matches!(&observations[1].fact, Fact::OwnershipUnavailable { reason } if reason.contains("no child/background registry")));
        let mut evidence = WorkEvidence::default();
        assert_eq!(evidence.observe(&expected, &observations[1]), ObservationDisposition::Unavailable);
        assert!(evidence.lifecycle_invalidated);
        assert_eq!(evidence.observe(&expected, &observations[0]), ObservationDisposition::ReducedConfidence);
        assert!(!evidence.foreground_terminated, "unfenced foreground evidence must not flip lifecycle state");
        assert!(!evidence.completion_verified(), "unknown owned work never becomes completion");
        assert_eq!(evidence.observe(&expected, &observations[0]), ObservationDisposition::Duplicate);
        assert_eq!(evidence.observe(&expected, &observations[1]), ObservationDisposition::Duplicate);
        evidence = serde_json::from_str(&serde_json::to_string(&evidence).unwrap()).unwrap();
        assert!(evidence.lifecycle_invalidated, "restart preserves the ownership limit");
        assert!(!evidence.completion_verified());
    }

    #[test]
    fn agy_background_busy_stop_is_yield_evidence_not_a_settled_turn() {
        use crate::autopilot::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        let hook = NativeHook::parse("agy", br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","fullyIdle":false}"#).unwrap();
        let receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("input-1".into()), session_incarnation: Some("7".into()),
            source_id: "agy-busy".into(), received_at_ms: 10, turn_fenced: false, submission_correlated: false, hook };
        let entry = crate::db::circuit::evidence::CircuitHistoryEntry { id: 2, node_id: Some("work".into()),
            attempt: Some(1), kind: "native_hook_received".into(), detail: String::new(), observed_at: String::new() };
        let super::super::CircuitEvent::ObservationBatch { expected, observations, .. } =
            normalize(42, entry, receipt, Some("550e8400-e29b-41d4-a716-446655440000".into()), Some("7".into()), Some("input-1".into()))
        else { panic!("native batch") };
        assert_eq!(observations.len(), 2);
        assert!(matches!(&observations[0].fact, Fact::Yielded));
        assert!(matches!(&observations[1].fact, Fact::OwnershipUnavailable { .. }));
        let mut evidence = WorkEvidence::default();
        assert_eq!(evidence.observe(&expected, &observations[1]), ObservationDisposition::Unavailable);
        assert_eq!(evidence.observe(&expected, &observations[0]), ObservationDisposition::ReducedConfidence);
        assert!(!evidence.foreground_terminated);
        assert!(!evidence.completion_verified());
    }

    #[test]
    fn agy_successive_turns_hash_to_distinct_source_ids() {
        // Issue #1901 review: without the retained `execution_num`, two
        // settled turns from one session hash byte-identically and the
        // history deduplicator permanently drops the second turn.
        let incarnation = Some("7".into());
        let id = |body: &[u8]| {
            receipt_source_id(&NativeHook::parse("agy", body).unwrap(), &incarnation).unwrap()
        };
        let turn0 = id(br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":0,"fullyIdle":true}"#);
        let turn1 = id(br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":1,"fullyIdle":true}"#);
        assert_ne!(turn0, turn1, "consecutive turns must not dedupe each other");
        assert_eq!(turn0, id(br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":0,"fullyIdle":true}"#),
            "a byte-identical redelivery still dedupes");
    }

    #[test]
    fn agy_receipts_persist_without_input_fence_and_keep_successive_turns() {
        use crate::autopilot::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        // Real persistence boundary: an in-memory DB with a running run and
        // a step bound to agent 9, driven through receive_native_hook_locked
        // exactly as production stores receipts.
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        let graph = serde_json::to_string(
            &crate::autopilot::circuit::model::CircuitGraph::walking_skeleton("work"),
        )
        .unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,agent_node_id,status,attempt) VALUES(1,'work',9,'running',1);")
            .unwrap();
        // Bound parameter: the serialized graph is full of braces that
        // `format!` would misread as placeholders.
        db.execute("UPDATE autopilot_circuits SET graph_json=?1 WHERE id=1", [&graph])
            .unwrap();
        let receipt = |body: &[u8], at_ms: i64| {
            let hook = NativeHook::parse("agy", body).unwrap();
            let session_incarnation = Some("7".into());
            // `receive()` reads the input stamp from the process registry;
            // persistence strips it again below without a UserPromptSubmit
            // turn-start binding, which is exactly what this test pins.
            let source_id = receipt_source_id(&hook, &session_incarnation).unwrap();
            NativeReceipt { agent_node_id: 9, input_stamp: Some("input-1".into()),
                session_incarnation, source_id, received_at_ms: at_ms,
                turn_fenced: false, submission_correlated: false, hook }
        };
        let turn0 = receipt(br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":0,"fullyIdle":true,"terminationReason":"model_stop"}"#, 10);
        let turn1 = receipt(br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":1,"fullyIdle":true,"terminationReason":"model_stop"}"#, 11);
        crate::db::circuit::evidence::receive_native_hook_locked(&mut db, &turn0).unwrap();
        crate::db::circuit::evidence::receive_native_hook_locked(&mut db, &turn1).unwrap();
        crate::db::circuit::evidence::receive_native_hook_locked(&mut db, &turn0).unwrap();
        let rows: Vec<(i64, String)> = {
            let mut stmt = db.prepare("SELECT id, detail FROM circuit_run_history WHERE run_id=1 AND kind='native_hook_received' ORDER BY id").unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap()
        };
        assert_eq!(rows.len(), 2, "both turns are kept; the redelivery dedupes");
        let mut evidence = WorkEvidence::default();
        for (id, detail) in &rows {
            let stored: serde_json::Value = serde_json::from_str(detail).unwrap();
            assert!(stored.get("input_stamp").is_none_or(|stamp| stamp.is_null()),
                "persistence strips the input stamp without a turn-start binding: no input fence exists for AGY");
            let entry = crate::db::circuit::evidence::CircuitHistoryEntry { id: *id, node_id: Some("work".into()),
                attempt: Some(1), kind: "native_hook_received".into(), detail: detail.clone(), observed_at: String::new() };
            let super::super::CircuitEvent::ObservationBatch { expected, observations, stale, input_guard, .. } =
                resolve_receipt(1, entry, |_| Ok((Some("550e8400-e29b-41d4-a716-446655440000".into()), Some("7".into()), Some("input-1".into()))))
                    .unwrap()
            else { panic!("native batch") };
            // Honest staleness: with no persisted input stamp the receipt can
            // never be stale-marked; session/incarnation fencing is the only
            // freshness boundary, and these observations stay non-authoritative.
            assert!(!stale);
            assert!(input_guard.is_none());
            assert_eq!(observations.len(), 2);
            assert!(observations.iter().all(|o| !o.authoritative && o.source == "agy_native_hook"));
            assert!(matches!(&observations[1].fact, Fact::OwnershipUnavailable { .. }));
            let ownership = evidence.observe(&expected, &observations[1]);
            let foreground = evidence.observe(&expected, &observations[0]);
            assert!(matches!(ownership, ObservationDisposition::Unavailable | ObservationDisposition::Duplicate));
            assert!(matches!(foreground, ObservationDisposition::ReducedConfidence | ObservationDisposition::Duplicate));
        }
        let stored_ids: Vec<String> = rows.iter().map(|(_, detail)| {
            serde_json::from_str::<serde_json::Value>(detail).unwrap()["source_id"].as_str().unwrap().to_owned()
        }).collect();
        assert_ne!(stored_ids[0], stored_ids[1], "successive turns persist under distinct source ids");
        assert!(evidence.lifecycle_invalidated);
        assert!(!evidence.completion_verified(), "unknown owned work never becomes completion");
    }

    #[test]
    fn agy_receipts_from_replaced_sessions_and_deleted_agents_cannot_advance_the_run() {
        use crate::autopilot::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        let hook = NativeHook::parse("agy", br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","fullyIdle":true}"#).unwrap();
        let receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("input-1".into()), session_incarnation: Some("7".into()),
            source_id: "agy-fenced".into(), received_at_ms: 10, turn_fenced: false, submission_correlated: false, hook };
        let entry = || crate::db::circuit::evidence::CircuitHistoryEntry { id: 3, node_id: Some("work".into()),
            attempt: Some(1), kind: "native_hook_received".into(), detail: String::new(), observed_at: String::new() };
        // A replaced session (restart under a new conversation) is rejected.
        let super::super::CircuitEvent::ObservationBatch { expected, observations, .. } =
            normalize(42, entry(), receipt, Some("550e8400-e29b-41d4-a716-446655440000".into()), Some("7".into()), Some("input-1".into()))
        else { panic!("native batch") };
        let mut evidence = WorkEvidence::default();
        assert_eq!(evidence.observe(&expected, &observations[0]), ObservationDisposition::ReducedConfidence);
        let mut restarted = observations[0].clone();
        restarted.identity.session_id = Some("replacement-session".into());
        assert_eq!(evidence.observe(&expected, &restarted), ObservationDisposition::Rejected);
        // A subagent `Stop` carries its own conversation id, so it fences as
        // a different session and can never complete the parent's turn.
        let subagent = NativeHook::parse("agy", br#"{"conversationId":"660e8400-e29b-41d4-a716-446655440001","fullyIdle":true}"#).unwrap();
        let subagent_receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("input-1".into()), session_incarnation: Some("7".into()),
            source_id: "agy-subagent".into(), received_at_ms: 11, turn_fenced: false, submission_correlated: false, hook: subagent };
        let super::super::CircuitEvent::ObservationBatch { observations: subagent_observations, .. } =
            normalize(42, entry(), subagent_receipt, Some("550e8400-e29b-41d4-a716-446655440000".into()), Some("7".into()), Some("input-1".into()))
        else { panic!("native batch") };
        assert!(matches!(&subagent_observations[0].fact, Fact::ForegroundTerminated));
        assert_eq!(evidence.observe(&expected, &subagent_observations[0]), ObservationDisposition::Rejected);
        assert!(!evidence.completion_verified());
        // A deleted node consumes its receipt without lifecycle effect.
        let deleted_hook = NativeHook::parse("agy", br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","fullyIdle":true}"#).unwrap();
        let deleted = NativeReceipt { agent_node_id: 9, input_stamp: Some("input-1".into()), session_incarnation: Some("7".into()),
            source_id: "agy-deleted".into(), received_at_ms: 12, turn_fenced: false, submission_correlated: false, hook: deleted_hook };
        let deleted_entry = crate::db::circuit::evidence::CircuitHistoryEntry { id: 4, node_id: Some("work".into()),
            attempt: Some(1), kind: "native_hook_received".into(), detail: serde_json::to_string(&deleted).unwrap(), observed_at: String::new() };
        let event = resolve_receipt(42, deleted_entry, |_| Err(rusqlite::Error::QueryReturnedNoRows)).unwrap();
        let super::super::CircuitEvent::ObservationBatch { observations: deleted_observations, .. } = event else { panic!("native batch") };
        assert_eq!(deleted_observations.len(), 1);
        assert_eq!(deleted_observations[0].source, "native_receipt_agent_deleted");
        assert!(matches!(&deleted_observations[0].fact, Fact::Unavailable));
    }
}
