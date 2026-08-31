use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::process::{AgentProcess, PROCESS_REGISTRY};
use crate::agent::provider::{Platform, CLAUDE_BACKEND_ENV_VARS};
use crate::agent::{session_lifecycle, spawn_environment};
use crate::models::{EnvType, Provider};
use crate::{db, env};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Threshold for the PTY reader thread's early-exit heuristic (issue #654).
/// If the reader thread exits within this window the agent is flagged
/// `Error` — typically because `--resume <uuid>` failed against an expired
/// session. The orchestrator's delayed `Spawning → Running` promotion sleeps
/// just past this same window (see `spawn_agent_inner` step 14b) so the two
/// sites MUST stay in sync; bumping this constant without re-checking the
/// promotion delay recreates the ghost-Running race.
/// Shared by the reader thread's early-exit heuristic and the
/// orchestrator's delayed Spawning→Running promotion sleep (#654). The two
/// MUST stay in lock-step — drifting them recreates the race in either
/// direction.
pub const EARLY_EXIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// What the PTY reader thread's epilogue should do to the node's status
/// after the read loop ends. Extracted as a pure decision so the
/// deliberate-kill / early-exit / plain-terminal matrix is unit-testable
/// without a live PTY.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PostExitAction {
    /// Natural exit — flip the node to Idle.
    MarkIdle,
    /// The process died on its own within `EARLY_EXIT_WINDOW` of its
    /// creation — almost always a `--resume <uuid>` that the CLI rejected
    /// ("No conversation found…"). Mark Error and emit `resume-failed`.
    MarkErrorResumeFailed,
    /// `kill_session` tore the PTY down deliberately (node close, spawn
    /// step-2 stale kill, app shutdown). The kill initiator owns the next
    /// status; any write from the reader would race it. The pre-fix bug:
    /// a <3s-old process killed by a respawn was stamped `Error`, which
    /// then blocked the new spawn's Spawning→Running promotion (`Error`
    /// is in that write's exclusion list) — the node showed "failed to
    /// start" while the replacing agent booted fine seconds later.
    LeaveStatusAlone,
}

pub(crate) fn post_exit_action(
    is_plain_terminal: bool,
    deliberately_killed: bool,
    elapsed_since_process_creation: std::time::Duration,
) -> PostExitAction {
    if deliberately_killed {
        return PostExitAction::LeaveStatusAlone;
    }
    if is_plain_terminal {
        // A shell exiting — `exit`, window close — is a normal Idle,
        // never an Error: a shell is not a --resume, so a fast exit
        // isn't a resume-failure signal.
        return PostExitAction::MarkIdle;
    }
    if elapsed_since_process_creation < EARLY_EXIT_WINDOW {
        PostExitAction::MarkErrorResumeFailed
    } else {
        PostExitAction::MarkIdle
    }
}

/// Session ids with a `spawn_agent_inner` call currently in flight.
///
/// `is_agent_already_running` only sees the PROCESS_REGISTRY, and
/// registration happens seconds into the pipeline (after git fetch +
/// worktree provisioning) — so two near-simultaneous spawn calls for the
/// same node (e.g. the backend's `start_node_background` racing the
/// frontend Terminal auto-spawn on an 'idle' row) both passed the check.
/// The loser's step-2 stale-kill (or registry insert-replace) then killed
/// the winner's freshly-booted process — the "failed to start, yet it
/// boots seconds later" symptom — and, when the frontend had already
/// picked up the captured `cli_session_id`, respawned with
/// `--resume <uuid>` against a session that never persisted a
/// conversation ("No conversation found with session ID").
///
/// This set closes the TOCTOU across the WHOLE pipeline: the claim is
/// taken at function entry and held (RAII) until the spawn returns.
///
/// Implementation note: the lock is `std::sync::Mutex` rather than
/// `tokio::sync::Mutex` because both the claim entry (synchronous) and
/// the Drop (synchronous) are short, non-suspending operations on a
/// tiny set. Holding the guard across `.await` suspension points is
/// safe because Drop runs only at function scope exit (Rust's
/// `NLL`-aware borrow checker keeps the binding alive across `.await`s
/// without contending with the lock — a single contended acquire on
/// Drop would be a tokio-worker-blocking scenario, but the only writer
/// of contention is another concurrent claim, and `HashSet::insert` is
/// bounded by the spawn rate which is ≪ 1k/s).

