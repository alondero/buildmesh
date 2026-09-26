//! A stable report read for Circuit interpretation, not proof of task ownership.

use super::*;

#[derive(Debug, Clone, PartialEq)]
enum ReportSource {
    File { path: PathBuf, length: u64, modified: std::time::SystemTime },
    OpenCode { path: PathBuf, session_id: String, rows: Vec<(String, serde_json::Value)> },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReportSnapshot {
    pub text: String,
    pub revision: String,
    pub published_at_ms: i64,
    /// Native turn boundary, independent of the displayed agent status.
    /// This says nothing about task correctness or complete owned-work coverage.
    pub turn_finished: bool,
    source: ReportSource,
}

impl ReportSnapshot {
    pub(crate) fn is_current(&self) -> bool {
        match &self.source {
            ReportSource::File { path, length, modified } => fs::metadata(path).is_ok_and(|metadata|
                metadata.len() == *length && metadata.modified().ok() == Some(*modified)),
            ReportSource::OpenCode { path, session_id, rows } =>
                read_opencode_message_rows(path, session_id, OPENCODE_DIGEST_WINDOW).as_ref() == Some(rows),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportReadError {
    Unsupported, NoSession, NoTranscript, Unreadable, PartialPublication,
    MalformedRecord, NoReport, WorkInProgress, NoNativeCompletion,
    NoTimestamp, ChangedDuringRead,
}

impl ReportReadError {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Unsupported => "this harness has no readable report adapter",
            Self::NoSession => "the harness session identity has not been captured",
            Self::NoTranscript => "the current session transcript has not been found",
            Self::Unreadable => "the transcript could not be read",
            Self::PartialPublication => "the harness is still publishing a transcript record",
            Self::MalformedRecord => "the transcript contains an unrecognised or malformed record",
            Self::NoReport => "the current turn has not published an assistant report",
            Self::WorkInProgress => "newer input or tool activity follows the assistant report",
            Self::NoNativeCompletion => "the harness has not recorded a current finished turn",
            Self::NoTimestamp => "the report has no usable publication timestamp",
            Self::ChangedDuringRead => "the transcript changed while it was being read",
        }
    }
}

pub(crate) fn read(format: TranscriptFormat, session_id: &str, node_path: &str) -> Result<ReportSnapshot, ReportReadError> {
    use ReportReadError as E;
    if format == TranscriptFormat::OpenCode {
        let (path, session_id) = opencode_resolve(Some(session_id), node_path).map_err(|_| E::NoTranscript)?;
        let rows = read_opencode_message_rows(&path, session_id, OPENCODE_DIGEST_WINDOW).ok_or(E::Unreadable)?;
        let parsed = parse_opencode_messages(&rows.iter().map(|(_, value)| value.clone()).collect::<Vec<_>>(), 1);
        let last = parsed.turns.last().ok_or(E::NoReport)?;
        if last.role != "assistant" || !last.tool_calls.is_empty() { return Err(E::WorkInProgress); }
        let report = opencode_assistant_report(&path, session_id).ok_or(E::NoReport)?;
        let published_at_ms = rows.last().and_then(|row| row.1.pointer("/info/time/completed"))
            .and_then(|time| time.as_i64()).ok_or(E::NoTimestamp)?;
        let snapshot = ReportSnapshot { text: crate::secret_scrubber::SecretScrubber::scrub(&report.text), revision: report.revision, published_at_ms,
            // Message completion is not a native session-idle boundary.
            turn_finished: false,
            source: ReportSource::OpenCode { path, session_id: session_id.into(), rows } };
        return if snapshot.is_current() { Ok(snapshot) } else { Err(E::ChangedDuringRead) };
    }
    read_file(&locate_transcript(format, session_id, node_path).ok_or(E::NoTranscript)?, format)
}

#[cfg(test)]
fn from_file(path: &Path, format: TranscriptFormat) -> Option<ReportSnapshot> {
    read_file(path, format).ok()
}

fn read_file(path: &Path, format: TranscriptFormat) -> Result<ReportSnapshot, ReportReadError> {
    use ReportReadError as E;
    let metadata = fs::metadata(path).map_err(|_| E::Unreadable)?;
    let mut file = fs::File::open(path).map_err(|_| E::Unreadable)?;
    if metadata.len() == 0 { return Err(E::NoReport); }
    file.seek(SeekFrom::End(-1)).map_err(|_| E::Unreadable)?;
    let mut last_byte = [0];
    file.read_exact(&mut last_byte).map_err(|_| E::Unreadable)?;
    if last_byte[0] != b'\n' { return Err(E::PartialPublication); }
    let start = metadata.len().saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(start)).map_err(|_| E::Unreadable)?;
    let mut reader = BufReader::new(file.take(metadata.len() - start));
    if start > 0 { reader.read_until(b'\n', &mut Vec::new()).map_err(|_| E::Unreadable)?; }
    let lines = reader.lines().collect::<Result<Vec<_>, _>>().map_err(|_| E::Unreadable)?;
    let mut last = None;
    let mut published_at_ms = None;
    for line in &lines {
        if line.trim().is_empty() { continue; }
        let value = serde_json::from_str::<serde_json::Value>(line).map_err(|_| E::MalformedRecord)?;
        // Only the last dialogue record decides readiness; full-turn parsing
        // coalesces earlier tool calls with a later final answer.
        let parsed = parse_transcript(format, std::iter::once(line.clone()), 1);
        if parsed.saw_malformed { return Err(E::MalformedRecord); }
        if let Some(turn) = parsed.turns.into_iter().last() {
            published_at_ms = record_time(format, &value);
            last = Some(turn);
        }
    }
    let last = last.ok_or(E::NoReport)?;
    if last.role != "assistant" || !last.tool_calls.is_empty() { return Err(E::WorkInProgress); }
    let turn_finished = match format {
        TranscriptFormat::Codex => {
            let completion = adapter::dispatch("codex").and_then(|adapter| adapter.completed_turn(&lines.join("\n")))
                .ok_or(E::NoNativeCompletion)?;
            published_at_ms = Some(completion.completed_at_ms);
            true
        }
        TranscriptFormat::Muse => {
            if !crate::services::muse_watcher::report_turn_finished(&lines) { return Err(E::NoNativeCompletion); }
            true
        }
        TranscriptFormat::CommandCode => {
            if !crate::services::commandcode_watcher::report_turn_finished(&lines) { return Err(E::NoNativeCompletion); }
            true
        }
        _ => false,
    };
    let report = assistant_report_from_file(path, format).ok_or(E::NoReport)?;
    let snapshot = ReportSnapshot {
        text: crate::secret_scrubber::SecretScrubber::scrub(&report.text), revision: report.revision,
        published_at_ms: published_at_ms.ok_or(E::NoTimestamp)?, turn_finished,
        source: ReportSource::File { path: path.into(), length: metadata.len(), modified: metadata.modified().map_err(|_| E::Unreadable)? },
    };
    if snapshot.is_current() { Ok(snapshot) } else { Err(E::ChangedDuringRead) }
}

fn record_time(format: TranscriptFormat, value: &serde_json::Value) -> Option<i64> {
    if format == TranscriptFormat::Muse { return value.get("recorded_at")?.as_i64().map(|micros| micros / 1000); }
    let timestamp = match format {
        TranscriptFormat::Mcode => value.pointer("/message/timestamp"),
        TranscriptFormat::Agy => value.get("created_at"),
        TranscriptFormat::CommandCode => value.get("timestamp").or_else(|| value.pointer("/message/meta/createdAt")),
        _ => value.get("timestamp"),
    }?;
    timestamp.as_i64().or_else(|| chrono::DateTime::parse_from_rfc3339(timestamp.as_str()?).ok().map(|time| time.timestamp_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot::circuit::{context::CircuitContext, model::{CircuitGraph, CircuitNode, CircuitNodeKind, CircuitEdge},
        observation::{ObservationIdentity, WorkEvidence}, stepper::*};

    fn classified_run(snapshot: ReportSnapshot) -> (RunView, CircuitEvent) {
        let mut context = CircuitContext::new();
        context.set("source.agent_id", "900");
        let run = RunView { run_id: 42, state: RunState::Running, context,
            graph: CircuitGraph { version: 1, blueprint: None, edges: vec![CircuitEdge {
                from: "await_source".into(), to: "done".into(), condition: Default::default(),
            }], nodes: vec![CircuitNode {
                id: "await_source".into(), kind: CircuitNodeKind::AwaitAgentTurn { target_node_id: Some("$source".into()) },
            }, CircuitNode { id: "done".into(), kind: CircuitNodeKind::Notify { message: "Finished".into() } }] },
            steps: vec![StepView { node_id: "await_source".into(), status: StepStatus::Unverified, attempt: 1,
                outcome: None, error: Some("Missing ownership adapter".into()), agent_node_id: None }],
        };
        let event = CircuitEvent::TurnClassified { node_id: "await_source".into(),
            classification: Some(crate::autopilot::evaluator::Classification::Completed), output: Some(snapshot.text.clone()),
            binding: Some(ClassificationBinding {
                owner: ObservationIdentity { run_id: 42, step_id: "await_source".into(), attempt: 1, agent_node_id: 900,
                    session_incarnation: Some("100".into()), session_id: Some("session".into()), turn_id: None,
                    report_revision: Some(snapshot.revision.clone()) },
                report_revision: snapshot.revision.clone(),
                input_guard: ObservationInputFence { transcript_guard: None, report_guard: Some(snapshot.clone()),
                    agent_node_id: 900, input_stamp: "1:0".into(), observed_at_ms: snapshot.published_at_ms,
                    session_id: "session".into(), session_incarnation: "100".into() },
            }),
        };
        (run, event)
    }

    #[test]
    fn native_report_preflight_recovers_running_projection_and_unverified_checkpoint() {
        use crate::services::circuit_worker::readiness;
        use crate::models::{AgentNode, SessionStatus};
        use crate::agent::process::InputUnavailable;
        use crate::autopilot::circuit::observation::CircuitObservationBlocker as B;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        // Minimal replay of run 239's native final-answer/task_complete pair.
        let lines = concat!(
            "{\"type\":\"response_item\",\"timestamp\":\"2026-09-26T21:31:29.928Z\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"Raised the PR. Checks passed.\"}]}}\n",
            "{\"type\":\"event_msg\",\"timestamp\":\"2026-09-26T21:31:31.048Z\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn\",\"last_agent_message\":\"Raised the PR. Checks passed.\"}}\n"
        );
        fs::write(&path, lines).unwrap();
        let report = read_file(&path, TranscriptFormat::Codex).unwrap();
        assert!(report.turn_finished);
        let (mut run, _) = classified_run(report.clone());
        let agent = AgentNode { id: 900, provider: "codex".into(), cli_session_id: Some("session".into()),
            status: SessionStatus::Running, ..Default::default() };
        for status in [StepStatus::Running, StepStatus::Unverified] {
            run.step_mut("await_source").unwrap().status = status;
            let candidate = readiness::prepare(&run, "await_source", &agent, Some("100:projection"), Ok("1:0".into()), Ok(report.clone())).unwrap().unwrap();
            assert_eq!(candidate.status, SessionStatus::Ready);
            let mut recovered = run.clone();
            let transition = advance(&mut recovered, &CircuitEvent::TurnClassified {
                node_id: "await_source".into(), classification: Some(crate::autopilot::evaluator::Classification::Completed),
                output: Some(candidate.output), binding: Some(candidate.binding),
            });
            assert_eq!(recovered.state, RunState::Completed);
            assert!(!transition.classifications[0].lifecycle_verified, "report handoff must not fabricate owned-work proof");
        }
        for (input, blocker) in [(InputUnavailable::Draft, B::InputDraft), (InputUnavailable::UnknownInput, B::InputUncertain), (InputUnavailable::Paste, B::InputPaste)] {
            assert_eq!(readiness::prepare(&run, "await_source", &agent, Some("100:projection"), Err(input), Ok(report.clone())).err(), Some(blocker));
        }
        run.context.set("agent.900.previous_report_revision", &report.revision);
        assert_eq!(readiness::prepare(&run, "await_source", &agent, Some("100:projection"), Ok("1:0".into()), Ok(report)).err(), Some(B::ReportSuperseded));
        fs::write(&path, format!("{lines}{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"next\"}}}}\n")).unwrap();
        assert_eq!(read_file(&path, TranscriptFormat::Codex).unwrap_err(), ReportReadError::NoNativeCompletion);
    }

    #[test]
    fn report_read_failures_preserve_the_actual_observation_problem() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        for (body, reason) in [
            ("", ReportReadError::NoReport),
            ("{\"type\":", ReportReadError::PartialPublication),
            ("not json\n", ReportReadError::MalformedRecord),
            ("{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"Next task\"}}\n", ReportReadError::WorkInProgress),
        ] {
            fs::write(&path, body).unwrap();
            assert_eq!(read_file(&path, TranscriptFormat::CommandCode).unwrap_err(), reason);
        }
    }

    #[test]
    fn a_guarded_report_recovers_a_checkpoint_without_fabricating_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, "{\"type\":\"message\",\"timestamp\":\"2026-09-25T12:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"Finished implementation and checks.\"}}\n").unwrap();
        let snapshot = from_file(&path, TranscriptFormat::CommandCode).unwrap();
        let (mut run, event) = classified_run(snapshot);
        let transition = advance(&mut run, &event);
        assert_eq!(run.step("await_source").unwrap().status, StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
        assert!(!transition.classifications[0].lifecycle_verified);
        assert!(transition.input_guard.as_ref().unwrap().report_guard.is_some());
        assert!(run.context.get("node.await_source.evidence.1").is_none());
        assert_eq!(run.context.get("source.output"), Some("Finished implementation and checks."));

        fs::write(&path, "{\"type\":\"message\",\"timestamp\":\"2026-09-25T12:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"New task\"}}\n").unwrap();
        let error = crate::db::circuit::evidence::commit_transition(42, Some("completed"), "{}", &[],
            crate::db::circuit::evidence::EvidenceWrite { input_guard: transition.input_guard.as_ref(), ..Default::default() }).unwrap_err();
        assert!(crate::db::circuit::evidence::is_observation_freshness_rejection(&error));
    }

    #[test]
    fn reported_completion_does_not_bypass_known_owned_work_or_mismatched_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, "{\"type\":\"message\",\"timestamp\":\"2026-09-25T12:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"Done.\"}}\n").unwrap();
        let snapshot = from_file(&path, TranscriptFormat::CommandCode).unwrap();
        let (mut run, event) = classified_run(snapshot.clone());
        let mut evidence = WorkEvidence::default();
        evidence.children.insert("background-task".into(), false);
        run.context.set("node.await_source.evidence.1", serde_json::to_string(&evidence).unwrap());
        let transition = advance(&mut run, &event);
        assert_eq!(run.step("await_source").unwrap().status, StepStatus::Unverified);
        assert!(transition.effects.is_empty());

