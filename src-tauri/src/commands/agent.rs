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
///
/// `initial_name` lets the caller seed the node with a meaningful name (e.g.
/// the issue-title slug for `spawn_issue_agent`); handover leaves it `None`
/// and falls back to a random default.
async fn spawn_new_agent_impl(
    app: &AppHandle,
    mesh: &crate::models::Mesh,
    prefill: String,
    provider: Option<String>,
    source_issue: Option<i64>,
    initial_name: Option<String>,
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
        None,
        initial_name.as_deref(),
    ).map_err(|e| e.to_string())?;

    // Parse once and reuse — the from_db_str allocation isn't free, and any
    // future change to the parsing rule should only need to update one site.
    let provider = Provider::from_db_str(&effective_provider);

    let prefill_text = if provider.adapter().supports_prefill() {
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
        provider,
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
    // Issue #111: seed the node with a slugified issue title so the user can
    // identify it in the mesh list from the moment the modal closes (instead
    // of waiting on the LLM rename). The name is prefixed with `gh{N}-` so
    // the user can spot the originating issue at a glance (e.g. issue #123
    // "Fix this feature" → `gh123-fix-this-feature`). Falls back to a random
    // default name if the title doesn't yield a valid `SLUG_REGEX` match —
    // the `gh` prefix is still applied to the fallback so the user always
    // sees which issue the node came from.
    let initial_name = crate::session_naming::issue_node_name(issue_number, &issue_title);

    let node = spawn_new_agent_impl(
        &app,
        &mesh,
        prefill,
        provider,
        Some(issue_number),
        Some(initial_name),
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

// ---------------------------------------------------------------------------
// Two-stage issue spawn (fast stage-1 + background stage-2)
//
// These two commands split the original `spawn_issue_agent` into a fast DB
// write and a slow background task. The intent is to remove the 5-10s lag
// between clicking "Spawn" in the GitHub Issues dialog and the modal
// closing: the desktop frontend calls `create_issue_node` (which only
// touches the DB) and immediately closes the modal, then fires
// `start_node_background` (which does the slow git/worktree/PTY work)
// without awaiting. The original synchronous `spawn_issue_agent` is kept
// for the mobile HTTP route — its callers tolerate the wait because they
// have no interactive UI to keep responsive.
// ---------------------------------------------------------------------------

/// A new agent node draft returned from the fast stage-1 spawn command.
/// The frontend holds onto `prefill` and passes it back to
/// `start_node_background` (no DB round-trip for the prefill — it's
/// transient and <500 bytes).
#[derive(serde::Serialize)]
pub struct IssueNodeDraft {
    #[serde(flatten)]
    pub node: crate::models::AgentNode,
    pub prefill: String,
}

/// Fast stage-1 of the GitHub-issue spawn flow. Creates a `Pending` agent
/// node row, returns the row + the prefill the caller must pass to
/// `start_node_background`. Does NOT touch the network, the worktree, or
/// the process tree — so it returns in ~20ms instead of 5-10s.
#[command]
pub fn create_issue_node(
    app: AppHandle,
    mesh_id: i64,
    issue_number: i64,
    issue_title: String,
    provider: Option<String>,
) -> Result<IssueNodeDraft, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(&mesh)
        .map_err(|e| format!("{} — cannot derive issue URL", e))?;
    let prefill = format_issue_prefill(&owner, &repo, issue_number, &issue_title);
    // Issue #111: seed the node with a `gh{N}-{slug}` name (mirrors
    // `spawn_issue_agent` so the desktop modal and mobile route produce
    // identical names). The `gh` prefix lets the user spot the originating
    // issue at a glance. Falls back to a random default if the title doesn't
    // yield a valid slug — the prefix is still applied in that case.
    let initial_name = crate::session_naming::issue_node_name(issue_number, &issue_title);

    let effective_provider = crate::preferences::resolve_default_provider(
        provider,
        mesh.default_provider.clone(),
        crate::preferences::default_provider(),
    );
    let branch = crate::commands::git::get_default_branch(mesh.path.clone());

    let node = crate::services::agent_node::create_pending(
        mesh.id,
        &mesh.path,
        &branch,
        Some(&effective_provider),
        Some(issue_number),
        Some(&initial_name),
    )
    .map_err(|e| e.to_string())?;

    // Reuse the existing semantic event so the frontend's `session-created`
    // listener (which already triggers `fetchAgentNodes`) picks up the
    // new node without us adding a new event-name coupling.
    let _ = app.emit(
        "session-created",
        serde_json::json!({ "id": node.id }),
    );

    tracing::info!(
        "create_issue_node: created pending node {} for issue #{} on mesh {}",
        node.id,
        issue_number,
        mesh_id
    );

    Ok(IssueNodeDraft { node, prefill })
}

/// Slow stage-2 of the two-stage spawn flow. Runs the existing
/// `spawn_agent_inner` pipeline (git fetch, worktree create, PTY spawn,
/// workspace-trust + attention-hook write, reader thread) for a node
/// row that already exists. Fire-and-forget — the Tauri command
/// returns immediately; the work happens on a background task.
///
/// Emits `node-spawn-completed` on success and `node-spawn-failed` on
/// failure. On failure, the node's status is updated to `Error` so the
/// UI shows a red badge and the user can close the node normally.
#[command]
pub fn start_node_background(
    app: AppHandle,
    node_id: i64,
    prefill: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn(async move {
        let result = start_node_inner(&app, node_id, prefill.as_deref()).await;
        match result {
            Ok(()) => {
                let _ = app.emit(
                    "node-spawn-completed",
                    serde_json::json!({ "node_id": node_id }),
                );
                tracing::info!("start_node_background: node {} ready", node_id);
            }
            Err(e) => {
                tracing::error!(
                    "start_node_background: node {} failed: {}",
                    node_id,
                    e
                );
                let _ = db::update_agent_node_status(node_id, SessionStatus::Error);
                let _ = app.emit(
                    "node-spawn-failed",
                    serde_json::json!({ "node_id": node_id, "error": e }),
                );
            }
        }
    });
    Ok(())
}

/// Body of stage-2: load the node, optionally filter the prefill through
/// the provider's `supports_prefill()` check, and delegate to the
/// existing `spawn_agent_inner`. The same function is reused by
/// `auto_resume_sessions` (via `spawn_agent_inner` directly) and the
/// handover path (via `spawn_handover_agent`); this is the issue-spawn
/// entry point.
async fn start_node_inner(
    app: &AppHandle,
    node_id: i64,
    prefill: Option<&str>,
) -> Result<(), String> {
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    let provider = node.provider;
    let prefill_text = if provider.adapter().supports_prefill() {
        prefill.filter(|s| !s.is_empty()).map(String::from)
    } else {
        if prefill.is_some() {
            tracing::warn!(
                "start_node_inner: --prefill not supported for provider '{:?}', skipping",
                provider
            );
        }
        None
    };

    crate::agent::spawn::spawn_agent_inner(
        app,
        crate::agent::spawn::SpawnOptions {
            session_id: node_id,
            provider,
            resume: None,
            rows: 24,
            cols: 80,
            prefill: prefill_text,
            node: Some(node),
        },
    )
    .await
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

    if data.bytes().any(|b| b == b'\n' || b == b'\r')
        && !should_skip_attention_signals(session_id)
    {
        db::update_agent_node_status(session_id, SessionStatus::Running).ok();
        let _ = app.emit("attention-cleared", serde_json::json!({ "session_id": session_id }));
    }
    Ok(())
}

/// Returns true if a newline in `write_to_agent` should NOT flip the
/// node to `Running` and should NOT emit `attention-cleared`. A plain
/// terminal's "Enter" is just shell input — the node has no LLM
/// attention state to clear, and flipping status would render a
/// spurious cyan "Running" badge for a shell sitting at a prompt.
fn should_skip_attention_signals(session_id: i64) -> bool {
    db::get_agent_node_by_id(session_id)
        .ok()
        .map(|n| provider_is_plain_terminal(n.provider))
        .unwrap_or(false)
}

fn provider_is_plain_terminal(provider: Provider) -> bool {
    provider.adapter().is_plain_terminal()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Provider;

    #[test]
    fn plain_terminal_provider_skips_attention_signals() {
        assert!(provider_is_plain_terminal(Provider::Terminal));
    }

    #[test]
    fn llm_providers_do_emit_attention_signals() {
        for p in [
            Provider::Anthropic,
            Provider::Minimax,
            Provider::Kimi,
            Provider::Agy,
            Provider::OpenCode,
            Provider::Codex,
        ] {
            assert!(
                !provider_is_plain_terminal(p),
                "LLM provider {p:?} should not skip attention signals"
            );
        }
    }
}
