use super::*;

fn node(id: i64, path: &str, session: Option<&str>) -> AgentNode {
    AgentNode {
        id,
        path: path.into(),
        provider: "mcode".into(),
        status: SessionStatus::Running,
        cli_session_id: session.map(str::to_owned),
        ..Default::default()
    }
}

#[test]
fn mcode_simultaneous_spawns_route_to_their_own_workspaces() {
    let nodes = [
        node(4724, "F:/repo/implementation", None),
        node(4725, "F:/repo/other", None),
    ];
    assert_eq!(
        select_hook_target(&nodes, "session-a", "f:\\repo\\implementation\\", |_| true)
            .unwrap()
            .id,
        4724
    );
    assert_eq!(
        select_hook_target(&nodes, "session-b", "F:/repo/other", |_| true)
            .unwrap()
            .id,
        4725
    );
    assert!(select_hook_target(&nodes, "standalone", "F:/pixelpath", |_| true).is_none());
}

#[test]
fn mcode_callback_cannot_rebind_a_known_conversation_or_guess_ownership() {
    let mut nodes = vec![
        node(4715, "F:/pixelpath", Some("pixel-session")),
        node(4724, "F:/repo", None),
    ];
    assert!(select_hook_target(&nodes, "pixel-session", "F:/repo", |_| true).is_none());
    assert!(select_hook_target(&nodes, "fresh", "F:/repo", |_| false).is_none());
    nodes.push(node(4726, "F:/repo", None));
    assert!(select_hook_target(&nodes, "fresh", "F:/repo", |_| true).is_none());
    nodes[2].path = "F:/elsewhere".into();
    nodes[2].cli_session_id = Some("pixel-session".into());
    assert!(select_hook_target(&nodes, "pixel-session", "F:/pixelpath", |_| true).is_none());
}

#[test]
fn mcode_resume_requires_the_same_session_and_workspace() {
    let nodes = [node(4724, "F:/repo", Some("resumed"))];
    assert_eq!(
        select_hook_target(&nodes, "resumed", "F:/repo", |_| true)
            .unwrap()
            .id,
        4724
    );
    assert!(select_hook_target(&nodes, "replacement", "F:/repo", |_| true).is_none());
    assert!(select_hook_target(&nodes, "resumed", "F:/other", |_| true).is_none());
    assert!(select_hook_target(&nodes, "resumed", "", |_| true).is_none());
    assert!(!same_directory("/home/user/Repo", "/home/user/repo"));
}

#[test]
fn mcode_archived_rows_do_not_own_a_conversation_identity() {
    let mut archived = node(4715, "F:/repo", Some("reusable-session"));
    archived.status = SessionStatus::Archived;
    let live = node(4724, "F:/repo", None);
    let nodes = [archived, live];
    assert_eq!(
        select_hook_target(&nodes, "reusable-session", "F:/repo", |_| true)
            .unwrap()
            .id,
        4724
    );
}

#[test]
fn mcode_wait_predicate_accepts_an_idle_node_while_its_spawn_is_in_flight() {
    let mut node = node(4724, "F:/repo", None);
    node.status = SessionStatus::Idle;
    let node_id = node.id;
    let nodes = [node];
    assert!(!waiting_for_spawn(&nodes, "fresh", "F:/repo", |_| false));
    assert!(waiting_for_spawn(
        &nodes,
        "fresh",
        "F:/repo",
        |candidate| candidate.id == node_id
    ));
    assert!(!waiting_for_spawn(
        &nodes,
        "fresh",
        "F:/standalone",
        |candidate| candidate.id == node_id
    ));
}

#[test]
fn mcode_hook_target_retries_for_spawn_registration_and_obeys_deadline() {
    let start = std::time::Instant::now();
    let elapsed = std::cell::Cell::new(std::time::Duration::ZERO);
    let mut calls = 0;
    let mut slept = std::time::Duration::ZERO;
    let target = retry_hook_target(
        start + SPAWN_REGISTRY_WAIT,
        || {
            calls += 1;
            if calls == 1 {
                Ok((None, true))
            } else {
                Ok((Some(4724), false))
            }
        },
        || start + elapsed.get(),
        |duration| {
            elapsed.set(elapsed.get() + duration);
            slept += duration;
        },
    )
    .expect("the attempt does not read the database");
    assert_eq!(target, Some(4724));
    assert_eq!(calls, 2);
    assert_eq!(slept, SPAWN_REGISTRY_POLL);

    let elapsed = std::cell::Cell::new(std::time::Duration::ZERO);
    let mut calls = 0;
    let result: Option<i64> = retry_hook_target(
        start + SPAWN_REGISTRY_WAIT,
        || {
            calls += 1;
            Ok((None, true))
        },
        || start + elapsed.get(),
        |duration| elapsed.set(elapsed.get() + duration),
    )
    .expect("the attempt does not read the database");
    assert_eq!(result, None);
    assert_eq!(elapsed.get(), SPAWN_REGISTRY_WAIT);
    assert_eq!(
        calls,
        (SPAWN_REGISTRY_WAIT.as_millis() / SPAWN_REGISTRY_POLL.as_millis()) as usize
    );
}

/// A read that failed is unknown, not "no such node" (issue #1533): the
/// error travels out so the route can answer "come back later" instead of
/// a 404 that tells the harness its node is gone.
#[test]
fn mcode_hook_target_reports_a_failed_read_instead_of_no_target() {
    let start = std::time::Instant::now();
    let error = retry_hook_target(
        start + SPAWN_REGISTRY_WAIT,
        || Err::<(Option<i64>, bool), _>(rusqlite::Error::InvalidQuery),
        || start,
        |_| {},
    );
    assert!(
        matches!(error, Err(rusqlite::Error::InvalidQuery)),
        "the read error must travel out, got: {error:?}"
    );
}
