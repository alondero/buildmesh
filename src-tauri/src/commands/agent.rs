//! Tauri command wrappers for agent process operations.
//!
//! All spawn/lifecycle logic lives in `crate::agent::spawn` and
//! `crate::agent::process`. This module exposes those as Tauri commands
//! and the small bits of orchestration (DB updates, scrollback clears,
//! attention events) that surround them.

use crate::agent::process::PROCESS_REGISTRY;
use crate::agent::provider::{Platform, ProviderInfo};
use crate::db;
use crate::models::{Provider, SessionStatus};
use tauri::{command, AppHandle, Emitter};

// ---------------------------------------------------------------------------
// Provider listing
// ---------------------------------------------------------------------------

/// Returns the list of agent providers available on this host platform.
/// Each provider declares which platforms it runs on via `AgentProvider::available_on()`.
pub(crate) fn available_providers() -> Vec<ProviderInfo> {
    let host = Platform::current();
    Provider::all()
        .iter()
        .map(|p| p.adapter())
        .filter(|adapter| adapter.available_on().contains(&host))
        .map(|adapter| {
            let ui = adapter.ui();
            ProviderInfo {
                id: adapter.id().into(),
                label: ui.label,
                color: ui.color,
                icon: ui.icon,
            }
        })
        .collect()
}

#[command]
pub async fn list_providers() -> Vec<ProviderInfo> {
    available_providers()
}

// ---------------------------------------------------------------------------
// Spawn / resume
// ---------------------------------------------------------------------------

/// Spawn a new agent for the given session with the specified provider.
#[command]
pub async fn spawn_agent(
    app: AppHandle,
    session_id: i64,
    provider: String,
    resume: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<(), String> {
    let r = rows.unwrap_or(24);
    let c = cols.unwrap_or(80);
    crate::agent::spawn::spawn_agent_inner(&app, session_id, provider, resume, r, c).await
}

/// Auto-resume all suspended sessions that have a stored CLI session ID.
/// Called by the frontend on startup after event listeners are ready.
#[command]
pub async fn auto_resume_sessions(app: AppHandle) -> Result<Vec<i64>, String> {
    let nodes = db::list_suspended_nodes().map_err(|e| e.to_string())?;

    if nodes.is_empty() {
        tracing::info!("auto_resume_sessions: no suspended sessions to resume");
        return Ok(vec![]);
    }

    tracing::info!("auto_resume_sessions: resuming {} sessions", nodes.len());
    let mut resumed: Vec<i64> = Vec::new();

    for node in &nodes {
        let cli_id = match &node.cli_session_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => {
                tracing::warn!("auto_resume_sessions: node {} has no cli_session_id, skipping", node.id);
                db::update_agent_node_status(node.id, SessionStatus::Idle).ok();
                continue;
            }
        };

        if !node.provider.adapter().auto_resume_on_startup() {
            tracing::info!("auto_resume_sessions: skipping non-resumable node {} ({:?})", node.id, node.provider);
            db::update_agent_node_status(node.id, SessionStatus::Idle).ok();
            continue;
        }

        let provider_str = node.provider.to_string();
        match crate::agent::spawn::spawn_agent_inner(&app, node.id, provider_str, Some(cli_id), 24, 80).await {
            Ok(()) => {
                resumed.push(node.id);
                tracing::info!("auto_resume_sessions: resumed node {}", node.id);
            }
            Err(e) => {
                tracing::error!("auto_resume_sessions: failed to resume node {}: {}", node.id, e);
                db::update_agent_node_status(node.id, SessionStatus::Error).ok();
                let _ = app.emit("resume-failed", serde_json::json!({
                    "session_id": node.id,
                    "error": e
                }));
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(resumed)
}

// ---------------------------------------------------------------------------
// Lifecycle commands
// ---------------------------------------------------------------------------

/// Kill all running agent processes. Used during graceful shutdown.
pub fn kill_all_agents() {
    for id in PROCESS_REGISTRY.session_ids() {
        PROCESS_REGISTRY.kill_session(id);
        PROCESS_REGISTRY.remove(&id);
        crate::http_server::clear_scrollback(id);
        tracing::info!("kill_all_agents: killed agent for session {}", id);
    }
}

#[command]
pub async fn resize_agent(session_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    PROCESS_REGISTRY.resize_pty(session_id, cols, rows)
}

#[command]
pub async fn write_to_agent(app: AppHandle, session_id: i64, data: String) -> Result<(), String> {
    PROCESS_REGISTRY.write_bytes(session_id, data.as_bytes())?;

    if data.bytes().any(|b| b == b'\n' || b == b'\r') {
        db::update_agent_node_status(session_id, SessionStatus::Running).ok();
        let _ = app.emit("attention-cleared", serde_json::json!({ "session_id": session_id }));
    }
    Ok(())
}

#[command]
pub async fn send_to_agent(app: AppHandle, session_id: i64, input: String) -> Result<(), String> {
    write_to_agent(app, session_id, format!("{}\n", input)).await
}

#[command]
pub async fn kill_agent(session_id: i64) -> Result<(), String> {
    crate::session_naming::reset_buffers(session_id);
    PROCESS_REGISTRY.kill_session(session_id);
    PROCESS_REGISTRY.remove(&session_id);
    crate::http_server::clear_scrollback(session_id);
    db::update_agent_node_status(session_id, SessionStatus::Idle).map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn is_agent_running(session_id: i64) -> bool {
    PROCESS_REGISTRY.is_alive(&session_id)
}

// ---------------------------------------------------------------------------
// Debug
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct AgentDebugState {
    pub session_id: i64,
    pub is_alive: bool,
}

#[command]
pub async fn debug_list_agents() -> Vec<AgentDebugState> {
    PROCESS_REGISTRY
        .session_ids()
        .into_iter()
        .map(|id| AgentDebugState {
            session_id: id,
            is_alive: PROCESS_REGISTRY.is_alive(&id),
        })
        .collect()
}

/// Snapshot of all relevant state at the time of a crash, for post-mortem diagnosis.
/// Call this via invoke('debug_crash_snapshot') immediately after a crash to get
/// a consistent view of what the backend was doing.
#[derive(serde::Serialize)]
pub struct CrashSnapshot {
    pub process_registry_ids: Vec<i64>,
    pub session_count: usize,
    pub renamed_sessions: usize,
    pub buffers_size_bytes: usize,
    pub turn_counters_entries: usize,
}

#[command]
pub async fn debug_crash_snapshot() -> CrashSnapshot {
    let process_ids = PROCESS_REGISTRY.session_ids();
    let session_count = db::list_agent_nodes().map(|s| s.len()).unwrap_or(0);
    let buffers_size = crate::session_naming::buffers_size_bytes();

    CrashSnapshot {
        process_registry_ids: process_ids,
        session_count,
        renamed_sessions: 0,
        buffers_size_bytes: buffers_size,
        turn_counters_entries: 0,
    }
}
