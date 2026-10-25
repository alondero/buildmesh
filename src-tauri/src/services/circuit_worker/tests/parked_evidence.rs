// Circuit worker parked evidence tests. Included in the existing tests scope.
/// A gate whose lineage agent still owns unfinished work cannot bind a report:
/// `report_blocker` reports `KnownWorkOutstanding`, so a valid-looking
/// classification is refused and the gate parks Unverified. This is the
/// production shape behind issue #2138.
fn view_with_outstanding_lineage_work() -> RunView {
    use crate::circuit::observation::WorkEvidence;
    let mut view = report_gate_view();
    // Agent 900 is the implementer `finish_classifier` borrows its evidence
    // from, so its unfinished owned work blocks this gate's report binding.
    let evidence = WorkEvidence {
        children: [("child-1".to_string(), false)].into_iter().collect(),
        ..Default::default()
    };
    view.context.set(
        "node.implementer.evidence.1",
        serde_json::to_string(&evidence).unwrap(),
    );
    view
}

/// The refusal half of the key that does not come from `report_blocker`. A
/// classification can be refused with **no** blocker at all — the report simply
/// fails the binding fence — and this round's code stamps an *empty* blocker
/// key for that case and suppresses re-judgement on it. Without this, the
/// no-blocker refusal would fall through `has_unconsumed_classifier_evidence`
/// and be re-classified on every observation tick, which is the original #2138
/// defect.
#[test]
fn a_no_blocker_refusal_is_not_reclassified_on_an_unchanged_report() {
    use crate::circuit::stepper::ClassificationBinding;
    let mut view = report_gate_view();
    let report = "Published the PR but the push could not complete.";
    // No outstanding work, no lifecycle blocker, no conflict: whatever refuses
    // this classification is the binding fence itself.
    assert!(
        view.report_blocker("finish_classifier").is_none(),
        "this fixture exercises the no-blocker refusal path"
    );
    // A real binding whose report revision does not match the recorded report
    // is refused by `accepts_report_binding` without any blocker involved.
    let mut bound = crate::circuit::test_support::record_report_evidence(
        &mut view,
        "finish_classifier",
        report,
    );
    bound.report_revision = "a-stale-revision".into();
    let parked = advance(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: Some(ClassificationBinding {
                report_revision: bound.report_revision.clone(),
                ..bound
            }),
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Blocked),
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Unverified,
        "a report the fence refuses parks the gate: {:?}",
        parked.step_writes
    );
    assert_eq!(
        view.context
            .get("node.finish_classifier.evaluated_evidence_blocker")
            .map(str::to_owned),
        Some(String::new()),
        "the no-blocker refusal stamps an empty blocker key"
    );
    for tick in 0..5 {
        assert!(
            !should_classify_report(
                &view,
                "finish_classifier",
                SessionStatus::AwaitingInput,
                report,
                None
            ),
            "tick {tick} re-admitted an unchanged no-blocker refusal"
        );
    }
}

/// Regression guard (PR review of #2138): a gate parked because owned work was
/// open must recover when that work finishes, even though the report text and
/// the evidence owner are unchanged. The refusal depends on `report_blocker`
/// (open children, lifecycle blockers, evidence conflicts), none of which are
/// part of the report revision or the evidence owner — so a dedupe key built
/// from those two alone hides the clearing of the blocker forever.
#[test]
fn parked_gate_rejudges_when_the_blocking_work_completes() {
    use crate::circuit::observation::WorkEvidence;
    let mut view = view_with_outstanding_lineage_work();
    let report = "Published the PR but the push could not complete.";
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Blocked),
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Unverified,
        "open owned work refuses the binding and parks the gate"
    );
    // The child completes. Report text and evidence owner are untouched.
    let settled = WorkEvidence::default();
    view.context.set(
        "node.implementer.evidence.1",
        serde_json::to_string(&settled).unwrap(),
    );
    assert!(
        view.report_blocker("finish_classifier").is_none(),
        "the blocker has cleared"
    );
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            None
        ),
        "a cleared blocker must re-admit the parked gate, or it stays Unverified \
         until the agent writes a new report or someone presses Recheck"
    );
    // And the stepper must actually accept the verdict now the blocker is gone.
    let reopened = advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Continue),
            output: Some(report.into()),
        },
    );
    assert_ne!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Unverified,
        "the stepper re-checks what the worker hands it, so the gate recovers: {:?}",
        reopened.step_writes
    );
}

/// Issue #2138: a gate parked Unverified on an unbindable report recorded an
/// identical classification on every ~10s observation tick, because neither
/// `evaluated_report_revision` nor `classified_evidence_owner` was stamped when
/// the classification was refused. `should_classify_report` therefore read the
/// unchanged report as never judged and re-classified forever.
/// Fails on the pre-fix code, which re-admits the unchanged report.
#[test]
fn parked_unverified_gate_is_not_reclassified_on_an_unchanged_report() {
    let mut view = view_with_outstanding_lineage_work();
    let report = "Published the PR but the push could not complete.";
    let parked = advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Blocked),
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Unverified,
        "an unbindable classification parks the gate: {:?}",
        parked.step_writes
    );
    assert_eq!(parked.classifications.len(), 1);
    for _ in 0..5 {
        assert!(
            !should_classify_report(
                &view,
                "finish_classifier",
                SessionStatus::AwaitingInput,
                report,
                None
            ),
            "an unchanged report bound to a parked gate must not be judged again"
        );
    }
}