/// Per-spawn timing log. Records elapsed milliseconds at each
/// `checkpoint(name)` call and at the end via `total()`. Output goes to
/// `buildmesh.log` via the existing `tracing` setup — no extra plumbing.
///
/// Born of the spawn-latency investigation (5-10s lag between clicking
/// "Spawn" and visible UI feedback). The checkpoints proved the bottleneck
/// was NOT the hypothesised `git::sync::fetch_origin` (network) but
/// `worktree_create` — 97% of which was libgit2's checkout. That checkout
/// now shells out to `git worktree add` (~20× faster; ADR 0007 amendment),
/// so a fresh node is usable in ~2s instead of ~14s. The timer is kept as a
/// cheap spawn-latency regression guard; its only consumer is the `tracing`
/// log file.
pub(super) struct SpawnTimer {
    start: std::time::Instant,
    session_id: i64,
}

impl SpawnTimer {
    pub(super) fn new(session_id: i64) -> Self {
        Self {
            start: std::time::Instant::now(),
            session_id,
        }
    }

    pub(super) fn checkpoint(&self, name: &str) {
        tracing::info!(
            "spawn_timing: session={} checkpoint={} elapsed={}ms",
            self.session_id,
            name,
            self.start.elapsed().as_millis()
        );
    }

    pub(super) fn total(&self) {
        tracing::info!(
            "spawn_timing: session={} TOTAL elapsed={}ms",
            self.session_id,
            self.start.elapsed().as_millis()
        );
    }

    /// Original start instant — exposed `pub(crate)` so `register_agent`
    /// can clone it onto `AgentProcess.spawn_start`, giving the
    /// `first_user_input` log line the same reference as every other
    /// `spawn_timing:` checkpoint.
    pub(crate) fn start(&self) -> std::time::Instant {
        self.start
    }
}

/// Open a PTY pair using the native PTY system.
pub fn open_pty_pair(rows: u16, cols: u16) -> Result<PtyPair, String> {
    let pty_system = native_pty_system();
    pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open PTY: {}", e))
}

/// Session ID mode: either assign a new ID or resume an existing one.
pub enum SessionIdMode {
    Assign(String),
    Resume(String),
    None,
}

/// Whether the PTY reader thread should attempt to capture a session ID
/// from live PTY output (issue #651).
///
/// Two independent code paths target the same `agent_nodes.cli_session_id`
/// column: the orchestrator's pre-write in `spawn_agent_inner` step 4 (Assign
/// mode) and the reader thread's `session_capture::try_extract_session_id`
/// match. They are unsynchronised, so a last-writer-wins race leaves the DB
/// holding either the orchestrator's UUID or a regex match — and on
/// auto-resume `claude --resume <wrong-uuid>` → "Conversation not found".
///
/// This predicate is the single source of truth for which path is allowed to
/// write for a given spawn:
///
/// * `Assign(_)` — orchestrator is authoritative; the reader MUST NOT
///   capture (the orchestrator just wrote the UUID that the agent was
///   launched with via `--session-id <uuid>`).
/// * `Resume(_)` — the resume arg is authoritative; the DB column already
///   holds the same ID from a prior spawn. A reader capture would race
///   `claude --resume <id>` with a possibly-different UUID.
/// * `None` — orchestrator did not pre-write (Codex / Agy self-assign
///   internally). Capture is allowed only if the provider's adapter
///   declares `captures_session_id_from_pty() = true`; otherwise any UUID
///   match would be spurious noise (OpenCode captures via `after_fresh_spawn`).
pub(super) fn reader_should_capture_session_id(
    session_id_mode: &SessionIdMode,
    pty_capture: bool,
) -> bool {
    pty_capture && matches!(session_id_mode, SessionIdMode::None)
}

