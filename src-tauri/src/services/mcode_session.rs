//! MiniMax's plugin is shared by all processes, including standalone sessions.
//! Route callbacks by session and workspace; timestamps cannot distinguish
//! simultaneous spawns or prove which conversation belongs to a node.

use crate::models::{AgentNode, SessionStatus};

pub(crate) fn hook_target(session_id: &str, cwd: &str) -> Option<AgentNode> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let nodes = crate::db::list_agent_nodes().ok()?;
        let generation_exists = |node: &AgentNode| crate::db::session_started_at_ms(node.id).ok().flatten().is_some();
        if let Some(target) = select_hook_target(&nodes, session_id, cwd, |node|
            matches!(node.status, SessionStatus::Running | SessionStatus::Ready | SessionStatus::AwaitingInput | SessionStatus::Completed | SessionStatus::Spawning)
                && crate::agent::process::PROCESS_REGISTRY.is_alive(&node.id) && generation_exists(node)) {
            return Some(target);
        }
        // SessionStart can beat registry insertion immediately after child spawn.
        // The spawn claim precedes child creation and Spawning publication.
        if std::time::Instant::now() >= deadline || !waiting_for_spawn(&nodes, session_id, cwd) { return None; }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn waiting_for_spawn(nodes: &[AgentNode], session_id: &str, cwd: &str) -> bool {
    select_hook_target(nodes, session_id, cwd,
        |node| crate::agent::spawn::is_spawn_in_flight(node.id)).is_some()
}

pub(crate) fn select_hook_target(
    nodes: &[AgentNode], session_id: &str, cwd: &str,
    eligible: impl Fn(&AgentNode) -> bool,
) -> Option<AgentNode> {
    if session_id.is_empty() || cwd.trim().is_empty() { return None; }
    let owners: Vec<_> = nodes.iter().filter(|node| node.cli_session_id.as_deref() == Some(session_id)).collect();
    // Known sessions never bind another empty node. Duplicate ownership
    // requires recovery rather than choosing between inconsistent rows.
    if owners.len() > 1 { return None; }
    let mut matches = nodes.iter().filter(|node| {
        crate::preferences::resolve_harness_provider(&node.provider) == crate::models::Provider::Mcode
            && node.status != SessionStatus::Archived && eligible(node)
            && owners.first().is_none_or(|owner| owner.id == node.id)
            && node.cli_session_id.as_deref().is_none_or(|id| id.is_empty() || id == session_id)
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
    } else { left == right }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, path: &str, session: Option<&str>) -> AgentNode {
        AgentNode { id, path: path.into(), provider: "mcode".into(),
            status: SessionStatus::Running, cli_session_id: session.map(str::to_owned),
            ..Default::default() }
    }

    #[test]
    fn mcode_simultaneous_spawns_route_to_their_own_workspaces() {
        let nodes = [node(4724, "F:/repo/implementation", None), node(4725, "F:/repo/other", None)];
        assert_eq!(select_hook_target(&nodes, "session-a", "f:\\repo\\implementation\\", |_| true).unwrap().id, 4724);
        assert_eq!(select_hook_target(&nodes, "session-b", "F:/repo/other", |_| true).unwrap().id, 4725);
        assert!(select_hook_target(&nodes, "standalone", "F:/pixelpath", |_| true).is_none());
    }

    #[test]
    fn mcode_callback_cannot_rebind_a_known_conversation_or_guess_ownership() {
        let mut nodes = vec![node(4715, "F:/pixelpath", Some("pixel-session")), node(4724, "F:/repo", None)];
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
        assert_eq!(select_hook_target(&nodes, "resumed", "F:/repo", |_| true).unwrap().id, 4724);
        assert!(select_hook_target(&nodes, "replacement", "F:/repo", |_| true).is_none());
        assert!(select_hook_target(&nodes, "resumed", "F:/other", |_| true).is_none());
        assert!(select_hook_target(&nodes, "resumed", "", |_| true).is_none());
        assert!(!same_directory("/home/user/Repo", "/home/user/repo"));
    }

    #[test]
    fn mcode_session_start_waits_for_the_spawn_claim_before_spawning_status() {
        let mut node = node(-276_277, "F:/repo", None);
        node.status = SessionStatus::Idle;
        let nodes = [node];
        assert!(!waiting_for_spawn(&nodes, "fresh", "F:/repo"));
        let claim = crate::agent::spawn::SpawnInFlightClaim::try_claim(nodes[0].id).unwrap();
        assert!(waiting_for_spawn(&nodes, "fresh", "F:/repo"));
        assert!(!waiting_for_spawn(&nodes, "fresh", "F:/standalone"));
        drop(claim);
        assert!(!waiting_for_spawn(&nodes, "fresh", "F:/repo"));
    }
}