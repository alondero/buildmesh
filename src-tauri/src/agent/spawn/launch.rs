//! Launch-PTY/process phase of Agent Node spawn.
//!
//! Resolves the capability-masked spawn config, builds the command (via
//! `command`), then opens a PTY and starts the child (via `process`).
//! Registry insert and the reader thread belong to `streams`.

use super::command::{build_spawn_command_prepared, resolve_spawn_config};
use super::process::{sandbox_spawn, spawn_child};
use super::provision::ProvisionedWorkspace;
use super::reader::{open_pty_pair, SessionIdMode, SpawnTimer};
use super::wire::emit_provider_error;
use crate::models::Provider;

/// PTY size, cascade overrides, and command-construction knobs.
///
/// Prepared in the prepare phase and passed straight to
/// [`launch_process`] — provision never sees these. Putting `rows` /
/// `prefill` / `explicit_*` on the workspace DTO was tramp data: git
/// worktree provisioning does not care about terminal columns.
pub(super) struct LaunchParams {
    pub rows: u16,
    pub cols: u16,
    pub prefill: Option<String>,
    pub explicit_model: Option<String>,
    pub explicit_effort: Option<String>,
    pub explicit_extra_args: Option<String>,
    /// Composite spawn-option id (`node.provider`), used as the harness
    /// map key for application defaults and per-mesh overrides.
    pub harness_id: String,
    /// `AgentNode.mesh_id` — lookup key for `get_mesh_harness_overrides`.
    pub node_mesh_id: i64,
    /// Mesh id resolved from the node path, stored on the process
    /// registry for per-mesh activity tracking.
    pub registry_mesh_id: i64,
    pub session_id_mode: SessionIdMode,
    pub sandbox: bool,
}

/// Live child + PTY handles, ready to register and start the reader.
pub(super) struct LaunchedProcess {
    pub session_id: i64,
    pub provider: Provider,
    pub rows: u16,
    pub cols: u16,
    pub session_id_mode: SessionIdMode,
    pub resolved: crate::env::ResolvedPath,
    pub mesh_id: i64,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    pub master: Box<dyn portable_pty::MasterPty + Send>,
    pub reader: Box<dyn std::io::Read + Send>,
    pub writer: Box<dyn std::io::Write + Send>,
    pub job: Option<crate::process_util::JobHandle>,
}

