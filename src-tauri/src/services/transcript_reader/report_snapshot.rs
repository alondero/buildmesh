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
        // Resolve the env-aware SQLite store, then read it. The resolve/read
        // split mirrors `read_opencode_tail` and lets the store-backed read be
        // exercised against a temporary DB without touching `$HOME`.
        let (path, session_id) = opencode_resolve(Some(session_id), node_path).map_err(|_| E::NoTranscript)?;
        return read_opencode_file(&path, session_id);
    }
    if format == TranscriptFormat::Cline {
        // Issue #1776: Cline *has* a transcript reader (the Node Digest reads
        // `<id>.messages.json`), but that reader is a whole-document parser and
        // this one is line-oriented: it requires a trailing newline, parses
        // each line as a standalone JSON record, and derives the publication
        // time from a per-record `timestamp`. A Cline document is a single JSON
        // object, so routing it here would not merely fail — it would fail
        // *misleadingly*, reporting `PartialPublication` ("still publishing a
        // transcript record") or `NoReport` ("has not published an assistant
        // report") for a node whose document is complete. `Unsupported` is both
        // the truth and the useful answer: `readiness::prepare` admits
        // hook-native evidence for exactly this variant, so a Cline circuit
        // continues on its `agent_end` receipt instead of being walled off.
        return Err(E::Unsupported);
    }
    read_file(&locate_transcript(format, session_id, node_path).ok_or(E::NoTranscript)?, format)
}