/// Issue #2138 follow-up: a gate stamped by the first fix
/// (`b7c8b029`) has no `evaluated_evidence_blocker` key at all. Treating an
/// absent key as a match keeps its cleared blocker pinned forever; treating an
/// absent key as *not* a match re-judges the gate exactly once, and the new code
/// then stamps the key. A dev-profile hub carries gates in that state, so the
/// healing path has to be the default.
#[test]
fn a_gate_without_a_recorded_blocker_is_rejudged_once_after_its_blocker_clears() {
    use crate::circuit::observation::WorkEvidence;
    let mut view = view_with_outstanding_lineage_work();
    let report = "Published the PR but the push could not complete.";
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Blocked),
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Unverified
    );
    // Exactly the older build's state: the refusal stamped the owner but never
    // the blocker key. The context is a flat dotted-key map, so drop that one
    // key and rebuild.
    let mut vars: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&view.context.to_json().unwrap()).unwrap();
    assert!(
        vars.remove("node.finish_classifier.evaluated_evidence_blocker")
            .is_some(),
        "the current build records the key; this test removes it"
    );
    view.context =
        crate::circuit::context::CircuitContext::from_json(&serde_json::to_string(&vars).unwrap())
            .unwrap();
    assert_eq!(
        view.context
            .get("node.finish_classifier.evaluated_evidence_blocker"),
        None,
        "this gate carries no recorded blocker"
    );
    view.context.set(
        "node.implementer.evidence.1",
        serde_json::to_string(&WorkEvidence::default()).unwrap(),
    );
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            None
        ),
        "a missing key must re-judge once, not freeze a cleared blocker"
    );
}

/// The suppression is scoped to the *unchanged* report: fresh evidence — a new
/// revision, or the same text from a new native turn — still re-classifies.
/// This passes on the pre-fix code too; it guards the fix from over-suppressing
/// rather than reproducing the original defect.
#[test]
fn parked_unverified_gate_reclassifies_when_the_report_or_turn_changes() {
    let mut view = view_with_outstanding_lineage_work();
    let report = "Published the PR but the push could not complete.";
    advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Blocked),
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Unverified
    );
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            "A different report with new findings.",
            None
        ),
        "a new report revision must still reach the classifier"
    );
    crate::circuit::test_support::record_report_evidence_for_turn(
        &mut view,
        "finish_classifier",
        report,
        "a-later-native-turn",
    );
    assert!(
        should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            None
        ),
        "the same text from a new native turn is new evidence"
    );
}

/// Issue #2138, acceptance criterion 3: re-observing a parked Unverified gate
/// on an unchanged report appends no ledger rows. The stable hub's ledger grew
/// by hundreds of identical `classification` rows per parked gate, each with a
/// fresh `step_transition: unverified` and `checkpoint_reason`.
///
/// Each tick runs the real gate and, when the gate admits it, is advanced and
/// committed — so the row counts below measure what a repeated tick really
/// appends. Asserting on the stepper alone would miss the defect, because
/// `advance` faithfully records whatever it is handed; the unbounded growth came
/// from the gate re-admitting an unchanged report on every tick.
#[test]
fn repeated_ticks_on_a_parked_gate_append_no_ledger_rows() {
    let mut view = view_with_outstanding_lineage_work();
    let report = "Published the PR but the push could not complete.";
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    let run_id = view.run_id;
    db.execute_batch(&format!(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
         INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(900,1,'agent','/repo','running');
         INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
         INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES({},1,1,'running');
         INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
             VALUES({},'finish_classifier',1,'running',NULL);
         INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
             VALUES({},'implementer',1,'completed',900);",
        run_id, run_id, run_id
    ))
    .unwrap();
    let commit = |db: &mut Connection, view: &RunView, transition: &Transition| {
        let ops: Vec<crate::db::circuit::CircuitStepOp> = transition
            .step_writes
            .iter()
            .map(|write| crate::db::circuit::CircuitStepOp {
                node_id: write.node_id.clone(),
                status: write.status.as_db_str().into(),
                attempt: write.attempt,
                outcome: write
                    .outcome
                    .map(|value| value.map(|outcome| outcome.as_db_str().into())),
                error: write.error.clone(),
                agent_node_id: None,
                fresh_attempt: false,
            })
            .collect();
        crate::db::circuit::evidence::commit_transition_locked(
            db,
            view.run_id,
            None,
            &view.context.to_json().unwrap(),
            &ops,
            crate::db::circuit::evidence::EvidenceWrite {
                classifications: &transition.classifications,
                ..Default::default()
            },
        )
        .unwrap();
    };
    let kind_count = |db: &Connection, kind: &str| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE run_id=?1 AND kind=?2",
            rusqlite::params![run_id, kind],
            |row| row.get(0),
        )
        .unwrap()
    };
    let rows = |db: &Connection| -> Vec<(String, String)> {
        let mut stmt = db
            .prepare("SELECT kind,detail FROM circuit_run_history WHERE run_id=?1 ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([run_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows
    };
    // The first tick judges the report and parks the gate.
    assert!(should_classify_report(
        &view,
        "finish_classifier",
        SessionStatus::AwaitingInput,
        report,
        None
    ));
    let first = advance_with_report_evidence(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "finish_classifier".into(),
            classification: Some(crate::circuit::evaluator::Classification::Blocked),
            output: Some(report.into()),
        },
    );
    assert_eq!(
        view.step("finish_classifier").unwrap().status,
        StepStatus::Unverified
    );
    assert_eq!(first.classifications.len(), 1);
    commit(&mut db, &view, &first);
    // Five more observation ticks of the same unchanged report. This is a
    // fail-fast predicate guard: if any tick is admitted, that tick is advanced
    // and committed and the test fails immediately, so the counts below only
    // ever observe the first commit. They still pin the durable shape — one
    // classification, one transition, one checkpoint reason — but the
    // discriminating assertion is the gate itself. Fails on the pre-fix code.
    for tick in 0..5 {
        if should_classify_report(
            &view,
            "finish_classifier",
            SessionStatus::AwaitingInput,
            report,
            None,
        ) {
            let transition = advance_with_report_evidence(
                &mut view,
                &CircuitEvent::TurnClassified {
                    binding: None,
                    node_id: "finish_classifier".into(),
                    classification: Some(crate::circuit::evaluator::Classification::Blocked),
                    output: Some(report.into()),
                },
            );
            commit(&mut db, &view, &transition);
            panic!("tick {tick} re-admitted an unchanged parked report");
        }
        assert_eq!(
            view.step("finish_classifier").unwrap().status,
            StepStatus::Unverified,
            "the gate is never disturbed by a suppressed tick"
        );
    }
    // One interpretation, and one transition and checkpoint reason for the park.
    assert_eq!(kind_count(&db, "classification"), 1, "{:?}", rows(&db));
    assert_eq!(kind_count(&db, "step_transition"), 1, "{:?}", rows(&db));
    assert_eq!(kind_count(&db, "checkpoint_reason"), 1, "{:?}", rows(&db));
}

