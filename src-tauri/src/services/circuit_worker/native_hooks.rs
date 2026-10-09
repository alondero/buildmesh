//! Native hook receipt. The attention route owns transport/session validation;
//! the harness adapter's observation strategy owns the vendor payload shape
//! (`crate::circuit::strategy`). This module preserves the lifecycle/ownership
//! facts and the final report, and normalizes them into Circuit observations.

use serde::{Deserialize, Serialize};

pub(crate) use crate::circuit::strategy::{submission_digest, NativeHook};

#[cfg(test)]
impl NativeHook {
    /// Parse with the strategy of the harness that owns `provider`. A harness
    /// that declares no hooks never yields a receipt.
    pub(crate) fn parse(provider: &str, body: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(body).ok()?;
        Self::parse_value(provider, &value)
    }

    pub(crate) fn parse_value(provider: &str, value: &serde_json::Value) -> Option<Self> {
        crate::circuit::strategy::for_stored(provider).parse_hook(value)
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NativeReceipt {
    pub agent_node_id: i64,
    pub input_stamp: Option<String>,
    pub session_incarnation: Option<String>,
    pub source_id: String,
    pub received_at_ms: i64,
    pub turn_fenced: bool,
    #[serde(default)]
    pub explicit_turn_mismatch: bool,
    #[serde(default)]
    pub submission_correlated: bool,
    /// Which recorded Buildmesh submission this turn acknowledged, when one
    /// was provably bound (issue #1898). Persisted so the ledger answers
    /// "which input was this?" without re-deriving it, and so a second turn
    /// cannot claim a submission another turn already acknowledged.
    #[serde(default)]
    pub submission_seq: Option<i64>,
    pub hook: NativeHook,
}

pub(crate) fn receive(
    agent_node_id: i64,
    hook: NativeHook,
    turn_fenced: bool,
    explicit_turn_mismatch: bool,
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
        explicit_turn_mismatch,
        // A turn token on its own says nothing about which Buildmesh
        // submission it belongs to. Correlation is decided against durable
        // evidence in `db::circuit::evidence::receive_native_hook`, which is
        // the only place a recorded submission and the harness prompt echo
        // can be compared (issue #1898).
        submission_correlated: false,
        submission_seq: None,
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
    use crate::circuit::observation::{CircuitObservation, ObservationIdentity, ObservedWorkFact};
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
    use crate::circuit::observation::{
        CircuitObservation, ObservationIdentity, ObservedWorkFact as Fact,
    };
    use sha2::{Digest, Sha256};
    let child_terminal = receipt.hook.event == "SubagentStop" && receipt.hook.child_id.is_some();
    let stale = !child_terminal
        && (receipt.explicit_turn_mismatch
            || current_input
                .as_ref()
                .zip(receipt.input_stamp.as_ref())
                .is_some_and(|(a, b)| a != b));
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
    // The strategy of the harness that parsed this hook. A receipt names its
    // adapter, so replay never consults mutable profile preferences and a hook
    // can only be read with its own harness's declaration.
    let strategy = crate::circuit::strategy::for_recorded_adapter(receipt.hook.provider.as_deref());
    let human_fact = receipt.hook.human_fact.clone();
    let authoritative = ((receipt.turn_fenced
        && (receipt.submission_correlated || human_fact.is_some()))
        || child_terminal)
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
    if let Some(fact) = human_fact {
        facts.push(fact);
    }
    match receipt.hook.event.as_str() {
        "UserPromptSubmit" => facts.push(Fact::Working),
        // `background_busy` is only ever set by a parser whose harness reports a yield with background work; the
        // harness signalled the turn yielded while owned background work is
        // still in flight, so this is yield evidence, not a settled turn.
        "Stop" => facts.push(if receipt.hook.background_busy {
            Fact::Yielded
        } else {
            Fact::ForegroundTerminated
        }),
        "PermissionRequest" if receipt.hook.human_fact.is_none() => {
            facts.push(Fact::PermissionRequested)
        }
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
        } else if let Some(reason) = strategy.hooks().and_then(|hooks| hooks.ownership_gap) {
            // Validated absence (issue #1901): the harness exposes no
            // child/background registry, so a settled foreground turn must
            // park the step Unverified instead of implying no owned work.
            facts.push(Fact::OwnershipUnavailable {
                reason: reason.into(),
            });
        } else {
            facts.push(Fact::Unavailable);
        }
    }
    let source = strategy.hooks().map_or("unwired_native_hook", |hooks| {
        if receipt.hook.human_fact.is_some() {
            hooks.request_source
        } else {
            hooks.source
        }
    });
    let observations = facts
        .into_iter()
        .enumerate()
        .map(|(index, fact)| CircuitObservation {
            identity: identity.clone(),
            source: source.into(),
            source_id: Some(format!("{}:{index}", receipt.source_id)),
            observed_at_ms: receipt.received_at_ms,
            authoritative,
            fact,
        })
        .collect();
    super::CircuitEvent::ObservationBatch {
        input_guard: authoritative.then(|| crate::circuit::stepper::ObservationInputFence {
            transcript_guard: None,
            report_guard: None,
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
    use std::collections::BTreeSet;

    #[test]
    fn claude_prompt_echo_is_the_only_native_submission_evidence() {
        // Issue #1898: `UserPromptSubmit` is the documented Claude Code event
        // that carries both the verbatim `prompt` text and the `prompt_id`
        // turn token. Nothing else may look like a submission acknowledgement.
        let submit = br#"{"session_id":"session","prompt_id":"turn-1","hook_event_name":"UserPromptSubmit","prompt":"run the tests"}"#;
        for provider in ["claude", "claude_code", "anthropic"] {
            let hook = NativeHook::parse(provider, submit).unwrap();
            assert_eq!(hook.turn_id.as_deref(), Some("turn-1"));
            assert_eq!(
                hook.prompt_digest.as_deref(),
                Some(submission_digest("run the tests").as_str()),
                "{provider} must hash the harness prompt echo through the shared submission digest"
            );
        }
        // Every other event, and the same event without the documented
        // `prompt` field, carries no submission evidence at all. An absent
        // digest must read as "cannot prove", never "assume it matched".
        for (provider, body) in [
            ("claude", br#"{"session_id":"session","prompt_id":"turn-1","hook_event_name":"Stop"}"#.as_slice()),
            ("claude", br#"{"session_id":"session","prompt_id":"turn-1","hook_event_name":"UserPromptSubmit"}"#.as_slice()),
            // A prompt echo without the turn token names content but no turn,
            // so it is not a complete acknowledgement candidate.
            ("claude", br#"{"session_id":"session","hook_event_name":"UserPromptSubmit","prompt":"run the tests"}"#.as_slice()),
            ("claude", br#"{"session_id":"session","prompt_id":"turn-1","hook_event_name":"UserPromptSubmit","prompt":""}"#.as_slice()),
        ] {
            let hook = NativeHook::parse(provider, body).unwrap();
            assert!(hook.prompt_digest.is_none(), "{provider} {body:?} must not fabricate submission evidence");
        }
        // A sibling harness reusing the shared parser keeps its own contract:
        // the prompt echo is Claude Code's documented field, not a shared one.
        assert!(
            NativeHook::parse("codex", submit)
                .unwrap()
                .prompt_digest
                .is_none(),
            "Codex has no documented UserPromptSubmit prompt echo"
        );
        // Antigravity's `Stop` shape keeps its own contract too.
        let agy = NativeHook::parse(
            "agy",
            br#"{"hook_event_name":"Stop","conversationId":"11111111-1111-4111-8111-111111111111","fullyIdle":true,"prompt":"run the tests"}"#,
        )
        .unwrap();
        assert!(
            agy.prompt_digest.is_none(),
            "AGY has no documented UserPromptSubmit prompt echo"
        );
    }

    #[test]
    fn submission_digest_is_a_byte_exact_prompt_fingerprint() {
        // The whole proof rests on this being a real byte-for-byte match, so
        // a one-character difference must not collide.
        assert_ne!(
            submission_digest("run the tests"),
            submission_digest("run the tests ")
        );
        assert_ne!(
            submission_digest("run the tests"),
            submission_digest("Run the tests")
        );
        assert_eq!(
            submission_digest("run the tests"),
            submission_digest("run the tests")
        );
        assert_eq!(
            submission_digest("run the tests").len(),
            64,
            "digest is a hex sha256"
        );
    }

    #[test]
    fn a_provably_bound_turn_reaches_accepted_and_an_unbound_one_cannot() {
        // Issue #1898, end to end through the replay path: the correlation
        // decided in the ledger is what makes a Claude turn's facts
        // authoritative, and an uncorrelated receipt is presented but cannot
        // advance the step.
        use crate::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        use crate::circuit::stepper::CircuitEvent;

        /// Replay one `Stop` receipt and report how the foreground
        /// termination was dispositioned, plus the resulting evidence.
        fn replay(
            turn: &str,
            correlated: bool,
            stamp: Option<&str>,
        ) -> (ObservationDisposition, WorkEvidence) {
            let receipt = NativeReceipt {
                agent_node_id: 9,
                input_stamp: stamp.map(str::to_owned),
                session_incarnation: Some("1000".into()),
                source_id: format!("stop-{turn}"),
                received_at_ms: 5,
                turn_fenced: true,
                explicit_turn_mismatch: false,
                submission_correlated: correlated,
                submission_seq: correlated.then_some(1),
                hook: NativeHook::parse(
                    "claude",
                    format!(
                        r#"{{"hook_event_name":"Stop","session_id":"session","prompt_id":"{turn}","last_assistant_message":"Final report"}}"#
                    )
                    .as_bytes(),
                )
                .unwrap(),
            };
            let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
                id: 10,
                node_id: Some("work".into()),
                attempt: Some(1),
                kind: "native_hook_received".into(),
                detail: String::new(),
                source: None,
                disposition: None,
                observed_at: String::new(),
            };
            let CircuitEvent::ObservationBatch {
                expected,
                observations,
                input_guard,
                ..
            } = normalize(
                42,
                entry,
                receipt,
                Some("session".into()),
                Some("1000".into()),
                Some("1:2".into()),
            )
            else {
                panic!("native batch expected")
            };
            assert_eq!(
                input_guard.is_some(),
                correlated,
                "the input fence follows the submission binding"
            );
            let mut evidence = WorkEvidence::default();
            let mut termination = None;
            for observation in &observations {
                let disposition = evidence.observe(&expected, observation);
                if matches!(observation.fact, Fact::ForegroundTerminated) {
                    termination = Some(disposition);
                }
            }
            (
                termination.expect("a Stop always reports foreground termination"),
                evidence,
            )
        }

        // The binding is what upgrades the turn's own facts from
        // reduced-confidence presentation to accepted evidence.
        let (disposition, evidence) = replay("turn-bound", true, Some("1:2"));
        assert_eq!(disposition, ObservationDisposition::Accepted);
        assert_eq!(
            evidence
                .identity
                .as_ref()
                .and_then(|i| i.turn_id.as_deref()),
            Some("turn-bound")
        );
        // Binding a turn is not completing a step. Claude's `Stop` carries no
        // child/background registry, so the ownership fact that follows the
        // termination is Unavailable and parks the lifecycle unverified.
        assert!(!evidence.ownership_covered);
        assert!(!evidence.lifecycle_verified());
        assert!(!evidence.completion_verified());

        // Without a provable binding the receipt is still recorded and
        // presented — including the final report — but the foreground
        // termination is reduced confidence and cannot advance the step. This
        // is the explicit unavailable path.
        for stamp in [None, Some("1:2")] {
            let (disposition, evidence) = replay("turn-unbound", false, stamp);
            assert_eq!(
                disposition,
                ObservationDisposition::ReducedConfidence,
                "a stamp without a binding grants no authority"
            );
            assert!(!evidence.foreground_terminated);
            assert!(!evidence.lifecycle_verified());
        }
    }

    #[test]
    fn circuit_native_human_requests_use_exact_ids_and_never_infer_responses_from_activity() {
        use crate::circuit::observation::{HumanWaitKind as Kind, ObservedWorkFact as Fact};
        let request = br#"{"hook_event_name":"PermissionRequest","session_id":"session","turn_id":"turn","tool_use_id":"tool-1","tool_name":"Bash"}"#;
        let reply = br#"{"hook_event_name":"PostToolUse","session_id":"session","turn_id":"turn","tool_use_id":"tool-1","tool_name":"Bash"}"#;
        for provider in ["claude", "codex"] {
            assert_eq!(
                NativeHook::parse(provider, request).unwrap().human_fact,
                Some(Fact::HumanWaitRequested {
                    wait_kind: Kind::Permission,
                    request_id: "tool-1".into()
                })
            );
            assert_eq!(
                NativeHook::parse(provider, reply).unwrap().human_fact,
                Some(Fact::ToolResponse {
                    wait_kind: Kind::Permission,
                    request_id: "tool-1".into()
                })
            );
            for event in ["Stop", "UserPromptSubmit"] {
                let payload = serde_json::json!({"hook_event_name":event,"tool_use_id":"tool-1"});
                assert!(
                    NativeHook::parse(provider, &serde_json::to_vec(&payload).unwrap())
                        .is_none_or(|hook| hook.human_fact.is_none())
                );
            }
        }
        assert!(
            NativeHook::parse("opencode", request).is_none(),
            "no other harness parser fallback"
        );
        assert!(
            NativeHook::parse(
                "codex",
                br#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#
            )
            .is_none(),
            "tool name alone is not a request identity"
        );
        let question = NativeHook::parse("codex", br#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_use_id":"q-1"}"#).unwrap();
        assert!(matches!(
            question.human_fact,
            Some(Fact::HumanWaitRequested {
                wait_kind: Kind::Question,
                ..
            })
        ));
    }

    #[test]
    fn codex_permission_without_request_id_is_retained_as_unresolved_permission() {
        use crate::circuit::observation::{
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
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
            hook,
        };
        let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 1,
            node_id: Some("work".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: String::new(),
            source: None,
            disposition: None,
            observed_at: String::new(),
        };
        let super::super::CircuitEvent::ObservationBatch {
            expected,
            observations,
            stale,
            ..
        } = normalize(
            42,
            entry,
            receipt,
            Some("session".into()),
            Some("1".into()),
            Some("input-1".into()),
        )
        else {
            panic!("native batch")
        };
        assert!(!stale);
        assert_eq!(observations.len(), 1);
        assert!(
            !observations[0].authoritative,
            "missing submission correlation cannot prove a current turn fact"
        );
        assert_eq!(observations[0].source, "codex_native_hook");
        assert_eq!(observations[0].fact, Fact::PermissionRequested);

        let mut evidence = WorkEvidence::default();
        assert_eq!(
            evidence.observe(&expected, &observations[0]),
            ObservationDisposition::ReducedConfidence
        );
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
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
            hook: reply,
        };
        let reply_entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 2,
            node_id: Some("work".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: String::new(),
            source: None,
            disposition: None,
            observed_at: String::new(),
        };
        let super::super::CircuitEvent::ObservationBatch {
            observations: reply_observations,
            ..
        } = normalize(
            42,
            reply_entry,
            reply_receipt,
            Some("session".into()),
            Some("1".into()),
            Some("input-1".into()),
        )
        else {
            panic!("native batch")
        };
        assert_eq!(
            evidence.observe(&expected, &reply_observations[0]),
            ObservationDisposition::Rejected
        );

        let mut stale_identity = expected.clone();
        stale_identity.session_id = Some("different-session".into());
        let stale_response = CircuitObservation {
            identity: stale_identity,
            source: "codex_request_hook".into(),
            source_id: Some("stale-response".into()),
            observed_at_ms: 12,
            authoritative: true,
            fact: Fact::ToolResponse {
                wait_kind: HumanWaitKind::Permission,
                request_id: "different-tool".into(),
            },
        };
        assert_eq!(
            evidence.observe(&expected, &stale_response),
            ObservationDisposition::Rejected
        );
        evidence = serde_json::from_str(&serde_json::to_string(&evidence).unwrap()).unwrap();
        assert_eq!(
            evidence.human_waits.len(),
            1,
            "restart preserves the typed wait without inventing identity"
        );

        let projection = CircuitObservation {
            identity: expected.clone(),
            source: "agent_status_projection".into(),
            source_id: Some("awaiting".into()),
            observed_at_ms: 12,
            authoritative: false,
            fact: Fact::NeedsInput,
        };
        evidence.observe(&expected, &projection);
        let ready_projection = CircuitObservation {
            identity: expected.clone(),
            source: "agent_status_projection".into(),
            source_id: Some("ready".into()),
            observed_at_ms: 13,
            authoritative: false,
            fact: Fact::Yielded,
        };
        evidence.observe(&expected, &ready_projection);
        let working_projection = CircuitObservation {
            identity: expected.clone(),
            source: "agent_status_projection".into(),
            source_id: Some("working".into()),
            observed_at_ms: 14,
            authoritative: false,
            fact: Fact::Working,
        };
        evidence.observe(&expected, &working_projection);
        assert_eq!(
            evidence.human_waits.len(),
            1,
            "status cannot invent another request or replace the permission request"
        );
        assert!(evidence
            .human_waits
            .iter()
            .any(|wait| wait.wait_kind == HumanWaitKind::Permission && wait.request_id.is_none()));
        assert!(
            evidence.has_human_wait(),
            "status projections cannot clear the permission wait"
        );
        assert!(!evidence.completion_verified());
    }

    #[test]
    fn circuit_request_receipts_preserve_attention_correlation_aliases() {
        use crate::circuit::observation::{HumanWaitKind, ObservedWorkFact};
        for session in [
            "session_id",
            "sessionId",
            "sessionID",
            "conversationId",
            "conversation_id",
            "taskId",
        ] {
            for turn in ["turn_id", "prompt_id", "promptId"] {
                for request in [
                    "request_id",
                    "tool_use_id",
                    "toolUseId",
                    "requestId",
                    "requestID",
                    "elicitation_id",
                    "toolCallId",
                    "tool_call_id",
                    "callId",
                    "call_id",
                    "permissionID",
                    "permission_id",
                ] {
                    let mut payload = serde_json::json!({"hookName":"PostToolUseFailure","toolName":"request_user_input"});
                    payload[session] = "session".into();
                    payload[turn] = "turn".into();
                    payload[request] = "request".into();
                    let hook =
                        NativeHook::parse("codex", &serde_json::to_vec(&payload).unwrap()).unwrap();
                    assert_eq!(hook.session_id.as_deref(), Some("session"));
                    assert_eq!(hook.turn_id.as_deref(), Some("turn"));
                    assert_eq!(
                        hook.human_fact,
                        Some(ObservedWorkFact::ToolFailed {
                            wait_kind: HumanWaitKind::Question,
                            request_id: "request".into()
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn circuit_native_request_projection_and_reply_reconcile_in_both_arrival_orders() {
        use crate::circuit::observation::{
            CircuitObservation, ObservedWorkFact as Fact, WorkEvidence,
        };
        for (provider, tool) in [
            ("claude", "AskUserQuestion"),
            ("codex", "request_user_input"),
            ("codex", "ask_user_question"),
        ] {
            for projection_first in [false, true] {
                let native = |event: &str, index: i64| {
                    let payload = serde_json::json!({"hook_event_name":event,"session_id":"session","turn_id":"turn","tool_use_id":"request","tool_name":tool});
                    let hook = NativeHook::parse(provider, &serde_json::to_vec(&payload).unwrap())
                        .unwrap();
                    let receipt = NativeReceipt {
                        agent_node_id: 9,
                        input_stamp: None,
                        session_incarnation: Some("1".into()),
                        source_id: format!("{event}:{index}"),
                        received_at_ms: index,
                        turn_fenced: true,
                        explicit_turn_mismatch: false,
                        submission_correlated: false,
                        submission_seq: None,
                        hook,
                    };
                    let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
                        id: index,
                        node_id: Some("work".into()),
                        attempt: Some(1),
                        kind: "native_hook_received".into(),
                        detail: String::new(),
                        source: None,
                        disposition: None,
                        observed_at: String::new(),
                    };
                    let super::super::CircuitEvent::ObservationBatch {
                        expected,
                        observations,
                        stale,
                        ..
                    } = normalize(
                        42,
                        entry,
                        receipt,
                        Some("session".into()),
                        Some("1".into()),
                        Some("input".into()),
                    )
                    else {
                        panic!("native batch")
                    };
                    assert!(!stale);
                    assert!(observations.iter().all(|item| item.authoritative));
                    (expected, observations)
                };
                let (identity, permission) = native("PermissionRequest", 2);
                let (_, question) = native("PreToolUse", 3);
                let mut projection_identity = identity.clone();
                projection_identity.turn_id = None;
                let projection = CircuitObservation {
                    identity: projection_identity.clone(),
                    source: "agent_status_projection".into(),
                    source_id: Some("awaiting".into()),
                    observed_at_ms: 1,
                    authoritative: false,
                    fact: Fact::NeedsInput,
                };
                let mut state = WorkEvidence::default();
                if projection_first {
                    state.observe(&projection_identity, &projection);
                }
                for fact in permission.iter().chain(&question) {
                    state.observe(&identity, fact);
                }
                if !projection_first {
                    state.observe(&projection_identity, &projection);
                }
                assert_eq!(
                    state.human_waits.len(),
                    2,
                    "projection must not create another obligation"
                );
                state = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
                let (_, replies) = native("PermissionResult", 4);
                for fact in &replies {
                    state.observe(&identity, fact);
                }
                assert!(
                    state.has_human_wait(),
                    "permission answer does not answer the question"
                );
                let waiting = state.clone();
                for reply_event in ["PostToolUse", "PostToolUseFailure"] {
                    state = waiting.clone();
                    let (_, replies) = native(reply_event, 5);
                    for fact in &replies {
                        state.observe(&identity, fact);
                    }
                    assert!(
                        !state.has_human_wait(),
                        "exact tool outcome resolves the question and its permission"
                    );
                    assert!(
                        !state.completion_verified(),
                        "failed or answered requests never establish completion or approval"
                    );
                }
                let mut late_projection = projection.clone();
                late_projection.source_id = Some("not-seen-before-reply".into());
                state.observe(&projection_identity, &late_projection);
                assert!(
                    !state.has_human_wait(),
                    "late status snapshot cannot recreate a resolved native request"
                );
                late_projection.source_id = Some("new-status-transition".into());
                late_projection.observed_at_ms = 7;
                state.observe(&projection_identity, &late_projection);
                assert!(
                    !state.has_human_wait(),
                    "a newer status projection is still not a native request"
                );
                assert!(!state.completion_verified());
            }
        }
    }

    #[test]
    fn malformed_and_deleted_receipts_are_consumed_without_starving_later_receipts() {
        use crate::circuit::observation::ObservationDisposition;
        use crate::circuit::{
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
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
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
                source: None,
                disposition: None,
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
        use crate::circuit::observation::ObservedWorkFact as Fact;
        let receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("1:2".into()), session_incarnation: Some("1000".into()),
            source_id: "native-event".into(), received_at_ms: 5, turn_fenced: true, explicit_turn_mismatch: false, submission_correlated: true, submission_seq: None,
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
                source: None,
                disposition: None,
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
    fn explicit_old_turn_hook_is_recorded_rejected_and_does_not_reopen_checkpoint() {
        use crate::circuit::observation::ObservationDisposition;
        use crate::circuit::{
            context::CircuitContext,
            model::CircuitGraph,
            stepper::{advance, RunState, RunView, StepStatus, StepView},
        };

        let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("current-input".into()),
            session_incarnation: Some("1000".into()),
            source_id: "delayed-old-stop".into(),
            received_at_ms: 50,
            turn_fenced: false,
            explicit_turn_mismatch: true,
            submission_correlated: false,
            submission_seq: None,
            hook: NativeHook::parse(
                "codex",
                br#"{"hook_event_name":"Stop","session_id":"session","turn_id":"old-turn"}"#,
            )
            .unwrap(),
        };
        let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 99,
            node_id: Some("spawn".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: String::new(),
            source: None,
            disposition: None,
            observed_at: String::new(),
        };
        let event = normalize(
            42,
            entry,
            receipt,
            Some("session".into()),
            Some("1000".into()),
            Some("current-input".into()),
        );
        let super::super::CircuitEvent::ObservationBatch {
            stale,
            observations,
            ..
        } = &event
        else {
            panic!("expected a receipt observation batch");
        };
        assert!(
            *stale,
            "an explicit old-turn mismatch stays stale even when its input stamp is current"
        );

        let mut run = RunView {
            run_id: 42,
            state: RunState::Running,
            graph: CircuitGraph::walking_skeleton(""),
            context: CircuitContext::default(),
            steps: vec![StepView {
                node_id: "spawn".into(),
                status: StepStatus::Unverified,
                outcome: None,
                error: Some("latest evidence remains unavailable".into()),
                agent_node_id: Some(9),
                attempt: 1,
            }],
        };
        let transition = advance(&mut run, &event);
        assert_eq!(
            transition.observations[0].disposition,
            ObservationDisposition::Rejected
        );
        assert!(transition.effects.is_empty());
        assert_eq!(run.step("spawn").unwrap().status, StepStatus::Unverified);
        assert_eq!(run.step("spawn").unwrap().attempt, 1);
        assert_eq!(run.context.get("observer.receipt_cursor"), Some("99"));
        assert!(observations.iter().all(|item| !item.authoritative));
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
    fn cline_hook_payloads_never_enter_the_native_hook_path() {
        // Issue #1902: Cline's file-hook layer is the riskiest cross-harness
        // shape in the tree, because unlike every other provider it keys its
        // event under `hookName` — the same key the Claude/Codex branch reads
        // as a fallback — and it ships `taskId` plus `agent_id`, which are
        // exactly the fields the generic parser folds into `session_id` and
        // `child_id`. Each body below is a verbatim 3.0.62 payload shape.
        let bodies = [
            // afterRun, status === "completed" -> agent_end (the only event
            // Buildmesh provisions, and the one attention maps to a turn end).
            br#"{"clineVersion":"3.0.62","timestamp":"2026-09-18T10:00:00.000Z","taskId":"session_1790003303940_9ouga","sessionContext":{"rootSessionId":"session_1790003303940_9ouga"},"workspaceRoots":["C:\\repo"],"userId":"alond","agent_id":"agent-1","parent_agent_id":null,"hookName":"agent_end","iteration":3,"turn":{"outputText":"done","status":"completed"},"taskComplete":{"taskMetadata":{}}}"#.as_slice(),
            // afterRun abort branch -> agent_abort, while the session is live.
            br#"{"clineVersion":"3.0.62","timestamp":"2026-09-18T10:00:00.000Z","taskId":"session_1790003303940_9ouga","hookName":"agent_abort","reason":"user-cancel","taskCancel":{"taskMetadata":{}}}"#.as_slice(),
            br#"{"clineVersion":"3.0.62","taskId":"session_1790003303940_9ouga","hookName":"agent_start","taskStart":{"taskMetadata":{}}}"#.as_slice(),
            br#"{"clineVersion":"3.0.62","taskId":"session_1790003303940_9ouga","hookName":"agent_resume","taskResume":{"taskMetadata":{},"previousState":{}}}"#.as_slice(),
            br#"{"clineVersion":"3.0.62","taskId":"session_1790003303940_9ouga","hookName":"agent_error","error":{"name":"Error","message":"boom","stack":"at x"}}"#.as_slice(),
            br#"{"clineVersion":"3.0.62","taskId":"session_1790003303940_9ouga","hookName":"tool_call","iteration":2,"tool_call":{"id":"call-1","name":"bash","input":{"command":"ls"}},"preToolUse":{"toolName":"bash","parameters":{}}}"#.as_slice(),
            br#"{"clineVersion":"3.0.62","taskId":"session_1790003303940_9ouga","hookName":"tool_result","iteration":2,"tool_result":{"name":"bash"},"postToolUse":{"toolName":"bash","parameters":{},"result":"ok","success":true,"executionTimeMs":5}}"#.as_slice(),
            br#"{"clineVersion":"3.0.62","taskId":"session_1790003303940_9ouga","hookName":"prompt_submit","userPromptSubmit":{"prompt":"do the thing","attachments":[]}}"#.as_slice(),
            br#"{"clineVersion":"3.0.62","taskId":"session_1790003303940_9ouga","hookName":"pre_tool_use","preToolUse":{"toolName":"bash","parameters":{}}}"#.as_slice(),
            br#"{"clineVersion":"3.0.62","taskId":"session_1790003303940_9ouga","hookName":"mystery"}"#.as_slice(),
            // Garbage and near-miss shapes.
            br#"not json at all"#.as_slice(),
            br#"{}"#.as_slice(),
            br#"{"hookName":"agent_end"}"#.as_slice(),
        ];
        // `anthropic`/`codex` are included deliberately: they are the two
        // harnesses whose branch would otherwise accept a Cline body, and
        // `agy` owns a shape-keyed `Stop` fallback.
        for body in bodies {
            for provider in [
                "cline",
                "claude",
                "claude_code",
                "anthropic",
                "codex",
                "agy",
                "terminal",
                "",
            ] {
                assert!(
                    NativeHook::parse(provider, body).is_none(),
                    "cline-shaped payload must not parse as a native hook for {provider:?}"
                );
            }
        }
        // Cline genuinely supplies no native Circuit receipt at all, so the
        // gate itself is provider-scoped: a sibling harness's event still
        // parses under its own id, and Cline never borrows it.
        assert!(NativeHook::parse("terminal", br#"{"hook_event_name":"Stop"}"#).is_none());
        assert!(
            NativeHook::parse("codex", br#"{"hook_event_name":"Stop","session_id":"s"}"#).is_some()
        );
    }

    #[test]
    fn native_registry_preserves_all_task_types_and_scheduled_wakeups() {
        let hook = NativeHook::parse("anthropic", br#"{"hook_event_name":"Stop","background_tasks":[{"id":"a","type":"subagent"},{"id":"b","type":"future-task-type"}],"session_crons":[{"id":"a"}]}"#).unwrap();
        assert_eq!(
            hook.active_work.unwrap(),
            BTreeSet::from(["task:a".into(), "task:b".into(), "cron:a".into()])
        );
        let codex_stop = NativeHook::parse(
            "codex",
            br#"{"hook_event_name":"Stop","background_tasks":[],"session_crons":[]}"#,
        )
        .expect("retain the Codex lifecycle callback for Circuit history");
        assert_eq!(codex_stop.active_work, Some(BTreeSet::new()));
        assert!(NativeHook::parse("claude", br#"{"hook_event_name":"TaskCompleted"}"#).is_none());
    }

    #[test]
    fn codex_foreground_callbacks_are_recordable_without_authoritative_completion() {
        use crate::circuit::observation::ObservedWorkFact as Fact;
        for (event, expected_fact) in [
            ("UserPromptSubmit", Fact::Working),
            ("Stop", Fact::ForegroundTerminated),
        ] {
            let hook = NativeHook::parse(
                "codex",
                serde_json::to_vec(&serde_json::json!({
                    "hook_event_name": event,
                    "session_id": "session",
                    "turn_id": "turn",
                }))
                .unwrap()
                .as_slice(),
            )
            .expect("retain lifecycle receipt even without a human-wait fact");
            assert_eq!(hook.event, event);
            assert!(hook.human_fact.is_none());

            let receipt = NativeReceipt {
                agent_node_id: 9,
                input_stamp: Some("input-1".into()),
                session_incarnation: Some("1000".into()),
                source_id: format!("{event}:source"),
                received_at_ms: 2_000,
                turn_fenced: true,
                explicit_turn_mismatch: false,
                submission_correlated: false,
                submission_seq: None,
                hook,
            };
            let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
                id: 1,
                node_id: Some("work".into()),
                attempt: Some(1),
                kind: "native_hook_received".into(),
                detail: String::new(),
                source: None,
                disposition: None,
                observed_at: String::new(),
            };
            let super::super::CircuitEvent::ObservationBatch { observations, .. } = normalize(
                42,
                entry,
                receipt,
                Some("session".into()),
                Some("1000".into()),
                Some("input-1".into()),
            ) else {
                panic!("expected a foreground callback observation");
            };
            assert!(observations
                .iter()
                .any(|observation| observation.fact == expected_fact));
            assert!(
                observations
                    .iter()
                    .all(|observation| !observation.authoritative),
                "a Codex lifecycle callback is not correlated to a Buildmesh submission"
            );
        }
    }

    #[test]
    fn agy_stop_shapes_parse_with_session_identity_and_no_turn_token() {
        // Issue #1901: pre-`hookEventName` shape from the attention-route
        // fixtures (`conversationId`, `executionNum`, `fullyIdle`,
        // `terminationReason`, …).
        let hook = NativeHook::parse("agy", br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":1,"terminationReason":"model_stop","error":"","fullyIdle":true,"workspacePaths":["F:\\src\\repo"],"transcriptPath":"C:\\x\\transcript.jsonl","artifactDirectoryPath":"C:\\x","modelName":"gemini-3.7-flash"}"#).unwrap();
        assert_eq!(hook.event, "Stop");
        assert_eq!(
            hook.session_id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(
            hook.turn_id, None,
            "executionNum is telemetry, not a turn fence"
        );
        assert!(!hook.background_busy);
        assert_eq!(
            hook.execution_num,
            Some(1),
            "the counter is retained for receipt disambiguation"
        );
        assert_eq!(hook.termination_reason.as_deref(), Some("model_stop"));
        assert_eq!(
            hook.active_work, None,
            "no child/background registry exists"
        );
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
        let busy = NativeHook::parse(
            "agy",
            br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","fullyIdle":false}"#,
        )
        .unwrap();
        assert!(busy.background_busy);
        assert_eq!(
            busy.session_id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(busy.execution_num, None, "absent counter stays absent");
        // snake_case spellings parse identically.
        let snake = NativeHook::parse("agy", br#"{"hook_event_name":"stop","conversation_id":"550e8400-e29b-41d4-a716-446655440000","fully_idle":false}"#).unwrap();
        assert_eq!(snake, busy);
        // Session comparison downstream is exact, so the conversation id is
        // canonicalized to the lowercase UUID the attention route stores.
        let upper = NativeHook::parse(
            "agy",
            br#"{"conversationId":"550E8400-E29B-41D4-A716-446655440000","fullyIdle":true}"#,
        )
        .unwrap();
        assert_eq!(
            upper.session_id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert!(NativeHook::parse(
            "agy",
            br#"{"conversationId":"not-a-uuid","fullyIdle":true}"#
        )
        .is_none());
        // Serde round-trip keeps the receipt (durable history rows persist it).
        assert_eq!(
            serde_json::from_str::<NativeHook>(&serde_json::to_string(&hook).unwrap()).unwrap(),
            hook
        );
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
            assert!(
                NativeHook::parse("agy", body.as_bytes()).is_none(),
                "{body}"
            );
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
        use crate::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        let hook = NativeHook::parse("agy", br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":3,"fullyIdle":true,"terminationReason":"model_stop"}"#).unwrap();
        let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("input-1".into()),
            session_incarnation: Some("7".into()),
            source_id: "agy-stop".into(),
            received_at_ms: 10,
            turn_fenced: false,
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
            hook,
        };
        let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 1,
            node_id: Some("work".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: String::new(),
            source: None,
            disposition: None,
            observed_at: String::new(),
        };
        let super::super::CircuitEvent::ObservationBatch {
            expected,
            observations,
            stale,
            input_guard,
            ..
        } = normalize(
            42,
            entry,
            receipt,
            Some("550e8400-e29b-41d4-a716-446655440000".into()),
            Some("7".into()),
            Some("input-1".into()),
        )
        else {
            panic!("native batch")
        };
        assert!(!stale);
        assert!(
            input_guard.is_none(),
            "no turn token means no authoritative guard"
        );
        assert_eq!(observations.len(), 2);
        assert!(observations.iter().all(|o| !o.authoritative));
        assert!(observations.iter().all(|o| o.source == "agy_native_hook"));
        assert!(matches!(&observations[0].fact, Fact::ForegroundTerminated));
        assert!(
            matches!(&observations[1].fact, Fact::OwnershipUnavailable { reason } if reason.contains("no child/background registry"))
        );
        let mut evidence = WorkEvidence::default();
        assert_eq!(
            evidence.observe(&expected, &observations[1]),
            ObservationDisposition::Unavailable
        );
        assert!(evidence.lifecycle_invalidated);
        assert_eq!(
            evidence.observe(&expected, &observations[0]),
            ObservationDisposition::ReducedConfidence
        );
        assert!(
            !evidence.foreground_terminated,
            "unfenced foreground evidence must not flip lifecycle state"
        );
        assert!(
            !evidence.completion_verified(),
            "unknown owned work never becomes completion"
        );
        assert_eq!(
            evidence.observe(&expected, &observations[0]),
            ObservationDisposition::Duplicate
        );
        assert_eq!(
            evidence.observe(&expected, &observations[1]),
            ObservationDisposition::Duplicate
        );
        evidence = serde_json::from_str(&serde_json::to_string(&evidence).unwrap()).unwrap();
        assert!(
            evidence.lifecycle_invalidated,
            "restart preserves the ownership limit"
        );
        assert!(!evidence.completion_verified());
    }

    #[test]
    fn agy_background_busy_stop_is_yield_evidence_not_a_settled_turn() {
        use crate::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        let hook = NativeHook::parse(
            "agy",
            br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","fullyIdle":false}"#,
        )
        .unwrap();
        let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("input-1".into()),
            session_incarnation: Some("7".into()),
            source_id: "agy-busy".into(),
            received_at_ms: 10,
            turn_fenced: false,
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
            hook,
        };
        let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 2,
            node_id: Some("work".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: String::new(),
            source: None,
            disposition: None,
            observed_at: String::new(),
        };
        let super::super::CircuitEvent::ObservationBatch {
            expected,
            observations,
            ..
        } = normalize(
            42,
            entry,
            receipt,
            Some("550e8400-e29b-41d4-a716-446655440000".into()),
            Some("7".into()),
            Some("input-1".into()),
        )
        else {
            panic!("native batch")
        };
        assert_eq!(observations.len(), 2);
        assert!(matches!(&observations[0].fact, Fact::Yielded));
        assert!(matches!(
            &observations[1].fact,
            Fact::OwnershipUnavailable { .. }
        ));
        let mut evidence = WorkEvidence::default();
        assert_eq!(
            evidence.observe(&expected, &observations[1]),
            ObservationDisposition::Unavailable
        );
        assert_eq!(
            evidence.observe(&expected, &observations[0]),
            ObservationDisposition::ReducedConfidence
        );
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
        use crate::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        // Real persistence boundary: an in-memory DB with a running run and
        // a step bound to agent 9, driven through receive_native_hook_locked
        // exactly as production stores receipts.
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        let graph = serde_json::to_string(&crate::circuit::model::CircuitGraph::walking_skeleton(
            "work",
        ))
        .unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,agent_node_id,status,attempt) VALUES(1,'work',9,'running',1);")
            .unwrap();
        // Bound parameter: the serialized graph is full of braces that
        // `format!` would misread as placeholders.
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1 WHERE id=1",
            [&graph],
        )
        .unwrap();
        let receipt = |body: &[u8], at_ms: i64| {
            let hook = NativeHook::parse("agy", body).unwrap();
            let session_incarnation = Some("7".into());
            // `receive()` reads the input stamp from the process registry;
            // persistence strips it again below without a UserPromptSubmit
            // turn-start binding, which is exactly what this test pins.
            let source_id = receipt_source_id(&hook, &session_incarnation).unwrap();
            NativeReceipt {
                agent_node_id: 9,
                input_stamp: Some("input-1".into()),
                session_incarnation,
                source_id,
                received_at_ms: at_ms,
                turn_fenced: false,
                explicit_turn_mismatch: false,
                submission_correlated: false,
                submission_seq: None,
                hook,
            }
        };
        let turn0 = receipt(br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":0,"fullyIdle":true,"terminationReason":"model_stop"}"#, 10);
        let turn1 = receipt(br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":1,"fullyIdle":true,"terminationReason":"model_stop"}"#, 11);
        crate::db::circuit::evidence::receive_native_hook_locked(&mut db, &turn0).unwrap();
        crate::db::circuit::evidence::receive_native_hook_locked(&mut db, &turn1).unwrap();
        crate::db::circuit::evidence::receive_native_hook_locked(&mut db, &turn0).unwrap();
        let rows: Vec<(i64, String)> = {
            let mut stmt = db.prepare("SELECT id, detail FROM circuit_run_history WHERE run_id=1 AND kind='native_hook_received' ORDER BY id").unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(rows.len(), 2, "both turns are kept; the redelivery dedupes");
        let mut evidence = WorkEvidence::default();
        for (id, detail) in &rows {
            let stored: serde_json::Value = serde_json::from_str(detail).unwrap();
            assert!(stored.get("input_stamp").is_none_or(|stamp| stamp.is_null()),
                "persistence strips the input stamp without a turn-start binding: no input fence exists for AGY");
            let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
                id: *id,
                node_id: Some("work".into()),
                attempt: Some(1),
                kind: "native_hook_received".into(),
                detail: detail.clone(),
                source: None,
                disposition: None,
                observed_at: String::new(),
            };
            let super::super::CircuitEvent::ObservationBatch {
                expected,
                observations,
                stale,
                input_guard,
                ..
            } = resolve_receipt(1, entry, |_| {
                Ok((
                    Some("550e8400-e29b-41d4-a716-446655440000".into()),
                    Some("7".into()),
                    Some("input-1".into()),
                ))
            })
            .unwrap()
            else {
                panic!("native batch")
            };
            // Honest staleness: with no persisted input stamp the receipt can
            // never be stale-marked; session/incarnation fencing is the only
            // freshness boundary, and these observations stay non-authoritative.
            assert!(!stale);
            assert!(input_guard.is_none());
            assert_eq!(observations.len(), 2);
            assert!(observations
                .iter()
                .all(|o| !o.authoritative && o.source == "agy_native_hook"));
            assert!(matches!(
                &observations[1].fact,
                Fact::OwnershipUnavailable { .. }
            ));
            let ownership = evidence.observe(&expected, &observations[1]);
            let foreground = evidence.observe(&expected, &observations[0]);
            assert!(matches!(
                ownership,
                ObservationDisposition::Unavailable | ObservationDisposition::Duplicate
            ));
            assert!(matches!(
                foreground,
                ObservationDisposition::ReducedConfidence | ObservationDisposition::Duplicate
            ));
        }
        let stored_ids: Vec<String> = rows
            .iter()
            .map(|(_, detail)| {
                serde_json::from_str::<serde_json::Value>(detail).unwrap()["source_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_ne!(
            stored_ids[0], stored_ids[1],
            "successive turns persist under distinct source ids"
        );
        assert!(evidence.lifecycle_invalidated);
        assert!(
            !evidence.completion_verified(),
            "unknown owned work never becomes completion"
        );
    }

    #[test]
    fn agy_receipts_from_replaced_sessions_and_deleted_agents_cannot_advance_the_run() {
        use crate::circuit::observation::{
            ObservationDisposition, ObservedWorkFact as Fact, WorkEvidence,
        };
        let hook = NativeHook::parse(
            "agy",
            br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","fullyIdle":true}"#,
        )
        .unwrap();
        let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("input-1".into()),
            session_incarnation: Some("7".into()),
            source_id: "agy-fenced".into(),
            received_at_ms: 10,
            turn_fenced: false,
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
            hook,
        };
        let entry = || crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 3,
            node_id: Some("work".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: String::new(),
            source: None,
            disposition: None,
            observed_at: String::new(),
        };
        // A replaced session (restart under a new conversation) is rejected.
        let super::super::CircuitEvent::ObservationBatch {
            expected,
            observations,
            ..
        } = normalize(
            42,
            entry(),
            receipt,
            Some("550e8400-e29b-41d4-a716-446655440000".into()),
            Some("7".into()),
            Some("input-1".into()),
        )
        else {
            panic!("native batch")
        };
        let mut evidence = WorkEvidence::default();
        assert_eq!(
            evidence.observe(&expected, &observations[0]),
            ObservationDisposition::ReducedConfidence
        );
        let mut restarted = observations[0].clone();
        restarted.identity.session_id = Some("replacement-session".into());
        assert_eq!(
            evidence.observe(&expected, &restarted),
            ObservationDisposition::Rejected
        );
        // A subagent `Stop` carries its own conversation id, so it fences as
        // a different session and can never complete the parent's turn.
        let subagent = NativeHook::parse(
            "agy",
            br#"{"conversationId":"660e8400-e29b-41d4-a716-446655440001","fullyIdle":true}"#,
        )
        .unwrap();
        let subagent_receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("input-1".into()),
            session_incarnation: Some("7".into()),
            source_id: "agy-subagent".into(),
            received_at_ms: 11,
            turn_fenced: false,
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
            hook: subagent,
        };
        let super::super::CircuitEvent::ObservationBatch {
            observations: subagent_observations,
            ..
        } = normalize(
            42,
            entry(),
            subagent_receipt,
            Some("550e8400-e29b-41d4-a716-446655440000".into()),
            Some("7".into()),
            Some("input-1".into()),
        )
        else {
            panic!("native batch")
        };
        assert!(matches!(
            &subagent_observations[0].fact,
            Fact::ForegroundTerminated
        ));
        assert_eq!(
            evidence.observe(&expected, &subagent_observations[0]),
            ObservationDisposition::Rejected
        );
        assert!(!evidence.completion_verified());
        // A deleted node consumes its receipt without lifecycle effect.
        let deleted_hook = NativeHook::parse(
            "agy",
            br#"{"conversationId":"550e8400-e29b-41d4-a716-446655440000","fullyIdle":true}"#,
        )
        .unwrap();
        let deleted = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("input-1".into()),
            session_incarnation: Some("7".into()),
            source_id: "agy-deleted".into(),
            received_at_ms: 12,
            turn_fenced: false,
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
            hook: deleted_hook,
        };
        let deleted_entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 4,
            node_id: Some("work".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: serde_json::to_string(&deleted).unwrap(),
            source: None,
            disposition: None,
            observed_at: String::new(),
        };
        let event = resolve_receipt(42, deleted_entry, |_| {
            Err(rusqlite::Error::QueryReturnedNoRows)
        })
        .unwrap();
        let super::super::CircuitEvent::ObservationBatch {
            observations: deleted_observations,
            ..
        } = event
        else {
            panic!("native batch")
        };
        assert_eq!(deleted_observations.len(), 1);
        assert_eq!(
            deleted_observations[0].source,
            "native_receipt_agent_deleted"
        );
        assert!(matches!(&deleted_observations[0].fact, Fact::Unavailable));
    }

    // ----- Strategy seam (issue #2128) -------------------------------------
    //
    // These cross the production seam from a persisted receipt back to
    // observations: `resolve_receipt` is what the worker runs after a restart,
    // and it must read each receipt with the strategy of the harness that
    // parsed it, not with whatever the node's profile preferences say today.

    fn replayed(
        persisted_hook: &serde_json::Value,
    ) -> Vec<crate::circuit::observation::CircuitObservation> {
        let receipt = serde_json::json!({
            "agent_node_id": 9,
            "input_stamp": null,
            "session_incarnation": "7",
            "source_id": "receipt-1",
            "received_at_ms": 5,
            "turn_fenced": false,
            "hook": persisted_hook,
        });
        let entry = crate::db::circuit::evidence::CircuitHistoryEntry {
            id: 10,
            node_id: Some("work".into()),
            attempt: Some(1),
            kind: "native_hook_received".into(),
            detail: receipt.to_string(),
            source: None,
            disposition: None,
            observed_at: String::new(),
        };
        let super::super::CircuitEvent::ObservationBatch { observations, .. } =
            resolve_receipt(1, entry, |_| {
                Ok((Some("session".into()), Some("7".into()), None))
            })
            .unwrap()
        else {
            panic!("native batch expected")
        };
        observations
    }

    fn persisted_stop(provider: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "session_id": "session", "turn_id": null, "event": "Stop", "child_id": null,
            "active_work": null, "final_report": null, "provider": provider,
        })
    }

    #[test]
    fn replay_reads_a_receipt_with_the_strategy_that_parsed_it() {
        use crate::circuit::observation::ObservedWorkFact as Fact;
        let ownership_gap = |provider: &str| {
            replayed(&persisted_stop(Some(provider)))
                .into_iter()
                .find_map(|o| match o.fact {
                    Fact::OwnershipUnavailable { reason } => Some(reason),
                    _ => None,
                })
        };
        for (provider, source) in [
            ("anthropic", "claude_native_hook"),
            ("claude", "claude_native_hook"),
            ("codex", "codex_native_hook"),
            ("agy", "agy_native_hook"),
        ] {
            let observations = replayed(&persisted_stop(Some(provider)));
            assert!(
                observations.iter().all(|o| o.source == source),
                "{provider} replays as {source}"
            );
        }
        // Only Antigravity's declaration states an ownership gap; Claude and
        // Codex leave a registry-less Stop merely unknown.
        assert!(ownership_gap("agy").unwrap().contains("Antigravity"));
        assert!(ownership_gap("anthropic").is_none());
        assert!(ownership_gap("codex").is_none());
    }

    #[test]
    fn replay_of_a_receipt_from_an_unwired_or_unrecorded_harness_borrows_no_strategy() {
        use crate::circuit::observation::ObservedWorkFact as Fact;
        for provider in [Some("opencode"), Some("future-harness"), None] {
            let observations = replayed(&persisted_stop(provider));
            assert!(
                observations
                    .iter()
                    .all(|o| o.source == "unwired_native_hook"),
                "{provider:?} must not replay as Claude, Codex or Antigravity"
            );
            assert!(observations.iter().all(|o| !o.authoritative));
            assert!(
                !observations
                    .iter()
                    .any(|o| matches!(o.fact, Fact::OwnershipUnavailable { .. })),
                "{provider:?} must not inherit another harness's ownership statement"
            );
        }
    }

    #[test]
    fn a_hook_survives_persistence_and_replays_identically_after_a_restart() {
        // Persist through the real serde shape (what `receive_native_hook`
        // stores), reload it as the restarted worker would, and compare the
        // facts to the pre-restart normalization for each declaring harness.
        let cases = [
            ("anthropic", br#"{"hook_event_name":"Stop","session_id":"session","prompt_id":"t1","last_assistant_message":"done"}"#.as_slice()),
            ("codex", br#"{"hook_event_name":"Stop","session_id":"session","turn_id":"t1","last_assistant_message":"done"}"#.as_slice()),
            ("agy", br#"{"hookEventName":"Stop","conversationId":"550e8400-e29b-41d4-a716-446655440000","executionNum":1,"fullyIdle":false}"#.as_slice()),
        ];
        for (provider, body) in cases {
            let hook = NativeHook::parse(provider, body).unwrap();
            let persisted = serde_json::to_value(&hook).unwrap();
            let reloaded: NativeHook = serde_json::from_value(persisted.clone()).unwrap();
            assert_eq!(reloaded, hook, "{provider}: persistence is lossless");
            let facts = |observations: Vec<crate::circuit::observation::CircuitObservation>| {
                observations
                    .into_iter()
                    .map(|o| (o.source, format!("{:?}", o.fact)))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                facts(replayed(&persisted)),
                facts(replayed(&serde_json::to_value(&reloaded).unwrap())),
                "{provider}: replay is deterministic"
            );
        }
    }
}