/// Build the spawn command by composing the provider's recipe with the runtime environment.
///
/// `backend_env` is the per-profile backend selection resolved by the caller
/// (`preferences::resolve_provider_env(&node.provider)`): the `ANTHROPIC_*`
/// variables a custom Claude-compatible profile (MiniMax/DeepSeek) needs to
/// target its endpoint. Empty for the built-in Anthropic subscription and for
/// the native-binary providers (Codex, Grok, Kimi Code, Antigravity, OpenCode).
/// Passed in (rather than resolved here) so this
/// function stays a pure composition of its inputs — no disk / preferences-cache
/// access — and the env injection can be unit-tested with an explicit list.
///
/// `config` carries the **already-resolved, capability-masked** model and
/// effort values (issue #1149). The caller runs
/// [`crate::agent::capabilities::resolve_agent_config`] with the harness's
/// capability descriptor and the per-field cascade inputs; this function
/// forwards the resolved values verbatim and never re-consults capability
/// flags. Empty / whitespace inputs and unsupported values are masked before
/// they reach here.
#[allow(clippy::too_many_arguments)]
pub fn build_spawn_command(
    resolved: &env::ResolvedPath,
    provider_enum: Provider,
    backend_env: &[(String, String)],
    session_id_mode: &SessionIdMode,
    session_id: i64,
    config: &crate::agent::capabilities::ResolvedAgentConfig,
    prefill: Option<&str>,
    sandbox: bool,
) -> CommandBuilder {
    build_spawn_command_prepared(
        resolved,
        provider_enum,
        &crate::agent::launch_routing::PreparedLaunchRouting::environment(backend_env),
        session_id_mode,
        session_id,
        config,
        prefill,
        sandbox,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_spawn_command_prepared(
    resolved: &env::ResolvedPath,
    provider_enum: Provider,
    routing: &crate::agent::launch_routing::PreparedLaunchRouting,
    session_id_mode: &SessionIdMode,
    session_id: i64,
    config: &crate::agent::capabilities::ResolvedAgentConfig,
    prefill: Option<&str>,
    sandbox: bool,
) -> CommandBuilder {
    let adapter = provider_enum.adapter();
    let platform = if resolved.env_type == EnvType::Wsl {
        Platform::Linux
    } else {
        Platform::current()
    };

    // Compose the harness's launch contribution: recipe + capability
    // descriptor + env policy, all from the same adapter. The
    // capability-mask guarantee still holds — the resolver ran before
    // we got here, and the helper re-asserts the descriptor on the
    // forward as defence in depth (issue #1179).
    let session_ref = match session_id_mode {
        SessionIdMode::Assign(id) => SessionIdModeRef::Assign(id.as_str()),
        SessionIdMode::Resume(id) => SessionIdModeRef::Resume(id.as_str()),
        SessionIdMode::None => SessionIdModeRef::None,
    };
    let input = HarnessLaunchInput {
        platform,
        runtime: resolved.env_type,
        session: session_ref,
        config,
        prefill,
        sandbox,
    };
    let prepared = crate::agent::launch::default_prepare(adapter, input);

    // CodexProxy contributes --profile / --model to the recipe. This
    // belongs at the orchestrator layer (not the harness): the
    // pairing's verified profile is the orchestrator's knowledge, and
    // the per-pairing model id is a routing fact, not a harness fact.
    let mut recipe = prepared.recipe;
    if let crate::agent::launch_routing::PreparedLaunchRouting::CodexProxy {
        profile_name,
        descriptor,
        ..
    } = routing
    {
        recipe.base_args.extend([
            "--profile".into(),
            profile_name.clone(),
            "--model".into(),
            descriptor.model_id.clone(),
        ]);
    }

    let (wsl_distro, executable_override) = match routing {
        crate::agent::launch_routing::PreparedLaunchRouting::CodexProxy { install, .. } => (
            install.wsl_distro.as_deref(),
            Some(install.executable.as_str()),
        ),
        _ => (None, None),
    };
    let mut cmd = spawn_environment::wrap(
        recipe,
        resolved.env_type,
        wsl_distro,
        executable_override,
        &resolved.spawn_path,
        session_id,
        sandbox,
    );

    // Apply the harness's environment policy (CLAUDE_BACKEND_ENV_VARS
    // reset + per-harness env_remove). The adapter owns this — the
    // Claude-backed anthropic adapter sets the reset, Codex sets
    // OPENAI_* strip; every other adapter uses HarnessEnvironmentPolicy::NONE.
    if prepared.environment.resets_backend_env {
        for k in CLAUDE_BACKEND_ENV_VARS {
            cmd.env_remove(k);
        }
    }
    for k in prepared.environment.env_remove {
        cmd.env_remove(k);
    }

    // Inject the per-profile backend env + Codex Proxy credential. WSLENV is
    // assembled once after all command-defined variables are known, avoiding
    // one routing branch overwriting another branch's entries.
    let mut command_wsl_env = apply_routing_env(&mut cmd, routing);
    if let Some(key) = apply_codex_proxy_credential(&mut cmd, routing, provider_enum) {
        command_wsl_env.push(key);
    }
    spawn_environment::apply_wsl_env(
        &mut cmd,
        resolved.env_type,
        &command_wsl_env,
        adapter.wsl_passthrough_env(),
    );
    cmd
}

/// Apply the per-profile backend env (`PreparedLaunchRouting::Environment`)
/// to the child command and return the names that need WSL propagation.
pub(super) fn apply_routing_env<'a>(
    cmd: &mut CommandBuilder,
    routing: &'a crate::agent::launch_routing::PreparedLaunchRouting,
) -> Vec<&'a str> {
    let backend_env: &[(String, String)] = match routing {
        crate::agent::launch_routing::PreparedLaunchRouting::Environment(values) => {
            values.as_slice()
        }
        _ => &[],
    };
    for (k, v) in backend_env {
        cmd.env(k, v);
    }
    backend_env.iter().map(|(key, _)| key.as_str()).collect()
}