#[test]
fn circuit_report_selection_rejects_pre_prompt_transcript_and_resume_redraw() {
    use crate::circuit::evaluator;
    let id = 910_019;
    let old = "Implementation complete.";
    let old_report = || {
        Some(crate::services::transcript_reader::AssistantReport {
            text: old.into(),
            revision: "turn-1".into(),
        })
    };
    evaluator::register_circuit(id);
    evaluator::on_output(id, old);
    evaluator::note_turn_start(id);
    // A gate just opened, but the agent has not answered the new prompt.
    assert_eq!(
        select_turn_report(
            old_report(),
            Some("turn-1"),
            evaluator::has_turn_start(id),
            || evaluator::cleaned_turn_tail(id)
        ),
        None
    );
    evaluator::on_output(id, "Permission required to run tests.");
    assert_eq!(
        select_turn_report(
            old_report(),
            Some("turn-1"),
            evaluator::has_turn_start(id),
            || evaluator::cleaned_turn_tail(id)
        ),
        Some("Permission required to run tests.".into())
    );
    evaluator::unregister(id);
    evaluator::register_circuit(id);
    evaluator::on_output(id, old); // resume redraw, not a new response
    assert_eq!(
        select_turn_report(
            old_report(),
            Some("turn-1"),
            evaluator::has_turn_start(id),
            || evaluator::cleaned_turn_tail(id)
        ),
        None
    );
    assert_eq!(
        select_turn_report(None, None, false, || "unanchored redraw".into()),
        None,
        "unreadable transcript cannot authorize a resume redraw"
    );
    assert_eq!(
        select_turn_report(
            Some(crate::services::transcript_reader::AssistantReport {
                text: "   \n".into(),
                revision: "turn-empty".into(),
            }),
            None,
            true,
            || "\t".into(),
        ),
        None,
        "empty reports are not classifier input",
    );
    // A concise new response made while capture was offline is recoverable.
    let report = select_turn_report(
        Some(crate::services::transcript_reader::AssistantReport {
            text: "Done. PR updated.".into(),
            revision: "turn-2".into(),
        }),
        Some("turn-1"),
        evaluator::has_turn_start(id),
        || evaluator::cleaned_turn_tail(id),
    )
    .unwrap();
    assert_eq!(report, "Done. PR updated.");
    assert!(should_classify_report(
        &report_gate_view(),
        "finish_classifier",
        SessionStatus::AwaitingInput,
        &report,
        None
    ));
    assert!(crate::circuit::evaluator::circuit_classify_prompt(&report).contains(&report));
    assert_eq!(
        select_turn_report(
            Some(crate::services::transcript_reader::AssistantReport {
                text: old.into(),
                revision: "turn-2".into()
            }),
            Some("turn-1"),
            false,
            String::new
        ),
        Some(old.into()),
        "identical text in a new assistant response is fresh"
    );
    evaluator::unregister(id);
}
