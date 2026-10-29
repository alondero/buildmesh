use super::*;

#[test]
fn waits_while_boot_output_is_recent_or_absent() {
    assert!(!ready_to_type(None, Duration::from_secs(5)));
    assert!(!ready_to_type(
        Some(Duration::from_millis(200)),
        Duration::from_secs(5)
    ));
}

#[test]
fn types_once_boot_output_goes_quiet() {
    assert!(ready_to_type(Some(BOOT_QUIET), Duration::from_secs(3)));
}

#[test]
fn types_at_the_deadline_into_a_tui_that_never_settles() {
    assert!(ready_to_type(
        Some(Duration::from_millis(10)),
        BOOT_DEADLINE
    ));
    assert!(ready_to_type(None, BOOT_DEADLINE));
}

#[test]
fn records_output_only_for_armed_nodes() {
    let armed = 9_100_001;
    let unarmed = 9_100_002;
    arm(armed);
    assert_eq!(quiet_for(armed), None, "no output before the TUI draws");
    on_output(armed);
    on_output(unarmed);
    assert!(quiet_for(armed).is_some());
    assert_eq!(quiet_for(unarmed), None);
    assert!(!ARMED.lock().unwrap().contains_key(&unarmed));
    disarm(armed);
    assert_eq!(quiet_for(armed), None);
}
