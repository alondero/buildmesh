/// Streams consume a launched process, not git/workspace state.
#[test]
fn start_streams_takes_launched_process_only() {
    let src = include_str!("streams.rs");
    let sig = src
        .split("pub(super) async fn start_streams")
        .nth(1)
        .expect("start_streams must exist");
    let header = &sig[..sig.find('{').expect("fn body")];
    assert!(
        header.contains("launched: LaunchedProcess"),
        "start_streams takes LaunchedProcess"
    );
    assert!(
        !header.contains("WorkspaceToProvision"),
        "streams must not accept git provision inputs"
    );
}

#[test]
fn start_streams_logs_under_its_own_name() {
    let src = include_str!("streams.rs");
    assert!(src.contains("start_streams: storing agent process"));
    assert!(
        !src.contains("spawn_agent_inner:"),
        "streams.rs must not log as the old monolith"
    );
}

#[test]
fn start_streams_registers_before_starting_the_reader() {
    let src = include_str!("streams.rs");
    let register = src.find("register_agent(").expect("register_agent call");
    let reader = src.find("start_reader(").expect("start_reader call");
    assert!(
        register < reader,
        "register must happen before start_reader (TOCTOU close for is_agent_already_running)"
    );
}