        let (mut run, mut event) = classified_run(snapshot);
        if let CircuitEvent::TurnClassified { binding: Some(binding), .. } = &mut event { binding.owner.attempt = 2; }
        advance(&mut run, &event);
        assert_eq!(run.step("await_source").unwrap().status, StepStatus::Unverified);
    }

    #[test]
    fn report_snapshot_rejects_tools_user_input_and_partial_publication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let report = serde_json::json!({"type":"message","timestamp":"2026-09-25T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Done. All checks pass."}]}}).to_string() + "\n";
        fs::write(&path, &report).unwrap();
        let snapshot = from_file(&path, TranscriptFormat::CommandCode).unwrap();
        assert_eq!(snapshot.text, "Done. All checks pass.");
        assert!(snapshot.is_current());
        for suffix in [
            serde_json::json!({"type":"message","timestamp":"2026-09-25T12:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool","name":"bash","input":{}}]}}).to_string() + "\n",
            serde_json::json!({"type":"message","timestamp":"2026-09-25T12:00:00Z","message":{"role":"user","content":"Do another task"}}).to_string() + "\n",
            serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"background","content":"Finished"}]}}).to_string() + "\n",
            "{\"type\":\"message\"".into(),
        ] {
            fs::write(&path, format!("{report}{suffix}")).unwrap();
            assert!(!snapshot.is_current());
            assert!(from_file(&path, TranscriptFormat::CommandCode).is_none());
        }
    }

    #[test]
    fn codex_report_snapshot_requires_the_current_native_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let report = serde_json::json!({"type":"response_item","timestamp":"2026-09-25T12:00:00Z",
            "payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}).to_string() + "\n";
        let complete = serde_json::json!({"type":"event_msg","timestamp":"2026-09-25T12:00:01Z",
            "payload":{"type":"task_complete","turn_id":"turn","last_agent_message":"Done."}}).to_string() + "\n";
        fs::write(&path, format!("{report}{complete}")).unwrap();
        assert_eq!(from_file(&path, TranscriptFormat::Codex).unwrap().published_at_ms, 1790337601000);
        for suffix in [
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"next"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"reasoning","summary":[]}}),
        ] {
            fs::write(&path, format!("{report}{complete}{suffix}\n")).unwrap();
            assert!(from_file(&path, TranscriptFormat::Codex).is_none());
        }
    }

    #[test]
    fn report_snapshots_cover_recent_harnesses_and_muse_turn_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let muse_report = serde_json::json!({"payload_type":"runtime.session","recorded_at":1790337600000000_i64,
            "payload":{"kind":"run","run_id":"run","event":{"kind":"assistant_message_committed","text":"Done.","message_id":"report"}}});
        let muse_terminal = serde_json::json!({"payload_type":"runtime.session","recorded_at":1790337601000000_i64,
            "payload":{"kind":"run","run_id":"run","event":{"kind":"terminal","terminal":"completed"}}});
        for (format, lines) in [
            (TranscriptFormat::CommandCode, vec![serde_json::json!({"type":"message","timestamp":"2026-09-25T12:00:00Z","message":{"role":"assistant","content":"Done."}})]),
            (TranscriptFormat::CommandCode, vec![serde_json::json!({"type":"message","id":"final","message":{"role":"assistant","content":[{"type":"text","text":"Done."}],"meta":{"source":"model","createdAt":1790337600000_i64,"messageId":"model-final"}}})]),
            (TranscriptFormat::Muse, vec![muse_report.clone(), muse_terminal.clone()]),
            (TranscriptFormat::Mcode, vec![serde_json::json!({"message_id":"message","turn_id":"turn","message":{"role":"assistant","timestamp":1790337600000_i64,"content":[{"type":"text","text":"Done."}]}})]),
            (TranscriptFormat::Agy, vec![serde_json::json!({"source":"MODEL","created_at":"2026-09-25T12:00:00Z","content":"Done.","status":"DONE"})]),
        ] {
            fs::write(&path, lines.iter().map(|line| format!("{line}\n")).collect::<String>()).unwrap();
            let snapshot = from_file(&path, format).unwrap_or_else(|| panic!("missing report for {format:?}"));
            assert_eq!(snapshot.text, "Done.");
            assert_eq!(snapshot.published_at_ms, 1790337600000);
        }
        let started = serde_json::json!({"payload_type":"runtime.session","recorded_at":1790337602000000_i64,
            "payload":{"kind":"run","run_id":"next","event":{"kind":"started"}}});
        fs::write(&path, format!("{muse_report}\n{muse_terminal}\n{started}\n")).unwrap();
        assert!(from_file(&path, TranscriptFormat::Muse).is_none());
        fs::write(&path, format!("{muse_report}\n{muse_terminal}\n{started}\n{muse_terminal}\n")).unwrap();
        assert!(from_file(&path, TranscriptFormat::Muse).is_none(), "a duplicate old terminal cannot finish the newer run");
    }

    #[test]
    fn review_blueprint_reaches_approval_from_guarded_reports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, "{\"type\":\"message\",\"timestamp\":\"2026-09-25T12:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"APPROVED: implementation and tests checked.\"}}\n").unwrap();
        let snapshot = from_file(&path, TranscriptFormat::CommandCode).unwrap();
        let (mut run, source_event) = classified_run(snapshot);
        run.graph = CircuitGraph::agent_review(None, None, 3);
        run.steps.clear();
        run.state = RunState::Pending;
        advance(&mut run, &CircuitEvent::Triggered);
        let tick = CircuitEvent::Tick(Capacity { circuit_free_slots: 2, agent_free_slots: 1 });
        advance(&mut run, &tick);
        advance(&mut run, &source_event);
        advance(&mut run, &tick);
        assert_eq!(run.step("reviewer").unwrap().status, StepStatus::Running);
        run.attach_agent_node("reviewer", 901);
        for step_id in ["reviewer", "verdict"] {
            let mut event = source_event.clone();
            let CircuitEvent::TurnClassified { node_id, binding: Some(binding), .. } = &mut event else { panic!("classification fixture"); };
            *node_id = step_id.into();
            binding.owner.step_id = step_id.into();
            binding.owner.agent_node_id = 901;
            binding.input_guard.agent_node_id = 901;
            let transition = advance(&mut run, &event);
            assert!(transition.input_guard.is_some());
            assert!(!transition.classifications[0].lifecycle_verified);
            advance(&mut run, &tick);
        }
        assert_eq!(run.context.get("node.verdict.review_verdict"), Some("approved"));
        assert_eq!(run.step("approved").unwrap().status, StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
    }
}
