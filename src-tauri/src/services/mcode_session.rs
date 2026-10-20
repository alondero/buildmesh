//! MiniMax's plugin is shared by all processes, including standalone sessions.
//! Route callbacks by session and workspace; timestamps cannot distinguish
//! simultaneous spawns or prove which conversation belongs to a node.

use crate::models::{AgentNode, SessionStatus};

const SPAWN_REGISTRY_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
const SPAWN_REGISTRY_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Resolve the node a MiniMax hook callback belongs to.
///
/// A read that failed is *unknown*, not "no such node" (issue #1533): the
/// caller must be able to answer "come back later" instead of telling a
/// harness its node is gone. The error therefore travels out of this
/// function, and only a genuinely absent target comes back as `None`.
pub(crate) fn hook_target(
    session_id: &str,
    cwd: &str,
) -> Result<Option<AgentNode>, rusqlite::Error> {
    retry_hook_target(
        std::time::Instant::now() + SPAWN_REGISTRY_WAIT,
        || {
            let nodes = crate::db::list_agent_nodes()?;
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
                // generation separately for every candidate row. A failed
                // read is unknown, not "this node has no generation": the
                // stale-generation guard would read that as a stale
                // callback and drop a live one.
                if crate::db::session_started_at_ms(target.id)?.is_some() {
                    return Ok((Some(target), false));
                }
            }
            // SessionStart can beat registry insertion immediately after child spawn.
            // The spawn claim precedes child creation and Spawning publication.
            let waiting = waiting_for_spawn(&nodes, session_id, cwd, |node| {
                crate::agent::spawn::is_spawn_in_flight(node.id)
            });
            Ok((None, waiting))
        },
        std::time::Instant::now,
        std::thread::sleep,
    )
}

fn retry_hook_target<T>(
    deadline: std::time::Instant,
    mut attempt: impl FnMut() -> Result<(Option<T>, bool), rusqlite::Error>,
    mut now: impl FnMut() -> std::time::Instant,
    mut sleep: impl FnMut(std::time::Duration),
) -> Result<Option<T>, rusqlite::Error> {
    loop {
        if now() >= deadline {
            return Ok(None);
        }
        let (target, spawn_in_flight) = attempt()?;
        if target.is_some() || !spawn_in_flight {
            return Ok(target);
        }
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Ok(None);
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
#[path = "mcode_session_tests.rs"]
mod tests;
