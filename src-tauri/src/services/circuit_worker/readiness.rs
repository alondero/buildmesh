//! Deterministic admission to report interpretation. All harnesses use this
//! seam; a classifier never receives a report it cannot safely act on.

use super::*;
use crate::agent::process::InputUnavailable;
use crate::circuit::observation::{CircuitObservationBlocker as Blocker, ObservationIdentity};
use crate::circuit::stepper::{ClassificationBinding, ObservationInputFence};
use crate::services::transcript_reader::report_snapshot::{ReportReadError, ReportSnapshot};

pub(crate) struct Candidate {
    pub binding: ClassificationBinding,
    pub output: String,
    pub status: SessionStatus,
}

pub(crate) fn prepare(
    view: &RunView,
    node_id: &str,
    agent: &crate::models::AgentNode,
    stamp: Option<&str>,
    input: Result<String, InputUnavailable>,
    report: Result<ReportSnapshot, ReportReadError>,
) -> Result<Option<Candidate>, Blocker> {
    if let Some(blocker) = view.report_blocker(node_id) { return Err(blocker); }
    let Some(step) = view.step(node_id) else { return Ok(None); };
    // Discovery cannot recover a suspended process. Name the actionable wait
    // instead of promising automatic session discovery forever.
    if matches!(input, Err(InputUnavailable::MissingProcess)) { return Err(Blocker::ProcessUnavailable); }
    let evidence = view.classifier_evidence(node_id).filter(|evidence| {
        let Some(owner) = &evidence.identity else { return false; };
        let Some(native_report) = &evidence.report else { return false; };
        owner.session_id == agent.cli_session_id
            && owner.session_incarnation.as_deref() == stamp.and_then(|stamp| stamp.split_once(':')).map(|(incarnation, _)| incarnation)
            && native_report.input_stamp.as_ref() == input.as_ref().ok()
            && match &report {
                Ok(report) => report.revision == native_report.revision && report.text == native_report.text,
                // Unavailable storage can fall back to a current native receipt;
                // positive evidence of unfinished/newer work cannot. The set is
                // load-bearing for hook-native harnesses whose report read can
                // only ever answer one of these three — Cline (#1776) is the
                // live case: it has a whole-document digest reader but no report
                // adapter, so `report_snapshot::read` returns `Unsupported` and
                // its `agent_end` receipt is admitted here. Returning `NoReport`
                // or `PartialPublication` for Cline instead would discard that
                // receipt and stall the circuit permanently.
                Err(ReportReadError::Unsupported | ReportReadError::NoTranscript | ReportReadError::Unreadable) => true,
                Err(_) => false,
            }
    });
    let finished = report.as_ref().is_ok_and(|report| report.turn_finished) || evidence.is_some();
    let declared = report.as_ref().is_ok_and(|report|
        report_contract::declares_completion(view, node_id, &report.text));
    let yielded = matches!(agent.status, SessionStatus::Ready | SessionStatus::Completed | SessionStatus::AwaitingInput);
    if !finished && !declared && !yielded && step.status != StepStatus::Unverified { return Ok(None); }
    let session_id = agent.cli_session_id.as_deref().filter(|id| !id.is_empty()).ok_or(Blocker::SessionIdentityUnavailable)?;
    let incarnation = stamp.and_then(|stamp| stamp.split_once(':')).map(|(incarnation, _)| incarnation)
        .ok_or(Blocker::SessionIdentityUnavailable)?;
    let incarnation_ms = incarnation.parse::<i64>().map_err(|_| Blocker::SessionIdentityUnavailable)?;
    let input = input.map_err(|reason| match reason {
        InputUnavailable::MissingProcess => Blocker::ProcessUnavailable,
        InputUnavailable::Draft => Blocker::InputDraft,
        InputUnavailable::UnknownInput => Blocker::InputUncertain,
        InputUnavailable::Paste => Blocker::InputPaste,
    })?;
    // Explicit handoff is report readiness, not native lifecycle verification.
    let status = if finished || declared { SessionStatus::Ready } else { agent.status };
    if let Ok(report) = &report {
        if report.published_at_ms < incarnation_ms
            || view.context.get(&format!("agent.{}.previous_report_revision", agent.id)).is_some_and(|previous|
                crate::services::transcript_reader::same_assistant_revision(&report.revision, previous)) {
            return Err(Blocker::ReportSuperseded);
        }
        if !report.is_current() { return Err(Blocker::ReportUnavailable { reason: ReportReadError::ChangedDuringRead.reason().into() }); }
        // A native finished report supersedes a misleading Running projection.
        // Otherwise require a yielded harness or an explicit final report result.
        if !yielded && !finished && !declared { return Ok(None); }
        return Ok(Some(Candidate {
            output: report.text.clone(), status,
            binding: ClassificationBinding {
                owner: ObservationIdentity {
                    run_id: view.run_id, step_id: node_id.into(), attempt: step.attempt, agent_node_id: agent.id,
                    session_incarnation: Some(incarnation.into()), session_id: Some(session_id.into()),
                    turn_id: None, report_revision: Some(report.revision.clone()),
                },
                report_revision: report.revision.clone(),
                input_guard: ObservationInputFence {
                    transcript_guard: None, report_guard: Some(report.clone()), agent_node_id: agent.id,
                    input_stamp: input, observed_at_ms: report.published_at_ms,
                    session_id: session_id.into(), session_incarnation: incarnation.into(),
                },
            },
        }));
    }
    // Retain hook-native evidence for harnesses whose report arrives in a
    // receipt rather than a readable file. Its input/session proof must match.
    if let Some(evidence) = evidence {
        if let (Some(owner), Some(native_report)) = (evidence.identity, evidence.report) {
            if owner.session_id.as_deref() == Some(session_id)
                && owner.session_incarnation.as_deref() == Some(incarnation)
                && native_report.input_stamp.as_deref() == Some(input.as_str()) {
                return Ok(Some(Candidate {
                    output: native_report.text, status,
                    binding: ClassificationBinding { owner, report_revision: native_report.revision,
                        input_guard: ObservationInputFence {
                            transcript_guard: None, report_guard: None, agent_node_id: agent.id,
                            input_stamp: input, observed_at_ms: native_report.observed_at_ms,
                            session_id: session_id.into(), session_incarnation: incarnation.into(),
                        },
                    },
                }));
            }
        }
    }
    Err(Blocker::ReportUnavailable { reason: report.err().unwrap_or(ReportReadError::NoReport).reason().into() })
}