/// Read an OpenCode session's report from its SQLite store. The store is the
/// OpenCode equivalent of a transcript file, so a malformed message record must
/// degrade to `MalformedRecord` exactly like the file-backed readers rather than
/// masquerading as "no report yet".
fn read_opencode_file(path: &Path, session_id: &str) -> Result<ReportSnapshot, ReportReadError> {
    use ReportReadError as E;
    let rows = read_opencode_message_rows(path, session_id, OPENCODE_DIGEST_WINDOW).ok_or(E::Unreadable)?;
    let parsed = parse_opencode_messages(&rows.iter().map(|(_, value)| value.clone()).collect::<Vec<_>>(), 1);
    if parsed.saw_malformed { return Err(E::MalformedRecord); }
    let last = parsed.turns.last().ok_or(E::NoReport)?;
    if last.role != "assistant" || !last.tool_calls.is_empty() { return Err(E::WorkInProgress); }
    let report = opencode_assistant_report(path, session_id).ok_or(E::NoReport)?;
    let published_at_ms = rows.last().and_then(|row| row.1.pointer("/info/time/completed"))
        .and_then(|time| time.as_i64()).ok_or(E::NoTimestamp)?;
    let snapshot = ReportSnapshot { text: crate::secret_scrubber::SecretScrubber::scrub(&report.text), revision: report.revision, published_at_ms,
        // Message completion is not a native session-idle boundary.
        turn_finished: false,
        source: ReportSource::OpenCode { path: path.into(), session_id: session_id.into(), rows } };
    if snapshot.is_current() { Ok(snapshot) } else { Err(E::ChangedDuringRead) }
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
    use crate::circuit::{context::CircuitContext, model::{CircuitGraph, CircuitNode, CircuitNodeKind, CircuitEdge},
        observation::{ObservationIdentity, WorkEvidence}, stepper::*};

    #[test]
    fn circuit_report_preserves_long_review_verdict_and_tail_revision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.jsonl");
        let findings = "Inspected source and verified the requirements.\n".repeat(150);
        let write = |verdict: &str| {
            let text = format!("{findings}\nVerdict: {verdict}");
            fs::write(&path, format!("{}\n", serde_json::json!({
                "type": "assistant", "timestamp": "2026-09-27T22:30:00Z",
                "message": {"id": "review", "role": "assistant", "content": [{"type":"text", "text":text}]}
            }))).unwrap();
            text
        };
        let expected = write("Approve");
        let approved = read_file(&path, TranscriptFormat::ClaudeCode).unwrap();
        assert_eq!(approved.text, expected, "a circuit must classify the verdict, not a display preview");
        let expected = write("Decline");
        let rejected = read_file(&path, TranscriptFormat::ClaudeCode).unwrap();
        assert_eq!(rejected.text, expected);
        assert_ne!(approved.revision, rejected.revision);
        assert_eq!(approved.revision.split(':').next(), rejected.revision.split(':').next(),
            "equal-length edits exercise content hashing, not just record position");
        let legacy = approved.revision.rsplit_once(':').unwrap().0;
        assert!(same_assistant_revision(&approved.revision, legacy));
        assert!(same_assistant_revision(&rejected.revision, legacy),
            "old preview-only boundaries cannot authorize unseen tail edits in the same record");
        assert!(!same_assistant_revision(&rejected.revision, &approved.revision));
        let TranscriptTail::Available { last_assistant_message: Some(preview), .. } =
            read_last_assistant_message_from_file(&path, TranscriptFormat::ClaudeCode)
        else { panic!("expected a display preview"); };
        assert!(preview.len() <= types::MAX_TURN_TEXT + '…'.len_utf8());
    }

    /// Issue #1776: wiring the Cline transcript reader must **not** silently
    /// hand a Cline document to the line-oriented report reader. Two halves:
    ///
    /// 1. The hazard, pinned on a real, valid Cline fixture — the document
    ///    parses as JSON (it is one object) but every line-oriented
    ///    precondition fails or misfires, so `read_file` reports a *wrong*
    ///    reason rather than "this format has no report adapter".
    /// 2. The guard — `read` refuses the format up front, with the honest
    ///    `Unsupported`, and never resolves a path.
    #[test]
    fn cline_document_is_never_read_as_a_line_oriented_report() {
        let path = crate::services::transcript_reader::tests::fixture("cline_messages.json");
        // The fixture is a genuinely valid Cline document (a single JSON
        // object), not a malformed stand-in.
        let document = fs::read_to_string(&path).expect("checked-in Cline fixture");
        assert!(serde_json::from_str::<serde_json::Value>(&document).is_ok(),
            "the fixture must be a valid Cline document for this test to mean anything");
        assert!(parse_transcript(TranscriptFormat::Cline, document.split('\n').map(str::to_string), 4)
            .turns.iter().any(|turn| turn.role == "assistant"),
            "the digest reader must find real turns in it");

        // (1) What the line-oriented reader would claim, and why each reason is
        // a lie for a complete Cline document. Both trailing-newline shapes
        // matter: Cline's non-atomic `writeFileSync(JSON.stringify(...))` emits
        // none, but a document can be read after a newline-terminated write.
        let no_newline = crate::services::transcript_reader::tests::write_fixture(
            "cline_no_trailing_newline",
            document.trim_end(),
        );
        assert_eq!(
            read_file(&no_newline, TranscriptFormat::Cline).err(),
            Some(ReportReadError::PartialPublication),
            "a missing trailing newline in a whole-document transcript is not a partial publication"
        );
        assert_eq!(
            read_file(&path, TranscriptFormat::Cline).err(),
            Some(ReportReadError::NoReport),
            "a document has no assistant *line*; 'has not published an assistant report' is wrong"
        );
        std::fs::remove_file(&no_newline).ok();

        // (2) The guard, at the seam the circuit actually calls.
        assert_eq!(
            read(TranscriptFormat::Cline, "session_1790003303940_9ouga", ".").err(),
            Some(ReportReadError::Unsupported),
            "Cline has no report adapter; the circuit must be told so, not sent through the JSONL reader"
        );
    }

    /// Why the guard's *exact* reason matters. `readiness::prepare` keeps
    /// hook-native evidence only when the report error is
    /// `Unsupported | NoTranscript | Unreadable`; every other reason
    /// (`NoReport`, `PartialPublication`, …) discards it. Cline's only turn
    /// evidence is the `agent_end` receipt, so a document falling through to the
    /// line-oriented reader would not merely report a misleading string — it
    /// would discard the receipt and stall the circuit behind a blocker it can
    /// never clear. See the `readiness` match arm for the other half of this
    /// coupling.
    #[test]
    fn cline_report_reason_stays_in_the_hook_native_admitted_set() {
        let reason = read(TranscriptFormat::Cline, "session_1790003303940_9ouga", ".").unwrap_err();
        assert_eq!(reason, ReportReadError::Unsupported);
        assert!(matches!(
            reason,
            ReportReadError::Unsupported | ReportReadError::NoTranscript | ReportReadError::Unreadable
        ), "reason {reason:?} must stay in the set that admits hook-native evidence");
        // The reasons the guard exists to avoid are all *outside* that set.
        for outside in [ReportReadError::NoReport, ReportReadError::PartialPublication] {
            assert!(!matches!(
                outside,
                ReportReadError::Unsupported | ReportReadError::NoTranscript | ReportReadError::Unreadable
            ), "{outside:?} must not be the reason a Cline circuit sees");
        }
    }

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
            classification: Some(crate::circuit::evaluator::Classification::Completed), output: Some(snapshot.text.clone()),
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
    fn explicit_mcode_handoff_recovers_running_without_claiming_native_completion() {
        use crate::services::circuit_worker::readiness;
        use crate::models::{AgentNode, SessionStatus};
        use crate::agent::process::InputUnavailable;
        use crate::circuit::observation::CircuitObservationBlocker as B;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        let lines = format!("{}\n{}\n", serde_json::json!({
            "message_id":"final", "turn_id":"fixes", "message": {"role":"assistant",
            "timestamp":1790718598978i64, "content":[{"type":"text",
            "text":"Both blocking findings fixed; 3582 tests passed.\nBUILDMESH_HANDOFF_V1: READY"}]}
        }), serde_json::json!({"message_id":"settlement", "turn_id":"fixes",
            "message":{"role":"custom", "customType":"background_task_read_settlement",
            "content":"", "timestamp":1790718611791i64}}));
        fs::write(&path, &lines).unwrap();
        let snapshot = read_file(&path, TranscriptFormat::Mcode).unwrap();
        assert!(!snapshot.turn_finished);
        let (mut run, _) = classified_run(snapshot.clone());
        run.context.set("source.review_preset", "1");
        let agent = AgentNode { id:900, provider:"mcode".into(), cli_session_id:Some("session".into()),
            status:SessionStatus::Running, ..Default::default() };
        for status in [StepStatus::Running, StepStatus::Unverified] {
            run.steps[0].status = status;
            let candidate = readiness::prepare(&run, "await_source", &agent, Some("100:projection"),
                Ok("1:0".into()), Ok(snapshot.clone())).unwrap().expect("explicit handoff");
            assert_eq!(candidate.status, SessionStatus::Ready);
            let mut routed = run.clone();
            let transition = advance(&mut routed, &CircuitEvent::TurnClassified {
                node_id:"await_source".into(), classification:Some(crate::circuit::evaluator::Classification::Completed),
                output:Some(candidate.output), binding:Some(candidate.binding) });
            assert_eq!(routed.state, RunState::Completed);
            assert!(!transition.classifications[0].lifecycle_verified);
            // Exercise the durable boundary too: a stale Running projection
            // must not contradict the readiness decision for this same report.
            let mut db = rusqlite::Connection::open_in_memory().unwrap();
            crate::db::init_schema(&db).unwrap();
            db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
                INSERT INTO agent_nodes(id,mesh_id,name,path,status,cli_session_id,session_started_at)
                VALUES(900,1,'agent','/repo','running','session',100);
                INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
                INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(42,1,1,'running');").unwrap();
            crate::db::circuit::evidence::commit_transition_locked(
                &mut db, 42, Some("completed"), &routed.context.to_json().unwrap(), &[],
                crate::db::circuit::evidence::EvidenceWrite {
                    input_guard: transition.input_guard.as_ref(),
                    classifications: &transition.classifications,
                    ..Default::default()
                },
            ).expect("the admitted report must commit despite a stale Running display");
        }
        for (input, blocker) in [(InputUnavailable::Draft, B::InputDraft),
            (InputUnavailable::UnknownInput, B::InputUncertain), (InputUnavailable::Paste, B::InputPaste)] {
            assert_eq!(readiness::prepare(&run, "await_source", &agent, Some("100:projection"),
                Err(input), Ok(snapshot.clone())).err(), Some(blocker));
        }
        let mut waiting = run.clone();
        waiting.context.set("node.await_source.human_wait", "1");
        assert_eq!(readiness::prepare(&waiting, "await_source", &agent, Some("100:projection"),
            Ok("1:0".into()), Ok(snapshot.clone())).err(), Some(B::HumanResponseRequired));
        for (evidence, blocker) in [
            (WorkEvidence { conflicted:true, ..Default::default() }, B::EvidenceConflict),
            (WorkEvidence { children: [("child".into(), false)].into_iter().collect(), ..Default::default() }, B::KnownWorkOutstanding),
        ] {
            let mut blocked = run.clone();
            blocked.context.set("node.await_source.evidence.1", serde_json::to_string(&evidence).unwrap());
            assert_eq!(readiness::prepare(&blocked, "await_source", &agent, Some("100:projection"),
                Ok("1:0".into()), Ok(snapshot.clone())).err(), Some(blocker));
        }
        run.context.set("agent.900.previous_report_revision", &snapshot.revision);
        assert_eq!(readiness::prepare(&run, "await_source", &agent, Some("100:projection"),
            Ok("1:0".into()), Ok(snapshot.clone())).err(), Some(B::ReportSuperseded));
        fs::write(&path, format!("{lines}{}\n", serde_json::json!({"message_id":"new-work", "turn_id":"next",
            "message":{"role":"assistant", "timestamp":1790718612000i64, "content":[
                {"type":"toolCall", "id":"call", "name":"bash", "arguments":{}}]}}))).unwrap();
        assert!(!snapshot.is_current());
        assert_eq!(read_file(&path, TranscriptFormat::Mcode).unwrap_err(), ReportReadError::WorkInProgress);
    }

    #[test]
    fn native_report_preflight_recovers_running_projection_and_unverified_checkpoint() {
        use crate::services::circuit_worker::readiness;
        use crate::models::{AgentNode, SessionStatus};
        use crate::agent::process::InputUnavailable;
        use crate::circuit::observation::CircuitObservationBlocker as B;
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
                node_id: "await_source".into(), classification: Some(crate::circuit::evaluator::Classification::Completed),
                output: Some(candidate.output), binding: Some(candidate.binding),
            });
            assert_eq!(recovered.state, RunState::Completed);
            assert!(!transition.classifications[0].lifecycle_verified, "report handoff must not fabricate owned-work proof");
        }
        for (input, blocker) in [(InputUnavailable::Draft, B::InputDraft), (InputUnavailable::UnknownInput, B::InputUncertain), (InputUnavailable::Paste, B::InputPaste)] {
            assert_eq!(readiness::prepare(&run, "await_source", &agent, Some("100:projection"), Err(input), Ok(report.clone())).err(), Some(blocker));
        }
        let suspended = AgentNode { status: SessionStatus::Suspended, cli_session_id: None, ..agent.clone() };
        assert_eq!(readiness::prepare(&run, "await_source", &suspended, Some("100:projection"),
            Err(InputUnavailable::MissingProcess), Err(ReportReadError::NoTranscript)).err(), Some(B::ProcessUnavailable));

        // A resumed process replaces a status-only identity without weakening
        // the report/session/input fences used by the production handoff.
        use crate::circuit::observation::{CircuitObservation, ObservedWorkFact};
        let owner = ObservationIdentity { run_id: 42, step_id: "await_source".into(), attempt: 1,
            agent_node_id: 900, session_incarnation: Some("50".into()), session_id: Some("session".into()),
            turn_id: None, report_revision: None };
        for incarnation in ["50", "100"] {
            let expected = ObservationIdentity { session_incarnation: Some(incarnation.into()), ..owner.clone() };
            advance(&mut run, &CircuitEvent::Observed { expected: expected.clone(), observation: Box::new(CircuitObservation {
                identity: expected, source: "agent_status_projection".into(), source_id: Some(incarnation.into()),
                observed_at_ms: incarnation.parse().unwrap(), authoritative: false, fact: ObservedWorkFact::Working,
            }) });
        }
        let candidate = readiness::prepare(&run, "await_source", &agent, Some("100:projection"),
            Ok("1:0".into()), Ok(report.clone())).unwrap().unwrap();
        let mut resumed = run.clone();
        advance(&mut resumed, &CircuitEvent::TurnClassified { node_id: "await_source".into(),
            classification: Some(crate::circuit::evaluator::Classification::Completed),
            output: Some(candidate.output), binding: Some(candidate.binding) });
        assert_eq!(resumed.state, RunState::Completed);
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

    // --- Per-harness identity-bound report coverage (#1912) ---
    //
    // Every harness with a wired report adapter must read its own real
    // transcript shape into the immutable classification envelope, must fail
    // its own malformed/unavailable case explicitly, and must bind the exact
    // source/session/attempt/revision identity. Harness/format shape is
    // recorded beside each fixture; a report can never borrow another
    // provider's parser or authorize completion on a wrong/stale/cross-session
    // identity.

    struct FileReportCase {
        harness: &'static str,
        format: TranscriptFormat,
        /// The harness/transcript shape this fixture was written against.
        shape: &'static str,
        valid: Vec<String>,
        text: &'static str,
        published_at_ms: i64,
        turn_finished: bool,
        /// A record that keeps the harness envelope but breaks its shape.
        shape_changed: &'static str,
        /// A newer turn with no assistant report yet.
        unfinished: &'static str,
    }

    fn file_report_cases() -> Vec<FileReportCase> {
        vec![
            FileReportCase {
                harness: "anthropic", format: TranscriptFormat::ClaudeCode,
                shape: "claude-code ~/.claude/projects/<cwd>/<session>.jsonl",
                valid: vec![r#"{"type":"assistant","timestamp":"2026-09-25T12:00:00Z","message":{"id":"m","role":"assistant","content":[{"type":"text","text":"Done."}]}}"#.into()],
                text: "Done.", published_at_ms: 1_790_337_600_000, turn_finished: false,
                shape_changed: r#"{"type":"assistant","message":{"author":"assistant","blocks":[]}}"#,
                unfinished: r#"{"type":"user","timestamp":"2026-09-25T12:00:00Z","message":{"role":"user","content":"keep going"}}"#,
            },
            FileReportCase {
                harness: "cursor", format: TranscriptFormat::Cursor,
                shape: "cursor ~/.cursor/projects/<slug>/agent-transcripts/<session>.jsonl",
                valid: vec![r#"{"type":"assistant","timestamp":"2026-09-25T12:00:00Z","message":{"id":"m","role":"assistant","content":[{"type":"text","text":"Done."}]}}"#.into()],
                text: "Done.", published_at_ms: 1_790_337_600_000, turn_finished: false,
                shape_changed: r#"{"type":"assistant","message":{"author":"assistant","blocks":[]}}"#,
                unfinished: r#"{"type":"user","timestamp":"2026-09-25T12:00:00Z","message":{"role":"user","content":"keep going"}}"#,
            },
            FileReportCase {
                harness: "codex", format: TranscriptFormat::Codex,
                shape: "codex ~/.codex/sessions/YYYY/MM/DD/rollout-*-<session>.jsonl",
                valid: vec![
                    r#"{"type":"response_item","timestamp":"2026-09-25T12:00:00Z","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Done."}]}}"#.into(),
                    r#"{"type":"event_msg","timestamp":"2026-09-25T12:00:01Z","payload":{"type":"task_complete","turn_id":"turn","last_agent_message":"Done."}}"#.into(),
                ],
                text: "Done.", published_at_ms: 1_790_337_601_000, turn_finished: true,
                shape_changed: r#"{"type":"response_item","payload":{"type":"message","author":"assistant"}}"#,
                unfinished: r#"{"type":"response_item","timestamp":"2026-09-25T12:00:02Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"keep going"}]}}"#,
            },
            FileReportCase {
                harness: "commandcode", format: TranscriptFormat::CommandCode,
                shape: "commandcode ~/.commandcode/projects/<slug>/<session>.jsonl",
                valid: vec![r#"{"type":"message","timestamp":"2026-09-25T12:00:00Z","message":{"role":"assistant","content":"Done."}}"#.into()],
                text: "Done.", published_at_ms: 1_790_337_600_000, turn_finished: true,
                shape_changed: r#"{"type":"message","message":{"role":"assistant"}}"#,
                unfinished: r#"{"type":"message","timestamp":"2026-09-25T12:00:00Z","message":{"role":"user","content":"keep going"}}"#,
            },
            FileReportCase {
                harness: "agy", format: TranscriptFormat::Agy,
                shape: "agy brain/<conversation>/.system_generated/logs/transcript.jsonl",
                valid: vec![r#"{"source":"MODEL","created_at":"2026-09-25T12:00:00Z","content":"Done.","status":"DONE"}"#.into()],
                text: "Done.", published_at_ms: 1_790_337_600_000, turn_finished: false,
                shape_changed: r#"{"source":"USER_EXPLICIT"}"#,
                unfinished: r#"{"source":"USER_EXPLICIT","created_at":"2026-09-25T12:00:00Z","content":"keep going"}"#,
            },
            FileReportCase {
                harness: "grok", format: TranscriptFormat::Grok,
                shape: "grok ~/.grok/sessions/<urlencoded-cwd>/<session>/chat_history.jsonl",
                valid: vec![r#"{"role":"assistant","timestamp":"2026-09-25T12:00:00Z","content":"Done."}"#.into()],
                text: "Done.", published_at_ms: 1_790_337_600_000, turn_finished: false,
                shape_changed: r#"{"role":"assistant"}"#,
                unfinished: r#"{"role":"user","timestamp":"2026-09-25T12:00:00Z","content":"keep going"}"#,
            },
            FileReportCase {
                harness: "mcode", format: TranscriptFormat::Mcode,
                shape: "mcode <dataDir>/v2/sessions/<date>/<time>-session_<id>/messages.jsonl",
                valid: vec![r#"{"message_id":"m","turn_id":"t","message":{"role":"assistant","timestamp":1790337600000,"content":[{"type":"text","text":"Done."}]}}"#.into()],
                text: "Done.", published_at_ms: 1_790_337_600_000, turn_finished: false,
                shape_changed: r#"{"message_id":"m","turn_id":"t","message":{"role":"assistant","timestamp":1}}"#,
                unfinished: r#"{"message_id":"u","turn_id":"t","message":{"role":"user","timestamp":1790337600000,"content":[{"type":"text","text":"keep going"}]}}"#,
            },
            FileReportCase {
                harness: "muse", format: TranscriptFormat::Muse,
                shape: "muse ~/.local/share/muse/sessions/YYYY/MM/DD/<id>/session.jsonl",
                valid: vec![
                    r#"{"payload_type":"runtime.session","recorded_at":1790337600000000,"payload":{"kind":"run","run_id":"run","event":{"kind":"assistant_message_committed","text":"Done.","message_id":"report"}}}"#.into(),
                    r#"{"payload_type":"runtime.session","recorded_at":1790337601000000,"payload":{"kind":"run","run_id":"run","event":{"kind":"terminal","terminal":"completed"}}}"#.into(),
                ],
                text: "Done.", published_at_ms: 1_790_337_600_000, turn_finished: true,
                shape_changed: r#"{"payload_type":"runtime.session","payload":{"event":{"text":"broken"}}}"#,
                unfinished: r#"{"payload_type":"runtime.session","recorded_at":1790337602000000,"payload":{"kind":"run","run_id":"run","event":{"kind":"user_prompt_display","prompt":"keep going"}}}"#,
            },
        ]
    }

    fn valid_lines(lines: &[String]) -> String {
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    fn opencode_db(dir: &std::path::Path, session_id: &str, rows: &[(&str, i64, serde_json::Value)]) -> std::path::PathBuf {
        let db_path = dir.join("opencode.db");
        let _ = std::fs::remove_file(&db_path);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);").unwrap();
        for (id, created, data) in rows {
            conn.execute("INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, session_id, created, data.to_string()]).unwrap();
        }
        db_path
    }

    fn opencode_assistant_value(text: &str, completed: i64) -> serde_json::Value {
        serde_json::json!({"info":{"role":"assistant","time":{"completed":completed}},
            "parts":[{"type":"text","text":text}]})
    }

    /// Every wired report adapter's valid fixture, read through the real
    /// envelope, with its temporary store kept alive for later freshness checks.
    fn wired_report_snapshots() -> Vec<(&'static str, tempfile::TempDir, ReportSnapshot)> {
        let mut snapshots = Vec::new();
        for case in file_report_cases() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("session.jsonl");
            fs::write(&path, valid_lines(&case.valid)).unwrap();
            let snapshot = read_file(&path, case.format)
                .unwrap_or_else(|error| panic!("{} ({}) valid report failed: {error:?}", case.harness, case.shape));
            snapshots.push((case.harness, dir, snapshot));
        }
        let dir = tempfile::tempdir().unwrap();
        let db_path = opencode_db(dir.path(), "ses_000000000000000000000000",
            &[("msg-1", 1, opencode_assistant_value("Done.", 1_790_337_600_000))]);
        let snapshot = read_opencode_file(&db_path, "ses_000000000000000000000000").unwrap();
        snapshots.push(("opencode", dir, snapshot));
        snapshots
    }

    #[test]
    fn every_wired_report_adapter_reads_its_own_valid_shape_into_the_envelope() {
        for case in file_report_cases() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("session.jsonl");
            fs::write(&path, valid_lines(&case.valid)).unwrap();
            let report = read_file(&path, case.format)
                .unwrap_or_else(|error| panic!("{} ({}) valid report failed: {error:?}", case.harness, case.shape));
            assert_eq!(report.text, case.text, "{} text", case.harness);
            assert!(!report.revision.is_empty(), "{} must carry a report revision", case.harness);
            assert_eq!(report.published_at_ms, case.published_at_ms, "{} publication time", case.harness);
            assert_eq!(report.turn_finished, case.turn_finished, "{} native turn boundary", case.harness);
        }
        // OpenCode is SQLite-backed: its report read is the store seam.
        let dir = tempfile::tempdir().unwrap();
        let db_path = opencode_db(dir.path(), "ses_000000000000000000000000",
            &[("msg-1", 1, opencode_assistant_value("Done.", 1_790_337_600_000))]);
        let snapshot = read_opencode_file(&db_path, "ses_000000000000000000000000").unwrap();
        assert_eq!(snapshot.text, "Done.");
        assert!(snapshot.revision.starts_with("msg-1:"), "opencode revision is message-id + content hash");
        assert_eq!(snapshot.published_at_ms, 1_790_337_600_000);
        assert!(!snapshot.turn_finished, "message completion is not a native session-idle boundary");
    }

    #[test]
    fn every_wired_report_adapter_preserves_long_reports_but_bounds_previews() {
        let text = format!("{}\nVerdict: Request changes", "Reviewed requirement and test. ".repeat(220));
        for case in file_report_cases() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("session.jsonl");
            let lines = valid_lines(&case.valid).replace(
                &serde_json::to_string(case.text).unwrap(), &serde_json::to_string(&text).unwrap(),
            );
            fs::write(&path, lines).unwrap();
            let report = read_file(&path, case.format)
                .unwrap_or_else(|error| panic!("{}: {error:?}", case.harness));
            assert_eq!(report.text, text, "{} complete report", case.harness);
            let TranscriptTail::Available { last_assistant_message: Some(preview), .. } =
                read_last_assistant_message_from_file(&path, case.format)
            else { panic!("{} missing preview", case.harness); };
            assert!(preview.len() <= types::MAX_TURN_TEXT + '…'.len_utf8(), "{} preview", case.harness);
        }
        let dir = tempfile::tempdir().unwrap();
        let db_path = opencode_db(dir.path(), "ses_000000000000000000000000",
            &[("msg-1", 1, opencode_assistant_value(&text, 1_790_337_600_000))]);
        assert_eq!(read_opencode_file(&db_path, "ses_000000000000000000000000").unwrap().text, text);
    }

    #[test]
    fn every_wired_report_adapter_rejects_its_own_malformed_partial_and_unavailable_shapes() {
        for case in file_report_cases() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("session.jsonl");
            let valid = valid_lines(&case.valid);
            fs::write(&path, format!("{valid}not json\n")).unwrap();
            assert_eq!(read_file(&path, case.format).unwrap_err(), ReportReadError::MalformedRecord, "{} malformed json", case.harness);
            fs::write(&path, format!("{valid}{}\n", case.shape_changed)).unwrap();
            assert_eq!(read_file(&path, case.format).unwrap_err(), ReportReadError::MalformedRecord, "{} shape change", case.harness);
            fs::write(&path, format!("{valid}{{\"type\":\"message\"")).unwrap();
            assert_eq!(read_file(&path, case.format).unwrap_err(), ReportReadError::PartialPublication, "{} partial publication", case.harness);
            fs::write(&path, format!("{valid}{}\n", case.unfinished)).unwrap();
            assert_eq!(read_file(&path, case.format).unwrap_err(), ReportReadError::WorkInProgress, "{} unfinished turn", case.harness);
            fs::write(&path, String::new()).unwrap();
            assert_eq!(read_file(&path, case.format).unwrap_err(), ReportReadError::NoReport, "{} quiet transcript", case.harness);
            assert_eq!(read_file(&dir.path().join("missing.jsonl"), case.format).unwrap_err(), ReportReadError::Unreadable, "{} missing transcript", case.harness);
        }
    }

    #[test]
    fn opencode_report_adapter_rejects_its_own_malformed_unfinished_and_unavailable_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let session = "ses_000000000000000000000000";
        // Malformed: a recognised message envelope with no role.
        let db_path = opencode_db(dir.path(), session, &[("msg-1", 1, serde_json::json!({"info":{"author":"assistant"},"parts":[]}))]);
        assert_eq!(read_opencode_file(&db_path, session).unwrap_err(), ReportReadError::MalformedRecord);
        // Unfinished: the newest turn is a user prompt, then an in-flight tool call.
        let db_path = opencode_db(dir.path(), session, &[("msg-1", 1, serde_json::json!({"info":{"role":"user"},"parts":[{"type":"text","text":"keep going"}]}))]);
        assert_eq!(read_opencode_file(&db_path, session).unwrap_err(), ReportReadError::WorkInProgress);
        let db_path = opencode_db(dir.path(), session, &[("msg-1", 1, serde_json::json!({"info":{"role":"assistant"},"parts":[{"type":"tool","state":{"title":"read","input":{}}}]}))]);
        assert_eq!(read_opencode_file(&db_path, session).unwrap_err(), ReportReadError::WorkInProgress);
        // Unavailable: no messages at all, then no store on disk.
        let db_path = opencode_db(dir.path(), session, &[]);
        assert_eq!(read_opencode_file(&db_path, session).unwrap_err(), ReportReadError::NoReport);
        assert_eq!(read_opencode_file(&dir.path().join("missing.db"), session).unwrap_err(), ReportReadError::Unreadable);
    }

    #[test]
    fn every_wired_report_adapter_binds_exact_identity_into_the_classification_envelope() {
        use crate::services::circuit_worker::readiness;
        use crate::models::{AgentNode, SessionStatus};
        for (harness, _dir, snapshot) in wired_report_snapshots() {
            let (run, _) = classified_run(snapshot.clone());
            let agent = AgentNode { id: 900, provider: harness.into(), cli_session_id: Some("session".into()),
                status: SessionStatus::Ready, ..Default::default() };
            let candidate = readiness::prepare(&run, "await_source", &agent, Some("100:projection"), Ok("1:0".into()), Ok(snapshot.clone()))
                .unwrap_or_else(|blocker| panic!("{harness} report blocked: {blocker:?}"))
                .unwrap_or_else(|| panic!("{harness} report produced no candidate"));
            let binding = candidate.binding;
            assert_eq!(binding.owner.run_id, 42, "{harness}");
            assert_eq!(binding.owner.step_id, "await_source", "{harness}");
            assert_eq!(binding.owner.attempt, 1, "{harness}");
            assert_eq!(binding.owner.agent_node_id, 900, "{harness}");
            assert_eq!(binding.owner.session_id.as_deref(), Some("session"), "{harness}");
            assert_eq!(binding.owner.session_incarnation.as_deref(), Some("100"), "{harness}");
            assert_eq!(binding.owner.report_revision.as_deref(), Some(snapshot.revision.as_str()), "{harness} source revision");
            assert_eq!(binding.report_revision, snapshot.revision, "{harness}");
            let guard = binding.input_guard;
            assert_eq!(guard.report_guard.as_ref(), Some(&snapshot), "{harness} immutable envelope");
            assert_eq!(guard.agent_node_id, 900, "{harness}");
            assert_eq!(guard.input_stamp, "1:0", "{harness}");
            assert_eq!(guard.session_id, "session", "{harness}");
            assert_eq!(guard.session_incarnation, "100", "{harness}");
            assert_eq!(guard.observed_at_ms, snapshot.published_at_ms, "{harness}");
        }
    }

    #[test]
    fn superseded_and_stale_reports_cannot_bind_for_any_harness() {
        use crate::services::circuit_worker::readiness;
        use crate::models::{AgentNode, SessionStatus};
        use crate::circuit::observation::CircuitObservationBlocker as B;
        for (harness, _dir, snapshot) in wired_report_snapshots() {
            let agent = AgentNode { id: 900, provider: harness.into(), cli_session_id: Some("session".into()),
                status: SessionStatus::Ready, ..Default::default() };
            let (mut run, _) = classified_run(snapshot.clone());
            run.context.set("agent.900.previous_report_revision", &snapshot.revision);
            assert_eq!(
                readiness::prepare(&run, "await_source", &agent, Some("100:projection"), Ok("1:0".into()), Ok(snapshot.clone())).err(),
                Some(B::ReportSuperseded),
                "{harness} a superseded report must not bind"
            );
            let (run, _) = classified_run(snapshot.clone());
            let incarnation = snapshot.published_at_ms + 1_000;
            assert_eq!(
                readiness::prepare(&run, "await_source", &agent, Some(&format!("{incarnation}:projection")), Ok("1:0".into()), Ok(snapshot.clone())).err(),
                Some(B::ReportSuperseded),
                "{harness} a report published before the bound session incarnation must not bind"
            );
        }
    }

    #[test]
    fn wrong_session_attempt_agent_or_report_cannot_authorize_completion_for_any_harness() {
        for (harness, _dir, snapshot) in wired_report_snapshots() {
            type BindingMutation = fn(&mut ClassificationBinding);
            let mutations: [(&str, BindingMutation); 4] = [
                ("session", |binding| binding.owner.session_id = Some("other-session".into())),
                ("attempt", |binding| binding.owner.attempt = 2),
                ("agent", |binding| binding.owner.agent_node_id = 901),
                ("report_revision", |binding| binding.owner.report_revision = Some("other:0".into())),
            ];
            for (label, mutate) in mutations {
                let (mut run, mut event) = classified_run(snapshot.clone());
                let CircuitEvent::TurnClassified { binding: Some(binding), .. } = &mut event else { panic!("classification fixture") };
                mutate(binding);
                advance(&mut run, &event);
                assert_eq!(run.step("await_source").unwrap().status, StepStatus::Unverified, "{harness} {label} must not complete");
                assert_eq!(run.state, RunState::Running, "{harness} {label}");
                assert!(run.context.get("source.output").is_none(), "{harness} {label} must not publish the report");
            }
        }
    }

    #[test]
    fn a_bound_report_cannot_clear_a_human_wait_for_any_harness() {
        for (harness, _dir, snapshot) in wired_report_snapshots() {
            let (mut run, event) = classified_run(snapshot);
            run.context.set("node.await_source.human_wait", "1");
            let transition = advance(&mut run, &event);
            assert_eq!(run.step("await_source").unwrap().status, StepStatus::Unverified, "{harness}");
            assert_eq!(run.state, RunState::Running, "{harness}");
            assert!(transition.effects.is_empty(), "{harness} a report must not emit an effect over a human wait");
            assert!(run.context.get("source.output").is_none(), "{harness}");
        }
    }

    #[test]
    fn a_report_handoff_never_claims_native_lifecycle_or_owned_work_for_any_harness() {
        use crate::circuit::observation::ReportCompleteness;
        for (harness, _dir, snapshot) in wired_report_snapshots() {
            let (mut run, event) = classified_run(snapshot);
            let transition = advance(&mut run, &event);
            assert_eq!(run.step("await_source").unwrap().status, StepStatus::Completed, "{harness}");
            assert_eq!(run.state, RunState::Completed, "{harness}");
            let recorded = &transition.classifications[0];
            assert!(!recorded.lifecycle_verified, "{harness} a report cannot prove native lifecycle");
            assert!(matches!(recorded.report_completeness, ReportCompleteness::Partial),
                "{harness} an unproven-completeness report is Partial, not Complete");
            assert!(transition.input_guard.is_some(), "{harness} the immutable envelope must survive the handoff");
        }
    }
}
