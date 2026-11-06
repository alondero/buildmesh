use super::*;

#[test]
fn origin_extension_and_ancestry_coexist_without_changing_storage() {
    let mut context = CircuitContext::new();
    RunLineage::mark_local_preset(&mut context);
    RunLineage::record_parent(&mut context, 42);
    context.set(REVIEW_EXTENDED_KEY, "1");
    context.set("unknown.key", "retained");
    let graph = CircuitGraph::agent_review(None, None, 3);
    let lineage = RunLineage::read(|key| context.get(key), &graph);
    assert_eq!(lineage.review_origin, ReviewOrigin::LocalPreset);
    assert!(lineage.extended);
    assert_eq!(lineage.recovery, RecoveryAncestry::FromRun { run_id: 42 });
    assert_eq!(context.get("source.review_preset"), Some("1"));
    assert_eq!(context.get("recovery.from_run_id"), Some("42"));
    assert_eq!(context.get("unknown.key"), Some("retained"));
}

#[test]
fn unknown_flags_and_invalid_ancestry_do_not_claim_borrowed_review_turns() {
    let graph = CircuitGraph::walking_skeleton("work");
    let mut context = CircuitContext::new();
    context.set(REVIEW_PRESET_KEY, "true");
    context.set(REVIEW_EXTENDED_KEY, "true");
    for invalid in ["", "unknown", "0", "-1", "9223372036854775808"] {
        context.set(RECOVERY_PARENT_KEY, invalid);
        let lineage = RunLineage::read(|key| context.get(key), &graph);
        assert_eq!(lineage.recovery, RecoveryAncestry::Invalid);
        assert_eq!(lineage.review_origin, ReviewOrigin::Ordinary);
        assert!(!lineage.extended);
        assert!(!lineage.uses_borrowed_review_turns());
    }
}

#[test]
fn issue_origin_survives_a_review_only_extension() {
    let mut context = CircuitContext::new();
    context.set(REVIEW_EXTENDED_KEY, "1");
    context.set("review.source_was_circuit_owned", "1");
    RunLineage::record_parent(&mut context, 7);
    let lineage = RunLineage::read(
        |key| context.get(key),
        &CircuitGraph::agent_review(None, None, 4),
    );
    assert_eq!(lineage.review_origin, ReviewOrigin::IssueDriven);
    assert!(lineage.extended);
    assert_eq!(lineage.recovery, RecoveryAncestry::FromRun { run_id: 7 });
    let ordinary = RunLineage::read(|_| None, &CircuitGraph::walking_skeleton("work"));
    assert_eq!(ordinary.recovery, RecoveryAncestry::None);
    assert_eq!(ordinary.review_origin, ReviewOrigin::Ordinary);
}