/// Build the spawn command and start the child inside a PTY (sandboxed
/// when the mesh opts in).
pub(super) async fn launch_process(
    app: &tauri::AppHandle,
    provisioned: ProvisionedWorkspace,
    launch: LaunchParams,
    timer: &SpawnTimer,
) -> Result<LaunchedProcess, String> {
    let ProvisionedWorkspace {
        session_id,
        provider,
        resolved,
        routing,
    } = provisioned;
    let LaunchParams {
        rows,
        cols,
        prefill,
        explicit_model,
        explicit_effort,
        explicit_extra_args,
        harness_id,
        node_mesh_id,
        registry_mesh_id,
        session_id_mode,
        sandbox,
    } = launch;
    let adapter = provider.adapter();

    // Resolve configuration values through the per-field cascade (issue
    // #1149 prefactor; #1150 fills the application slot; #1151 fills the
    // per-Mesh override slot). The resolver applies the capability mask,
    // so `build_spawn_command` receives values the harness actually accepts
    // — unsupported values never reach the harness process regardless of
    // which layer supplied them. The application slot reads the latest
    // in-process preferences cache (no disk read on the spawn hot path);
    // the validator already removed any value the harness couldn't accept
    // at save time, so the resolver's mask here is the second-and-final gate.
    //
    // `harness_id` for a Proxied Provider row is the composite id
    // `"<harness>:<provider>"` (e.g. `"claude:minimax"`, `"codex:minimax"`).
    // The per-Mesh override map and the application-defaults map are both
    // keyed by the harness *profile* id (the half before the first `:`),
    // so a raw lookup would miss every Proxied spawn — failing AC #12
    // ("Native and Proxied Provider Spawn Options consume the same
    // application-default layer"). Split the composite id through
    // `parse_spawn_option_id` before both lookups so native and Proxied
    // rows hit the same map key.
    let (harness_id_for_default, _) = crate::agent::provider::parse_spawn_option_id(&harness_id);
    let mesh_override = crate::db::get_mesh_harness_overrides(node_mesh_id)
        .ok()
        .flatten()
        .and_then(|m| m.get(harness_id_for_default).cloned());
    let app_default = match crate::preferences::load() {
        Ok(prefs) => crate::preferences::harness_default_for(&prefs, harness_id_for_default),
        Err(e) => {
            tracing::warn!("launch_process: harness-default load failed, treating as absent: {e}");
            None
        }
    };
    let resolved_config = resolve_spawn_config(
        provider,
        explicit_model.as_deref(),
        explicit_effort.as_deref(),
        // Cascade layer-1 verbatim CLI flags from the v2 SpawnAgentNode
        // explicit slot (issue #1358). The resolver capability-masks
        // this against `HarnessCapabilities.supports_extra_args` —
        // Terminal drops it; every interactive harness keeps it. The
        // `non_empty_trim` collapse happens inside `resolve_agent_config`
        // / `resolve_extra_args` so whitespace-only inputs cascade-fall.
        explicit_extra_args.as_deref(),
        // Legacy `meshes.model` / `meshes.effort` columns are physically
        // present for positional row compatibility but are no longer
        // read as active spawn configuration — the v33 one-shot
        // migration copied any non-empty legacy values into the
        // `claude` override entry of the new map (issue #1151 acceptance
        // criteria 6). On a healthy v33+ DB this slot is always `None`.
        app_default.as_ref(),
        mesh_override.as_ref(),
    );
    let cmd = build_spawn_command_prepared(
        &resolved,
        provider,
        &routing,
        &session_id_mode,
        session_id,
        &resolved_config,
        prefill.as_deref(),
        sandbox,
    );

    // A resumed Command Code process can append its first turn immediately.
    // Give the adapter a pre-spawn seam to snapshot the old transcript and
    // install its watcher before the child receives CPU time.
    let start_resume_services = || async {
        if let SessionIdMode::Resume(cli_session_id) = &session_id_mode {
            adapter
                .before_resume_spawn(
                    session_id,
                    cli_session_id,
                    &resolved.spawn_path,
                    resolved.env_type,
                    app,
                )
                .await;
        }
    };

    let (child, master): (
        Box<dyn portable_pty::Child + Send + Sync>,
        Box<dyn portable_pty::MasterPty + Send>,
    ) = if crate::sandbox::sandbox_enabled(sandbox) {
        tracing::info!(
            "launch_process: spawning session {} inside process sandbox",
            session_id
        );
        start_resume_services().await;
        match sandbox_spawn(&cmd, session_id, &resolved.host_path, rows, cols) {
            Ok(process) => process,
            Err(error) => {
                adapter.on_process_terminated(session_id);
                emit_provider_error(app, session_id, provider, &error);
                return Err(error);
            }
        }
    } else {
        let pair = open_pty_pair(rows, cols)?;
        start_resume_services().await;
        let child = match spawn_child(&pair, cmd) {
            Ok(child) => child,
            Err(error) => {
                adapter.on_process_terminated(session_id);
                emit_provider_error(app, session_id, provider, &error);
                return Err(error);
            }
        };
        (child, pair.master)
    };

    tracing::info!("launch_process: process spawned successfully");
    timer.checkpoint("after_pty_spawn");

    // Contain the whole process tree in a Job Object straight away, before the
    // shell launches the agent CLI — so any process the agent later detaches
    // (e.g. a dev server it backgrounds) is still killed on close, even when its
    // parent has exited and `taskkill /T` could no longer reach it.
    let job = child
        .process_id()
        .and_then(crate::process_util::JobHandle::contain);
    if job.is_none() {
        tracing::warn!(
            "launch_process: could not contain session {} in a Job Object; \
             close will fall back to taskkill (detached children may survive)",
            session_id
        );
    }

    // Clone the PTY reader/writer. Registry insert belongs to streams.
    let reader = match master.try_clone_reader() {
        Ok(reader) => reader,
        Err(error) => {
            adapter.on_process_terminated(session_id);
            return Err(error.to_string());
        }
    };
    let writer = match master.take_writer() {
        Ok(writer) => writer,
        Err(error) => {
            adapter.on_process_terminated(session_id);
            return Err(error.to_string());
        }
    };

    Ok(LaunchedProcess {
        session_id,
        provider,
        rows,
        cols,
        session_id_mode,
        resolved,
        mesh_id: registry_mesh_id,
        child,
        master,
        reader,
        writer,
        job,
    })
}
