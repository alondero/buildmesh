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
