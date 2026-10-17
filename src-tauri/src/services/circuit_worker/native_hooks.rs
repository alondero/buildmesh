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
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&(hook, session_incarnation)).map_err(|e| e.to_string())?,
    )))
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
        let revision = hex::encode(Sha256::digest(text.as_bytes()));
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
#[path = "native_hooks_tests.rs"]
mod tests;
