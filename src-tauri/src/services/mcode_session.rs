//! MiniMax's plugin is shared by all processes, including standalone sessions.
//! Route callbacks by session and workspace; timestamps cannot distinguish
//! simultaneous spawns or prove which conversation belongs to a node.

use crate::models::{AgentNode, SessionStatus};

const SPAWN_REGISTRY_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
const SPAWN_REGISTRY_POLL: std::time::Duration = std::time::Duration::from_millis(25);

pub(crate) fn hook_target(session_id: &str, cwd: &str) -> Option<AgentNode> {
    retry_hook_target(
        std::time::Instant::now() + SPAWN_REGISTRY_WAIT,
        || {
            let nodes = crate::db::list_agent_nodes().ok()?;
            let target = select_hook_target(&nodes, session_id, cwd, |node| {
                matches!(
                    node.status,
                    SessionStatus::Running
                        | SessionStatus::Ready
                        | SessionStatus::AwaitingInput
                        | SessionStatus::Completed
                        | SessionStatus::Spawning
                ) && crate::agent::process::PROCESS_REGISTRY.is_alive(&node.id)
            });
            if let Some(target) = target {
                // Check the single selected owner, rather than querying the
                // generation separately for every candidate row.
                if crate::db::session_started_at_ms(target.id)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    return Some((Some(target), false));
                }
            }
            // SessionStart can beat registry insertion immediately after child spawn.
            // The spawn claim precedes child creation and Spawning publication.
            let waiting = waiting_for_spawn(&nodes, session_id, cwd, |node| {
                crate::agent::spawn::is_spawn_in_flight(node.id)
            });
            Some((None, waiting))
        },
        std::time::Instant::now,
        std::thread::sleep,
    )
}

fn retry_hook_target<T>(
    deadline: std::time::Instant,
    mut attempt: impl FnMut() -> Option<(Option<T>, bool)>,
    mut now: impl FnMut() -> std::time::Instant,
    mut sleep: impl FnMut(std::time::Duration),
) -> Option<T> {
    loop {
        if now() >= deadline {
            return None;
        }
        let (target, spawn_in_flight) = attempt()?;
        if target.is_some() || !spawn_in_flight {
            return target;
        }
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return None;
        }
        sleep(SPAWN_REGISTRY_POLL.min(remaining));
    }
}

fn waiting_for_spawn(
    nodes: &[AgentNode],
    session_id: &str,
    cwd: &str,
    is_spawn_in_flight: impl Fn(&AgentNode) -> bool,
) -> bool {
    select_hook_target(nodes, session_id, cwd, is_spawn_in_flight).is_some()
}

pub(crate) fn select_hook_target(
    nodes: &[AgentNode],
    session_id: &str,
    cwd: &str,
    eligible: impl Fn(&AgentNode) -> bool,
) -> Option<AgentNode> {
    if session_id.is_empty() || cwd.trim().is_empty() {
        return None;
    }
    let owners: Vec<_> = nodes
        .iter()
        .filter(|node| {
            node.status != SessionStatus::Archived
                && node.cli_session_id.as_deref() == Some(session_id)
        })
        .collect();
    // Known sessions never bind another empty node. Duplicate ownership
    // requires recovery rather than choosing between inconsistent rows.
    if owners.len() > 1 {
        return None;
    }
    let mut matches = nodes.iter().filter(|node| {
        crate::preferences::resolve_harness_provider(&node.provider)
            == crate::models::Provider::Mcode
            && node.status != SessionStatus::Archived
            && eligible(node)
            && owners.first().is_none_or(|owner| owner.id == node.id)
            && node
                .cli_session_id
                .as_deref()
                .is_none_or(|id| id.is_empty() || id == session_id)
            && same_directory(&crate::env::node_working_path(node).spawn_path, cwd)
    });
    let target = matches.next()?.clone();
    matches.next().is_none().then_some(target)
}

fn same_directory(left: &str, right: &str) -> bool {
    let normalize = |path: &str| path.replace('\\', "/").trim_end_matches('/').to_string();
    let left = normalize(left);
    let right = normalize(right);
    // Windows identities are case-insensitive even on a WSL host; guest
    // POSIX paths remain case-sensitive. Do not guess host/guest equivalence.
    if left.as_bytes().get(1) == Some(&b':') || left.starts_with("//") {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
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
                    Some((None, true))
                } else {
                    Some((Some(4724), false))
                }
            },
            || start + elapsed.get(),
            |duration| {
                elapsed.set(elapsed.get() + duration);
                slept += duration;
            },
        );
        assert_eq!(target, Some(4724));
        assert_eq!(calls, 2);
        assert_eq!(slept, SPAWN_REGISTRY_POLL);

        let elapsed = std::cell::Cell::new(std::time::Duration::ZERO);
        let mut calls = 0;
        let result: Option<i64> = retry_hook_target(
            start + SPAWN_REGISTRY_WAIT,
            || {
                calls += 1;
                Some((None, true))
            },
            || start + elapsed.get(),
            |duration| elapsed.set(elapsed.get() + duration),
        );
        assert_eq!(result, None);
        assert_eq!(elapsed.get(), SPAWN_REGISTRY_WAIT);
        assert_eq!(
            calls,
            (SPAWN_REGISTRY_WAIT.as_millis() / SPAWN_REGISTRY_POLL.as_millis()) as usize
        );
    }
}
