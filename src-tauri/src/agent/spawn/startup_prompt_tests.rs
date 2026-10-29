use super::*;

#[test]
fn waits_while_boot_output_is_recent_or_absent() {
    assert!(!ready_to_type(None, true, Duration::from_secs(5)));
    assert!(!ready_to_type(
        Some(Duration::from_millis(200)),
        true,
        Duration::from_secs(5)
    ));
}

#[test]
fn types_once_boot_output_goes_quiet_and_the_adapter_accepts_it() {
    assert!(ready_to_type(
        Some(BOOT_QUIET),
        true,
        Duration::from_secs(3)
    ));
}

#[test]
fn waits_for_the_adapter_ready_marker_even_when_quiet() {
    assert!(!ready_to_type(
        Some(Duration::from_secs(10)),
        false,
        Duration::from_secs(12)
    ));
}

#[test]
fn types_at_the_deadline_into_a_tui_that_never_settles() {
    assert!(ready_to_type(
        Some(Duration::from_millis(10)),
        false,
        BOOT_DEADLINE
    ));
    assert!(ready_to_type(None, false, BOOT_DEADLINE));
}

/// Node ids come from this test's isolated database range, so the
/// process-global `ARMED` map cannot collide with a parallel test.
#[test]
fn records_boot_output_only_for_armed_nodes() {
    let _db = crate::db::test_support::isolated();
    let (first, _) = crate::db::test_support::installed_node_id_range()
        .expect("isolated database owns a node id range");
    let (armed, unarmed) = (first, first + 1);
    arm(armed);
    assert_eq!(observed(armed).0, None, "no output before the TUI draws");
    on_output(armed, "\u{1b}[2J\u{1b}[1;1HReady >");
    on_output(unarmed, "ignored");
    let (quiet, tail) = observed(armed);
    assert!(quiet.is_some());
    assert_eq!(
        tail, "Ready >",
        "ANSI escapes are stripped for the ready marker"
    );
    assert_eq!(observed(unarmed), (None, String::new()));
    assert!(!ARMED.lock().unwrap().contains_key(&unarmed));
    disarm(armed);
    assert_eq!(observed(armed).0, None);
}

#[test]
fn boot_tail_keeps_only_the_newest_bytes() {
    let _db = crate::db::test_support::isolated();
    let (node, _) = crate::db::test_support::installed_node_id_range()
        .expect("isolated database owns a node id range");
    arm(node);
    on_output(node, &"a".repeat(TAIL_BYTES));
    on_output(node, "é-end");
    let (_, tail) = observed(node);
    disarm(node);
    assert!(tail.len() <= TAIL_BYTES);
    assert!(tail.ends_with("é-end"));
}