/// Apply the Codex Proxy pairing-scoped credential. A verified profile
/// authenticates exclusively through its pairing-scoped reference
/// (`PROXY_CREDENTIAL_ENV`); generic `OPENAI_API_KEY` / `OPENAI_BASE_URL`
/// inherited by Buildmesh are stripped so they cannot become an alternate
/// credential/endpoint. The generated credential key is returned for the
/// shared WSL environment pass.
pub(super) fn apply_codex_proxy_credential(
    cmd: &mut CommandBuilder,
    routing: &crate::agent::launch_routing::PreparedLaunchRouting,
    provider_enum: Provider,
) -> Option<&'static str> {
    if !matches!(provider_enum, Provider::Codex) {
        return None;
    }
    let key = crate::agent::provider::adapters::codex::PROXY_CREDENTIAL_ENV;
    cmd.env_remove(key);
    let crate::agent::launch_routing::PreparedLaunchRouting::CodexProxy {
        credential_reference,
        credential,
        ..
    } = routing
    else {
        return None;
    };
    debug_assert_eq!(credential_reference, key);
    cmd.env_remove("OPENAI_API_KEY");
    cmd.env_remove("OPENAI_BASE_URL");
    cmd.env(credential_reference, credential);
    Some(key)
}

/// Spawn the child process.
pub fn spawn_child(
    pair: &PtyPair,
    cmd: CommandBuilder,
) -> Result<Box<dyn portable_pty::Child + Send + Sync>, String> {
    pair.slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn agent: {}", e))
}

/// Spawn `cmd` inside the Windows agent sandbox. Returns the same
/// `Child`/`MasterPty` trait objects as the normal path. Windows-only; the
/// non-Windows stub exists only so `spawn_agent_inner` compiles cross-platform
/// (the `sandbox_enabled` seam never selects this branch off Windows).
///
/// Uses the **restricted-token** primitive (ADR-0014), not the AppContainer:
/// the AppContainer's object-namespace isolation hung `claude.exe` at libuv's
/// named-pipe creation (#528) and blocked loopback (#533). The §4 spike proved
/// the restricted token fixes both. It is launched **permissive**
/// (`include_user_sid = true`) — read-confinement is *not* delivered here (a
/// same-user token can't deny home reads while MSYS `bash` runs; see the spike's
/// `tradeoff` test and ADR-0014 §Spike result), so home grants are unnecessary
/// (`grant_home = false`). Deny-by-default reads are a tracked follow-up
/// (separate-user principal / WSL).
#[cfg(target_os = "windows")]
#[allow(clippy::type_complexity)]
pub(super) fn sandbox_spawn(
    cmd: &CommandBuilder,
    session_id: i64,
    host_path: &str,
    rows: u16,
    cols: u16,
) -> Result<
    (
        Box<dyn portable_pty::Child + Send + Sync>,
        Box<dyn portable_pty::MasterPty + Send>,
    ),
    String,
> {
    crate::sandbox::spawn::spawn_sandboxed_restricted(
        cmd, session_id, host_path, rows, cols, false, true,
    )
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::type_complexity)]
pub(super) fn sandbox_spawn(
    _cmd: &CommandBuilder,
    _session_id: i64,
    _host_path: &str,
    _rows: u16,
    _cols: u16,
) -> Result<
    (
        Box<dyn portable_pty::Child + Send + Sync>,
        Box<dyn portable_pty::MasterPty + Send>,
    ),
    String,
> {
    Err("process sandbox is only supported on Windows".to_string())
}

