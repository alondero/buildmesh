//! Tauri command wrappers for agent process operations.
//!
//! All spawn/lifecycle logic lives in `crate::agent::spawn` and
//! `crate::agent::process`. This module exposes those as Tauri commands
//! and the small bits of orchestration (DB updates, scrollback clears,
//! attention events) that surround them.

use crate::agent::process::PROCESS_REGISTRY;
use crate::agent::provider::{Platform, ProviderInfo};
use crate::agent::spawn::SpawnOptions;
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
    crate::agent::spawn::spawn_agent_inner(&app, SpawnOptions {
        session_id,
        provider: Provider::from_db_str(&provider),
        resume,
        rows: rows.unwrap_or(24),
        cols: cols.unwrap_or(80),
        prefill: None,
        node: None,
    }).await
}

/// Internal implementation shared by spawn_issue_agent and spawn_handover_agent.
/// Takes a pre-fetched `&Mesh` and a fully-formatted prefill string. Source-specific
/// shaping (e.g. GitHub issue → URL+title) happens in the caller before this is
/// called, so the impl is just: resolve provider → create node → spawn.
async fn spawn_new_agent_impl(
    app: &AppHandle,
    mesh: &crate::models::Mesh,
    prefill: String,
    provider: Option<String>,
    source_issue: Option<i64>,
) -> Result<crate::models::AgentNode, String> {
    let effective_provider = crate::preferences::resolve_default_provider(
        provider,
        mesh.default_provider.clone(),
        crate::preferences::default_provider(),
    );

    let branch = crate::commands::git::get_default_branch(mesh.path.clone());

    let node = crate::services::agent_node::create(
        mesh.id,
        &mesh.path,
        &branch,
        Some(&effective_provider),
        source_issue,
    ).map_err(|e| e.to_string())?;

    let prefill_text = if Provider::from_db_str(&effective_provider).adapter().supports_prefill() {
        Some(prefill)
    } else {
        tracing::warn!(
            "spawn_new_agent_impl: --prefill not supported for provider '{}', skipping",
            effective_provider
        );
        None
    };

    let node_id = node.id;
    crate::agent::spawn::spawn_agent_inner(app, SpawnOptions {
        session_id: node_id,
        provider: Provider::from_db_str(&effective_provider),
        resume: None,
        rows: 24,
        cols: 80,
        prefill: prefill_text,
        node: Some(node),
    }).await?;

    db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
}

/// Spawn an agent pre-filled with a pointer to a GitHub issue (URL + title hint).
///
/// We deliberately pass just the URL and title, not the full issue body. Shipping
/// a multi-KB markdown body through the Windows PowerShell `-EncodedCommand`
/// argv path is the worst-case input for that pipeline (backticks, code fences,
/// nested quotes) and was the main reason this flow was unreliable on Windows.
/// LLMs can read the URL themselves and they need the link anyway to cite the
/// issue in the closing PR.
///
/// The `--prefill` arg is only passed for providers whose adapter declares
/// `supports_prefill() = true`; others spawn without prefill and log a warning.
#[command]
pub async fn spawn_issue_agent(
    app: AppHandle,
    mesh_id: i64,
    issue_number: i64,
    issue_title: String,
    provider: Option<String>,
) -> Result<crate::models::AgentNode, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(&mesh)
        .map_err(|e| format!("{} — cannot derive issue URL", e))?;

    let prefill = format_issue_prefill(&owner, &repo, issue_number, &issue_title);

    let node = spawn_new_agent_impl(
        &app,
        &mesh,
        prefill,
        provider,
        Some(issue_number),
    ).await?;

    tracing::info!("spawn_issue_agent: spawned node {} for issue #{}", node.id, issue_number);
    Ok(node)
}

/// Compose the prefill string handed to the agent on GitHub-issue spawn.
///
/// Shape: `Please work on GitHub issue #N — Title\n<html_url>` — terse imperative,
/// the issue number for cite-ability, the title as a one-line freebie so the
/// agent can usually start without round-tripping for the body, and the URL as
/// the canonical source. An empty title degrades to just `#N\n<url>` (no
/// dangling em-dash artifact).
///
/// The title is passed verbatim — the consumer is an LLM, not a parser, and
/// the prefill bytes round-trip safely through the PowerShell `-EncodedCommand`
/// path (see memory: powershell-encoding-fix). Using an em-dash instead of
/// surrounding quotes sidesteps any escape question entirely.
///
/// Pure function — exposed `pub(crate)` for unit tests in `agent_tests.rs`.
pub(crate) fn format_issue_prefill(
    owner: &str,
    repo: &str,
    issue_number: i64,
    issue_title: &str,
) -> String {
    let url = format!("https://github.com/{}/{}/issues/{}", owner, repo, issue_number);
    let trimmed = issue_title.trim();
    if trimmed.is_empty() {
        format!("Please work on GitHub issue #{}\n{}", issue_number, url)
    } else {
        format!(
            "Please work on GitHub issue #{} — {}\n{}",
            issue_number, trimmed, url
        )
    }
}

/// Spawn a new agent node pre-filled with selected text from a parent terminal.
/// Used by the "Handover to new Node" context menu option.
#[command]
pub async fn spawn_handover_agent(
    app: AppHandle,
    mesh_id: i64,
    prefill: String,
    provider: Option<String>,
) -> Result<crate::models::AgentNode, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let node = spawn_new_agent_impl(
        &app,
        &mesh,
        prefill,
        provider,
        None,
    ).await?;

    tracing::info!("spawn_handover_agent: spawned node {} via handover", node.id);
    Ok(node)
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

        match crate::agent::spawn::spawn_agent_inner(&app, SpawnOptions {
            session_id: node.id,
            provider: node.provider,
            resume: Some(cli_id),
            rows: 24,
            cols: 80,
            prefill: None,
            node: Some(node.clone()),
        }).await {
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
