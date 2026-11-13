//! Bounded recovery of explicit provider failures in live, unpiloted sessions.
//! Native evidence owns detection; prompt_delivery owns all terminal writes.
use super::transcript_reader::interruption::{self, Failure};
use crate::{
    agent::{
        process::PROCESS_REGISTRY,
        prompt_delivery::{
            self, DeliveryMode, DeliveryRequest, ObservationRequirement, PreparedPrompt,
        },
        session_lifecycle::{self, AppSessionLifecycleSink, HookSignalDetail},
    },
    db,
    models::AgentNode,
};
use std::{collections::HashMap, time::Duration};

const EVENT: &str = "buildmesh_native_interruption";
const PROMPT: &str = "Buildmesh observed that the previous turn ended with a temporary provider error. Continue the existing authorized task from its current state. Check what already completed before repeating any action. Preserve the current model, account, permissions and spending settings. If the task is complete or needs user input, report that instead.";

trait RecoveryHost {
    fn now(&self) -> i64;
    fn node(&self, id: i64) -> Result<AgentNode, String>;
    fn observation(&self, id: i64) -> Result<db::agent_node::AgentStatusObservation, String>;
    fn failure(&self, node: &AgentNode) -> Option<Option<Failure>>;
    fn input(&self, id: i64) -> Option<String>;
    fn unpiloted(&self, id: i64) -> Result<bool, String>;
    fn publish(
        &self,
        node: &AgentNode,
        observed: &db::agent_node::AgentStatusObservation,
        input: &str,
        failure: &Failure,
        message: String,
    ) -> Result<Option<db::agent_node::AgentStatusObservation>, String>;
    fn deliver(
        &self,
        node: &AgentNode,
        pending: &Pending,
    ) -> Result<prompt_delivery::DeliveryOutcome, String>;
    fn notice(&self, id: i64, pending: &Pending, message: &str) -> Result<(), String>;
}

struct LiveHost;
impl RecoveryHost for LiveHost {
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
    fn node(&self, id: i64) -> Result<AgentNode, String> {
        db::get_agent_node_by_id(id).map_err(|e| e.to_string())
    }
    fn observation(&self, id: i64) -> Result<db::agent_node::AgentStatusObservation, String> {
        db::agent_node::agent_status_observation(id).map_err(|e| e.to_string())
    }
    fn failure(&self, node: &AgentNode) -> Option<Option<Failure>> {
        interruption::read(node)
    }
    fn input(&self, id: i64) -> Option<String> {
        PROCESS_REGISTRY.input_stamp(id)
    }
    fn unpiloted(&self, id: i64) -> Result<bool, String> {
        unpiloted(id)
    }
    fn publish(
        &self,
        node: &AgentNode,
        observed: &db::agent_node::AgentStatusObservation,
        input: &str,
        failure: &Failure,
        message: String,
    ) -> Result<Option<db::agent_node::AgentStatusObservation>, String> {
        let app = crate::http::app_handle().ok_or("App handle unavailable")?;
        session_lifecycle::on_native_interruption(
            &AppSessionLifecycleSink { app },
            node.id,
            observed,
            input,
            failure.at_ms,
            &HookSignalDetail {
                provider: Some(node.provider.clone()),
                provider_event: Some(EVENT.into()),
                provider_session_id: node.cli_session_id.clone(),
                completion_reason: Some(failure.code.clone()),
                message: Some(message),
                ..Default::default()
            },
        )
    }
    fn deliver(
        &self,
        node: &AgentNode,
        pending: &Pending,
    ) -> Result<prompt_delivery::DeliveryOutcome, String> {
        deliver_continuation(&PROCESS_REGISTRY, node, pending, &|| {
            current_evidence(self, node.id, pending)
        })
    }
    fn notice(&self, id: i64, pending: &Pending, message: &str) -> Result<(), String> {
        let app = crate::http::app_handle().ok_or("App handle unavailable")?;
        session_lifecycle::native_interruption_notice(
            &AppSessionLifecycleSink { app },
            id,
            &pending.revision,
            message,
        )
    }
}

fn deliver_continuation(
    registry: &crate::agent::process::AgentProcessRegistry,
    node: &AgentNode,
    pending: &Pending,
    current: &dyn Fn() -> Result<bool, String>,
) -> Result<prompt_delivery::DeliveryOutcome, String> {
    let provider =
        crate::circuit::strategy::provider_for_agent(node).ok_or("Unknown launch provider")?;
    let adapter = provider.adapter();
    let prompt = PreparedPrompt::for_adapter(node.id, adapter, PROMPT);
    Ok(prompt_delivery::deliver(
        registry,
        adapter,
        DeliveryRequest {
            prompt: &prompt,
            mode: DeliveryMode::LaterTurn,
            observation: ObservationRequirement::Unobserved,
            expected_input: Some(&pending.input),
            still_current: current,
        },
    ))
}

#[derive(Default)]
struct Recovery {
    incarnation: String,
    attempts: usize,
    handled: Option<Failure>,
    pending: Option<Pending>,
}
struct Pending {
    failure: Failure,
    due_ms: i64,
    input: String,
    revision: String,
}

use session_lifecycle::interruption_eligible as eligible;

fn unpiloted(node: i64) -> Result<bool, String> {
    // Circuit workers own prompts/cancellation. Never restart retained reviewers.
    Ok(!db::circuit::leases::list_circuit_agent_ownerships()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|row| row.0 == node && row.4 != "completed" && row.4 != "cancelled"))
}