/// Ensures the Claude Code attention hooks exist in
/// `{project}/.claude/settings.local.json`.
///
/// Writes a catch-all `Notification` hook (fires on permission prompts, idle
/// prompts, MCP elicitations — every type that means "the user is needed") plus
/// a `Stop` hook (fires the instant a turn ends). Both POST to the local
/// attention endpoint. Idempotent: re-runs no-op once the config matches, and
/// migrate an older `idle_prompt`-only config on the next spawn.
///
/// This is the Claude-harness implementation behind
/// `AnthropicAdapter::provision_attention_hooks` (issue #886); the mesh commands
/// also call it directly to pre-provision the default harness's hook at mesh
/// creation, before any node/provider exists.
pub fn inject_attention_hook(project_path: &std::path::Path) -> Result<(), String> {
    let claude_dir = project_path.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .map_err(|e| format!("failed to create .claude dir: {e}"))?;

    let settings_path = claude_dir.join("settings.local.json");
    let mut settings: serde_json::Value = match std::fs::read_to_string(&settings_path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };

    // Resolve the port from $BUILDMESH_PORT at hook-run time (set per-agent in
    // spawn_environment) rather than baking a literal. This keeps the hook
    // correct across the 1992→1994 fallback and routes a dev-profile agent's
    // attention to the dev instance (2992), not the stable hub.
    // `--data-binary @-` forwards the hook's stdin JSON ({hook_event_name,
    // transcript_path, …}) as the POST body (issue #878). The backend uses it
    // to tell "turn ended, user needed" from "turn ended, waiting on
    // background tasks"; an empty body degrades to always-mark.
    let hook_command =
        "curl -sf -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true"
            .to_string();

    // We register two hooks so the user is told the *instant* their input is
    // needed, not just when the agent goes idle:
    //   - Notification with an empty (catch-all) matcher fires on every
    //     notification type — crucially `permission_prompt` (the agent is asking
    //     to run a tool / answer a question) as well as `idle_prompt`. Matching
    //     only `idle_prompt` (the old behaviour) missed every permission prompt,
    //     so the user was never alerted when an agent paused to ask something.
    //   - Stop fires the moment the agent finishes a turn, so "agent is waiting
    //     for you" lands immediately instead of after Claude Code's idle timer.
    // Both POST to the same attention endpoint; `mark_attention` is idempotent.
    let notification_hook = serde_json::json!({
        "type": "command",
        "command": hook_command
    });
    let expected_hooks = serde_json::json!({
        "Notification": [{
            "matcher": "",
            "hooks": [notification_hook.clone()]
        }],
        "Stop": [{
            "hooks": [notification_hook]
        }]
    });

    if settings.get("hooks") == Some(&expected_hooks) {
        return Ok(());
    }

    settings["hooks"] = expected_hooks;

    let content =
        serde_json::to_string_pretty(&settings).map_err(|e| format!("serialize failed: {e}"))?;
    std::fs::write(&settings_path, content).map_err(|e| format!("failed to write: {e}"))?;
    tracing::info!("inject_attention_hook: wrote hook at {:?}", settings_path);
    Ok(())
}

/// Check if an agent is already running for this session.
pub fn is_agent_already_running(session_id: &i64) -> bool {
    if let Some(agent) = PROCESS_REGISTRY.get(session_id) {
        if agent.reader_alive.load(Ordering::SeqCst) {
            tracing::info!(
                "spawn_agent: session {} is already running, skipping spawn",
                session_id
            );
            return true;
        }
    }
    false
}

/// Register the agent process in the registry.
///
/// `spawn_start` is the original `SpawnTimer.start` clone — used by
/// `record_first_input_if_first` (via `AgentProcess.spawn_start`) to
/// timestamp the `first_user_input` log line against the same reference
/// as every other `spawn_timing:` checkpoint.
/// Issue #1122: per-agent dedicated PTY writer thread. The thread
/// owns the `Box<dyn Write + Send>` exclusively (no mutex on the
/// hot path) and drains a `std::sync::mpsc::SyncSender` channel that
/// `AgentProcessRegistry::write_bytes` enqueues bytes into from the
/// async runtime. The thread exits when the channel closes (sender
/// dropped) or when the underlying write returns an error (broken
/// pipe — the channel is then disconnected, subsequent `try_send`s
/// return `Disconnected`, and `write_bytes` surfaces "Agent not
/// running" to the caller).
pub(super) fn pty_writer_thread(
    session_id: i64,
    mut writer: Box<dyn std::io::Write + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
) {
    while let Ok(bytes) = rx.recv() {
        if let Err(e) = writer.write_all(&bytes) {
            tracing::warn!(session_id, "PTY writer thread exiting on write error: {e}");
            return;
        }
        if let Err(e) = writer.flush() {
            tracing::warn!(session_id, "PTY writer thread exiting on flush error: {e}");
            return;
        }
    }
    // Channel closed cleanly (kill_session dropped the sender). The
    // writer's `Drop` closes the underlying PTY pipe, so the agent's
    // stdin EOFs and the agent CLI exits cleanly.
    tracing::debug!(session_id, "PTY writer thread exiting (channel closed)");
}

