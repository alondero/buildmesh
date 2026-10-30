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
    /// The admitted assistant turn stays the continuation baseline even when
    /// classification binds a separate result-file report.
    pub(super) turn_revision: String,
}

#[cfg(test)]
pub(crate) fn prepare(
    view: &RunView,
    node_id: &str,
    agent: &crate::models::AgentNode,
    stamp: Option<&str>,
    input: Result<String, InputUnavailable>,
    report: Result<ReportSnapshot, ReportReadError>,
) -> Result<Option<Candidate>, Blocker> {
    let lifecycle_blocker =
        crate::circuit::report_admission::lifecycle_blocker(agent.lifecycle.as_ref());
    prepare_with_lifecycle_veto(
        view,
        node_id,
        agent,
        stamp,
        input,
        report,
        lifecycle_blocker,
    )
}

pub(crate) fn prepare_with_lifecycle_veto(
    view: &RunView,
    node_id: &str,
    agent: &crate::models::AgentNode,
    stamp: Option<&str>,
    input: Result<String, InputUnavailable>,
    report: Result<ReportSnapshot, ReportReadError>,
    lifecycle_blocker: Option<Blocker>,
) -> Result<Option<Candidate>, Blocker> {
    if let Some(blocker) = view.report_blocker(node_id) {
        return Err(blocker);
    }
    if let Some(blocker) = lifecycle_blocker {
        return Err(blocker);
    }
    let Some(step) = view.step(node_id) else {
        return Ok(None);
    };
    // Discovery cannot recover a suspended process. Name the actionable wait
    // instead of promising automatic session discovery forever.
    if matches!(input, Err(InputUnavailable::MissingProcess)) {
        return Err(Blocker::ProcessUnavailable);
    }
    let evidence = view.classifier_evidence(node_id).filter(|evidence| {
        let Some(owner) = &evidence.identity else {
            return false;
        };
        let Some(native_report) = &evidence.report else {
            return false;
        };
        owner.session_id == agent.cli_session_id
            && owner.session_incarnation.as_deref()
                == stamp
                    .and_then(|stamp| stamp.split_once(':'))
                    .map(|(incarnation, _)| incarnation)
            && native_report.input_stamp.as_ref() == input.as_ref().ok()
            && match &report {
                Ok(report) => {
                    report.revision == native_report.revision && report.text == native_report.text
                }
                // Unavailable storage can fall back to a current native receipt;
                // positive evidence of unfinished/newer work cannot. The set is
                // load-bearing for hook-native harnesses whose report read can
                // only ever answer one of these three — Cline (#1776) is the
                // live case: it has a whole-document digest reader but no report
                // adapter, so `report_snapshot::read` returns `Unsupported` and
                // its `agent_end` receipt is admitted here. Returning `NoReport`
                // or `PartialPublication` for Cline instead would discard that
                // receipt and stall the circuit permanently.
                Err(
                    ReportReadError::Unsupported
                    | ReportReadError::NoTranscript
                    | ReportReadError::Unreadable,
                ) => true,
                Err(_) => false,
            }
    });
    let finished = report.as_ref().is_ok_and(|report| report.turn_finished) || evidence.is_some();
    let declared = report
        .as_ref()
        .is_ok_and(|report| report_contract::declares_completion(view, node_id, &report.text));
    let yielded = matches!(
        agent.status,
        SessionStatus::Ready | SessionStatus::Completed | SessionStatus::AwaitingInput
    );
    if !finished && !declared && !yielded && step.status != StepStatus::Unverified {
        return Ok(None);
    }
    let (session_id, incarnation, incarnation_ms, input) = bound_identity(agent, stamp, input)?;
    // Explicit handoff is report readiness, not native lifecycle verification.
    let status = if finished || declared {
        SessionStatus::Ready
    } else {
        agent.status
    };
    if let Ok(report) = &report {
        if report.published_at_ms < incarnation_ms
            || view
                .context
                .get(&format!("agent.{}.previous_report_revision", agent.id))
                .is_some_and(|previous| {
                    crate::services::transcript_reader::same_assistant_revision(
                        &report.revision,
                        previous,
                    )
                })
        {
            return Err(Blocker::ReportSuperseded);
        }
        if !report.is_current() {
            return Err(Blocker::ReportUnavailable {
                reason: ReportReadError::ChangedDuringRead.reason().into(),
            });
        }
        // A native finished report supersedes a misleading Running projection.
        // Otherwise require a yielded harness or an explicit final report result.
        if !yielded && !finished && !declared {
            return Ok(None);
        }
        return Ok(Some(Candidate {
            output: report.text.clone(),
            status,
            turn_revision: report.revision.clone(),
            binding: ClassificationBinding {
                owner: ObservationIdentity {
                    run_id: view.run_id,
                    step_id: node_id.into(),
                    attempt: step.attempt,
                    agent_node_id: agent.id,
                    session_incarnation: Some(incarnation.into()),
                    session_id: Some(session_id.into()),
                    turn_id: None,
                    report_revision: Some(report.revision.clone()),
                },
                report_revision: report.revision.clone(),
                input_guard: ObservationInputFence {
                    transcript_guard: None,
                    report_guard: Some(report.clone()),
                    agent_node_id: agent.id,
                    input_stamp: input,
                    observed_at_ms: report.published_at_ms,
                    session_id: session_id.into(),
                    session_incarnation: incarnation.into(),
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
                && native_report.input_stamp.as_deref() == Some(input.as_str())
            {
                return Ok(Some(Candidate {
                    output: native_report.text,
                    status,
                    turn_revision: native_report.revision.clone(),
                    binding: ClassificationBinding {
                        owner,
                        report_revision: native_report.revision,
                        input_guard: ObservationInputFence {
                            transcript_guard: None,
                            report_guard: None,
                            agent_node_id: agent.id,
                            input_stamp: input,
                            observed_at_ms: native_report.observed_at_ms,
                            session_id: session_id.into(),
                            session_incarnation: incarnation.into(),
                        },
                    },
                }));
            }
        }
    }
    Err(Blocker::ReportUnavailable {
        reason: report
            .err()
            .unwrap_or(ReportReadError::NoReport)
            .reason()
            .into(),
    })
}

/// The session, incarnation and input ownership a report binding is fenced to.
fn bound_identity<'a>(
    agent: &'a crate::models::AgentNode,
    stamp: Option<&'a str>,
    input: Result<String, InputUnavailable>,
) -> Result<(&'a str, &'a str, i64, String), Blocker> {
    let session_id = agent
        .cli_session_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or(Blocker::SessionIdentityUnavailable)?;
    let incarnation = stamp
        .and_then(|stamp| stamp.split_once(':'))
        .map(|(incarnation, _)| incarnation)
        .ok_or(Blocker::SessionIdentityUnavailable)?;
    let incarnation_ms = incarnation
        .parse::<i64>()
        .map_err(|_| Blocker::SessionIdentityUnavailable)?;
    let input = input.map_err(|reason| match reason {
        InputUnavailable::MissingProcess => Blocker::ProcessUnavailable,
        InputUnavailable::Draft => Blocker::InputDraft,
        InputUnavailable::UnknownInput => Blocker::InputUncertain,
        InputUnavailable::Paste => Blocker::InputPaste,
    })?;
    Ok((session_id, incarnation, incarnation_ms, input))
}

/// Admit a yielded agent whose harness has no readable report, but which owes
/// this run a result file, with that file as its report.
///
/// Only called after [`prepare_with_lifecycle_veto`] found no report it could
/// bind, so the report and lifecycle blockers it checks first have already
/// passed. The candidate carries no output: the caller binds the result file
/// through `with_result_file`, which reads it, checks the gate's result-line
/// contract and fences the binding to the file's exact contents. A missing,
/// blank or contract-violating file therefore takes the ordinary bounded
/// reminder path, exactly as it would for a harness with a transcript.
///
/// The agent must already read Ready or Completed. A harness with no lifecycle
/// of its own reaches Ready through the result-file turn recovery in the
/// lost-turn watchdog, once the file has settled.
pub(crate) fn prepare_result_handoff(
    view: &RunView,
    node_id: &str,
    agent: &crate::models::AgentNode,
    stamp: Option<&str>,
    input: Result<String, InputUnavailable>,
    result: &std::path::Path,
    now_ms: i64,
) -> Result<Option<Candidate>, Blocker> {
    let Some(step) = view.step(node_id) else {
        return Ok(None);
    };
    if !matches!(
        agent.status,
        SessionStatus::Ready | SessionStatus::Completed
    ) {
        return Ok(None);
    }
    let (session_id, incarnation, _, input) = bound_identity(agent, stamp, input)?;
    let observed_at_ms = super::result_handoff::written_at_ms(result).unwrap_or(now_ms);
    let turn_revision = super::result_handoff::turn_revision(result);
    Ok(Some(Candidate {
        output: String::new(),
        status: SessionStatus::Ready,
        turn_revision: turn_revision.clone(),
        binding: ClassificationBinding {
            owner: ObservationIdentity {
                run_id: view.run_id,
                step_id: node_id.into(),
                attempt: step.attempt,
                agent_node_id: agent.id,
                session_incarnation: Some(incarnation.into()),
                session_id: Some(session_id.into()),
                turn_id: None,
                report_revision: Some(turn_revision.clone()),
            },
            report_revision: turn_revision,
            input_guard: ObservationInputFence {
                transcript_guard: None,
                report_guard: None,
                agent_node_id: agent.id,
                input_stamp: input,
                observed_at_ms,
                session_id: session_id.into(),
                session_incarnation: incarnation.into(),
            },
        },
    }))
}