pub(crate) fn start() {
    if let Err(error) = std::thread::Builder::new().name("interruption-recovery".into()).spawn(|| {
        let mut states = HashMap::<i64, Recovery>::new();
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let Ok(nodes) = db::list_agent_nodes() else { continue; };
            states.retain(|id, _| nodes.iter().any(|n| n.id == *id && PROCESS_REGISTRY.is_alive(id)));
            for node in nodes.iter().filter(|n| PROCESS_REGISTRY.is_alive(&n.id)) {
                if let Err(error) = observe(&LiveHost, node, states.entry(node.id).or_default()) {
                    tracing::debug!(node_id=node.id, %error, "native interruption observation unavailable");
                }
            }
        }
    }) { tracing::error!(%error, "Could not start native interruption recovery"); }
}

fn observe(host: &impl RecoveryHost, node: &AgentNode, state: &mut Recovery) -> Result<(), String> {
    if !eligible(node) {
        state.pending = None;
        return Ok(());
    }
    // Capture before external I/O: a new native turn during the read must lose
    // the conditional lifecycle commit, even if it submitted no terminal input.
    let observed = host.observation(node.id)?;
    let Some(observation) = host.failure(node) else {
        // Busy native storage and partial writes are unavailable, not cancellation.
        return Ok(());
    };
    let Some(failure) = observation else {
        if let Some(pending) = state.pending.as_ref() {
            host.notice(node.id, pending, "Automatic continuation cancelled because the harness no longer reports that failure.")?;
        }
        state.pending = None;
        return Ok(());
    };
    let Some(generation) = observed.session_incarnation.as_deref() else {
        return Ok(());
    };
    let incarnation = format!("{generation}:{:?}", observed.session_id);
    if incarnation != state.incarnation {
        *state = Recovery {
            incarnation,
            ..Default::default()
        };
    }
    if generation
        .parse::<i64>()
        .ok()
        .is_none_or(|started| failure.at_ms < started)
        || observed.session_id != node.cli_session_id
        || !host.unpiloted(node.id)?
    {
        state.pending = None;
        return Ok(());
    }
    let now = host.now();
    if failure.at_ms > now {
        return Ok(());
    }
    if state.handled.as_ref() != Some(&failure) {
        state.pending = None;
        // Claim even cancelled observations: clearing a user's draft must not re-arm.
        let Some(input) = host.input(node.id) else {
            state.handled = Some(failure);
            return Ok(());
        };
        let delay = failure.delay_ms(state.attempts);
        let message = match delay {
            Some(ms) => format!(
                "{} Automatic continuation attempt {}/3 in {} seconds; typing cancels it.",
                failure.description(),
                state.attempts + 1,
                ms / 1000
            ),
            None if state.attempts >= 3 => format!(
                "{} Automatic continuation limit reached; inspect the session before continuing.",
                failure.description()
            ),
            None => failure.description().into(),
        };
        let accepted = host.publish(node, &observed, &input, &failure, message)?;
        state.handled = Some(failure.clone());
        if let Some(next) = accepted {
            tracing::info!(node_id=node.id, code=%failure.code, attempt=state.attempts, retry_in_ms=?delay, "native turn interruption detected");
            if let Some(delay) = delay {
                state.pending = Some(Pending {
                    failure,
                    due_ms: now.saturating_add(delay),
                    input,
                    revision: next.source_revision,
                });
            }
        }
        return Ok(());
    }
    let Some(pending) = state.pending.as_ref() else {
        return Ok(());
    };
    if !current(host, node.id, pending)? {
        host.notice(
            node.id,
            pending,
            "Automatic continuation cancelled because the session or terminal input changed.",
        )?;
        state.pending = None;
        return Ok(());
    }
    if now < pending.due_ms {
        return Ok(());
    }
    let Some(pending) = state.pending.take() else {
        return Ok(());
    };
    // Spend before I/O: an uncertain write must not be replayed.
    state.attempts += 1;
    let outcome = match host.deliver(node, &pending) {
        Ok(outcome) => outcome,
        Err(error) => {
            host.notice(node.id, &pending, "Automatic continuation could not be delivered. Inspect the terminal before continuing.")?;
            return Err(error);
        }
    };
    let message = if outcome.stop.is_some() {
        "Automatic continuation delivery was not confirmed. Inspect the terminal before continuing."
    } else {
        "Automatic continuation submitted; waiting for the harness to resume."
    };
    host.notice(node.id, &pending, message)?;
    tracing::info!(
        node_id = node.id,
        attempt = state.attempts,
        ?outcome,
        "native interruption continuation delivery"
    );
    Ok(())
}

fn current(host: &impl RecoveryHost, node_id: i64, pending: &Pending) -> Result<bool, String> {
    if host.input(node_id).as_deref() != Some(&pending.input) {
        return Ok(false);
    }
    current_evidence(host, node_id, pending)
}

fn current_evidence(
    host: &impl RecoveryHost,
    node_id: i64,
    pending: &Pending,
) -> Result<bool, String> {
    let observed = host.observation(node_id)?;
    if observed.source_revision != pending.revision || !host.unpiloted(node_id)? {
        return Ok(false);
    }
    let node = host.node(node_id)?;
    let failure = host
        .failure(&node)
        .ok_or("Native interruption evidence is temporarily unavailable")?;
    let after = host.observation(node_id)?;
    Ok(after.source_revision == pending.revision
        && eligible(&node)
        && failure.as_ref() == Some(&pending.failure))
}

#[cfg(test)]
#[path = "interruption_recovery_tests.rs"]
mod tests;