/// Capacity of the bounded `SyncSender` channel between the async
/// Tauri command and the dedicated PTY writer thread. 64 entries ×
/// ~tens of bytes per entry is a few KB of in-flight data — comfortably
/// within the PTY pipe buffer (64 KB on Linux, similar on Windows
/// ConPTY) yet bounded enough that a stuck agent can't grow memory
/// without limit. A full channel surfaces as a `warn!` log and the
/// bytes are dropped (the user can re-type); the alternative — blocking
/// the async runtime on a full bounded channel — would defeat the
/// whole reason the dedicated thread exists.
pub(super) const PTY_WRITER_CHANNEL_CAPACITY: usize = 64;

#[allow(clippy::too_many_arguments)]
pub(super) fn register_agent(
    session_id: i64,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    reader_alive: Arc<AtomicBool>,
    job: Option<crate::process_util::JobHandle>,
    spawn_start: std::time::Instant,
    mesh_id: i64,
    deliberate_kill: Arc<AtomicBool>,
) {
    // Issue #1122: spawn the dedicated PTY writer thread and stand up
    // the bounded channel *before* the registry `insert` so a concurrent
    // `write_bytes` call (visible the moment the entry exists) can never
    // race against a missing channel. The writer thread owns the
    // `Box<dyn Write + Send>` exclusively; the registry holds the
    // sender side.
    let (writer_tx, writer_rx) =
        std::sync::mpsc::sync_channel::<Vec<u8>>(PTY_WRITER_CHANNEL_CAPACITY);
    let writer_handle = std::thread::Builder::new()
        .name(format!("pty-writer-{session_id}"))
        .spawn(move || pty_writer_thread(session_id, writer, writer_rx))
        .expect("failed to spawn PTY writer thread");

    PROCESS_REGISTRY.insert(
        session_id,
        AgentProcess {
            child: Arc::new(Mutex::new(child)),
            writer_tx,
            writer_handle: Mutex::new(Some(writer_handle)),
            // Wrap the master in `Some` so `kill_session` can `take()` it
            // out to drop the pseudoconsole (issue #300).
            master: Arc::new(Mutex::new(Some(master))),
            reader_alive,
            // Shared with the reader thread (started right after this
            // insert) so a `kill_session` teardown is distinguishable
            // from the child dying on its own — see the field docs.
            deliberate_kill,
            job,
            // The handle is set after the reader thread is spawned, via
            // `AgentProcess::set_reader_handle`. We insert first so a
            // concurrent `is_agent_already_running` sees the entry; the
            // window between insert and setter is benign (see process.rs).
            reader_handle: Mutex::new(None),
            spawn_start,
            // First-write gate: starts false, flipped true exactly once
            // by `record_first_input_if_first` on the first successful
            // `write_bytes` call for this session. Plain `AtomicBool` —
            // the field lives inside `Arc<AgentProcess>` already, so no
            // inner Arc is needed (the reader thread doesn't share this
            // flag).
            first_user_input_logged: AtomicBool::new(false),
            // Issue #634: stored at registration so `write_bytes` and the
            // PTY read loop can record per-mesh activity without a DB
            // lookup on every chunk. `mesh_id` was already resolved at
            // `spawn_agent_inner:797` via `db::get_mesh_by_path(&node.path)`
            // — the value is in scope here.
            mesh_id,
        },
    );
}

/// Core PTY read loop: read [`crate::pty::batch::PTY_READ_BUF`] chunks until
/// EOF or error, handing raw bytes to `on_chunk`. Returns when the PTY closes.
///
/// Extracted so the production reader thread and the real-PTY integration test
/// exercise the exact same read path (see `src-tauri/tests/pty_spawn.rs`).
pub fn pump_pty_output(mut reader: Box<dyn std::io::Read + Send>, mut on_chunk: impl FnMut(&[u8])) {
    let mut buf = [0u8; crate::pty::batch::PTY_READ_BUF];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                on_chunk(&buf[..n]);
            }
            Err(e) => {
                tracing::error!("PTY read error: {}", e);
                break;
            }
        }
    }
}

