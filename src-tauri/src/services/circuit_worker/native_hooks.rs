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
    #[serde(default)]
    pub human_fact: Option<crate::autopilot::circuit::observation::ObservedWorkFact>,
    #[serde(default)]
    pub provider: Option<String>,
}

impl NativeHook {
    pub(crate) fn parse(provider: &str, body: &[u8]) -> Option<Self> {
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
    use sha2::{Digest, Sha256};
    let input_stamp = crate::agent::process::PROCESS_REGISTRY.input_stamp(agent_node_id);
    let session_incarnation = crate::db::agent_turn_stamp(agent_node_id)
        .map_err(|e| e.to_string())?
        .and_then(|stamp| {
            stamp
                .split_once(':')
                .map(|(generation, _)| generation.to_owned())
        });
    let source_id = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&hook, &session_incarnation))
                .map_err(|e| e.to_string())?
        )
    );
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
        "Stop" => facts.push(Fact::ForegroundTerminated),
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
                transcript_guard: None,
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
        assert_eq!(evidence.human_waits.len(), 2, "status may add a generic aggregate wait but cannot replace the permission request");
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
                assert!(state.has_human_wait(),"a genuinely newer input wait is retained");
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
}
