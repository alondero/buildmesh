use std::path::Path;

#[test]
fn only_minimax_code_declares_the_idle_wake_up_gap() {
    // The nudge is a MiniMax Code behaviour, not a Circuit-wide rule: every
    // other reader must keep the default "no gap", or a transcript that
    // happens to look like a launch record could wake an unrelated harness.
    // MiniMax's native-store wake path is exercised by its reader tests.
    let lines = r#"{"message":{"role":"toolResult","timestamp":100,"details":{"status":"started","task_id":"bg_pending"},"content":[]}}"#;
    let readers = crate::services::transcript_reader::adapter::registered();
    assert_eq!(
        readers.len(),
        11,
        "the registry gained a reader; this contract must cover it"
    );
    let others: Vec<&str> = readers
        .iter()
        .map(|reader| reader.id())
        .filter(|id| *id != "mcode")
        .collect();
    assert_eq!(others.len(), 10, "every non-mcode reader is covered");
    for id in others {
        let reader = crate::services::transcript_reader::adapter::dispatch(id).expect("registered");
        assert_eq!(
            reader.stalled_background_task(lines, Path::new("/transcript"), 400),
            None,
            "{id} must not inherit mcode's rule"
        );
    }
}