/// Buffer a PTY chunk for session auto-naming — every chunk for LLM
/// providers, never for a plain terminal. A terminal's rename buffer is
/// never consumed: the rename LLM only fires from `on_turn`, which only
/// the Claude stop hook calls. Ungated, each Terminal node would retain
/// up to `MAX_BUFFER_CHARS` and contend the global NAMING mutex on every
/// chunk for the node's whole lifetime (issue #296).
///
/// Extracted from `start_reader`'s pump callback so the gate is
/// unit-testable without standing up an AppHandle / PTY (same seam
/// pattern as `resolve_base_ref_for_spawn`).
pub(crate) fn maybe_buffer_for_naming(is_plain_terminal: bool, session_id: i64, text: &str) {
    if !is_plain_terminal {
        crate::session_naming::on_output(session_id, text);
    }
}

/// Start the PTY reader thread. Returns the `JoinHandle` so the caller
/// can store it on `AgentProcess` and let `kill_session` join with a
/// bounded timeout (issue #300).
///
/// Output dispatch (issue #1385): each OS `read()` still feeds capture /
/// naming / autopilot on this thread, then the bytes go through
/// `pty::batch::with_batcher` (8 ms / 32 KiB) onto a binary Tauri Channel
/// (`OutputSink::send_owned`). Production PTY bytes never share the JSON
/// `agent-output` event — that path is test injection only. The Channel
/// is node-scoped: this reader must not unregister it on exit.
///
/// Two time references are passed in, with distinct semantics — keep
/// them separate:
///
/// * `spawned_at` — process-creation time (`Instant::now()` right after
///   `spawn_child` returns). Used by the 3-second early-exit heuristic
///   to detect a likely-failed `--resume`. **Must NOT be unified with
///   `spawn_start`**: a slow 14s spawn pipeline followed by an agent
///   dying 1s after process creation must still trigger `resume-failed`,
///   and the original "3s after process creation" semantic preserves
///   that detection.
/// * `spawn_start` — the original `SpawnTimer.start` from the top of
///   `spawn_agent_inner`. Used by the `first_pty_output` checkpoint log
///   so it lines up with every other `spawn_timing:` line (all
///   measured against the same "user clicked Spawn" instant).
#[allow(clippy::too_many_arguments)]
pub(super) fn start_reader(
    app: tauri::AppHandle,
    session_id: i64,
    needs_session_capture: bool,
    reader: Box<dyn std::io::Read + Send>,
    spawned_at: std::time::Instant,
    reader_alive: Arc<AtomicBool>,
    is_plain_terminal: bool,
    spawn_start: std::time::Instant,
    mesh_id: i64,
    deliberate_kill: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let app_clone = app;
    let reader_alive_clone = reader_alive;
    // Issue #1221: stateful wrapper that stitches PTY chunks so the
    // `session id: <uuid>` regex can match a banner that straddles an
    // 8 KiB read boundary, and so multi-byte UTF-8 sequences split
    // across reads aren't corrupted to U+FFFD before being handed to
    // `session_naming::on_output` and `autopilot::evaluator::on_output`.
    // `captured` is a plain `bool` (not `AtomicBool`) because the
    // reader thread is the only writer — the `AtomicBool` here used to
    // be load-bearing for `start_reader`'s outer scope but it's now
    // folded into the wrapper. Initialise pre-armed when the caller
    // already knows no capture is needed (e.g. providers like Anthropic
    // that we pre-assigned a UUID to).
    let mut chunk_capture = crate::session_capture::ChunkCapture::default();
    if !needs_session_capture {
        // Force the latch on so post-init feeds skip the regex.
        chunk_capture.mark_captured();
    }

    std::thread::spawn(move || {
        // The SpawnTimer in spawn_agent_inner stops at process *creation*
        // (`after_pty_spawn`), so the shell → agent-CLI boot tail is invisible
        // to it. Log the gap from spawn to the first byte of PTY output here —
        // that first byte is the earliest signal the agent process is actually
        // alive and producing a UI. Same `spawn_timing:` prefix so it sits
        // alongside the other checkpoints. Measured against `spawn_start` (not
        // `spawned_at`) so this elapsed time is comparable to every other
        // checkpoint in the log.
        // Issue #1385: coalesce OS reads onto a dedicated batcher thread
        // so a build-storm of tiny PTY chunks becomes one IPC dispatch
        // per 8 ms / 32 KiB. Capture / naming / autopilot still see every
        // OS read on this thread (ChunkCapture already stitches split
        // banners). `with_batcher` drops the producer before joining —
        // joining while still holding `SyncSender` deadlocks the reader
        // on every PTY exit.
        let sink = crate::agent::output::ensure(session_id);
        let batch_session_id = session_id;
        crate::pty::batch::with_batcher(
            move |batch| {
                // One transport only: Channel (or the sink's pre-subscribe
                // buffer). Never emit JSON `agent-output` for PTY bytes —
                // that path and the Channel have no ordering, so a
                // subscribe landing mid-stream would let later chunks
                // overtake earlier ones and split ANSI.
                crate::http_server::send_pty_output(batch_session_id, &batch);
                sink.send_owned(batch);
            },
            |batch_tx| {
                let mut first_chunk = true;
                pump_pty_output(reader, |data| {
                    if first_chunk {
                        first_chunk = false;
                        tracing::info!(
                            "spawn_timing: session={} checkpoint=first_pty_output elapsed={}ms (spawn start → first output; agent CLI boot tail)",
                            session_id,
                            spawn_start.elapsed().as_millis()
                        );
                    }
                    // Mark THIS MESH as active so the background warm-pool worker
                    // holds off its idle refills for this mesh's pool while an agent
                    // is actively producing output (issue #613 AC2; issue #634 scopes
                    // the activity per-mesh so a chatty agent on mesh A doesn't
                    // starve mesh B's pool). `mesh_id` is captured from the spawn
                    // context at thread start — the closure outlives the agent's
                    // registry entry, so reading it from `PROCESS_REGISTRY` inside
                    // the closure would race with `kill_session`'s `remove`.
                    crate::services::pool_worker::note_activity_for_mesh(mesh_id);

                    let (text, uuid) = chunk_capture.feed(data);
                    maybe_buffer_for_naming(is_plain_terminal, session_id, &text);
                    // Autopilot state evaluator tail (issue #483) — one in-memory
                    // set lookup for non-piloted nodes.
                    crate::autopilot::evaluator::on_output(session_id, &text);
                    // Stale-attention safety net (issue #878) — one map lookup for
                    // unarmed nodes.
                    crate::attention_autoclear::on_output(session_id, data.len());

                    if let Some(uuid) = uuid {
                        // The structured hook and Codex rollout fallback can
                        // capture the same self-assigned ID first. Do not let a
                        // delayed PTY banner replace an already-verified value.
                        let captured =
                            db::set_cli_session_id_if_missing(session_id, &uuid).unwrap_or(false);
                        if captured {
                            tracing::info!(
                                "session_capture: captured session ID {} for node {}",
                                uuid,
                                session_id
                            );
                        }
                    }

                    let _ = batch_tx.send(data.to_vec());
                });
            },
        );
        // The Channel subscription belongs to the Agent Node's persistent
        // terminal, not this process incarnation. Keep it live so retry,
        // resume, and regenerate output reaches the same xterm instance.
        tracing::debug!(
            "PTY reader loop ended for session {}, reader exiting",
            session_id
        );
        reader_alive_clone.store(false, Ordering::SeqCst);

        // `spawned_at` is process-creation time, NOT `spawn_start`: the
        // early-exit heuristic answers "did the process die almost
        // immediately after it was created?" — a slow 14s pipeline
        // followed by a 1s-later death must still read as an early exit.
        match post_exit_action(
            is_plain_terminal,
            deliberate_kill.load(Ordering::SeqCst),
            spawned_at.elapsed(),
        ) {
            PostExitAction::LeaveStatusAlone => {
                // kill_session initiated this exit; the kill initiator
                // owns the node's next status (see PostExitAction docs).
                tracing::debug!(
                    "Node {} reader exited after deliberate kill — leaving status to the kill initiator",
                    session_id
                );
            }
            PostExitAction::MarkIdle => {
                // Routes through SessionLifecycle (issue #132) — single writer
                // for `agent_nodes.status`.
                let sink = session_lifecycle::AppSessionLifecycleSink { app: &app_clone };
                let _ = session_lifecycle::on_pty_eof(&sink, session_id);
            }
            PostExitAction::MarkErrorResumeFailed => {
                tracing::warn!(
                    "Node {} reader exited after {:?} — likely resume failure",
                    session_id,
                    spawned_at.elapsed()
                );
                // Routes through SessionLifecycle (issue #132) — the
                // `unless_in(Error, Archived)` guard (#654) lives inside
                // `on_resume_failed`, and `resume-failed` is emitted from
                // exactly one place (the lifecycle sink).
                let sink = session_lifecycle::AppSessionLifecycleSink { app: &app_clone };
                let _ = session_lifecycle::on_resume_failed(
                    &sink,
                    session_id,
                    "Agent exited immediately after spawn — session may have expired",
                );
            }
        }

        tracing::debug!("PTY reader thread exited for session {}", session_id);
    })
}

// ---------------------------------------------------------------------------
// Resume decision surface (issue #949 / PR #1121)
// ---------------------------------------------------------------------------
