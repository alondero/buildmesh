//! Agent spawning — the `spawn_agent_inner` function and its helpers.
//!
//! Provider-specific recipe lives behind `Provider::adapter()` (see `agent/provider`).
//! OS-specific wrapping lives in `spawn_environment`.
//! PTY-specific helpers (open_pty_pair, spawn_child) live in `process.rs`.

use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::process::{AgentProcess, PROCESS_REGISTRY};
use crate::agent::provider::{Platform, CLAUDE_BACKEND_ENV_VARS};
use crate::agent::session_lifecycle;
use crate::agent::spawn_environment;
use crate::db;
use crate::env;
use crate::git::worktree::provision::{
    fork_remote_alias, locked_fetch_pr_head, read_origin_ref_sha, AppHandleSink,
    ProvisionHooks, SpawnContext, SpawnSource, provision_for_spawn,
};
// The bare fetch helpers (`fetch_single_ref`, `fetch_fork_head`) are
// re-exported under `#[cfg(test)]` below — production code goes through
// `locked_fetch_pr_head` (issue #698), which wraps the per-Mesh
// `with_mesh_sync_lock` around the bare helpers so callers can't forget to
// serialize concurrent PR-spawn fetches.
use crate::models::{AgentNode, EnvType, Provider};

mod intent;
pub(crate) use intent::{
    ExplicitSpawnOverrides, GitHubWorkContext, ResumeCause, SpawnIntent, SpawnOutcome,
    SpawnRequest, TerminalSize,
};
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Wire types — Tauri event payloads (issue #161)
// ---------------------------------------------------------------------------

/// Payload of the `agent-output` Tauri event. Streamed from the PTY reader
/// thread for every node. Exactly one of `data` (base64-encoded bytes) or
/// `line` (raw UTF-8 string) is populated — the listener branches on which
/// is `Some`. The empty-both case is meaningless and ignored.
///
/// Generated to `src/types/generated/AgentOutputPayload.ts`; the TS half is
/// imported by `src/components/Terminal/TerminalRegistry.ts`. The wire key is
/// `session_id` (NOT `node_id`) — historically this mismatched the test
/// server's emit (which used `node_id`) and silently dropped test
/// injections; issue #161 realigns both sides on `session_id`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AgentOutputPayload.ts")]
pub struct AgentOutputPayload {
    #[ts(as = "i32")]
    pub session_id: i64,
    pub data: Option<String>,
    pub line: Option<String>,
}

/// Payload of the `agent-spawned` Tauri event. Emitted once stage-2 of the
/// two-stage spawn (the slow path that registers the process in
/// `PROCESS_REGISTRY`) completes. The frontend listener re-pushes the
/// terminal's fitted dimensions via `resize_agent`, closing the auto-spawn
/// attach-fit race (issue #332).
///
/// Generated to `src/types/generated/AgentSpawnedPayload.ts`; the TS half is
/// imported by `src/components/Terminal/TerminalRegistry.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AgentSpawnedPayload.ts")]
pub struct AgentSpawnedPayload {
    #[ts(as = "i32")]
    pub session_id: i64,
    #[ts(as = "i32")]
    pub rows: i32,
    #[ts(as = "i32")]
    pub cols: i32,
}

/// Payload of the `provider-error` Tauri event. Emitted by the sandbox-spawn
/// and direct-pty branches when the agent CLI could not be launched
/// (network denied, AppContainer mis-config, cwrap spawn failed, etc). The
/// frontend surfaces it as an error toast; the `provider` field drives the
/// toast label so the user knows which harness choked.
///
/// Generated to `src/types/generated/ProviderErrorPayload.ts`; the TS half is
/// imported by `src/App.tsx`. `session_id` is included for completeness
/// (lets future listners jump to the failing node) — pre-#161 the inline TS
/// type only declared `provider` + `message` and silently dropped it.
/// `provider` is the typed [`Provider`] enum (not a plain string) so the
/// serde-derived lowercase wire value (`"anthropic"`, `"codex"`, …) is
/// preserved across the typed rewrite.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "ProviderErrorPayload.ts")]
pub struct ProviderErrorPayload {
    #[ts(as = "i32")]
    pub session_id: i64,
    pub provider: crate::models::Provider,
    pub message: String,
}

/// Payload of the `mesh-sync-warning` Tauri event. Emitted for any non-fatal
/// auto-sync failure (network down, diverged history, repo unusable, PR-head
/// unfetchable, PR SHA drift). The `outcome` discriminator drives the toast
/// label / extra actions in the frontend; per-variant fields populate
/// context for that copy.
///
/// Generated to `src/types/generated/MeshSyncWarningPayload.ts`; the TS half
/// is imported by `src/App.tsx`. Kept as a flat struct with `Option<...>`
/// fields rather than a tagged-enum so a single TS type covers all six
/// outcomes — the frontend only reads `message` regardless.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "MeshSyncWarningPayload.ts")]
pub struct MeshSyncWarningPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub mesh_path: String,
    pub outcome: MeshSyncOutcome,
    #[ts(as = "Option<i32>")]
    pub new_commits: Option<u32>,
    #[ts(as = "Option<i32>")]
    pub pr_number: Option<i64>,
    pub head_ref: Option<String>,
    pub expected_sha: Option<String>,
    pub actual_sha: Option<String>,
    pub fallback_base_ref: Option<String>,
    pub head_repo_owner: Option<String>,
    pub head_repo_clone_url: Option<String>,
    pub message: String,
}

/// `outcome` discriminant for [`MeshSyncWarningPayload`]. The Rust enum's
/// `#[serde(rename_all = "snake_case")]` keeps the wire variants in the
/// shape the pre-#161 inline TS union expected.
#[derive(Debug, Clone, Copy, Serialize, TS)]
#[ts(export, export_to = "MeshSyncOutcome.ts")]
#[serde(rename_all = "snake_case")]
pub enum MeshSyncOutcome {
    Diverged,
    FetchFailed,
    RepoUnusable,
    PrHeadUnfetchable,
    PrForkUnfetchable,
    PrShaDrift,
}

/// Default `worktree_mode` when the mesh config leaves it unset. Pinned by
/// the unit test in this module (`default_worktree_mode_is_branched`).
///
/// This was previously paired with a TS sentinel at `src/lib/worktreeMode.ts`,
/// deleted in #411 once the TS side lost its only consumer (a self-referential
/// test). If a future UI re-exposes a worktree-mode selector, re-introduce
/// the TS constant alongside it and re-couple by doc comment + paired test
/// (see [[feedback_cross-language-default-coupling]]). See
/// `docs/knowledge-primer.md` (Worktree Support) for the branched-vs-detached
/// rationale.
pub const DEFAULT_WORKTREE_MODE: &str = "branched";

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
static SPAWNS_IN_FLIGHT: once_cell::sync::Lazy<parking_lot::Mutex<std::collections::HashSet<i64>>> =
    once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(std::collections::HashSet::new()));

/// RAII claim on a session id in [`SPAWNS_IN_FLIGHT`]. Dropping releases
/// the claim on every exit path, including a cancelled async task.
pub(crate) struct SpawnInFlightClaim {
    session_id: i64,
}

impl SpawnInFlightClaim {
    /// `Some(claim)` if no spawn is in flight for this session, `None` if
    /// one already is (the caller should short-circuit as a duplicate).
    pub(crate) fn try_claim(session_id: i64) -> Option<Self> {
        // parking_lot::Mutex::lock is non-poisoning and a strict upgrade
        // over std here: no unwrap on contention, and `try_lock` lets
        // Drop fall back gracefully if the runtime is mid-shutdown.
        let mut guard = SPAWNS_IN_FLIGHT.lock();
        if guard.insert(session_id) {
            Some(Self { session_id })
        } else {
            None
        }
    }
}

impl Drop for SpawnInFlightClaim {
    fn drop(&mut self) {
        // `lock` (blocking) is correct here: the guard's lifetime is the
        // whole spawn function, so contention is at most a few µs of
        // contended HashSet::remove — never long enough to starve a
        // tokio worker. `try_lock` would silently leak the claim on
        // contention, which is the opposite of what the bug requires.
        SPAWNS_IN_FLIGHT.lock().remove(&self.session_id);
    }
}

/// Resolve the `base_ref` string that `git::sync::fetch_origin` will use for
/// the spawn-time auto-sync. The chain (each tier only runs if the previous
/// one yields nothing useful):
///
/// 1. The mesh's `base_ref` column from the `meshes` DB row — explicit
///    user intent wins, even on a repo whose default branch disagrees.
///    **The COALESCE default `'origin/main'` is treated as "no config"**:
///    a fresh mesh whose `base_ref` column was never explicitly set reads
///    as `'origin/main'` from the DB (see `db::MESH_COLUMNS`), and a
///    user who never touched the field is functionally identical to a
///    user who has no config. Detecting both via the same path is what
///    closes the master-trunk regression. **There is no `mesh.toml`
///    file**: the value lives on the `meshes` SQLite row (and is
///    mirrored to `.claude/settings.json` at the mesh root for Claude
///    Code, see `commands::mesh_properties`).
/// 2. The repo's actual default branch read from
///    `refs/remotes/origin/HEAD` (populated by `git clone` / `git fetch`)
///    — closes the master-trunk regression where a repo whose default
///    branch is `master` was always fetched as `origin/main`.
/// 3. The literal `"origin/main"` as a last resort. Used only for a
///    non-repo / unconfigured path so the spawn path never blocks.
///
/// Extracted from `spawn_agent_inner` so the regression test in
/// `mod tests` can call it directly without standing up the full async /
/// PTY / DB machinery — the call site is a single expression.
///
/// `pub(crate)` so the background mesh sync (`services::pool_worker`)
/// resolves its fetch target through the exact same 3-tier chain the spawn
/// uses — a worker that fetched a literal `origin/main` on a repo whose
/// default branch is `master` would fail every pass and never satisfy the
/// spawn-time freshness TTL.
pub(crate) fn resolve_base_ref_for_spawn(mesh_path: &str, config_base_ref: Option<&str>) -> String {
    const COALESCE_DEFAULT: &str = "origin/main";
    let user_set = config_base_ref.filter(|b| b.trim() != COALESCE_DEFAULT);
    if let Some(b) = user_set {
        let trimmed = b.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // No explicit config (or the COALESCE sentinel) — read the repo's
    // actual default branch from `refs/remotes/origin/HEAD` (populated by
    // `git clone` / `git fetch`). `get_default_branch` falls back to
    // "main" if the repo can't be opened or the symbolic ref is missing,
    // so a non-repo / unconfigured mesh path still resolves to
    // "origin/main" — preserving pre-fix behaviour and never blocking the
    // spawn.
    //
    // Called from the synchronous spawn path — use the sync core directly
    // (issue #762). The blocking pool offload that the async wrapper
    // provides is irrelevant for this single repo-open + symbolic-ref read;
    // the small wall-clock cost is well within the spawn budget and the
    // outer spawn task already runs on a blocking-pool thread.
    let branch = crate::commands::git::get_default_branch_blocking(mesh_path.to_string())
        .unwrap_or_else(|_| "main".to_string());
    format!("origin/{}", branch)
}

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
struct SpawnTimer {
    start: std::time::Instant,
    session_id: i64,
}

impl SpawnTimer {
    fn new(session_id: i64) -> Self {
        Self {
            start: std::time::Instant::now(),
            session_id,
        }
    }

    fn checkpoint(&self, name: &str) {
        tracing::info!(
            "spawn_timing: session={} checkpoint={} elapsed={}ms",
            self.session_id,
            name,
            self.start.elapsed().as_millis()
        );
    }

    fn total(&self) {
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

/// Options for spawning or resuming an agent process.
pub struct SpawnOptions {
    pub session_id: i64,
    pub provider: Provider,
    pub resume: Option<String>,
    pub rows: u16,
    pub cols: u16,
    pub prefill: Option<String>,
    /// Pre-fetched node to avoid a redundant DB read when the caller already has it.
    pub node: Option<AgentNode>,
    /// Cascade layer-1 model override (issue #1155). Highest precedence
    /// in the spawn-config cascade — wins over the Mesh row and the
    /// application default. `None` or whitespace-only collapses to
    /// absent at [`cascade_inputs_for`] so the cascade falls through.
    pub explicit_model: Option<String>,
    /// Cascade layer-1 effort / reasoning override (issue #1155). Same
    /// semantics as [`Self::explicit_model`] — independent field, only
    /// matters when the harness's capability descriptor declares effort
    /// support (otherwise the resolver mask drops it).
    pub explicit_effort: Option<String>,
    /// Cascade layer-1 verbatim CLI flag string (issue #1358). No mesh
    /// / application layer carries per-spawn flags, so this is the only
    /// layer of supply. Capability-masked downstream — a harness whose
    /// descriptor reports `supports_extra_args = false` (Terminal is
    /// the only one) silently drops the value at the resolver rather
    /// than splicing a synthetic flag into its argv.
    pub explicit_extra_args: Option<String>,
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
fn reader_should_capture_session_id(
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
        crate::agent::launch_routing::PreparedLaunchRouting::CodexProxy { install, .. } => {
            (install.wsl_distro.as_deref(), Some(install.executable.as_str()))
        }
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

    // Inject the per-profile backend env + Codex Proxy credential.
    // Extracted as helpers because the WSLENV bookkeeping is intricate
    // and was previously duplicated in two large inline blocks.
    apply_routing_env(&mut cmd, routing, resolved.env_type);
    apply_codex_proxy_credential(&mut cmd, routing, provider_enum, resolved.env_type);
    cmd
}

/// Apply the per-profile backend env (`PreparedLaunchRouting::Environment`)
/// to the child command. On WSL, appends the new key names to `WSLENV` with
/// the `/u` suffix so values cross the WSL boundary (only WSLENV-listed
/// vars propagate). Existing WSLENV entries are deduped by base name.
fn apply_routing_env(
    cmd: &mut CommandBuilder,
    routing: &crate::agent::launch_routing::PreparedLaunchRouting,
    env_type: EnvType,
) {
    let backend_env: &[(String, String)] = match routing {
        crate::agent::launch_routing::PreparedLaunchRouting::Environment(values) => {
            values.as_slice()
        }
        _ => &[],
    };
    if backend_env.is_empty() {
        return;
    }
    for (k, v) in backend_env {
        cmd.env(k, v);
    }
    if env_type == EnvType::Wsl {
        let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
        for (k, _) in backend_env {
            append_to_wslenv(&mut wslenv, k, "/u");
        }
        if !wslenv.is_empty() {
            cmd.env("WSLENV", wslenv);
        }
    }
}

/// Apply the Codex Proxy pairing-scoped credential. A verified profile
/// authenticates exclusively through its pairing-scoped reference
/// (`PROXY_CREDENTIAL_ENV`); generic `OPENAI_API_KEY` / `OPENAI_BASE_URL`
/// inherited by Buildmesh are stripped so they cannot become an alternate
/// credential/endpoint. On WSL the credential key (and `CODEX_HOME` when
/// set) is appended to WSLENV.
fn apply_codex_proxy_credential(
    cmd: &mut CommandBuilder,
    routing: &crate::agent::launch_routing::PreparedLaunchRouting,
    provider_enum: Provider,
    env_type: EnvType,
) {
    if !matches!(provider_enum, Provider::Codex) {
        return;
    }
    let key = crate::agent::provider::adapters::codex::PROXY_CREDENTIAL_ENV;
    cmd.env_remove(key);
    let crate::agent::launch_routing::PreparedLaunchRouting::CodexProxy {
        credential_reference,
        credential,
        ..
    } = routing
    else {
        return;
    };
    debug_assert_eq!(credential_reference, key);
    cmd.env_remove("OPENAI_API_KEY");
    cmd.env_remove("OPENAI_BASE_URL");
    cmd.env(credential_reference, credential);
    if env_type == EnvType::Wsl {
        let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
        append_to_wslenv(&mut wslenv, key, "/u");
        if std::env::var_os("CODEX_HOME").is_some() {
            append_to_wslenv(&mut wslenv, "CODEX_HOME", "/u");
        }
        if !wslenv.is_empty() {
            cmd.env("WSLENV", wslenv);
        }
    }
}

/// Append a key (with its WSLENV suffix flag, usually `/u`) to the
/// colon-delimited WSLENV list, deduplicated by base name. Pure helper
/// so the Codex and the per-profile-backend paths agree on the rule.
fn append_to_wslenv(wslenv: &mut String, key: &str, suffix: &str) {
    let already_has = wslenv.split(':').any(|part| {
        part.split('/').next() == Some(key)
    });
    if already_has {
        return;
    }
    let entry = format!("{key}{suffix}");
    if wslenv.is_empty() {
        wslenv.push_str(&entry);
    } else {
        wslenv.push(':');
        wslenv.push_str(&entry);
    }
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
fn sandbox_spawn(
    cmd: &CommandBuilder,
    session_id: i64,
    host_path: &str,
    rows: u16,
    cols: u16,
) -> Result<(Box<dyn portable_pty::Child + Send + Sync>, Box<dyn portable_pty::MasterPty + Send>), String> {
    crate::sandbox::spawn::spawn_sandboxed_restricted(cmd, session_id, host_path, rows, cols, false, true)
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::type_complexity)]
fn sandbox_spawn(
    _cmd: &CommandBuilder,
    _session_id: i64,
    _host_path: &str,
    _rows: u16,
    _cols: u16,
) -> Result<(Box<dyn portable_pty::Child + Send + Sync>, Box<dyn portable_pty::MasterPty + Send>), String> {
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
/// `AnthropicAdapter::inject_attention_hook` (issue #886); the mesh commands
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

    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize failed: {e}"))?;
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
fn pty_writer_thread(
    session_id: i64,
    mut writer: Box<dyn std::io::Write + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
) {
    while let Ok(bytes) = rx.recv() {
        if let Err(e) = writer.write_all(&bytes) {
            tracing::warn!(
                session_id,
                "PTY writer thread exiting on write error: {e}"
            );
            return;
        }
        if let Err(e) = writer.flush() {
            tracing::warn!(
                session_id,
                "PTY writer thread exiting on flush error: {e}"
            );
            return;
        }
    }
    // Channel closed cleanly (kill_session dropped the sender). The
    // writer's `Drop` closes the underlying PTY pipe, so the agent's
    // stdin EOFs and the agent CLI exits cleanly.
    tracing::debug!(
        session_id,
        "PTY writer thread exiting (channel closed)"
    );
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
const PTY_WRITER_CHANNEL_CAPACITY: usize = 64;

#[allow(clippy::too_many_arguments)]
fn register_agent(
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

fn encode_pty_chunk(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Core PTY read loop: read 8 KiB chunks until EOF or error, handing raw bytes
/// to `on_chunk`. Returns when the PTY closes.
///
/// Extracted so the production reader thread and the real-PTY integration test
/// exercise the exact same read path (see `src-tauri/tests/pty_spawn.rs`).
pub fn pump_pty_output(
    mut reader: Box<dyn std::io::Read + Send>,
    mut on_chunk: impl FnMut(&[u8]),
) {
    let mut buf = [0u8; 8192];
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
fn start_reader(
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
                let captured = db::set_cli_session_id_if_missing(session_id, &uuid)
                    .unwrap_or(false);
                if captured {
                    tracing::info!("session_capture: captured session ID {} for node {}", uuid, session_id);
                }
            }

            let _ = app_clone.emit(
                "agent-output",
                AgentOutputPayload {
                    session_id,
                    data: Some(encode_pty_chunk(data)),
                    line: None,
                },
            );

            // Forward to any connected mobile WebSocket clients
            crate::http_server::send_pty_output(session_id, data.to_vec());
        });
        tracing::debug!("PTY reader loop ended for session {}, reader exiting", session_id);
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
                let sink = session_lifecycle::AppSessionLifecycleSink {
                    app: &app_clone,
                };
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
                let sink = session_lifecycle::AppSessionLifecycleSink {
                    app: &app_clone,
                };
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

/// Pure decision for "given the stored CLI session id, the resume cause,
/// and whether the adapter auto-resumes on startup, what should
/// `spawn_with_intent` do?". The Skip variants are the regression-pin
/// for issue #949: a future refactor that re-introduces an `on_idle`
/// call inside the Skip arms fails review by virtue of the decision
/// being a single enum variant.
///
/// Empty-string defense: legacy writes can leave an empty string in
/// `agent_nodes.cli_session_id`. `db::list_suspended_nodes`'s SQL
/// `IS NOT NULL` filter only catches NULL, so the empty case is
/// defended here.
pub(crate) fn decide_startup_resume(
    cli_session_id: Option<&str>,
    cause: ResumeCause,
    auto_resume_on_startup: bool,
) -> ResumeSkipDecision {
    let stored = cli_session_id.filter(|s| !s.is_empty());
    match (cause, stored) {
        (ResumeCause::Startup, None) => ResumeSkipDecision::SkipSuspended,
        (_, None) => ResumeSkipDecision::NoSessionId,
        (ResumeCause::Startup, Some(_id)) if !auto_resume_on_startup => {
            ResumeSkipDecision::SkipAdapterDeclines
        }
        (_, Some(id)) => ResumeSkipDecision::Proceed(id.to_string()),
    }
}

/// Decision surface for the Startup resume-skip path (issue #949 /
/// PR #1121). See [`decide_startup_resume`] for the full rationale —
/// in short: every `Skip*` variant MUST be paired with a
/// `SpawnOutcome::Skipped(node)` return path that does NOT call any
/// `sink.write_status`. The node stays `Suspended` so the user's
/// Resume / Regenerate affordances remain reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeSkipDecision {
    /// The Startup resume is not viable (missing or empty
    /// `cli_session_id`); return `SpawnOutcome::Skipped(node)` without
    /// touching `agent_nodes.status`. The node stays `Suspended`.
    SkipSuspended,
    /// The Startup resume is not viable because the adapter declines
    /// (`auto_resume_on_startup() == false`); return
    /// `SpawnOutcome::Skipped(node)` without touching
    /// `agent_nodes.status`. The node stays `Suspended`.
    SkipAdapterDeclines,
    /// The Explicit (user-driven) resume is not viable because there
    /// is no captured session id; the caller surfaces this as an
    /// `Err`. Distinct from `SkipSuspended` because the user expects
    /// an error toast, not a silent no-op.
    NoSessionId,
    /// The resume IS viable; the caller continues the spawn flow and
    /// the captured `cli_session_id` is returned.
    Proceed(String),
}

// ---------------------------------------------------------------------------
// Public Tauri command interface
// ---------------------------------------------------------------------------

/// Start an Agent Node from a domain intent.
///
/// The durable node is the source of truth for provider, path, and session
/// identity. Callers only select the reason for starting it and the initial
/// terminal size; low-level `SpawnOptions` stays inside this module while
/// existing callers migrate to the intent seam.
///
/// ## Resume-skip decision (issue #949, PR #1121)
///
/// `spawn_with_intent` previously wrote `Idle` for every Startup-resume
/// branch it couldn't honour, which silently dropped Suspended nodes with
/// no UI recovery affordance. The fix was to short-circuit these branches
/// to `SpawnOutcome::Skipped` WITHOUT touching `agent_nodes.status` —
/// leaving them as `Suspended` so the user can drive the new Resume /
/// Regenerate affordances.
///
/// [`decide_startup_resume`] is the testable surface of that contract:
/// it takes only the three facts the decision depends on (the stored
/// `cli_session_id`, the cause, and whether the adapter auto-resumes on
/// startup) and returns the decided outcome. The Skip variants in
/// particular must NEVER trigger a sink write — the regression test in
/// `mod tests` pins the decision matrix.
pub(crate) async fn spawn_with_intent(
    app: &tauri::AppHandle,
    request: SpawnRequest,
) -> Result<SpawnOutcome, String> {
    let SpawnRequest {
        node_id,
        intent,
        terminal_size,
        explicit,
    } = request;
    // Bind the type name so the `ExplicitSpawnOverrides` re-export stays
    // live at the module scope (the destructure pattern alone doesn't
    // count as a use). The value flows through to `SpawnOptions` below;
    // the annotation is the only thing this line adds.
    let explicit: ExplicitSpawnOverrides = explicit;
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    let provider = crate::preferences::resolve_harness_provider(&node.provider);
    let adapter = provider.adapter();
    let is_resume_intent = matches!(intent, SpawnIntent::Resume { .. });

    let resume = match &intent {
        SpawnIntent::Resume { cause } => {
            let decision = decide_startup_resume(
                node.cli_session_id.as_deref(),
                *cause,
                adapter.auto_resume_on_startup(),
            );
            match decision {
                // Startup resume with no cli_session_id: there is nothing
                // for us to resume. DO NOT write Idle -- that was the
                // silent-drop bug that stranded Suspended OpenCode /
                // Terminal nodes with no UI recovery affordance.
                // Leaving the status as Suspended means the user can
                // click the new Resume button in the sidebar / header
                // to retry with ResumeCause::Explicit. The
                // auto_resume_agent_nodes caller in commands/agent.rs
                // queries db::list_suspended_nodes so the row is
                // always already Suspended here; the prior on_idle
                // was redundant at best and silently destructive.
                ResumeSkipDecision::SkipSuspended
                // Startup resume but the adapter declines (OpenCode,
                // Terminal -- they have no --resume flag and no
                // auto-resume). DO NOT write Idle (same rationale as
                // the cli_session_id-missing branch above): the node
                // stays Suspended so the user's new Resume button can
                // retry later, or the node can be regenerated to a
                // different provider. The Explicit branch
                // (ResumeCause::Explicit from user-driven Resume /
                // Regenerate) is not affected -- the explicit user's
                // expectation is that we try the captured session id
                // via `supports_resume()`, not that we silently skip.
                | ResumeSkipDecision::SkipAdapterDeclines => {
                    return Ok(SpawnOutcome::Skipped(node));
                }
                ResumeSkipDecision::NoSessionId => {
                    return Err(format!(
                        "cannot resume node {}: no CLI session ID is stored",
                        node.id
                    ));
                }
                // Adapter cannot honour a resume arg (OpenCode,
                // Terminal -- no --resume flag) under an Explicit
                // cause: fall through to a fresh process launch while
                // retaining the captured id. Unlike an explicit Fresh
                // intent, this preserves the identity so a future
                // Regenerate to a resumable harness can still pick it up.
                // Without this, the user-driven Resume button on a
                // Suspended OpenCode node would surface a toast instead
                // of starting fresh on the same worktree.
                ResumeSkipDecision::Proceed(id) => Some(id),
            }
        }
        _ => None,
    };

    // Issue #1180 — the prefill comes from `SpawnIntent::initial_prompt`,
    // the single source of truth shared with the desktop draft response
    // and the Autopilot watcher. `into_string()` consumes the
    // `InitialPrompt` wrapper, giving us an owned `String` without an
    // extra `as_str().to_string()` re-allocation. A supporting harness
    // forwards the same string the user already saw on the draft
    // response (byte-identical).
    let prefill = intent
        .initial_prompt()
        .map(|prompt| prompt.into_string())
        .filter(|prefill| {
            if adapter.supports_prefill() {
                true
            } else {
                tracing::warn!(
                    "spawn_with_intent: provider '{}' does not support prefill; skipping {} bytes",
                    node.provider,
                    prefill.len()
                );
                false
            }
        });

    if is_agent_already_running(&node_id) {
        return Ok(SpawnOutcome::AlreadyActive(node));
    }

    if intent_replaces_conversation(&intent) {
        // Every non-resume intent is a deliberate new conversation, so no old harness identity
        // may survive it. In particular, self-assigning providers persist
        // their new id fill-only after launch; retaining an old id here would
        // make the next startup resume the wrong conversation.
        db::clear_cli_session_id(node_id).map_err(|e| e.to_string())?;
    }

    let result = spawn_agent_inner(
        app,
        SpawnOptions {
            session_id: node_id,
            provider,
            resume,
            rows: terminal_size.rows,
            cols: terminal_size.cols,
            prefill,
            node: Some(node.clone()),
            // Issue #1358: per-spawn extra_args ride the explicit layer
            // through to `spawn_agent_inner`, where `resolve_spawn_config`
            // capability-masks them against `HarnessCapabilities
            // .supports_extra_args` (Terminal drops; every interactive
            // harness keeps).
            explicit_extra_args: explicit.extra_args,
            // Cascade layer-1 overrides flow through verbatim. Empty /
            // whitespace-only values are normalised at `cascade_inputs_for`
            // inside `spawn_agent_inner` so the cascade falls through to
            // the next layer rather than forwarding a synthetic blank
            // arg to the harness (issue #1148 AC #32 + #1155 AC #3).
            explicit_model: explicit.model,
            explicit_effort: explicit.effort,
        },
    )
    .await;

    match result {
        Ok(()) => {
            let refreshed = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
            let _ = app.emit(
                "node-spawn-completed",
                crate::commands::agent::NodeSpawnCompletedPayload { node_id },
            );
            Ok(SpawnOutcome::Started(refreshed))
        }
        Err(error) => {
            let sink = session_lifecycle::AppSessionLifecycleSink { app };
            if is_resume_intent {
                let _ = session_lifecycle::on_resume_failed(&sink, node_id, &error);
            } else {
                let _ = session_lifecycle::on_error(&sink, node_id);
            }
            let _ = app.emit(
                "node-spawn-failed",
                crate::commands::agent::NodeSpawnFailedPayload {
                    node_id,
                    error: error.clone(),
                },
            );
            Err(error)
        }
    }
}

/// Whether this request intentionally discards the node's prior conversation.
/// A Resume request can still launch a fresh process for a non-resumable
/// adapter, but that is not user intent to replace the captured identity.
fn intent_replaces_conversation(intent: &SpawnIntent) -> bool {
    !matches!(intent, SpawnIntent::Resume { .. })
}

/// Build the per-field cascade inputs the spawn pipeline hands to
/// [`crate::agent::capabilities::resolve_agent_config`] (issue #1155).
///
/// Pure helper so the wiring is testable independent of the resolver —
/// the resolver already has unit tests for its cascade order
/// (`resolver_cascade_prefers_explicit_over_mesh_over_application`), but
/// that test never proves the spawn pipeline *populates* the explicit
/// slot from `SpawnOptions`. This helper is the seam every future spawn
/// site writes to if it wants layer-1 precedence, and the unit tests in
/// `mod tests` below pin both the field-by-field wiring AND the cascade
/// precedence when fed through the resolver.
///
/// Whitespace-only / empty strings on the explicit slot collapse to
/// `None` here (closer to the transport — mobile HTTP / autopilot / UI
/// — than the resolver) so the cascade falls through to the next layer
/// regardless of whether the caller or the resolver did the trimming.
/// Mirrors `resolve_field`'s `normalize_non_empty` (issue #1148 AC #32,
/// #1155 AC #3).
///
/// `mesh_*` and the `application` slot borrow from the mesh row /
/// preferences cache and pass straight through; the resolver normalises
/// those layers at its seam.
pub(crate) fn cascade_inputs_for<'a>(
    explicit_model: Option<&'a str>,
    explicit_effort: Option<&'a str>,
    mesh_model: Option<&'a str>,
    mesh_effort: Option<&'a str>,
    app_default: Option<&'a crate::preferences::HarnessConfigValue>,
    mesh_override: Option<&'a crate::preferences::HarnessConfigValue>,
) -> crate::agent::capabilities::AgentConfigInputs<'a> {
    /// Trim; collapse empty / whitespace-only to `None`. Mirrors
    /// `capabilities::normalize_non_empty` at the spawn seam (issue
    /// #1148 AC #32 + #1155 AC #3). Inline closure hoisted here so the
    /// model + effort legs share the same shape.
    fn non_empty_trim(s: &str) -> Option<&str> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
    crate::agent::capabilities::AgentConfigInputs {
        model: crate::agent::capabilities::FieldInputs {
            explicit: explicit_model.and_then(non_empty_trim),
            mesh_override: mesh_override.and_then(|v| v.model.as_deref()),
            mesh: mesh_model,
            application: app_default.and_then(|v| v.model.as_deref()),
        },
        effort: crate::agent::capabilities::FieldInputs {
            explicit: explicit_effort.and_then(non_empty_trim),
            mesh_override: mesh_override.and_then(|v| v.effort.as_deref()),
            mesh: mesh_effort,
            application: app_default.and_then(|v| v.effort.as_deref()),
        },
    }
}

/// Pure seam for the spawn orchestrator's resolver call (issue #1157).
/// Composes `capabilities_for(provider.adapter())` +
/// `cascade_inputs_for` + `resolve_agent_config` into a single pure
/// function so the integration test for issue #1155 AC #4 ("Regression
/// tests must verify layer-1 behavior at a real spawn site, not just
/// resolver unit tests") can drive the full `SpawnRequest → resolver`
/// path through the same call shape `spawn_agent_inner` uses — without
/// standing up a Tauri runtime, a preferences cache, or a DB.
///
/// `app_default` is the ALREADY-LOOKED-UP value for the harness
/// profile. The orchestrator parses the composite `node.provider` id
/// (`"<harness>:<provider>"` for Proxied rows) and resolves the harness
/// default at its seam so this helper stays free of
/// `preferences::load()` (which would force the test to populate the
/// in-process preferences cache).
pub(crate) fn resolve_spawn_config(
    provider: Provider,
    explicit_model: Option<&str>,
    explicit_effort: Option<&str>,
    explicit_extra_args: Option<&str>,
    app_default: Option<&crate::preferences::HarnessConfigValue>,
    mesh_override: Option<&crate::preferences::HarnessConfigValue>,
) -> crate::agent::capabilities::ResolvedAgentConfig {
    let capabilities = crate::agent::capabilities::capabilities_for(provider.adapter());
    // Issue #1358: all three cascaded fields (model / effort /
    // extra-args) flow into one resolver call so `ResolvedAgentConfig`
    // is constructed atomically (issue #1362 code review). The extra-args
    // mask lives inside `resolve_agent_config` next to the others.
    crate::agent::capabilities::resolve_agent_config(
        &capabilities,
        cascade_inputs_for(
            explicit_model,
            explicit_effort,
            None,
            None,
            app_default,
            mesh_override,
        ),
        explicit_extra_args,
    )
}

/// Transitional implementation retained while transport callers migrate to
/// [`spawn_with_intent`]. It is private to the agent module once migration is
/// complete.
pub(crate) async fn spawn_agent_inner(
    app: &tauri::AppHandle,
    opts: SpawnOptions,
) -> Result<(), String> {
    let SpawnOptions {
        session_id,
        provider,
        resume,
        rows,
        cols,
        prefill,
        node: preloaded_node,
        explicit_model,
        explicit_effort,
        explicit_extra_args,
    } = opts;

    tracing::info!(
        "spawn_agent_inner: session_id={}, provider={:?}, resume={:?}, size={}x{}",
        session_id,
        provider,
        resume,
        cols,
        rows
    );

    let timer = SpawnTimer::new(session_id);

    // 0. Claim the session for the WHOLE pipeline. `is_agent_already_running`
    //    below only sees registered processes, and registration is seconds
    //    away (git fetch + worktree provisioning) — without this claim a
    //    concurrent duplicate call (backend stage-2 vs frontend Terminal
    //    auto-spawn) passes that check and its step-2 stale-kill destroys
    //    THIS call's freshly-booted process. Returning Ok mirrors the
    //    already-running short-circuit: the node is being brought up, the
    //    caller has nothing further to do.
    let _spawn_claim = match SpawnInFlightClaim::try_claim(session_id) {
        Some(claim) => claim,
        None => {
            tracing::info!(
                "spawn_agent_inner: spawn already in flight for session {}, skipping duplicate call",
                session_id
            );
            return Ok(());
        }
    };

    // 1. Check if already running
    if is_agent_already_running(&session_id) {
        return Ok(());
    }

    // 2. Kill any stale process for this session
    tracing::debug!("spawn_agent_inner: killing stale processes for session {}", session_id);
    crate::agent::process::kill_agent(session_id).await.ok();

    // 3. Get node and resolve paths (skip DB read if caller provided the node)
    let node = match preloaded_node {
        Some(n) => n,
        None => db::get_agent_node_by_id(session_id).map_err(|e| {
            let err = format!("spawn_agent: failed to get agent node {}: {}", session_id, e);
            tracing::error!("{}", err);
            err
        })?,
    };
    tracing::info!("spawn_agent_inner: node path={}, env={:?}", node.path, node.env);
    timer.checkpoint("after_node_db_read");

    let adapter = provider.adapter();

    // 4. Determine session ID mode
    let session_id_mode = if adapter.supports_resume() {
        match resume {
            Some(ref id) if !id.is_empty() => SessionIdMode::Resume(id.clone()),
            _ => {
                if adapter.self_assigns_session_id() {
                    SessionIdMode::None
                } else {
                    let cli_uuid = uuid::Uuid::new_v4().to_string();
                    db::update_cli_session_id(session_id, &cli_uuid).map_err(|e| e.to_string())?;
                    tracing::info!("spawn_agent_inner: assigned cli_session_id={}", cli_uuid);
                    SessionIdMode::Assign(cli_uuid)
                }
            }
        }
    } else {
        SessionIdMode::None
    };

    // 5. Read mesh row for use_worktree / worktree_mode (legacy
    // model/effort columns are no longer read as active spawn
    // configuration — the v33 migration copied any non-empty legacy
    // values into the new map; see issue #1151 acceptance criteria 6).
    let row = env::mesh_row(&std::path::PathBuf::from(&node.path));
    let use_worktree = row.as_ref().map(|r| r.use_worktree).unwrap_or(true);
    // OS-level sandbox toggle (macOS Seatbelt #497, Windows AppContainer #498).
    // Off by default; the per-OS spawn policy is decided in `spawn_environment::wrap`
    // and `crate::sandbox::spawn::spawn_sandboxed`.
    let sandbox = row.as_ref().map(|r| r.sandbox).unwrap_or(false);
    let worktree_mode = row
        .as_ref()
        .and_then(|r| r.worktree_mode.as_deref())
        .unwrap_or(DEFAULT_WORKTREE_MODE);
    // Autopilot enforcement (issue #482, PRD #480): auto-spawned nodes must
    // always work on a real branch (and in a worktree) — the wrap-up sequence
    // pushes a branch and opens a PR, which a detached-HEAD worktree or a
    // shared mesh root cannot do. The ledger row is written before stage-2
    // starts, so this read is ordered correctly. The node row itself already
    // carries `use_worktree = true` (spawn override in `services::autopilot`).
    let is_autopilot = db::get_autopilot_run(session_id)
        .ok()
        .flatten()
        .is_some();
    let use_worktree = use_worktree || is_autopilot;
    let worktree_mode = if is_autopilot { "branched" } else { worktree_mode };
    let base_ref = resolve_base_ref_for_spawn(
        &node.path,
        row.as_ref().and_then(|r| r.base_ref.as_deref()),
    );

    timer.checkpoint("after_mesh_row_read");

    // 6. Compute spawn path. The pool claim (issue #609/#612) decides whether
    //    the spawn adopts a pre-warmed worktree (Manual: pool slug IS the
    //    node name; Issue/PR: `git worktree move` the pool dir onto the
    //    `gh{N}-`/`pr{N}-` target) or falls through to a cold create. A
    //    claim failure is non-fatal — the spawn falls back to cold; it
    //    only fails on an actual worktree-create error.
    let mesh_id = db::get_mesh_by_path(&node.path).map(|m| m.id).unwrap_or(-1);
    // `is_rename_spawn` selects between the two warm-pool adoption modes
    // downstream: Manual adopts the pool's slug as the node name (issue #609);
    // Issue/PR keep their own `gh{N}-`/`pr{N}-` name and move the pool dir
    // to match (issue #612). Consumed by the post-spawn name adoption
    // (further below) and by the SpawnContext built at phase 7.
    let is_rename_spawn = node.source_issue.is_some() || node.source_pr.is_some();
    let mut warm_claimed: Option<crate::services::warm_pool::ClaimedWarmEntry> = None;
    // Issue #653: a successful `try_claim` that the use-site recheck later
    // dropped still drained the pool by one row — `warm_claimed` is None
    // (the spawn fell back to cold), but the mesh's pool inventory is one
    // short. Track "we claimed at least once this spawn" so the post-spawn
    // refill still fires (otherwise the pool stays at target-1 until the
    // next reconcile). Distinct from `warm_claimed` because `warm_claimed`
    // tracks "we adopted the warm entry as this node's worktree" — that's
    // what `forget_after_spawn` and the manual name adoption gate on.
    let mut pool_was_drained_by_this_spawn = false;
    if use_worktree {
        // The path the node resolves to WITHOUT a pool claim. If it's already
        // on disk this spawn is a resume / handover / re-spawn reusing an
        // existing worktree — never claim a pool entry for it (that would
        // re-point the node at a different directory and abandon its work).
        let existing = env::resolve_agent_path(&node.path, node.worktree_name.as_deref());
        let existing_present = std::path::Path::new(&existing.host_path).exists();
        if mesh_id > 0
            && crate::services::warm_pool::should_claim_for_spawn(existing_present)
        {
            match crate::services::warm_pool::try_claim(app, mesh_id) {
                Ok(Some(entry)) => {
                    tracing::info!(
                        "spawn_agent_inner: claimed warm pool entry id={} path={} slug={} base_sha={}",
                        entry.id,
                        entry.path,
                        entry.preassigned_name,
                        entry.base_sha.as_deref().unwrap_or("none"),
                    );
                    // Issue #653 use-site guard: `try_claim` just checked
                    // the directory exists, but the spawn then waits
                    // seconds inside `fetch_origin` + git worktree move;
                    // another thread can delete the directory in that gap.
                    // Re-check immediately before committing to the warm
                    // path. On false, `recheck_after_claim` already dropped
                    // the row + tombstone; we just leave `warm_claimed`
                    // None so the existing `spawn_worktree_name` fallback
                    // resolves to the throwaway slug and the cold-create
                    // block runs naturally for both spawn modes (Issue/PR
                    // and manual).
                    if crate::services::warm_pool::recheck_after_claim(entry.id, &entry.path) {
                        warm_claimed = Some(entry);
                        pool_was_drained_by_this_spawn = true;
                    } else {
                        // Note: `recheck_after_claim` already logs the
                        // reason (claimed row N's directory disappeared...),
                        // so don't duplicate that WARN here.
                        // warm_claimed stays None — do NOT adopt. The row
                        // was already dropped by recheck_after_claim, but
                        // the pool inventory is still down by one; the
                        // post-spawn refill below must run regardless of
                        // the local `did_claim_warm` flag (which checks
                        // `warm_claimed.is_some()`, not the DB).
                        pool_was_drained_by_this_spawn = true;
                    }
                }
                Ok(None) => {
                    tracing::info!(
                        "spawn_agent_inner: warm pool empty for mesh {}; cold spawn",
                        mesh_id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "spawn_agent_inner: warm pool claim failed (non-fatal, falling back to cold): {}",
                        e
                    );
                }
            }
        }
    }

    // The effective spawn_worktree_name + path.
    //
    //  * Manual warm claim (`!is_rename_spawn`): adopt the pool's preassigned
    //    slug as the node's `worktree_name`, so the rest of the pipeline
    //    resolves straight onto the already-on-disk pool directory (#609).
    //  * Issue/PR warm claim (`is_rename_spawn`): keep the node's own
    //    `gh{N}-`/`pr{N}-` `worktree_name`. It resolves to a path that does
    //    NOT exist yet, so we enter the cold-create block below — where the
    //    PR-head fetch runs — and there `git worktree move` the pool directory
    //    onto this target instead of a cold `git worktree add` (#612).
    //  * No claim: fall back to whatever the node row carries (resumes, or a
    //    cold issue/PR spawn).
    //
    // Owned (`Option<String>`, not `Option<&str>`) on purpose: the Issue/PR
    // path mutates `warm_claimed` (take / re-assign) inside the worktree block
    // below, so `spawn_worktree_name` must not hold a borrow into it. The slugs
    // are short, so the clone is negligible.
    let spawn_worktree_name: Option<String> = if let Some(ref entry) = warm_claimed {
        if is_rename_spawn {
            node.worktree_name.clone()
        } else {
            Some(entry.preassigned_name.clone())
        }
    } else if use_worktree {
        node.worktree_name.clone()
    } else {
        tracing::info!("spawn_agent_inner: use_worktree=false, using repo root directly");
        None
    };

    let resolved = env::resolve_agent_path(&node.path, spawn_worktree_name.as_deref());
    tracing::info!(
        "spawn_agent_inner: resolved spawn_path={}, host_path={}, env={:?}",
        resolved.spawn_path,
        resolved.host_path,
        resolved.env_type
    );

    // For a Manual warm claim, the pool's preassigned slug IS the node's
    // `worktree_name` once the spawn completes — the post-spawn DB write
    // (below, before `register_agent`) persists that, but `provision_for_spawn`
    // needs the right branch name in the Spawn Context NOW so the manual
    // `Upgraded` branch's `git checkout -B <branch>` targets the pool's slug
    // rather than the node's stage-1 throwaway. Mutate `node.worktree_name`
    // in place here; `node.clone()` carries the value into the Spawn Context.
    let mut node = node;
    if let (false, Some(ref entry)) = (is_rename_spawn, &warm_claimed) {
        node.worktree_name = Some(entry.preassigned_name.clone());
    }

    // Set true when the spawn-time fetch advances the mesh's base ref, so the
    // single post-spawn pool-maintenance task at the end runs the ref-freshness
    // pass (issue #613 AC3). Carried to the end rather than firing its own
    // thread here so refresh + refill share ONE fill-lock acquisition and can
    // never lose a lock race to each other (issue #613 review).
    let mut ref_advanced_for_pool = false;

    // Auto-sync (issue #213) + PR-head-fetch (#420/#443) + worktree_base_ref
    // resolution only run when the host path doesn't exist yet — for resume /
    // handover / re-spawn the existing worktree's tree IS the agent's starting
    // point and re-syncing would churn refs unnecessarily. Root Nodes
    // (`use_worktree = false`) skip both auto-sync and the PR-head-fetch by
    // virtue of `spawn_worktree_name` being None.
    let host_path_exists = std::path::Path::new(&resolved.host_path).exists();
    let worktree_base_ref = if spawn_worktree_name.is_some() {
        if !host_path_exists {
            // Auto-sync the parent **Mesh** before we cut a new worktree
            // (issue #213). The sync is best-effort: a network failure or
            // a non-fast-forwardable history is surfaced as a `mesh-sync-
            // warning` Tauri event so the frontend can show a non-fatal
            // toast, but spawn always proceeds from the local HEAD.
            // Skips (dirty parent, no remote, already up to date) are
            // silent — the user doesn't need to know about them.
            //
            // The remote is derived from the mesh's `base_ref` (issue
            // #276), so a Mesh with `base_ref = "upstream/main"` syncs
            // against `upstream` rather than hardcoded `origin`. We move
            // `base_ref` into the closure because `spawn_blocking` needs
            // a `'static` closure.
            // Freshness skip (ADR 0020): the background mesh sync in
            // `services::pool_worker` (and any recent spawn / manual Sync)
            // stamps `services::fetch_freshness` on every successful fetch.
            // When the mesh was synced within `SPAWN_FETCH_TTL`, this whole
            // network round-trip is redundant — the remote-tracking ref the
            // worktree is cut from is already current to within minutes —
            // so we skip it and the spawn goes straight to provisioning.
            // `ref_advanced_for_pool` stays false: whichever path recorded
            // the fresh fetch already ran the warm-pool freshness pass.
            // The manual Sync button remains the "I need the latest RIGHT
            // NOW" override — it fetches unconditionally.
            if crate::services::fetch_freshness::spawn_can_skip_fetch(&node.path) {
                tracing::info!(
                    "spawn_agent_inner: skipping auto-sync for session {} — mesh {} was synced {}s ago (< TTL)",
                    session_id,
                    node.path,
                    crate::services::fetch_freshness::time_since_success(&node.path).as_secs()
                );
                timer.checkpoint("fetch_origin_skipped_fresh");
            } else {
                let root = node.path.clone();
                let base_ref_owned = base_ref.to_string();
                timer.checkpoint("before_fetch_origin");
                // Issue #652 — per-Mesh serialization. Without this lock, N
                // concurrent spawns against the same Mesh race on
                // .git/FETCH_HEAD, .git/index.lock, and refs/heads/<branch>.lock:
                // one git fetch wins, the others fail with "another git process"
                // and the spawn lands on a stale ref. The lock is *blocking*
                // (not try_lock-or-skip), so caller #2 waits for caller #1 to
                // populate the refs and then reuses them (its natural outcome
                // is UpToDate, which is correct).
                //
                // Issue #709 — the wrap is consolidated into
                // `git::sync::locked_fetch_origin` so the lock-acquisition
                // shape is identical to the manual `git_sync`'s
                // `locked_do_sync`, the PR-spawn's `locked_fetch_pr_head`,
                // and the prune's `locked_prune_remote_tracking`. The
                // `tokio::task::spawn_blocking` + `with_mesh_sync_lock`
                // pair used to live inline here.
                let sync_result =
                    crate::git::sync::locked_fetch_origin(root, base_ref_owned).await;
                timer.checkpoint("after_fetch_origin");
                // Ref-freshness (issue #613 AC3): if the fetch actually pulled new
                // commits, the mesh's base ref has moved, so any OTHER warm pool
                // entries for this mesh are now parked on a stale SHA and must be
                // `git reset --hard`ed onto the new commit. Only `Synced` /
                // `FetchedButDiverged` advance the ref — `UpToDate` / skipped means
                // nothing moved. We record the fact here and let the single
                // post-spawn maintenance task (at the end of this fn) run the
                // freshness pass, so refresh and refill share one fill-lock
                // acquisition instead of racing on two threads (issue #613 review).
                ref_advanced_for_pool = sync_result
                    .as_ref()
                    .map(|o| o.advanced_ref())
                    .unwrap_or(false);
                emit_sync_outcome_event(app, session_id, &node.path, sync_result);
            } // end freshness-gated fetch_origin block

            // Worktree adoption for PR-spawned nodes (issue #420, extended
            // by #443 for fork PRs). When the node carries a `source_pr`,
            // the head ref stored in `node.branch` is the PR's actual source
            // branch (e.g. `feat/420-pr-spawn`), and the worktree needs to
            // be cut from `<remote>/<head_ref>` so the agent lands on the
            // same commits the PR is built from. Two cases:
            //
            //  - Same-repo PRs (`head_repo_owner` is `None`): the head
            //    lives on `origin` — we call `locked_fetch_pr_head` and
            //    use `origin/<head_ref>` (the #420 path).
            //  - Fork PRs (`head_repo_owner` is `Some`): the head lives on
            //    the fork's clone URL — `locked_fetch_pr_head` calls
            //    `fetch_fork_head`, which registers the fork as a remote
            //    (`fork-<login>`) and fetches from there (issue #443,
            //    follow-up to #36). The worktree base_ref becomes
            //    `fork-<login>/<head_ref>`.
            //
            // The fetch is best-effort: a network failure or stale local ref
            // falls back to the mesh's `base_ref` (the ADR 0001 offline
            // pattern), and the user sees the agent spawn on the wrong
            // commits rather than a hard error — strictly worse than a clean
            // spawn on the right commits, but a strict-error spawn is
            // brittle to the very first offline session.
            //
            // Even so, the fallback MUST surface to the user: the spawn
            // otherwise reports success, the dock closes, and the agent
            // silently lands on the wrong commits. We piggy-back on the
            // existing `mesh-sync-warning` event (the same non-fatal channel
            // the auto-sync path uses) with a `pr_head_unfetchable` or
            // `pr_fork_unfetchable` outcome — the App.tsx listener already
            // renders a toast for that event, so no frontend change is
            // required.
            if node.source_pr.is_some() {
                let head_ref_owned = node.branch.clone();
                let root = node.path.clone();
                let fork_owner_owned = node.head_repo_owner.clone();
                let fork_url_owned = node.head_repo_clone_url.clone();
                timer.checkpoint("before_fetch_pr_head");
                let fetch_ok = tokio::task::spawn_blocking(move || {
                    // Issue #698 — per-Mesh serialization for the PR-spawn
                    // fetch. The match lives inside `locked_fetch_pr_head`
                    // so both branches share one lock acquisition keyed on
                    // `&root` (the mesh's DB-stored path, same key
                    // `fetch_origin` uses two steps above). Without the
                    // lock, two concurrent PR-spawns (or a PR-spawn racing
                    // the manual `git_sync` from #680) collide on
                    // `.git/FETCH_HEAD` / `refs/remotes/<remote>/<ref>.lock`
                    // and the losing spawn silently falls back to `base_ref`.
                    // The fork branch additionally writes `git remote add/
                    // set-url` config that the next caller must observe,
                    // so the lock covers both remote registration and
                    // fetch in one critical section.
                    locked_fetch_pr_head(
                        &root,
                        &head_ref_owned,
                        fork_owner_owned.as_deref(),
                        fork_url_owned.as_deref(),
                    )
                })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        "spawn_agent_inner: fetch task panicked: {}",
                        e
                    );
                    false
                });
                timer.checkpoint("after_fetch_pr_head");
                if fetch_ok {
                    // Pick the right remote-name prefix for the base_ref
                    // string the worktree will be cut from. Same-repo PRs
                    // use `origin/<head_ref>` (matches the mesh's default
                    // `base_ref`); fork PRs use `fork-<login>/<head_ref>`.
                    // The mesh's `base_ref` is overwritten to use the fork
                    // remote so a future head-branch push is picked up by
                    // the same fetch the auto-sync path runs.
                    let remote_name = match node.head_repo_owner.as_deref() {
                        Some(owner) => fork_remote_alias(owner),
                        None => "origin".to_string(),
                    };
                    let remote_ref = format!("{}/{}", remote_name, node.branch);

                    // Issue #444 — exact-pinning: after a successful fetch,
                    // compare the local SHA at the remote ref we just
                    // populated to the `source_pr_pinned_sha` we stored at
                    // spawn time. On mismatch (PR was force-pushed / rebased
                    // between click-time and spawn-time) emit a non-fatal
                    // `pr_sha_drift` warning via the same `mesh-sync-warning`
                    // channel the offline-fallback path uses. The worktree
                    // proceeds on the new tip — strict-fail would block
                    // legitimate rebase-and-merge workflows for one stale
                    // click. The drift check is a no-op for v15-and-earlier
                    // PR-spawned rows where `source_pr_pinned_sha` is None
                    // (the column was added in v16) and for any empty
                    // GitHub response: read_origin_ref_sha returns None
                    // for a missing ref, and a None expected/actual pair
                    // is treated as "no SHA to compare" and skipped.
                    let root_for_sha = node.path.clone();
                    let head_ref_for_sha = remote_ref.clone();
                    let expected_sha = node.source_pr_pinned_sha.clone();
                    let actual_sha = tokio::task::spawn_blocking(move || {
                        read_origin_ref_sha(&root_for_sha, &head_ref_for_sha)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "spawn_agent_inner: read_origin_ref_sha task panicked: {}",
                            e
                        );
                        None
                    });
                    if let (Some(expected), Some(actual)) = (expected_sha.as_deref(), actual_sha.as_deref()) {
                        if expected != actual {
                            let pr_number = node.source_pr.unwrap_or(-1);
                            let head_ref = node.branch.clone();
                            let message = format!(
                                "PR #{} was force-pushed or rebased after you clicked Spawn                                  (expected {}, now {} on {}). Spawning on the new tip —                                  re-spawn to pin to a fresh SHA.",
                                pr_number, expected, actual, remote_ref,
                            );
                            tracing::warn!(
                                "spawn_agent_inner: {} (node {})",
                                message,
                                session_id,
                            );
                            let _ = app.emit(
                                "mesh-sync-warning",
                                MeshSyncWarningPayload {
                                    node_id: session_id,
                                    mesh_path: node.path.clone(),
                                    outcome: MeshSyncOutcome::PrShaDrift,
                                    new_commits: None,
                                    pr_number: Some(pr_number),
                                    head_ref: Some(head_ref.clone()),
                                    expected_sha: Some(expected.to_string()),
                                    actual_sha: Some(actual.to_string()),
                                    fallback_base_ref: None,
                                    head_repo_owner: None,
                                    head_repo_clone_url: None,
                                    message,
                                },
                            );
                        }
                    }
                    remote_ref
                } else {
                    let pr_number = node.source_pr.unwrap_or(-1);
                    let head_ref = node.branch.clone();
                    // Distinguish the two failure modes in the toast: a
                    // fork fetch failure is more likely to be permanent
                    // (the user renamed or deleted the fork) than a same-
                    // repo failure (usually transient network).
                    let is_fork = node.head_repo_owner.is_some();
                    let source_label = if is_fork {
                        let alias = node
                            .head_repo_owner
                            .as_deref()
                            .map(fork_remote_alias)
                            .unwrap_or_else(|| "fork".to_string());
                        format!("the fork remote '{}'", alias)
                    } else {
                        "origin".to_string()
                    };
                    let message = format!(
                        "Could not fetch PR #{} head ref '{}' from {};                          spawning from the mesh's base ref '{}' instead.                          The agent may land on stale commits — re-spawn                          when the network is back to retry.",
                        pr_number, head_ref, source_label, base_ref,
                    );
                    tracing::warn!(
                        "spawn_agent_inner: {} (node {})",
                        message,
                        session_id,
                    );
                    let mut head_repo_owner_str: Option<String> = None;
                    let mut head_repo_clone_url_str: Option<String> = None;
                    if let (Some(owner), Some(url)) = (
                        node.head_repo_owner.as_deref(),
                        node.head_repo_clone_url.as_deref(),
                    ) {
                        head_repo_owner_str = Some(owner.to_string());
                        head_repo_clone_url_str = Some(url.to_string());
                    }
                    let outcome_enum = if is_fork {
                        MeshSyncOutcome::PrForkUnfetchable
                    } else {
                        MeshSyncOutcome::PrHeadUnfetchable
                    };
                    let _ = app.emit(
                        "mesh-sync-warning",
                        MeshSyncWarningPayload {
                            node_id: session_id,
                            mesh_path: node.path.clone(),
                            outcome: outcome_enum,
                            new_commits: None,
                            pr_number: Some(pr_number),
                            head_ref: Some(head_ref.clone()),
                            expected_sha: None,
                            actual_sha: None,
                            fallback_base_ref: Some(base_ref.to_string()),
                            head_repo_owner: head_repo_owner_str,
                            head_repo_clone_url: head_repo_clone_url_str,
                            message,
                        },
                    );
                    base_ref.to_string()
                }
            } else {
                base_ref.to_string()
            }
        } else {
            // Path already exists (resume / handover / re-spawn). No
            // auto-sync, no PR-head-fetch — the existing worktree's tree IS
            // the spawn point.
            base_ref.to_string()
        }
    } else {
        // Root Node (`use_worktree = false`) — no worktree, no base_ref
        // resolution needed; `provision_for_spawn` short-circuits to `Reused`.
        base_ref.to_string()
    };

    // 7. Provision the Worktree Node via `provision_for_spawn` (issue #677).
    //    The seam deepened: the provisioner now owns the four-way decision
    //    (Reused / Adopted / Upgraded / Created), the warm-failure cold
    //    fallback, the post-success pool row cleanup (`forget_after_spawn`),
    //    the Manual name-adoption DB write, and the `post_spawn_maintenance`
    //    thread trigger. This orchestrator hands it:
    //      * a SpawnContext (data only),
    //      * ProvisionHooks (decision inputs: ref-advanced / pool-drained),
    //      * an AppHandleSink (side-effect surface),
    //    then awaits the call and propagates the result.
    //
    //    CRITICAL CORRECTNESS:
    //    * `ctx.base_ref` is `worktree_base_ref` (post-fetch for PR/Issue,
    //      the mesh base otherwise). Setting this AFTER the PR-head-fetch
    //      block — not the original `base_ref` — is what makes every PR
    //      spawn land on the freshly fetched PR head rather than going
    //      cold. For Resume / Root Node it's `base_ref` (no fetch ran).
    //    * `warm_claimed.take()` moves the claim into the context; on a warm
    //      failure the provisioner cleans both possible paths up, forgets the
    //      row, and re-cuts cold — all internally. This orchestrator no
    //      longer threads the entry back out.
    //    * `is_rename_spawn` is preserved unchanged — the pre-provision
    //      `spawn_worktree_name` resolution still reads it.
    let provision_ctx = SpawnContext {
        node: node.clone(),
        source: SpawnSource::from_node(&node),
        base_ref: worktree_base_ref.clone(),
        worktree_mode: worktree_mode.to_string(),
        use_worktree,
        warm_entry: warm_claimed.take(),
        host_path: resolved.host_path.clone(),
    };
    let provision_hooks = ProvisionHooks {
        ref_advanced_for_pool,
        pool_was_drained_by_this_spawn,
    };
    let provision_sink = AppHandleSink { app: app.clone() };
    timer.checkpoint("before_provision");
    let provision_result = tokio::task::spawn_blocking(move || {
        provision_for_spawn(provision_ctx, &provision_hooks, &provision_sink)
    })
    .await
    .unwrap_or_else(|e| Err(format!("provision_for_spawn task panicked: {}", e)));
    timer.checkpoint("after_provision");
    // The provisioner owns its own post-success bookkeeping (`forget_after_spawn`,
    // Manual name adoption, `post_spawn_maintenance` thread) — the orchestrator
    // only needs to know whether the worktree is on disk (Ok) or whether the
    // provisioner gave up entirely (Err, already combined warm+cold strings).
    match provision_result {
        Ok(_outcome) => {}
        Err(e) => {
            tracing::error!("spawn_agent_inner: provision_for_spawn failed: {}", e);
            let sink = session_lifecycle::AppSessionLifecycleSink { app };
            let _ = session_lifecycle::on_error(&sink, session_id);
            let _ = app.emit(
                "node-spawn-failed",
                crate::commands::agent::NodeSpawnFailedPayload {
                    node_id: session_id,
                    error: e.clone(),
                },
            );
            return Err(e);
        }
    }

    // Fix WSL/Windows path mismatches in the worktree's .git file — without
    // this, agent commands run inside the worktree see a broken gitlink on
    // Windows-side shells. Best-effort: a failure is logged, never fatal.
    if let Err(e) = crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type) {
        tracing::warn!("spawn_agent_inner: failed to sanitize worktree .git file: {}", e);
    }

    // 8-9. Build the command, then spawn it — either normally (portable-pty)
    //       or, when the mesh opts in on Windows, inside an AppContainer sandbox
    //       (issue #498). The sandbox path owns its ConPTY spawn but returns the
    //       same `Child`/`MasterPty` trait objects, so everything downstream
    //       (Job Object containment, reader thread, resize, kill) is identical.
    timer.checkpoint("before_provider_preflight");
    let routing_harness_id = node.provider.clone();
    let routing_resolved = resolved.clone();
    let routing = match crate::commands::run_blocking("prepare_provider_routing", move || {
        crate::agent::launch_routing::prepare(&routing_harness_id, provider, &routing_resolved)
    })
    .await
    {
        Ok(routing) => routing,
        Err(error) => {
            // Verification failures are fail-closed. Schedule a runtime-specific
            // refresh so a later launch can proceed without a settings round-trip.
            if provider == Provider::Codex {
                if let Ok(Some((pairing, _))) =
                    crate::preferences::resolve_stored_pairing_and_account(&node.provider)
                {
                    crate::commands::preferences::schedule_pairing_verification_for_runtime(
                        app.clone(),
                        pairing.harness_id,
                        pairing.provider_id,
                        resolved.env_type,
                    );
                }
            }
            timer.checkpoint("provider_preflight_failed");
            return Err(format!("spawn preflight failed: {error}"));
        }
    };
    timer.checkpoint("after_provider_preflight");
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
    // `node.provider` for a Proxied Provider row is the composite id
    // `"<harness>:<provider>"` (e.g. `"claude:minimax"`, `"codex:minimax"`).
    // The per-Mesh override map and the application-defaults map are both
    // keyed by the harness *profile* id (the half before the first `:`),
    // so a raw lookup would miss every Proxied spawn — failing AC #12
    // ("Native and Proxied Provider Spawn Options consume the same
    // application-default layer"). Split the composite id through
    // `parse_spawn_option_id` before both lookups so native and Proxied
    // rows hit the same map key.
    let (harness_id_for_default, _) =
        crate::agent::provider::parse_spawn_option_id(&node.provider);
    let mesh_override = crate::db::get_mesh_harness_overrides(node.mesh_id)
        .ok()
        .flatten()
        .and_then(|m| m.get(harness_id_for_default).cloned());
    let app_default = match crate::preferences::load() {
        Ok(prefs) => crate::preferences::harness_default_for(&prefs, harness_id_for_default),
        Err(e) => {
            tracing::warn!(
                "spawn_agent_inner: harness-default load failed, treating as absent: {e}"
            );
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

    let emit_provider_error = |e: &str| {
        let _ = app.emit(
            "provider-error",
            ProviderErrorPayload {
                session_id,
                provider,
                message: e.to_string(),
            },
        );
    };

    // 8. Provision workspace trust and attention hooks before child process launches
    // so CLI harnesses discover hooks and trusted workspaces at boot time (issue #1367).
    crate::agent::workspace_trust::ensure_trusted(&resolved);
    timer.checkpoint("after_workspace_trust");
    if adapter.requires_attention_hook() {
        // The adapter owns its harness's hook format (issue #886). A failure
        // must not abort the spawn — the agent still works, but telemetry
        // is missing so we surface a provider-warning event.
        if let Err(e) = adapter.inject_attention_hook(std::path::Path::new(&resolved.host_path)) {
            tracing::warn!(
                "spawn_agent_inner: attention hook injection failed for session {}: {}",
                session_id,
                e
            );
            emit_provider_error(&format!("Attention hook injection warning: {e}"));
        }
    }
    timer.checkpoint("after_inject_hook");

    // 9. Spawn child process (either sandboxed or direct PTY)
    let (child, master): (
        Box<dyn portable_pty::Child + Send + Sync>,
        Box<dyn portable_pty::MasterPty + Send>,
    ) = if crate::sandbox::sandbox_enabled(sandbox) {
        tracing::info!("spawn_agent_inner: spawning session {} inside AppContainer sandbox", session_id);
        sandbox_spawn(&cmd, session_id, &resolved.host_path, rows, cols)
            .inspect_err(|e| emit_provider_error(e))?
    } else {
        let pair = open_pty_pair(rows, cols)?;
        let child = spawn_child(&pair, cmd).inspect_err(|e| emit_provider_error(e))?;
        (child, pair.master)
    };

    tracing::info!("spawn_agent_inner: process spawned successfully");
    timer.checkpoint("after_pty_spawn");

    // Contain the whole process tree in a Job Object straight away, before the
    // shell launches the agent CLI — so any process the agent later detaches
    // (e.g. a dev server it backgrounds) is still killed on close, even when its
    // parent has exited and `taskkill /T` could no longer reach it.
    let job = child.process_id().and_then(crate::process_util::JobHandle::contain);
    if job.is_none() {
        tracing::warn!(
            "spawn_agent_inner: could not contain session {} in a Job Object; \
             close will fall back to taskkill (detached children may survive)",
            session_id
        );
    }

    // 10. Setup IO
    let reader = master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = master.take_writer().map_err(|e| e.to_string())?;
    let reader_alive = Arc::new(AtomicBool::new(true));

    // 11. Register BEFORE starting the reader thread. The pre-#300 order
    //     (register-then-start) is the one that closes the TOCTOU window
    //     in `is_agent_already_running`: a concurrent spawn for the
    //     same session_id sees the entry and bails. The `reader_handle`
    //     is stashed via a setter after the thread is spawned — the
    //     tiny window between insert and setter is benign (kill_session
    //     arriving then sees `reader_handle = None` and skips the join,
    //     matching the natural-exit test path).
    tracing::info!("spawn_agent_inner: storing agent process for session {}", session_id);
    // Slug adoption (`name` + `worktree_name`) is NOT done here. It belongs to
    // the provisioner, which applies it via `ProvisionSink::adopt_manual_slug`
    // before this point — see `git::worktree::provision`.
    //
    // This is where a compensating `set_agent_node_worktree_name` used to
    // live. #1057 moved the claim into `SpawnContext` via
    // `warm_claimed.take()` a few hundred lines above, which silently made
    // this block's `Some(entry)` guard unmatchable — `None` is a perfectly
    // valid value to pattern-test, so nothing failed to compile and no test
    // covered it. The row kept its stage-1 throwaway slug, and every close
    // then queued a directory that had never existed (#1080). Do not
    // reintroduce a second adoption site here: one owner, in the provisioner.
    // One flag instance shared three ways: the registry entry (kill_session
    // sets it), the reader thread (its epilogue reads it), and nothing else.
    let deliberate_kill = Arc::new(AtomicBool::new(false));
    register_agent(session_id, child, writer, master, reader_alive.clone(), job, timer.start(), mesh_id, deliberate_kill.clone());
    tracing::info!("spawn_agent_inner: stored agent process");

    // 13. Start reader thread
    let spawned_at = std::time::Instant::now();
    // `spawn_start` is the original SpawnTimer reference, used by the
    // reader-thread `first_pty_output` checkpoint log for timeline
    // alignment with every other `spawn_timing:` line. Distinct from
    // `spawned_at` (process-creation time) which the early-exit
    // heuristic needs — see `start_reader` doc comment.
    let spawn_start = timer.start();
    tracing::debug!("spawn_agent_inner: starting reader thread for session {}", session_id);
    crate::http_server::ensure_pty_channel(session_id);
    // Issue #651: derive the reader-capture gate from `session_id_mode`
    // (the orchestrator's authoritative decision) rather than from
    // `adapter.self_assigns_session_id() && node.cli_session_id.is_none()`
    // (a derived condition that could drift if a future adapter violates the
    // "Assign => !self_assigns" invariant). The two writes — orchestrator
    // pre-write at step 4 and reader capture at `start_reader` — are
    // unsynchronised; only one path must own the column for any given spawn.
    let needs_session_capture = reader_should_capture_session_id(
        &session_id_mode,
        adapter.captures_session_id_from_pty(),
    );
    let reader_handle = start_reader(
        app.clone(),
        session_id,
        needs_session_capture,
        reader,
        spawned_at,
        reader_alive,
        adapter.is_plain_terminal(),
        spawn_start,
        mesh_id,
        deliberate_kill,
    );

    // 13b. Start natural-exit watcher (issue #287). On Windows ConPTY
    //      10.0.28120 the master read pipe no longer EOFs on child
    //      exit, so the reader thread stays blocked in `read()` until
    //      the pseudoconsole itself is closed. This poller drops the
    //      master within ~500ms of the child exiting, EOFing the
    //      reader, which then sets `reader_alive = false` and flips
    //      the node status to `Idle`. The watcher uses `try_wait` +
    //      `try_lock` on the child so it never blocks kill_session
    //      (which also locks that mutex).
    if let Some(entry) = PROCESS_REGISTRY.get(&session_id) {
        crate::agent::process::watch_child_exit(entry.child.clone(), entry.master.clone());
    }

    // 14. Stash the JoinHandle on the registered entry. `kill_session`
    //     reads it under a Mutex so the concurrent kill_session path
    //     is race-free (see `process.rs::kill_session`).
    if let Some(entry) = PROCESS_REGISTRY.get(&session_id) {
        entry.set_reader_handle(reader_handle);
    }

    if matches!(session_id_mode, SessionIdMode::None) {
        adapter.after_fresh_spawn(session_id, &resolved.spawn_path, resolved.env_type);
    }

    tracing::info!("spawn_agent_inner: reader thread spawned, updating node status");
    // Issue #654 — close the post-spawn status + early-exit race. The
    // `NOT IN (Error, Archived)` guard is the symmetric race fix: prevents
    // the orchestrator from resurrecting a reader-written Error back to
    // Spawning (which would let the delayed promotion later write Running
    // onto a dead node — same ghost-Running bug, other direction). Routes
    // through SessionLifecycle (issue #132) so the `unless_in` predicate
    // lives in one place.
    let sink = session_lifecycle::AppSessionLifecycleSink { app };
    session_lifecycle::on_spawn_started(&sink, session_id).map_err(|e| e.to_string())?;
    let app_for_promotion = app.clone();
    std::thread::spawn(move || {
        // Promote to Running iff the reader hasn't already written Error.
        // Both delay and reader check must share `EARLY_EXIT_WINDOW`.
        std::thread::sleep(EARLY_EXIT_WINDOW);
        let promotion_sink = session_lifecycle::AppSessionLifecycleSink {
            app: &app_for_promotion,
        };
        if let Err(e) = session_lifecycle::on_spawn_complete(&promotion_sink, session_id) {
            tracing::warn!(
                "spawn_agent_inner: conditional Running promotion failed for session {}: {}",
                session_id,
                e
            );
        }
    });

    // Warm-pool post-claim housekeeping (issue #609) and the post-spawn
    // maintenance task (issue #613) live inside `provision_for_spawn` now
    // — the provisioner owns the warm-failure cold fallback, the warm-row
    // `forget_after_spawn`, the Manual name adoption (DB write +
    // `node-renamed` event), and the single thread that runs refresh +
    // refill under one fill-lock acquisition. This orchestrator just gets
    // back the final `ProvisionOutcome`; see `git::worktree::provision`
    // for the seam contract.

    // Emit the post-spawn reconcile trigger (issue #332). Async-spawn paths
    // (auto-resume on startup, fresh auto-spawn, handover, etc.) race the
    // frontend's attach-fit: term.onResize fires `resize_agent(real cols)`
    // before the agent process exists, so the IPC returns "Agent not
    // running" and is silently swallowed. The PTY was created at the
    // caller-supplied `rows`/`cols` (80x24 for auto_resume_sessions), and
    // because term.cols is already the fitted value no further onResize
    // fires — the PTY stays at the spawn-time size and the agent wraps
    // its first lines of output inside a wider pane. By emitting here
    // (after the agent is registered AND the DB status flips to
    // `Spawning` — the transient state between process launch and the
    // conditional `Spawning → Running` promotion 3s later; issue #654),
    // we give the frontend a definitive "agent is up, push the real
    // size now" signal that closes the race uniformly for all three
    // paths. Frontend consumer: TerminalRegistry listens and calls
    // syncPtySize, which is self-guarding (no-op on detached/missing
    // terminals) and swallows the "Agent not running" rejection.
    let _ = app.emit(
        "agent-spawned",
        AgentSpawnedPayload {
            session_id,
            rows: rows as i32,
            cols: cols as i32,
        },
    );

    tracing::info!("spawn_agent_inner: complete");
    timer.total();
    Ok(())
}

/// Map an `crate::git::sync::fetch_origin` outcome to either a silent `tracing` log
/// or a `mesh-sync-warning` Tauri event. The frontend's `App.tsx`
/// listens for the event and shows a non-fatal warning toast.
///
/// Per issue #213:
/// - `FetchedButDirty`, `SkippedNoRemote`, `UpToDate`, `Synced` are silent.
/// - `FetchedButDiverged`, `FetchFailed`, `RepoUnusable` emit a
///   warning so the user knows the spawn fell back to local HEAD.
///
/// Spawn proceeds either way; the event is purely informational.
fn emit_sync_outcome_event(
    app: &tauri::AppHandle,
    session_id: i64,
    mesh_path: &str,
    outcome: Result<crate::git::sync::FetchOutcome, crate::git::sync::FetchError>,
) {
    let payload = match outcome {
        Ok(crate::git::sync::FetchOutcome::FetchedButDirty { new_commits }) => {
            // Silent, like Synced/UpToDate: the fetch reached the remote and
            // advanced the tracking refs the worktree is cut from — the new
            // node IS fresh. Only the parent checkout's fast-forward was
            // skipped, and the user already knows their own tree is dirty.
            tracing::info!(
                "spawn_agent_inner: auto-sync fetched {} commit(s) but skipped the pull \
                 (parent dirty) for session {}",
                new_commits,
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::SkippedNoRemote) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync skipped (no origin) for session {}",
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::UpToDate) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync up-to-date for session {}",
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::Synced { new_commits }) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync pulled {} commit(s) for session {}",
                new_commits,
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::FetchedButDiverged { new_commits, reason }) => {
            // Diverged is informational, not an error — the fetch
            // succeeded, the new commits are visible locally, we just
            // can't auto-apply them without a real merge. The user
            // should know so they can decide whether to `git pull`
            // themselves or rebase.
            let message = format!(
                "Fetched {} new commit(s) from origin, but local history has diverged ({}). Spawning from local HEAD — pull manually to sync.",
                new_commits, reason
            );
            tracing::warn!("spawn_agent_inner: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::Diverged,
                new_commits: Some(new_commits),
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
        Err(crate::git::sync::FetchError::RepoUnusable(reason)) => {
            let message = format!(
                "Couldn't auto-sync the mesh — repository is unusable: {}. Spawning from local HEAD instead.",
                reason
            );
            tracing::warn!("spawn_agent_inner: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::RepoUnusable,
                new_commits: None,
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
        Err(crate::git::sync::FetchError::FetchFailed(reason)) => {
            // The most common case: network down. We don't try to
            // distinguish "no network" from "auth failure" — both look
            // the same to `git fetch`. The user knows whether they
            // have connectivity; we just tell them we couldn't sync.
            let message = if reason.is_empty() {
                "Couldn't auto-sync the mesh (fetch failed). Spawning from local HEAD instead.".to_string()
            } else {
                format!(
                    "Couldn't auto-sync the mesh ({}). Spawning from local HEAD instead.",
                    reason
                )
            };
            tracing::warn!("spawn_agent_inner: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::FetchFailed,
                new_commits: None,
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
    };
    if let Some(payload) = payload {
        let _ = app.emit("mesh-sync-warning", payload);
    }
}

// The worktree-provision helpers — `fetch_single_ref`, `locked_fetch_pr_head`,
// `fork_remote_alias`, `fetch_fork_head`, `read_origin_ref_sha`,
// `upgrade_warm_to_mode`, `adopt_warm_worktree_by_move`,
// `checkout_worktree_to_base`, `run_git_checkout` — live in
// `crate::git::worktree::provision` (ADR 0007 consolidation, issue #677, plus
// #698's `locked_fetch_pr_head` wrapper). The spawn path reaches them through
// the module-level `use` at the top of this file; the call sites inside
// `spawn_agent_inner` use them transparently.

#[cfg(test)]
mod tests {
    use super::*;
    // The eight worktree-provision helpers were moved to
    // `crate::git::worktree::provision` in PR #676 / issue #677, and #698
    // added `locked_fetch_pr_head` on top. The tests here exercise them by
    // name, so re-import at the test-module scope.
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::git::worktree::provision::{
        adopt_warm_worktree_by_move, fetch_fork_head, fetch_single_ref, fork_remote_alias,
        locked_fetch_pr_head, read_origin_ref_sha, upgrade_warm_to_mode,
    };
    use tempfile::TempDir;

    /// Pin the spawn-time fallback. Sole pin of `DEFAULT_WORKTREE_MODE`
    /// after #411 deleted the TS-side sentinel (it had no real consumer).
    #[test]
    fn default_worktree_mode_is_branched() {
        assert_eq!(DEFAULT_WORKTREE_MODE, "branched");
    }

    // -----------------------------------------------------------------------
    // Cascade layer-1 wiring at the spawn seam (issue #1155).
    //
    // The `resolve_agent_config` resolver already had unit tests for its
    // cascade order, but those tests never proved the spawn pipeline
    // *populated* the explicit slot from `SpawnOptions`. Before #1155 the
    // `explicit:` field on `FieldInputs` was hard-coded `None`, so layer
    // 1 of the documented cascade (issue #1148: explicit > mesh >
    // application > native) was unreachable. `cascade_inputs_for` is the
    // spawn-side seam; these tests pin both the wiring AND the cascade
    // precedence when the helper's output is fed through the resolver.
    // -----------------------------------------------------------------------

    use crate::agent::capabilities::{
        FieldInputs, HarnessCapabilities, resolve_agent_config,
    };
    use crate::preferences::HarnessConfigValue;

    /// Helper that returns the Anthropic capabilities descriptor for the
    /// integration tests below. Pulled out so each test reads as the
    /// cascade it pins without dragging harness-table setup inline.
    fn anthropic_caps() -> HarnessCapabilities {
        crate::agent::capabilities::capabilities_for(
            &crate::agent::provider::adapters::ANTHROPIC,
        )
    }

    /// Regression pin for issue #1155 acceptance criterion 4 — the spawn
    /// pipeline must populate the `explicit` slot from the values the
    /// caller passed in. Without this wiring the helper would feed `None`
    /// for the explicit slot and the top layer of the cascade would never
    /// fire. The test fails compilation if any future refactor drops the
    /// `explicit_*` parameters from `cascade_inputs_for`.
    #[test]
    fn cascade_inputs_for_populates_explicit_slot_for_both_fields() {
        let app_default = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
        };
        let inputs = cascade_inputs_for(
            Some("sonnet-4"),
            Some("medium"),
            Some("haiku-4"),
            Some("low"),
            Some(&app_default),
            None,
        );
        assert_eq!(
            inputs.model,
            FieldInputs {
                explicit: Some("sonnet-4"),
                mesh_override: None,
                mesh: Some("haiku-4"),
                application: Some("opus-4-1"),
            },
            "explicit must win over mesh which wins over application",
        );
        assert_eq!(
            inputs.effort,
            FieldInputs {
                explicit: Some("medium"),
                mesh_override: None,
                mesh: Some("low"),
                application: Some("high"),
            },
            "explicit effort must win over mesh effort which wins over application effort",
        );
    }

    /// Whitespace-only / empty strings on the explicit slot must collapse
    /// to `None` so the cascade falls through (issue #1148 AC #32,
    /// #1155 AC #3). Mirrors the resolver's `normalize_non_empty` so the
    /// cascade behaves identically regardless of which layer trimmed the
    /// blank.
    #[test]
    fn cascade_inputs_for_collapses_whitespace_explicit_to_none() {
        let app_default = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
        };
        let inputs = cascade_inputs_for(
            Some("   "),
            Some("\t\n  "),
            Some("haiku-4"),
            Some("low"),
            Some(&app_default),
            None,
        );
        assert_eq!(
            inputs.model.explicit, None,
            "whitespace-only explicit model must collapse so mesh/application win"
        );
        assert_eq!(
            inputs.effort.explicit, None,
            "whitespace-only explicit effort must collapse so mesh/application win"
        );
        // Mesh + application survive the collapse — they're the layers
        // that win when explicit is blank.
        assert_eq!(inputs.model.mesh, Some("haiku-4"));
        assert_eq!(inputs.effort.application, Some("high"));
    }

    /// Trimming: a layer value with surrounding whitespace keeps its
    /// trimmed content (the harness shouldn't receive ` opus `, but
    /// `opus`). Mirrors `resolver_trims_layer_values` at the spawn seam
    /// so an explicit value like `" opus "` lands at the resolver as
    /// `"opus"` regardless of which side trimmed it.
    #[test]
    fn cascade_inputs_for_trims_explicit_values() {
        let inputs = cascade_inputs_for(
            Some("  opus  "),
            Some(" high\t"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(inputs.model.explicit, Some("opus"));
        assert_eq!(inputs.effort.explicit, Some("high"));
    }

    /// Independence: model and effort can be set independently. A spawn
    /// site that only wants to override model must NOT accidentally
    /// clobber effort. Pin for issue #1155 AC #1 — "explicit model
    /// and/or effort argument".
    #[test]
    fn cascade_inputs_for_independent_fields() {
        let app_default = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
        };

        // Explicit model only — effort falls through to the app default.
        let model_only = cascade_inputs_for(Some("sonnet-4"), None, None, None, Some(&app_default), None);
        assert_eq!(model_only.model.explicit, Some("sonnet-4"));
        assert_eq!(model_only.effort.explicit, None);
        assert_eq!(model_only.effort.application, Some("high"));

        // Explicit effort only — model falls through to the app default.
        let effort_only = cascade_inputs_for(None, Some("low"), None, None, Some(&app_default), None);
        assert_eq!(effort_only.model.explicit, None);
        assert_eq!(effort_only.model.application, Some("opus-4-1"));
        assert_eq!(effort_only.effort.explicit, Some("low"));
    }

    /// Integration pin: feed the helper's output through the resolver
    /// and verify the explicit value wins over the mesh + application
    /// layers. This is the "real spawn site" regression test for issue
    /// #1155 AC #4 — every layer is populated, so any layer's value
    /// reaching the resolver instead of the explicit one flips the
    /// assertion. The harness is Anthropic (model + effort both
    /// supported) so the capability mask passes everything through.
    #[test]
    fn cascade_inputs_for_layer1_wins_over_mesh_and_application_at_resolver() {
        let app_default = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
        };
        let inputs = cascade_inputs_for(
            Some("sonnet-4"),
            Some("low"),
            Some("haiku-4"),
            Some("medium"),
            Some(&app_default),
            None,
        );
        let resolved = resolve_agent_config(&anthropic_caps(), inputs, None);
        assert_eq!(
            resolved.model.as_deref(),
            Some("sonnet-4"),
            "layer-1 explicit must win over mesh and application"
        );
        assert_eq!(
            resolved.effort.as_deref(),
            Some("low"),
            "layer-1 explicit must win over mesh and application"
        );
    }

    /// Integration pin for the fall-through path: when explicit is empty
    /// (whitespace) at the spawn seam, the resolver sees `None` for that
    /// slot and the mesh layer drives the resolved value (cascade order:
    /// explicit > mesh > application). Combined with
    /// `cascade_inputs_for_collapses_whitespace_explicit_to_none`, this is
    /// the end-to-end "no silent blank arg to the harness" regression
    /// pin — the explicit slot's whitespace doesn't reach the resolver,
    /// and the mesh slot wins over the application slot per the
    /// documented cascade.
    #[test]
    fn cascade_inputs_for_empty_explicit_falls_through_at_resolver() {
        let app_default = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
        };
        let inputs = cascade_inputs_for(
            Some("   "),
            Some(""),
            Some("haiku-4"),
            Some("medium"),
            Some(&app_default),
            None,
        );
        let resolved = resolve_agent_config(&anthropic_caps(), inputs, None);
        // Explicit collapsed → mesh wins over application.
        assert_eq!(resolved.model.as_deref(), Some("haiku-4"));
        assert_eq!(resolved.effort.as_deref(), Some("medium"));
    }

    /// Regression pin for issue #1155 AC #2: the explicit layer must
    /// drive the resolved value even when the mesh slot ALSO has a
    /// value — proving the helper routes the explicit value to the
    /// resolver's `explicit` slot (not, say, the `mesh` slot). A
    /// future refactor that re-orders the helper's parameters or
    /// mistakenly maps the explicit arg to the mesh slot would flip
    /// this assertion (model would resolve to "haiku-4" — the mesh
    /// value — instead of "sonnet-4").
    #[test]
    fn cascade_inputs_for_explicit_wins_over_mesh_when_application_empty() {
        let inputs = cascade_inputs_for(
            Some("sonnet-4"),
            Some("medium"),
            Some("haiku-4"),
            Some("low"),
            None,
            None,
        );
        let resolved = resolve_agent_config(&anthropic_caps(), inputs, None);
        assert_eq!(resolved.model.as_deref(), Some("sonnet-4"));
        assert_eq!(resolved.effort.as_deref(), Some("medium"));
    }

    /// Per-Mesh harness override wiring at the spawn seam (issue #1151).
    /// The `mesh_override` slot sits between explicit and the legacy mesh
    /// layer (cascade: explicit > mesh_override > mesh > application > native).
    /// A populated mesh override wins over the application default and
    /// falls below explicit.
    #[test]
    fn cascade_inputs_for_mesh_override_wins_over_application() {
        let app_default = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
        };
        let mesh_override = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("medium".into()),
        };
        let inputs = cascade_inputs_for(
            None,
            None,
            None,
            None,
            Some(&app_default),
            Some(&mesh_override),
        );
        let resolved = resolve_agent_config(&anthropic_caps(), inputs, None);
        assert_eq!(resolved.model.as_deref(), Some("opus-4-1"));
        assert_eq!(resolved.effort.as_deref(), Some("medium"));
    }

    /// Mesh override is masked per-field by the harness's capability
    /// contract: OpenCode accepts model (`--model provider/model`) but
    /// has no effort control, so effort drops and model passes.
    #[test]
    fn cascade_inputs_for_mesh_override_drops_effort_for_opencode() {
        let mesh_override = HarnessConfigValue {
            model: Some("some-model".into()),
            effort: Some("high".into()),
        };
        let inputs = cascade_inputs_for(
            None,
            None,
            None,
            None,
            None,
            Some(&mesh_override),
        );

        let resolved = crate::agent::capabilities::resolve_agent_config(
            &crate::agent::capabilities::capabilities_for(
                &crate::agent::provider::adapters::OPENCODE,
            ),
            inputs,
            None,
        );
        // OpenCode accepts `--model provider/model` and has no effort
        // control. The mesh override model must pass; effort must drop.
        assert_eq!(resolved.model.as_deref(), Some("some-model"));
        assert_eq!(resolved.effort, None);
    }

    /// `SpawnOptions` must carry the explicit slots through to
    /// `spawn_agent_inner` (issue #1155 AC #1). The orchestrator
    /// destructures them out of `opts`; this test pins the struct
    /// shape so a refactor that drops either field fails compilation.
    #[test]
    fn spawn_options_carries_explicit_slots() {
        let opts = SpawnOptions {
            session_id: -1,
            provider: Provider::Anthropic,
            resume: None,
            rows: 24,
            cols: 80,
            prefill: None,
            node: None,
            explicit_model: Some("sonnet-4".into()),
            explicit_effort: Some("low".into()),
            // Issue #1358: every transport that builds a `SpawnRequest`
            // and reaches `spawn_agent_inner` via `spawn_with_intent`
            // forwards `explicit_extra_args` from the v2 SpawnAgentNode
            // explicit slot. None is fine — the resolver then cascades
            // through mesh / app defaults and `default_prepare` only
            // forwards the string when `supports_extra_args = true`.
            explicit_extra_args: None,
        };
        assert_eq!(opts.explicit_model.as_deref(), Some("sonnet-4"));
        assert_eq!(opts.explicit_effort.as_deref(), Some("low"));
        assert!(opts.explicit_extra_args.is_none());
    }

    /// `SpawnRequest` must carry an `explicit` field (issue #1155 AC #1).
    /// The struct shape pin protects `spawn_with_intent` from a future
    /// refactor that drops the field — the orchestrator destructures
    /// `explicit` out of the request and feeds it into `SpawnOptions`.
    #[test]
    fn spawn_request_carries_explicit_overrides() {
        let req = SpawnRequest {
            node_id: -1,
            intent: SpawnIntent::Fresh,
            terminal_size: TerminalSize { rows: 24, cols: 80 },
            explicit: ExplicitSpawnOverrides {
                model: Some("opus-4-1".into()),
                effort: Some("high".into()),
                extra_args: None,
            },
        };
        assert_eq!(req.explicit.model.as_deref(), Some("opus-4-1"));
        assert_eq!(req.explicit.effort.as_deref(), Some("high"));

        // `Default` lets spawn sites opt out via `..Default::default()`.
        assert_eq!(ExplicitSpawnOverrides::default().model, None);
        assert_eq!(ExplicitSpawnOverrides::default().effort, None);
    }

    // -----------------------------------------------------------------------
    // `SpawnRequest::new` constructor + integration pin for the cascade
    // layer-1 wiring at a real spawn site (issue #1157).
    //
    // The cascade tests above (lines 2556-2744) pin the helper +
    // resolver precedence — issue #1155 AC #4 ("Regression tests must
    // verify layer-1 behavior at a real spawn site, not just resolver
    // unit tests") is satisfied at the helper level. The tests below
    // close the remaining gap by driving a *real* `SpawnRequest` —
    // built through the new constructor + `with_explicit` builder —
    // through the same call shape `spawn_agent_inner` uses, asserting
    // the explicit value reaches `FieldInputs::explicit` and wins over
    // the mesh + application layers. The harness is Anthropic
    // (`anthropic_caps()`, supports both model + effort) so the
    // capability mask passes everything through.
    // -----------------------------------------------------------------------

    /// Constructor contract pin (issue #1157): `SpawnRequest::new` must
    /// set `explicit` to `Default::default()` so every existing call site
    /// that doesn't wire layer-1 overrides gets the layer-1-empty
    /// behaviour without re-declaring the field. Without this pin a
    /// future refactor that returns `Self { ... explicit: <something> }`
    /// silently changes the cascade behaviour at every call site.
    #[test]
    fn spawn_request_new_sets_explicit_default() {
        let req = SpawnRequest::new(
            42,
            SpawnIntent::Fresh,
            TerminalSize::default(),
        );
        assert_eq!(req.node_id, 42);
        assert_eq!(req.terminal_size, TerminalSize { rows: 24, cols: 80 });
        assert_eq!(req.explicit, ExplicitSpawnOverrides::default());
        assert_eq!(req.explicit.model, None);
        assert_eq!(req.explicit.effort, None);
    }

    #[test]
    fn every_non_resume_intent_replaces_a_stored_conversation() {
        assert!(intent_replaces_conversation(&SpawnIntent::Fresh));
        assert!(intent_replaces_conversation(&SpawnIntent::Loop {
            initial_prompt: "continue".into(),
        }));
        assert!(intent_replaces_conversation(&SpawnIntent::Handover {
            selected_text: "context".into(),
        }));
        assert!(!intent_replaces_conversation(&SpawnIntent::Resume {
            cause: ResumeCause::Explicit,
        }));
    }

    /// AC #4 pin: a `SpawnRequest` with a populated layer-1 override
    /// (model + effort) drives the helper extracted from
    /// `spawn_agent_inner` and the resolved config carries the explicit
    /// value — winning over the mesh + application layers. This is the
    /// "real spawn site" regression test issue #1155 AC #4 called for:
    /// the helper-level tests above (lines 2556-2744) exercise the
    /// same inputs against the same resolver, but this test drives them
    /// *through* the `SpawnRequest` shape every transport hands the
    /// orchestrator. A future refactor that drops the `explicit` field
    /// or maps it to the wrong `SpawnOptions` slot would flip this
    /// assertion.
    #[test]
    fn spawn_request_explicit_wins_at_resolver() {
        let req = SpawnRequest::new(
            42,
            SpawnIntent::Fresh,
            TerminalSize::default(),
        )
        .with_explicit(ExplicitSpawnOverrides {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
            // Issue #1358: extra_args ride the same cascade layer-1
            // slot. A non-None value here proves the wiring from
            // `SpawnRequest.explicit.extra_args` → `SpawnOptions
            // .explicit_extra_args` → `resolve_spawn_config` — the
            // gap the spec review flagged (#1358) where this string
            // was collected by the Inspector but dropped at the
            // `spawn_with_intent` seam.
            extra_args: Some("--dangerously-skip-permissions --verbose".into()),
        });
        let app_default = HarnessConfigValue {
            model: Some("sonnet-4".into()),
            effort: Some("medium".into()),
        };
        let resolved = resolve_spawn_config(
            Provider::Anthropic,
            req.explicit.model.as_deref(),
            req.explicit.effort.as_deref(),
            req.explicit.extra_args.as_deref(),
            Some(&app_default),
            None,
        );
        assert_eq!(
            resolved.model.as_deref(),
            Some("opus-4-1"),
            "SpawnRequest.explicit.model must reach the resolver as FieldInputs::explicit and win over mesh + application"
        );
        assert_eq!(
            resolved.extra_args.as_deref(),
            Some("--dangerously-skip-permissions --verbose"),
            "SpawnRequest.explicit.extra_args must reach ResolvedAgentConfig \
             (issue #1358 AC: extra-args override honoured per harness capability contract)"
        );
        assert_eq!(
            resolved.effort.as_deref(),
            Some("high"),
            "SpawnRequest.explicit.effort must reach the resolver as FieldInputs::explicit and win over mesh + application"
        );
    }

    /// AC #3 pin: whitespace-only explicit values collapse to `None`
    /// inside the helper so the cascade falls through to the next
    /// layer (issue #1148 AC #32 + #1155 AC #3). Mirrors
    /// `cascade_inputs_for_empty_explicit_falls_through_at_resolver`
    /// (line 2706) but driven from `SpawnRequest`, proving the
    /// collapse from #1155 AC #3 holds end-to-end — i.e. the
    /// `SpawnRequest → SpawnOptions → resolver` path doesn't smuggle
    /// a blank past the `non_empty_trim` guard in `cascade_inputs_for`.
    #[test]
    fn spawn_request_whitespace_explicit_falls_through_at_resolver() {
        let req = SpawnRequest::new(
            42,
            SpawnIntent::Fresh,
            TerminalSize::default(),
        )
        .with_explicit(ExplicitSpawnOverrides {
            model: Some("   ".into()),
            effort: Some("\t\n".into()),
            extra_args: None,
        });
        let mesh_override = HarnessConfigValue {
            model: Some("haiku-4".into()),
            effort: Some("medium".into()),
        };
        let app_default = HarnessConfigValue {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
        };
        let resolved = resolve_spawn_config(
            Provider::Anthropic,
            req.explicit.model.as_deref(),
            req.explicit.effort.as_deref(),
            req.explicit.extra_args.as_deref(),
            // Legacy mesh columns are no longer read as active config
            // (issue #1151 AC #6) — the v33 migration copied any
            // non-empty legacy values into the mesh override map.
            Some(&app_default),
            Some(&mesh_override),
        );
        // Explicit collapsed → mesh_override wins over application.
        assert_eq!(resolved.model.as_deref(), Some("haiku-4"));
        assert_eq!(resolved.effort.as_deref(), Some("medium"));
    }

    /// Issue #1358 end-to-end pin: the `SpawnRequest → SpawnOptions →
    /// resolve_spawn_config` path **must** deliver `explicit_extra_args`
    /// end-to-end AND capability-mask it against the harness's
    /// `supports_extra_args`. Terminal is the standing opt-out (a
    /// plain-shell harness must not get synthetic flags spliced into
    /// its argv) — the spec review (#1358) flagged this as the AC
    /// violation that needed a regression pin. This test is the pin.
    #[test]
    fn spawn_request_extra_args_capability_mask_at_resolver() {
        // Anthropic — interactive harness, `supports_extra_args = true`.
        let req_interactive = SpawnRequest::new(
            42,
            SpawnIntent::Fresh,
            TerminalSize::default(),
        )
        .with_explicit(ExplicitSpawnOverrides {
            model: None,
            effort: None,
            extra_args: Some("--dangerously-skip-permissions".into()),
        });
        let resolved_anthropic = resolve_spawn_config(
            Provider::Anthropic,
            req_interactive.explicit.model.as_deref(),
            req_interactive.explicit.effort.as_deref(),
            req_interactive.explicit.extra_args.as_deref(),
            None,
            None,
        );
        assert_eq!(
            resolved_anthropic.extra_args.as_deref(),
            Some("--dangerously-skip-permissions"),
            "Anthropic supports extra_args — the explicit slot must reach ResolvedAgentConfig"
        );

        // Terminal — plain-shell harness, `supports_extra_args = false`.
        let req_terminal = SpawnRequest::new(
            42,
            SpawnIntent::Fresh,
            TerminalSize::default(),
        )
        .with_explicit(ExplicitSpawnOverrides {
            model: None,
            effort: None,
            extra_args: Some("--dangerously-skip-permissions --verbose".into()),
        });
        let resolved_terminal = resolve_spawn_config(
            Provider::Terminal,
            req_terminal.explicit.model.as_deref(),
            req_terminal.explicit.effort.as_deref(),
            req_terminal.explicit.extra_args.as_deref(),
            None,
            None,
        );
        assert!(
            resolved_terminal.extra_args.is_none(),
            "Terminal masks extra_args at the resolver (issue #1358). Got: {:?}",
            resolved_terminal.extra_args
        );
    }

    // -----------------------------------------------------------------------
    // Reader-epilogue decision matrix (false "failed to start" fix).
    //
    // The reader thread's post-exit status write used to apply the 3s
    // early-exit Error heuristic unconditionally, so a process that
    // `kill_session` tore down deliberately (spawn step-2 stale kill, node
    // close, app shutdown) within 3s of its creation was stamped `Error`
    // + toasted `resume-failed` — and that stale Error then blocked the
    // replacing spawn's Spawning→Running promotion. These tests pin the
    // full matrix of `post_exit_action`.
    // -----------------------------------------------------------------------

    #[test]
    fn deliberate_kill_never_writes_status_even_within_early_exit_window() {
        // The heart of the fix: a deliberate kill 1s after process creation
        // must NOT be misread as a failed --resume.
        assert_eq!(
            post_exit_action(false, true, std::time::Duration::from_secs(1)),
            PostExitAction::LeaveStatusAlone,
        );
        // …nor may it write Idle over the replacing spawn's Spawning.
        assert_eq!(
            post_exit_action(false, true, std::time::Duration::from_secs(60)),
            PostExitAction::LeaveStatusAlone,
        );
        // Plain terminals too: the kill initiator owns the next status.
        assert_eq!(
            post_exit_action(true, true, std::time::Duration::from_secs(1)),
            PostExitAction::LeaveStatusAlone,
        );
    }

    #[test]
    fn natural_early_exit_still_flags_resume_failure() {
        // The heuristic's true positive is preserved: an LLM process that
        // dies on its own within the window (typically `--resume` against
        // an expired session) still reads as a resume failure.
        assert_eq!(
            post_exit_action(false, false, std::time::Duration::from_secs(1)),
            PostExitAction::MarkErrorResumeFailed,
        );
    }

    #[test]
    fn natural_exit_after_window_marks_idle() {
        assert_eq!(
            post_exit_action(false, false, EARLY_EXIT_WINDOW),
            PostExitAction::MarkIdle,
        );
    }

    #[test]
    fn plain_terminal_natural_exit_is_idle_regardless_of_elapsed() {
        // A shell exiting fast is not a resume signal.
        assert_eq!(
            post_exit_action(true, false, std::time::Duration::from_millis(10)),
            PostExitAction::MarkIdle,
        );
    }

    // -----------------------------------------------------------------------
    // Per-session spawn claim (duplicate-spawn fix). `is_agent_already_running`
    // only sees registered processes and registration is seconds into the
    // pipeline, so the claim must cover the whole `spawn_agent_inner` body.
    // Test ids are unique across the suite (tests share the process-global
    // set and run in parallel).
    // -----------------------------------------------------------------------

    #[test]
    fn spawn_claim_rejects_concurrent_duplicate_for_same_session() {
        let first = SpawnInFlightClaim::try_claim(-917_0001);
        assert!(first.is_some(), "first claim must succeed");
        assert!(
            SpawnInFlightClaim::try_claim(-917_0001).is_none(),
            "second claim for the same session while the first is held must \
             be rejected — this is what stops a duplicate spawn_agent_inner \
             from killing the in-flight spawn's freshly-booted process"
        );
    }

    #[test]
    fn spawn_claim_is_per_session() {
        let _a = SpawnInFlightClaim::try_claim(-917_0002).expect("claim a");
        assert!(
            SpawnInFlightClaim::try_claim(-917_0003).is_some(),
            "claims for different sessions must not contend"
        );
    }

    #[test]
    fn spawn_claim_released_on_drop() {
        {
            let _claim = SpawnInFlightClaim::try_claim(-917_0004).expect("claim");
        }
        assert!(
            SpawnInFlightClaim::try_claim(-917_0004).is_some(),
            "dropping the claim must release the session for the next spawn \
             (RAII covers every return path, including cancelled tasks)"
        );
    }

    /// Regression guard for the user-visible "failed to start" symptom.
    ///
    /// Spawn RACERS threads racing `try_claim` for the same session —
    /// the first to acquire the HashSet entry wins, the rest see the
    /// entry present and get `None`. Pins the entire atomicity story:
    /// without it, two concurrent `spawn_agent_inner` calls for the
    /// same node (backend stage-2 vs frontend Terminal auto-spawn on
    /// `'idle'`) both passed the registry check and the loser's step-2
    /// stale-kill destroyed the winner's freshly-booted process — the
    /// "failed to start, yet it boots seconds later" symptom.
    ///
    /// Uses a fresh session id per round so the test doesn't depend on
    /// the racing threads' Drop ordering vs the next round's claim —
    /// the global HashSet could in principle still hold a stale entry
    /// from a previous round's racer that hasn't yet been observed as
    /// dropped by the test thread (parking_lot's Drop is synchronous,
    /// but the test thread's join() happens-before the next round).
    #[test]
    fn concurrent_spawn_claim_exactly_one_winner() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrd};

        const RACERS: usize = 8;
        const ROUNDS: usize = 200;

        for round in 0..ROUNDS {
            // Fresh session id per round so there's no cross-round
            // dependency on Drop ordering.
            let session: i64 = -917_1000 - round as i64;

            let winners = Arc::new(AtomicUsize::new(0));
            // Two barriers: gate the racers before the lock, AND gate
            // them before the drop. Without the second gate, a racer
            // that loses the lock race still releases its (empty) claim
            // path before the next racer even tries — the second
            // barrier forces every racer to attempt the lock with the
            // claim held until the round-end signal.
            let start_barrier = Arc::new(std::sync::Barrier::new(RACERS + 1));
            let end_barrier = Arc::new(std::sync::Barrier::new(RACERS + 1));

            let handles: Vec<_> = (0..RACERS)
                .map(|_| {
                    let winners = winners.clone();
                    let start = start_barrier.clone();
                    let end = end_barrier.clone();
                    std::thread::spawn(move || {
                        // Phase 1: align all racers at the lock.
                        start.wait();
                        let claim = SpawnInFlightClaim::try_claim(session);
                        if claim.is_some() {
                            winners.fetch_add(1, AOrd::SeqCst);
                        }
                        // Phase 2: hold the claim until the test thread
                        // signals round end. Any racer arriving at the
                        // lock now MUST see the existing entry (the
                        // insert returns false → claim is None).
                        end.wait();
                        drop(claim);
                    })
                })
                .collect();

            // Fire the start gun — every racer races for the lock now.
            start_barrier.wait();
            // Give every racer time to acquire the lock, observe the
            // entry, and reach the end barrier.
            end_barrier.wait();
            for h in handles {
                h.join().unwrap();
            }

            assert_eq!(
                winners.load(AOrd::SeqCst),
                1,
                "exactly one racer must win the claim (round {round}, session {session})"
            );

            // After the last racing thread joined, its _claim dropped,
            // releasing the entry. Confirm by claiming it ourselves —
            // this exercises the post-drop "slot is empty" invariant
            // and prevents cross-round state pollution if a future
            // refactor accidentally leaks entries.
            assert!(
                SpawnInFlightClaim::try_claim(session).is_some(),
                "round {round}: racers all joined so their claims dropped — \
                 the next try_claim for session {session} must find the slot empty"
            );
        }
    }

    /// RAII must release on a *cancelled* async task too — the field doc
    /// on `SpawnInFlightClaim` makes that an explicit guarantee. A
    /// `tokio::time::timeout` racing a future that holds the claim is
    /// the cheapest reproduction: the future is dropped at the await
    /// point, the claim's Drop runs synchronously, and the next
    /// `try_claim` must succeed.
    #[test]
    fn spawn_claim_released_when_async_task_is_cancelled() {
        // No real DB / PTY needed — the claim itself is what we're
        // pinning. Drive it on a runtime so the cancellation path
        // (Future::drop mid-await) actually runs.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let session = -917_0006;
        rt.block_on(async {
            // Spawn a task that holds the claim for "the whole pipeline"
            // (here, forever). Cancel it via timeout.
            let task = tokio::spawn(async move {
                let _claim = SpawnInFlightClaim::try_claim(session)
                    .expect("first claim must succeed");
                // Park forever. The test cancels this task below.
                std::future::pending::<()>().await;
            });

            // Let the task reach its pending await.
            tokio::task::yield_now().await;
            task.abort();
            // The abort drops the task's locals → Drop runs → claim released.
            let _ = task.await;

            assert!(
                SpawnInFlightClaim::try_claim(session).is_some(),
                "aborting the holding task must release the claim (RAII covers \
                 cancelled futures, not just successful return)"
            );
        });
    }

    // -----------------------------------------------------------------------
    // Warm-pool manual claim — .worktreeinclude re-application (issue #639
    // gap 1). The cold `create_git_worktree` and the Issue/PR `adopt…by_move`
    // both call `apply_worktree_include` so an adopted worktree is byte-for-
    // byte equivalent to a cold spawn. The manual warm-claim fast path
    // (upgrade_warm_to_mode) MUST do the same — otherwise a user who edits a
    // `.worktreeinclude`-referenced file (typical: `.env`, build cache) between
    // prewarm time and spawn time lands on a stale copy.
    // -----------------------------------------------------------------------

    #[test]
    fn upgrade_warm_to_mode_reapplies_worktreeinclude_after_checkout() {
        use std::fs;
        let (_td, root, pool) = setup_warm_pool_with_include();

        // User edits the source file BETWEEN prewarm and manual spawn —
        // exactly the window the missing apply_worktree_include used to leak.
        fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

        // The manual warm claim's mode upgrade — must re-copy `.worktreeinclude`
        // sources so the agent's worktree matches the live repo state, not the
        // stale prewarm snapshot.
        upgrade_warm_to_mode(root.to_str().unwrap(), pool.to_str().unwrap(), "bold-amber-fox", "branched")
            .expect("upgrade_warm_to_mode must succeed");

        // The worktree's `.worktreeinclude`-referenced file must now reflect
        // the live repo content (NEW), not the prewarm-time snapshot (old).
        assert_eq!(
            fs::read_to_string(pool.join("secrets.env")).unwrap(),
            "v1=NEW\n",
            "manual warm claim must re-apply .worktreeinclude so the agent sees the live source"
        );
    }

    /// No `.worktreeinclude` at the repo root → the upgrade is still a no-op
    /// rather than an error. Prevents a regression where adding the include
    /// re-application broke a repo that never used the feature.
    #[test]
    fn upgrade_warm_to_mode_is_noop_when_no_worktreeinclude() {
        use crate::env::test_helpers::init_repo_with_commit;
        use std::fs;
        // Skip the .worktreeinclude side of the helper — bare repo + pool.
        let td = TempDir::new().unwrap();
        let root = td.path();
        let _ = init_repo_with_commit(root, &[("f.txt", "tracked\n")]);
        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();
        let _ = td; // keep alive for the duration of the test

        upgrade_warm_to_mode(root.to_str().unwrap(), pool.to_str().unwrap(), "bold-amber-fox", "branched")
            .expect("must succeed when no .worktreeinclude exists");
        // No spurious `.worktreeinclude` was created in the worktree.
        assert!(
            !pool.join(".worktreeinclude").exists(),
            "absent manifest must not be materialised by the upgrade"
        );
        // The tracked file round-trips.
        assert_eq!(fs::read_to_string(pool.join("f.txt")).unwrap(), "tracked\n");
    }

    /// Detached mode must also re-apply `.worktreeinclude` (issue #639 gap 1,
    /// review finding). The original `upgrade_warm_to_mode` returned early on
    /// `mode == "detached"` and skipped the include copy — a regression that
    /// re-instated that early-return would pass `…_reapplies…_after_checkout`
    /// (branched) but leave a detached-mode spawn on the stale prewarm
    /// snapshot, defeating the gap-1 fix for half the meshes.
    #[test]
    fn upgrade_warm_to_mode_reapplies_worktreeinclude_in_detached_mode() {
        use std::fs;
        let (_td, root, pool) = setup_warm_pool_with_include();

        // User edits the source — same window as the branched-mode test.
        fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

        // Upgrade in DETACHED mode. The branch name is unused (no checkout),
        // but we pass the preassigned slug for consistency with the call site.
        upgrade_warm_to_mode(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
        )
        .expect("upgrade_warm_to_mode must succeed in detached mode");

        assert_eq!(
            fs::read_to_string(pool.join("secrets.env")).unwrap(),
            "v1=NEW\n",
            "manual warm claim in detached mode must also re-apply .worktreeinclude"
        );
        // And the worktree stayed detached — no branch was created.
        let wt = git2::Repository::open(&pool).unwrap();
        assert!(
            wt.head_detached().unwrap_or(false),
            "detached mode must leave the worktree detached"
        );
    }

    /// Shared setup for the two `upgrade_warm_to_mode` `.worktreeinclude`
    /// re-application tests (#642.5). The third test
    /// (`…_is_noop_when_no_worktreeinclude`) deliberately inlines its own
    /// setup because the no-manifest case is the whole point of that test
    /// — running it through the helper would materialise `secrets.env` and
    /// `.worktreeinclude` in the worktree, defeating the no-op assertion.
    ///
    /// The helper stands up: a tempdir holding a real git repo with
    /// `secrets.env` + `.worktreeinclude` (both tracked), AND a pool-shaped
    /// DETACHED worktree under `.claude/worktrees/warm-amber-fox` that has
    /// already had the include copied at prewarm time (so the tests assert
    /// the upgrade re-applies, not the original copy). Both the branched and
    /// the detached call-site tests cut the pool as detached (the pool's
    /// on-disk shape) — the difference between them is the
    /// `upgrade_warm_to_mode` mode argument, not the helper's setup.
    ///
    /// Returns `(tempdir, repo_root_path, pool_path)`. The tempdir is held
    /// to keep the underlying directory alive for the duration of the test
    /// — dropping it would delete the repo and break subsequent asserts.
    fn setup_warm_pool_with_include() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        use crate::env::test_helpers::{commit_file, init_repo_with_commit};
        use std::fs;

        let td = TempDir::new().unwrap();
        let root = td.path().to_path_buf();

        init_repo_with_commit(&root, &[("f.txt", "tracked\n")]);
        fs::write(root.join("secrets.env"), "v1=old\n").unwrap();
        fs::write(root.join(".worktreeinclude"), "secrets.env\n").unwrap();
        // Commit the manifest so `.worktreeinclude` is reachable for `git
        // worktree add`; the pool helper copies files relative to the repo
        // root regardless of whether the manifest itself is tracked, but
        // committing keeps the test setup close to a realistic repo.
        let repo = git2::Repository::open(&root).unwrap();
        commit_file(&repo, &root, ".worktreeinclude", "secrets.env\n");

        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .expect("prewarm-shape worktree must be creatable for this helper");
        assert_eq!(
            fs::read_to_string(pool.join("secrets.env")).unwrap(),
            "v1=old\n",
            "prewarm-time copy must reflect the original source"
        );
        (td, root, pool)
    }

    // -----------------------------------------------------------------------
    // Warm-pool Issue/PR adoption (issue #612): move a detached pool worktree
    // onto the node's target name and check it out to the resolved base SHA on
    // its own branch. These pin the code-review fixes for two confirmed bugs:
    // resolving `base_ref` → SHA (offline resilience), and using `-b` (NOT
    // `-B`) so a re-spawn can never force-reset a branch carrying prior work.
    // -----------------------------------------------------------------------

    #[test]
    fn adopt_warm_worktree_moves_and_branches_at_base_sha() {
        use crate::env::test_helpers::init_repo_with_commit;
        let td = TempDir::new().unwrap();
        let root = td.path();
        let repo = init_repo_with_commit(root, &[("f.txt", "a\n")]);
        let head = repo.head().unwrap().peel_to_commit().unwrap().id().to_string();

        // The pool's on-disk shape: a DETACHED worktree under a plain slug.
        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();

        let target = root.join(".claude").join("worktrees").join("gh123-fix");
        adopt_warm_worktree_by_move(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            target.to_str().unwrap(),
            "gh123-fix",
            "branched",
            "HEAD",
        )
        .expect("adoption must succeed");

        assert!(!pool.exists(), "pool directory must be gone after the move");
        assert!(target.exists(), "target directory must exist after the move");
        let wt = git2::Repository::open(&target).unwrap();
        assert_eq!(
            wt.head().unwrap().shorthand().unwrap(),
            "gh123-fix",
            "the adopted worktree must be on the node's own branch"
        );
        assert_eq!(
            wt.head().unwrap().peel_to_commit().unwrap().id().to_string(),
            head,
            "the branch must sit at the resolved base SHA"
        );
    }

    #[test]
    fn adopt_warm_worktree_refuses_to_clobber_an_existing_branch() {
        use crate::env::test_helpers::init_repo_with_commit;
        let td = TempDir::new().unwrap();
        let root = td.path();
        let repo = init_repo_with_commit(root, &[("f.txt", "a\n")]);
        // A pre-existing deterministic branch standing in for a prior spawn's
        // work. Force-resetting it (the old `-B` bug) would orphan its commits.
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("gh7-x", &head_commit, false).unwrap();

        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();

        let target = root.join(".claude").join("worktrees").join("gh7-x");
        let err = adopt_warm_worktree_by_move(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            target.to_str().unwrap(),
            "gh7-x",
            "branched",
            "HEAD",
        )
        .expect_err("adoption must refuse to overwrite an existing branch");
        assert!(
            err.contains("already exists"),
            "the failure must name the existing branch refusal, got: {}",
            err
        );
        // Fail-fast contract: refusal is pre-move — see the guard in
        // `adopt_warm_worktree_by_move`.
        assert!(pool.exists(), "pool entry must be untouched after a refused adoption");
        assert!(!target.exists(), "target must not be materialised by a refused adoption");
    }

    // -----------------------------------------------------------------------
    // base_ref resolution (master-trunk regression)
    //
    // Pre-fix, the spawn path hardcoded `"origin/main"` as the default
    // `base_ref` when the `meshes.base_ref` DB column was `'origin/main'`
    // (its COALESCE default) — meaning a master-trunk repo always hit
    // `mesh-sync-warning` on every spawn (`fatal: couldn't find remote
    // ref main`). These tests pin the resolution chain:
    //
    //   1. meshes.base_ref (BUT NOT the COALESCE default — that's
    //      treated as "no config" so the detection chain runs)
    //   2. refs/remotes/origin/HEAD read from the local repo
    //   3. "origin/main" last resort
    //
    // The COALESCE-sentinel treatment is critical: the DB column is
    // NOT NULL with default `'origin/main'`, so `Mesh.base_ref` is
    // ALWAYS a non-empty `String` and `MeshRow.base_ref` is ALWAYS
    // `Some(_)` — a naive `if let Some(b) = config_base_ref { return b }`
    // would make the detection chain dead code in production. The
    // `resolve_base_ref_treats_coalesce_sentinel_as_unset` test pins the
    // production call path (`Some("origin/main")`).
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_base_ref_uses_config_value_when_set() {
        // The config wins even on a non-repo / non-master path — explicit
        // user intent overrides any auto-detection. Empty / whitespace
        // config falls through to the detection chain (regression guard
        // for an empty-string value slipping through the COALESCE).
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("origin/develop")),
            "origin/develop"
        );
        // Empty / whitespace strings are treated as "no config" so the
        // detection chain runs — mirrors the COALESCE-to-default contract
        // in the DB layer.
        assert_eq!(
            resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("")),
            "origin/main",
            "empty config base_ref must fall through to detection, not propagate"
        );
        assert_eq!(
            resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("   ")),
            "origin/main",
            "whitespace-only config base_ref must fall through to detection"
        );
    }

    #[test]
    fn resolve_base_ref_falls_back_to_origin_main_for_non_repo() {
        // Non-repo path with no config — must not panic. Last-resort
        // behaviour preserved: `get_default_branch` returns "main" on a
        // failed `Repository::open`, and we prefix it with "origin/".
        // The spawn path itself short-circuits to `RepoUnusable` so the
        // auto-sync result is non-blocking.
        let tmp = TempDir::new().unwrap();
        let resolved = resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), None);
        assert_eq!(resolved, "origin/main");
    }

    #[test]
    fn resolve_base_ref_detects_master_via_origin_head() {
        // Headline regression test: a master-trunk repo with no
        // `base_ref` in mesh config must produce "origin/master", not
        // the legacy "origin/main". Pre-fix, this always returned
        // "origin/main" and the spawn emitted a `mesh-sync-warning` on
        // every node.
        use crate::env::test_helpers::TestDir;
        use git2;

        let td = TestDir::new("base_ref_master");
        let parent = td.path();
        // Create a working repo on whatever default branch git picks.
        // The local branch name doesn't matter — what matters is that
        // `refs/remotes/origin/HEAD` points at `refs/remotes/origin/master`.
        crate::env::test_helpers::init_repo_with_commit(
            parent,
            &[("README.md", "v1\n")],
        );

        let repo = git2::Repository::open(parent).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        // Build the symbolic ref that `get_default_branch` reads.
        repo.reference(
            "refs/remotes/origin/master",
            oid,
            true,
            "test setup",
        )
        .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
            true,
            "test setup",
        )
        .unwrap();

        // Sanity: precondition for the test to be meaningful.
        let head_ref = repo
            .find_reference("refs/remotes/origin/HEAD")
            .unwrap()
            .symbolic_target()
            .unwrap()
            .to_string();
        assert_eq!(
            head_ref, "refs/remotes/origin/master",
            "precondition: origin/HEAD must point at refs/remotes/origin/master"
        );

        let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
        assert_eq!(
            resolved, "origin/master",
            "master-trunk repo with no base_ref in config must yield origin/master, \
             not the legacy hardcoded origin/main (this is the master-trunk regression)"
        );
    }

    #[test]
    fn resolve_base_ref_detects_main_via_origin_head() {
        // Sanity pin: the existing main-trunk behaviour (a repo whose
        // origin/HEAD points at `main`) must still resolve to
        // "origin/main" after the fix. Guards against the master fix
        // accidentally regressing the main case.
        use crate::env::test_helpers::TestDir;
        use git2;

        let td = TestDir::new("base_ref_main");
        let parent = td.path();
        crate::env::test_helpers::init_repo_with_commit(
            parent,
            &[("README.md", "v1\n")],
        );

        let repo = git2::Repository::open(parent).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference(
            "refs/remotes/origin/main",
            oid,
            true,
            "test setup",
        )
        .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            true,
            "test setup",
        )
        .unwrap();

        let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
        assert_eq!(
            resolved, "origin/main",
            "main-trunk repo must still resolve to origin/main (no regression)"
        );
    }

    #[test]
    fn resolve_base_ref_treats_coalesce_sentinel_as_unset() {
        // The production call path: `meshes.base_ref` is a NOT NULL
        // column with a COALESCE default of `'origin/main'` (see
        // `db::MESH_COLUMNS`). A fresh mesh whose base_ref was never
        // explicitly set reads as `Some("origin/main")` from the DB →
        // `MeshRow.base_ref = Some("origin/main")` →
        // `config.as_ref().and_then(|c| c.base_ref.as_deref())` returns
        // `Some("origin/main")`. The helper MUST treat this sentinel as
        // "no config" and fall through to the detection chain, otherwise
        // a master-trunk repo's spawn still hits `mesh-sync-warning`.
        // The earlier `_detects_master_via_origin_head` test passes
        // `None` (which never reaches production); THIS test pins the
        // actual production contract.
        use crate::env::test_helpers::TestDir;
        use git2;

        let td = TestDir::new("base_ref_coalesce_master");
        let parent = td.path();
        crate::env::test_helpers::init_repo_with_commit(
            parent,
            &[("README.md", "v1\n")],
        );

        let repo = git2::Repository::open(parent).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference(
            "refs/remotes/origin/master",
            oid,
            true,
            "test setup",
        )
        .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
            true,
            "test setup",
        )
        .unwrap();

        // Production-shaped input: COALESCE default from the DB.
        let resolved = resolve_base_ref_for_spawn(
            parent.to_str().unwrap(),
            Some("origin/main"),
        );
        assert_eq!(
            resolved, "origin/master",
            "the COALESCE default 'origin/main' from a fresh mesh's DB row \
             must be treated as 'no config' — fall through to origin/HEAD \
             detection. A master-trunk repo with an unconfigured mesh \
             produces origin/master, not origin/main. This is the actual \
             production contract; the test passing None never reaches \
             production."
        );
    }

    #[test]
    fn resolve_base_ref_keeps_explicit_user_value_for_main_trunk() {
        // A user who LEGITIMATELY sets `base_ref = "origin/main"` (via
        // the 'Fresh' UI option) on a main-trunk repo must still get
        // "origin/main" back. The COALESCE-sentinel treatment must
        // apply to the *fresh* / *unconfigured* case, not penalize a
        // user who explicitly chose the same value. For a main-trunk
        // repo the auto-detect would return the same value, so this
        // test is mostly a documentation pin.
        use crate::env::test_helpers::TestDir;
        use git2;

        let td = TestDir::new("base_ref_explicit_main");
        let parent = td.path();
        crate::env::test_helpers::init_repo_with_commit(
            parent,
            &[("README.md", "v1\n")],
        );

        let repo = git2::Repository::open(parent).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference(
            "refs/remotes/origin/main",
            oid,
            true,
            "test setup",
        )
        .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            true,
            "test setup",
        )
        .unwrap();

        let resolved = resolve_base_ref_for_spawn(
            parent.to_str().unwrap(),
            Some("origin/main"),
        );
        assert_eq!(
            resolved, "origin/main",
            "explicit user-set 'origin/main' on a main-trunk repo must resolve \
             to 'origin/main' (same as auto-detect — no behaviour change)"
        );
    }

    // -----------------------------------------------------------------------
    // SHA-drift detection (issue #444)
    //
    // `read_origin_ref_sha` returns the local SHA at `origin/<head_ref>` so
    // the spawn path can compare it to the user-pinned `source_pr_pinned_sha`
    // and emit a `pr_sha_drift` warning on mismatch. The unit test creates
    // the local ref directly via git2 (no real remote / fetch roundtrip) so
    // the test is hermetic and fast.
    // -----------------------------------------------------------------------

    #[test]
    fn read_origin_ref_sha_returns_local_sha_when_ref_exists() {
        let tmp = TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        // Create a real commit on a known branch — we need a tree OID the
        // commit can point at. `Repository::init` leaves the index empty
        // but write_tree() on an empty index still produces a valid tree.
        let tree_oid = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Manually create the remote-tracking ref the function reads. In
        // production this is what `git fetch origin -- <head_ref>` writes;
        // here we shortcut the network roundtrip to keep the test hermetic.
        let ref_name = "refs/remotes/origin/feat-x";
        repo.reference(ref_name, commit_oid, true, "test").unwrap();

        let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/feat-x");
        assert_eq!(
            sha.as_deref(),
            Some(commit_oid.to_string().as_str()),
            "read_origin_ref_sha must return the full 40-char SHA the ref points to"
        );
    }

    #[test]
    fn read_origin_ref_sha_returns_none_for_missing_ref() {
        let tmp = TempDir::new().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        // No refs/remotes/origin/* exists; the function must return None
        // (the spawn path treats this as "skip drift check" rather than
        // failing — same fail-open semantics as `pr_head_unfetchable`).
        let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/nope");
        assert!(sha.is_none(), "missing ref must return None, not error");
    }

    #[test]
    fn read_origin_ref_sha_returns_none_for_non_git_directory() {
        // A path that isn't a git repo at all — `git rev-parse` exits non-zero,
        // the helper must swallow that and return None rather than panicking.
        let tmp = TempDir::new().unwrap();
        let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/main");
        assert!(sha.is_none(), "non-repo path must return None, not error");
    }

    fn read_injected_settings(project: &std::path::Path) -> serde_json::Value {
        let path = project.join(".claude").join("settings.local.json");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("settings.local.json not written: {}", e));
        serde_json::from_str(&content).expect("settings.local.json is not valid JSON")
    }

    /// The Notification hook must fire on EVERY notification type, not just
    /// `idle_prompt`. An empty matcher is Claude Code's "match all" — without it
    /// the hook ignores `permission_prompt` notifications, so the user is never
    /// alerted when an agent asks to run a tool or otherwise needs a decision.
    /// Regression guard for the "only alerted after the agent finishes" gap.
    #[test]
    fn attention_hook_notification_matcher_is_catch_all() {
        let temp = TempDir::new().unwrap();
        inject_attention_hook(temp.path()).unwrap();

        let settings = read_injected_settings(temp.path());
        let notification = &settings["hooks"]["Notification"][0];
        assert_eq!(
            notification["matcher"], "",
            "Notification matcher must be empty (catch-all) so permission_prompt \
             notifications alert the user, not just idle_prompt"
        );
        let command = notification["hooks"][0]["command"]
            .as_str()
            .expect("notification hook command should be a string");
        assert!(
            command.contains("/api/attention/"),
            "notification hook should POST to the attention endpoint, got: {command}"
        );
    }

    /// A `Stop` hook fires the instant the agent finishes a turn, so the user is
    /// alerted immediately rather than waiting for the `idle_prompt` idle timer.
    #[test]
    fn attention_hook_includes_stop_event() {
        let temp = TempDir::new().unwrap();
        inject_attention_hook(temp.path()).unwrap();

        let settings = read_injected_settings(temp.path());
        let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .expect("Stop hook command should be present so turn-end alerts fire immediately");
        assert!(
            command.contains("/api/attention/"),
            "Stop hook should POST to the attention endpoint, got: {command}"
        );
    }

    /// Both hooks must forward the hook's stdin JSON as the POST body (issue
    /// #878). Claude Code pipes `{hook_event_name, transcript_path, …}` into
    /// the command; without `--data-binary @-` the backend gets an empty body
    /// and cannot tell "turn ended, user needed" from "turn ended, waiting on
    /// background tasks".
    #[test]
    fn attention_hook_forwards_stdin_payload() {
        let temp = TempDir::new().unwrap();
        inject_attention_hook(temp.path()).unwrap();

        let settings = read_injected_settings(temp.path());
        for (event, path) in [
            ("Notification", &settings["hooks"]["Notification"][0]["hooks"][0]),
            ("Stop", &settings["hooks"]["Stop"][0]["hooks"][0]),
        ] {
            let command = path["command"].as_str().unwrap();
            assert!(
                command.contains("--data-binary @-"),
                "{event} hook must forward stdin as the POST body, got: {command}"
            );
            assert!(
                command.contains("Content-Type: application/json"),
                "{event} hook must declare a JSON body, got: {command}"
            );
        }
    }

    /// Injection is idempotent: a second call over an already-correct file must
    /// not rewrite it (the early-return guard) and must leave it parseable.
    #[test]
    fn attention_hook_injection_is_idempotent() {
        let temp = TempDir::new().unwrap();
        inject_attention_hook(temp.path()).unwrap();
        let first = read_injected_settings(temp.path());
        inject_attention_hook(temp.path()).unwrap();
        let second = read_injected_settings(temp.path());
        assert_eq!(first, second, "second injection should be a no-op");
    }

    /// Injection must preserve unrelated keys already present in the user's
    /// settings.local.json (e.g. `permissions`) — it only owns `hooks`.
    #[test]
    fn attention_hook_preserves_other_settings() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.local.json"),
            r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#,
        )
        .unwrap();

        inject_attention_hook(temp.path()).unwrap();

        let settings = read_injected_settings(temp.path());
        assert_eq!(
            settings["permissions"]["allow"][0], "Bash(ls:*)",
            "pre-existing permissions must survive hook injection"
        );
        assert_eq!(settings["hooks"]["Notification"][0]["matcher"], "");
    }

    // ----- fork alias + fetch_fork_head (issue #443) ---------------------

    /// `fork-<login>` is the human-readable alias used in `git remote -v` and
    /// the worktree `base_ref` string. The `fork-` prefix keeps our entries
    /// easy to spot in the remote list and trivial to clean up if we ever
    /// need to. Pin the format so a future refactor that swaps the prefix
    /// surfaces as a test failure rather than a silent rename in user
    /// worktrees.
    #[test]
    fn fork_remote_alias_uses_fork_prefix() {
        assert_eq!(fork_remote_alias("alice"), "fork-alice");
        assert_eq!(fork_remote_alias("alondero"), "fork-alondero");
    }

    /// Build a bare "fork" repo (a real local clone target so the test
    /// doesn't need a network round-trip) and a regular repo that will
    /// register the fork as a remote. The fork has a single commit on
    /// `main` plus a `feat/443-fork` branch so the fetch can target a
    /// non-default ref. Returns `(local, fork_bare_dir, fork_path)` —
    /// the caller holds the dirs for the duration of the test.
    fn init_fork_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        // Source: a regular repo with a feature branch we can fetch.
        let src = TempDir::new().unwrap();
        let src_path = src.path().to_path_buf();
        let src_repo = git2::Repository::init(&src_path).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        std::fs::write(src_path.join("README.md"), "fork-source\n").unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(std::path::Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_oid).unwrap();
        let main_commit = src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        // Branch off a feature branch.
        let feat_commit = src_repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "feat: fork-only commit",
            &tree,
            &[&src_repo.find_commit(main_commit).unwrap()],
        )
        .unwrap();
        let _ = tree;
        // `main_commit` is a `git2::Oid` (Copy) — no need to `drop` it; the
        // explicit `drop()` was a no-op flagged by clippy.
        let feat_commit = src_repo.find_commit(feat_commit).unwrap();
        src_repo
            .branch("feat/443-fork", &feat_commit, true)
            .unwrap();
        // Bare clone target (so the fork has no working tree, like a real
        // remote on GitHub — `git fetch` reads its objects directly).
        // Use a unique, path-safe name — avoid `{:?}` on the source path
        // (it produces `C:\...` with backslashes and quotes that don't
        // round-trip as a directory name on Windows).
        let bare_dir = std::env::temp_dir().join(format!(
            "buildmesh_fork_bare_{}_{}",
            std::process::id(),
            NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        let _ = std::fs::remove_dir_all(&bare_dir);
        let clone = git2::Repository::init_bare(&bare_dir).unwrap();
        let mut remote = clone.remote("origin", src_path.to_str().unwrap()).unwrap();
        remote
            .fetch(&["refs/heads/*:refs/heads/*"], None, None)
            .unwrap();
        // Local: a fresh repo with no remotes — this is what
        // `fetch_fork_head` will register the fork on.
        let local = TempDir::new().unwrap();
        git2::Repository::init(local.path()).unwrap();
        (local, bare_dir, src_path)
    }

    /// Atomic counter for unique bare-repo paths (one per test run).
    static NEXT_FORK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// First-time registration: the fork is added as `fork-alice` and the
    /// head ref is materialised. `fetch_fork_head` returns `true` and
    /// the resulting `git ls-remote` shows the ref under the alias.
    /// This is the end-to-end "fork spawn" path that issue #443 opens up.
    #[test]
    fn fetch_fork_head_registers_remote_and_fetches_ref() {
        let (local, bare_dir, _src) = init_fork_fixture();
        let bare_dir_str = bare_dir.to_str().unwrap().to_string();

        let ok = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &bare_dir_str,
            "feat/443-fork",
        );
        assert!(ok, "fetch_fork_head must succeed on a real bare repo");

        // Verify the alias + URL are registered.
        let local_repo = git2::Repository::open(local.path()).unwrap();
        let remote = local_repo
            .find_remote("fork-alice")
            .expect("fork-alice remote must be registered");
        let url = remote.url().expect("remote URL must be set");
        assert_eq!(url, bare_dir_str, "remote URL must match the fork's clone URL");

        // Verify the ref was fetched — it should be visible as
        // `fork-alice/feat/443-fork`.
        let reference = local_repo
            .find_reference("refs/remotes/fork-alice/feat/443-fork")
            .expect("fetched ref must be present under fork-alice/");
        assert!(reference.target().is_some(), "ref target must be a real OID");
    }

    /// Idempotent: a second call on a repo that already has the remote
    /// registered AND the right URL is a no-op. The user can spawn a
    /// second agent on the same fork PR (e.g. after closing the first)
    /// without `git remote add` failing. The function still returns
    /// `true` because the fetch succeeds.
    #[test]
    fn fetch_fork_head_is_idempotent_on_repeat_call() {
        let (local, bare_dir, _src) = init_fork_fixture();
        let bare_dir_str = bare_dir.to_str().unwrap().to_string();

        let first = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &bare_dir_str,
            "feat/443-fork",
        );
        assert!(first, "first call must succeed");

        // Second call with the SAME URL — must not error (the `remote add`
        // path is the failure-prone one without the existence check; the
        // `get-url` probe should return the right URL and skip the add).
        let second = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &bare_dir_str,
            "feat/443-fork",
        );
        assert!(second, "second call must still succeed (idempotent)");

        // Remote is still there, single entry.
        let local_repo = git2::Repository::open(local.path()).unwrap();
        let remote = local_repo
            .find_remote("fork-alice")
            .expect("fork-alice remote must still be registered after repeat call");
        assert_eq!(remote.url().unwrap(), bare_dir_str);
    }

    /// URL drift: if the fork's clone URL changes between spawns (the
    /// user renamed the repo, or — more likely — the first call stored a
    /// stale URL), the second call should update the existing remote's
    /// URL via `git remote set-url` rather than fail or keep the stale
    /// URL. Pin this so a future refactor that skips the set-url branch
    /// surfaces as a test failure (the second call would silently fetch
    /// the wrong ref).
    #[test]
    fn fetch_fork_head_updates_url_on_drift() {
        let (local, bare_dir, _src) = init_fork_fixture();
        let stale_url = bare_dir.to_str().unwrap().to_string();
        // Reuse the SAME bare dir (so the second call still finds a real
        // repo) but pretend the URL "drifted" by passing a different
        // string that ALSO resolves to the same on-disk repo. We achieve
        // that with a file:// URL on Windows (path with backslashes
        // round-trip cleanly through git remote add).
        let drifted_url = format!("file://{}", stale_url.replace('\\', "/"));

        // First call: register the stale URL.
        let first = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &stale_url,
            "feat/443-fork",
        );
        assert!(first, "first call must succeed");

        // Second call: same alias, drifted URL — the function should run
        // `git remote set-url` and re-fetch.
        let second = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &drifted_url,
            "feat/443-fork",
        );
        assert!(second, "second call must still succeed after URL drift");

        // The stored URL must be the drifted one, not the original.
        let local_repo = git2::Repository::open(local.path()).unwrap();
        let remote = local_repo
            .find_remote("fork-alice")
            .expect("remote must still be registered");
        let stored = remote.url().unwrap();
        // git normalises file:// URLs slightly on Windows — assert it's
        // the drifted one rather than the original.
        assert_ne!(
            stored, stale_url,
            "URL must have been updated, not left at the stale value"
        );
    }

    /// Failure path: a non-existent clone URL must return `false` rather
    /// than panic. The caller (`spawn_agent_inner`) falls back to the
    /// mesh's `base_ref` and emits a `mesh-sync-warning` toast with
    /// `outcome: "pr_fork_unfetchable"`. Without the failure-as-false
    /// contract, a typo'd clone URL would either spawn on the wrong
    /// commits silently or surface as a hard error every offline session.
    #[test]
    fn fetch_fork_head_returns_false_on_bad_clone_url() {
        let (local, _bare_dir, _src) = init_fork_fixture();
        let bad_url = "/nonexistent/path/to/fork/that/does/not/exist".to_string();

        let ok = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &bad_url,
            "feat/443-fork",
        );
        assert!(!ok, "fetch_fork_head must return false on a bad clone URL");
    }

    // ----- fetch_single_ref (issue #420) ---------------------------------
    //
    // Same-repo PR spawn (#420) — the worktree adoption path calls
    // `fetch_single_ref` to materialise `origin/<head_ref>` so the worktree
    // can be cut from it. As of issue #446 the function is a thin wrapper
    // over `git::sync::do_fetch_only` (the fetch-only half of `do_sync` —
    // open + dirty-check + has-remote + `git fetch`, NO `git pull` tail);
    // the `-`-adversarial-ref hardening is preserved at the wrapper
    // boundary because `do_fetch_only` passes the branch as a plain argv
    // entry without a `--` separator (it doesn't know about the spawn
    // context).
    //
    // These tests pin the cases the issue calls out:
    //   1. success — ref exists on origin
    //   2. ref-not-found — ref missing on origin (caller falls back to base_ref)
    //   3. non-git path — caller passed a directory that isn't a repo
    //   4. adversarial ref — `-`-prefixed input is rejected by the wrapper
    //      before `do_fetch_only` sees it (the hardening migrated from the
    //      shell-out's `--` separator to an upfront string check, since
    //      `do_fetch_only` doesn't pass a `--` separator to `git fetch`)
    //   5. dirty-skip (issue #446 acceptance #2) — a parent repo with
    //      uncommitted changes must return `false` (mirrors
    //      `fetch_origin_skips_dirty_parent` in `git/fetch_origin_tests.rs`)
    //
    // The fixture mirrors `init_fork_fixture` but for the same-repo path:
    // a bare repo holds a single branch, the local repo has `origin`
    // pointed at the bare, and the test calls `fetch_single_ref` against
    // the local repo's path.

    /// Build a "remote + local" pair: the bare repo has a single commit on
    /// `main` plus a `feat/420-pr-spawn` branch; the local repo has `origin`
    /// pointed at the bare. Returns `(local, bare_path)` — the local TempDir
    /// owns its on-disk path; `bare_path` is a plain PathBuf that lives
    /// inside `std::env::temp_dir()` and is reused across calls (it gets
    /// re-populated with the same content each time, so the SHA is stable
    /// per-test-process).
    fn init_same_repo_fixture() -> (TempDir, std::path::PathBuf) {
        // Source: a working repo with a feature branch we can fetch.
        // We reuse the same on-disk source across tests in a single
        // process — `init_same_repo_fixture` is only called from the
        // same-repo tests below, and the contents are deterministic.
        static SRC_DIR: std::sync::OnceLock<std::path::PathBuf> =
            std::sync::OnceLock::new();
        let src_path = SRC_DIR
            .get_or_init(|| {
                let src = TempDir::new().unwrap();
                let src_path = src.path().to_path_buf();
                let src_repo = git2::Repository::init(&src_path).unwrap();
                let sig = git2::Signature::now("test", "test@example.com").unwrap();
                std::fs::write(src_path.join("README.md"), "init\n").unwrap();
                let mut index = src_repo.index().unwrap();
                index.add_path(std::path::Path::new("README.md")).unwrap();
                index.write().unwrap();
                let tree_oid = index.write_tree().unwrap();
                let tree = src_repo.find_tree(tree_oid).unwrap();
                let main_commit = src_repo
                    .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                    .unwrap();
                let main_commit_obj = src_repo.find_commit(main_commit).unwrap();
                src_repo
                    .branch("feat/420-pr-spawn", &main_commit_obj, true)
                    .unwrap();
                // Leak the TempDir guard — we want src_path to stay alive
                // for the whole process, and the bare-fetch step below
                // re-reads from the on-disk path on every test.
                std::mem::forget(src);
                src_path
            })
            .clone();

        // Bare remote — same pattern as `init_fork_fixture`. A unique
        // name per process so parallel `cargo test` invocations don't
        // collide on the bare dir.
        let bare_dir = std::env::temp_dir().join(format!(
            "buildmesh_same_repo_bare_{}_{}",
            std::process::id(),
            NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        let _ = std::fs::remove_dir_all(&bare_dir);
        let clone = git2::Repository::init_bare(&bare_dir).unwrap();
        let mut remote = clone
            .remote("origin", src_path.to_str().unwrap())
            .unwrap();
        remote
            .fetch(&["refs/heads/*:refs/heads/*"], None, None)
            .unwrap();

        // Local repo with `origin` pointed at the bare. `fetch_single_ref`
        // will use this `origin` remote to materialise the ref.
        let local = TempDir::new().unwrap();
        let local_repo = git2::Repository::init(local.path()).unwrap();
        local_repo
            .remote("origin", bare_dir.to_str().unwrap())
            .unwrap();
        (local, bare_dir)
    }

    /// Success path: a ref that exists on `origin` is fetched into
    /// `refs/remotes/origin/<head_ref>` and the function returns `true`.
    /// This is the happy path the spawn-time worktree adoption relies on.
    #[test]
    fn fetch_single_ref_returns_true_when_ref_exists() {
        let (local, _bare) = init_same_repo_fixture();
        let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
        assert!(
            ok,
            "fetch_single_ref must return true when the ref exists on origin"
        );
        // Verify the ref actually got materialised — a true return with no
        // visible ref would mean a silent no-op, which is a worse failure
        // mode than a hard error.
        let local_repo = git2::Repository::open(local.path()).unwrap();
        let reference = local_repo
            .find_reference("refs/remotes/origin/feat/420-pr-spawn")
            .expect("origin/feat/420-pr-spawn must be materialised after success");
        assert!(
            reference.target().is_some(),
            "fetched ref must point at a real OID, not be unborn"
        );
    }

    /// Ref-not-found path: a ref that does NOT exist on `origin` causes
    /// `git fetch` to exit non-zero. The function returns `false` (not
    /// an error) so the spawn path can fall back to the mesh's
    /// `base_ref` — this is the ADR 0001 offline pattern, surface as
    /// `pr_head_unfetchable` rather than failing the spawn.
    #[test]
    fn fetch_single_ref_returns_false_when_ref_missing() {
        let (local, _bare) = init_same_repo_fixture();
        let ok = fetch_single_ref(local.path().to_str().unwrap(), "does-not-exist");
        assert!(
            !ok,
            "fetch_single_ref must return false when the ref is missing on origin \
             (caller falls back to base_ref per the offline-fallback contract)"
        );
    }

    /// Non-git path: a directory that isn't a git repo at all. `git fetch`
    /// errors immediately; the function swallows that and returns `false`.
    /// This is the "user has a partial / broken clone" edge case — the
    /// spawn must not panic.
    #[test]
    fn fetch_single_ref_returns_false_for_non_git_directory() {
        let tmp = TempDir::new().unwrap();
        let ok = fetch_single_ref(tmp.path().to_str().unwrap(), "feat/420-pr-spawn");
        assert!(
            !ok,
            "fetch_single_ref must return false (not panic) for a non-git path"
        );
    }

    /// Adversarial-ref pin (issue #420 hardening): a ref starting with `-`
    /// (e.g. `--upload-pack=evil`) is rejected by `git` itself because of
    /// the `--` separator before `head_ref`. Without the separator, `git`
    /// would parse `--upload-pack=evil` as a flag and use it for the
    /// fetch — a vector for arbitrary command execution on a malicious
    /// server (CVE-2017-1000117 / CVE-2018-17456 class). The hardening
    /// lives in `fetch_single_ref`; this test pins the contract so a
    /// future refactor that drops the `--` separator fails the test
    /// rather than silently re-introducing the vulnerability.
    ///
    /// We pass a ref that, WITHOUT the separator, `git` would parse as a
    /// flag (`--upload-pack=evil`) — `git fetch` will then error out on
    /// "fatal: bad config name", proving the separator did its job. With
    /// the separator, the value reaches the ref-spec parser as a
    /// literal ref name (which still doesn't exist on origin, so the
    /// call returns `false` either way — the contract is "the function
    /// returns false rather than letting `--upload-pack` reach git").
    #[test]
    fn fetch_single_ref_rejects_adversarial_dash_ref() {
        let (local, _bare) = init_same_repo_fixture();
        let ok = fetch_single_ref(local.path().to_str().unwrap(), "--upload-pack=evil");
        assert!(
            !ok,
            "fetch_single_ref must return false for a ref starting with '-' \
             (the wrapper rejects it before do_sync sees it)"
        );
    }

    /// Dirty-parent pin (issue #446 acceptance #2, inverted 2026-07-17): a
    /// parent repo with uncommitted changes must STILL fetch the PR head.
    /// A `git fetch` never touches the working tree — the pre-2026-07-17
    /// dirty-skip meant a mesh whose root checkout stayed dirty silently
    /// fell back to `base_ref` on every PR spawn, cutting the worktree
    /// from the wrong commits. Pin the new contract so a future refactor
    /// that re-introduces a pre-fetch dirty gate fails this test.
    ///
    /// `is_dirty` includes untracked files, so writing one to the freshly-
    /// init'd local repo is enough to dirty it — no need to seed a tracked
    /// file first.
    #[test]
    fn fetch_single_ref_fetches_despite_dirty_parent() {
        let (local, _bare) = init_same_repo_fixture();
        // Precondition: the fixture's local repo must start clean, then we
        // make it dirty with an untracked file.
        assert!(
            !crate::env::test_helpers::repo_is_dirty(local.path()),
            "precondition: freshly-init'd local repo must start clean"
        );
        std::fs::write(local.path().join("dirty-marker.txt"), "uncommitted\n").unwrap();
        assert!(
            crate::env::test_helpers::repo_is_dirty(local.path()),
            "precondition: writing an untracked file must dirty the repo"
        );

        let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
        assert!(
            ok,
            "fetch_single_ref must fetch on a dirty parent — a fetch never \
             touches the working tree, and skipping cut PR worktrees from \
             stale refs"
        );
        // The head ref must be materialised so the worktree can be cut
        // from it — the whole point of the fetch.
        let repo = git2::Repository::open(local.path()).unwrap();
        assert!(
            repo.find_reference("refs/remotes/origin/feat/420-pr-spawn")
                .is_ok(),
            "the fetch must materialise refs/remotes/origin/<head_ref>"
        );
        // And the dirty marker must be untouched.
        assert_eq!(
            std::fs::read_to_string(local.path().join("dirty-marker.txt")).unwrap(),
            "uncommitted\n"
        );
    }

    // -----------------------------------------------------------------------
    // locked_fetch_pr_head — per-Mesh sync_lock wrap (issue #698)
    //
    // `locked_fetch_pr_head` must run inside `services::sync_lock::with_mesh_
    // sync_lock` so two concurrent PR-spawns (or a PR-spawn racing the manual
    // `git_sync` from #680 / the spawn-time `fetch_origin` from #652) can't
    // collide on `.git/FETCH_HEAD` / `.git/refs/remotes/<remote>/<ref>.lock`.
    // Without the wrap the losing fetch fails with "another git process" and
    // the spawn silently lands on `base_ref` (the wrong commits).
    //
    // We test the wrap with a wall-clock bound (mirroring the #680
    // `git_sync_serializes_via_per_mesh_sync_lock_gh680` shape in
    // `commands/git_tests.rs`). The `with_mesh_sync_lock` unit tests in
    // `services::sync_lock` prove the primitive itself serialises; this test
    // proves THIS specific call site uses the SAME key the spawn path uses,
    // which is the bug class #698 closes.
    //
    // Holder enters the per-mesh lock and announces entry via an AtomicUsize
    // flag before sleeping. Main thread spin-waits on the flag (deterministic
    // — no `thread::sleep` race), then times `locked_fetch_pr_head`. With the
    // wrap, `locked_fetch_pr_head` blocks ~450 ms waiting for the holder;
    // without, it runs concurrently with the holder and finishes in tens of ms.
    // -----------------------------------------------------------------------

    /// Regression test for issue #698 — `locked_fetch_pr_head` must acquire
    /// the per-Mesh `with_mesh_sync_lock` keyed on the spawn's `node.path`,
    /// matching what `spawn_agent_inner` calls `fetch_origin` with two steps
    /// earlier. Without this wrap, concurrent PR-spawns on the same Mesh
    /// (and a PR-spawn racing the manual `git_sync` button) race on
    /// `.git/FETCH_HEAD` / `refs/remotes/<remote>/<ref>.lock` and the loser
    /// silently falls back to `base_ref`.
    ///
    /// Strategy: holder thread enters `with_mesh_sync_lock(&path_key, ...)`
    /// and announces via an AtomicUsize flag, then sleeps. Main thread
    /// spin-waits on the flag (deterministic — no `thread::sleep` race), then
    /// times `locked_fetch_pr_head`. With the wrap, `locked_fetch_pr_head`
    /// blocks waiting for the holder; without, it returns immediately while
    /// the holder is still inside its critical section.
    ///
    /// Why wall-clock (not `fetch_add`): the per-Mesh lock is correctly
    /// implemented (issue #652 + `services::sync_lock` unit tests prove it),
    /// so it *prevents* simultaneous critical-section entries — `max_concurrent
    /// == 1` even on a working lock. The only signal that `locked_fetch_pr_head`
    /// shares the same key is that it waits for the holder to release the lock.
    ///
    /// The test uses the same-repo branch (passes `None, None` for fork
    /// fields). The fork branch shares the same wrapper so the regression
    /// coverage is sufficient with one call site — a #698 regression that
    /// branched out of the wrapper entirely would fail this test and the
    /// #443 fork tests would still pass on the unwrapped helper, surfacing
    /// the gap.
    #[test]
    fn locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698() {
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        use std::time::{Duration, Instant};

        let (local, _bare) = init_same_repo_fixture();
        let path_key = local.path().to_string_lossy().into_owned();

        // Holder enters the per-mesh lock and announces entry via
        // `entered_flag` before sleeping. Spinning on the flag avoids the
        // `thread::sleep` race — CI jitter can't make `locked_fetch_pr_head`
        // sneak in first.
        let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
        let holder_path = path_key.clone();
        let entered_holder = std::sync::Arc::clone(&entered_flag);
        let holder = std::thread::spawn(move || {
            crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
                entered_holder.store(1, AOrdering::SeqCst);
                std::thread::sleep(Duration::from_millis(500));
            });
        });

        // Spin-wait (bounded) for the holder to actually be inside the
        // critical section. Cap at 2 s so a hung holder surfaces as a
        // test panic, not a forever-wait.
        let deadline = Instant::now() + Duration::from_secs(2);
        while entered_flag.load(AOrdering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "holder thread never entered the per-mesh lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        let start = Instant::now();
        let _ = locked_fetch_pr_head(&path_key, "feat/420-pr-spawn", None, None);
        let elapsed = start.elapsed();

        holder.join().unwrap();

        // With wrap: elapsed >= ~450 ms (`locked_fetch_pr_head` waited for
        // the holder). Without wrap: elapsed = tens of ms (the fetch ran
        // concurrently with the holder's sleep). Bound is 400 ms — leaves
        // 100 ms of slack for setup overhead and CI jitter on a busy box.
        assert!(
            elapsed >= Duration::from_millis(400),
            "locked_fetch_pr_head did not block on the per-mesh lock \
             (elapsed = {:?}); issue #698 wrap is missing — concurrent PR-spawn \
             and spawn-time fetch_origin (or manual git_sync from #680) would \
             race on .git/FETCH_HEAD and refs/remotes/<remote>/<ref>.lock",
            elapsed,
        );
    }

    /// Companion to `locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698`
    /// — exercises the FORK branch (`Some/Some` → `fetch_fork_head`) of the
    /// wrapper. The same-repo test alone leaves a CI blind spot: a #698
    /// regression that bypassed the wrapper for fork PRs (e.g. an inlined
    /// `fetch_fork_head` call in `spawn_agent_inner` to skip the remote-
    /// config lock acquisition) would still pass the same-repo test and
    /// every existing #443 fork unit test (those hit the bare helper
    /// directly, no lock). This test closes the gap by hitting the fork
    /// arm of the wrapper with the same wall-clock shape; its `git remote
    /// add` then `git fetch` sequence MUST hold the lock for the holder's
    /// 500 ms sleep.
    #[test]
    fn locked_fetch_pr_head_serializes_fork_branch_via_per_mesh_sync_lock_gh698() {
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        use std::time::{Duration, Instant};

        let (local, bare_dir, _src) = init_fork_fixture();
        let bare_dir_str = bare_dir.to_str().unwrap().to_string();
        let path_key = local.path().to_string_lossy().into_owned();

        // Holder enters the per-mesh lock (same key as the wrapper) and
        // announces via `entered_flag` before sleeping.
        let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
        let holder_path = path_key.clone();
        let entered_holder = std::sync::Arc::clone(&entered_flag);
        let holder = std::thread::spawn(move || {
            crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
                entered_holder.store(1, AOrdering::SeqCst);
                std::thread::sleep(Duration::from_millis(500));
            });
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while entered_flag.load(AOrdering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "holder thread never entered the per-mesh lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        let start = Instant::now();
        let _ = locked_fetch_pr_head(
            &path_key,
            "feat/443-fork",
            Some("alice"),
            Some(&bare_dir_str),
        );
        let elapsed = start.elapsed();

        holder.join().unwrap();

        assert!(
            elapsed >= Duration::from_millis(400),
            "locked_fetch_pr_head (fork branch) did not block on the per-mesh \
             lock (elapsed = {:?}); issue #698 wrap is missing for the fork path \
             — concurrent fork-PR spawns would race on .git/FETCH_HEAD, \
             refs/remotes/fork-<login>/<ref>.lock, AND the git remote add/config \
             files that fetch_fork_head writes before its fetch",
            elapsed,
        );
    }

    // -----------------------------------------------------------------------
    // Reader-thread session-id capture gate (issue #651)
    //
    // The orchestrator's pre-write at spawn_agent_inner (Assign mode) and the
    // PTY reader thread's capture-from-output path both target the same
    // `agent_nodes.cli_session_id` column. They are unsynchronised, so a
    // last-writer-wins race left the row holding a UUID the agent never
    // claimed — and auto-resume later invoked `claude --resume <wrong-uuid>`
    // → "Conversation not found". The fix pins the gate to a single function
    // of `session_id_mode` (the source of truth) so the two writers can never
    // both target the same column. Each test pins one row of the truth table;
    // the regression test is the `Assign(_)` row.
    // -----------------------------------------------------------------------

    /// Regression for issue #651. Even if a future adapter returns
    /// `self_assigns_session_id() = true`, the reader thread MUST NOT capture
    /// when the orchestrator is in Assign mode — the orchestrator already
    /// wrote a UUID at `spawn_agent_inner` step 4, and the reader would
    /// overwrite it with whatever UUID matched the regex on PTY output
    /// (possibly a different log line, possibly never echoed back).
    #[test]
    fn reader_should_not_capture_in_assign_mode_even_if_provider_self_assigns() {
        assert!(
            !reader_should_capture_session_id(
                &SessionIdMode::Assign("orchestrator-uuid".into()),
                true,
            ),
            "Assign mode is authoritative — reader MUST NOT overwrite the \
             orchestrator's pre-written UUID with a regex match from PTY output \
             (issue #651: 'a UUID the agent never claimed')"
        );
    }

    /// Resume already has the authoritative ID stored in `cli_session_id`
    /// (or, for fresh `--resume` calls, the resume arg passed to the CLI).
    /// Capture would race the in-flight `claude --resume <id>` with a
    /// possibly-different UUID from the regex, so the reader must stay quiet.
    #[test]
    fn reader_should_not_capture_in_resume_mode() {
        assert!(
            !reader_should_capture_session_id(
                &SessionIdMode::Resume("resume-uuid".into()),
                true,
            ),
            "Resume mode carries the authoritative ID; reader MUST NOT capture"
        );
    }

    /// `None` mode is the only mode where reader capture is allowed — and only
    /// for providers that print a labeled UUID on the PTY (Codex, Agy).
    /// OpenCode self-assigns `ses_…` IDs but captures them in
    /// `after_fresh_spawn` (SQLite), so its PTY-capture flag is false.
    #[test]
    fn reader_should_capture_when_provider_self_assigns_and_mode_is_none() {
        assert!(
            reader_should_capture_session_id(&SessionIdMode::None, true),
            "Codex / Agy fresh spawns rely on the reader capturing the UUID \
             from PTY output (orchestrator has no pre-write in None mode)"
        );
    }

    /// Self-assigning capability is necessary but not sufficient — if the
    /// provider accepts `--session-id` (Anthropic) or captures in
    /// `after_fresh_spawn` (OpenCode), the PTY regex is not the source of
    /// truth even when the orchestrator didn't pre-write.
    #[test]
    fn reader_should_not_capture_when_provider_does_not_self_assign() {
        assert!(
            !reader_should_capture_session_id(&SessionIdMode::None, false),
            "reader MUST NOT capture when provider does not self-assign; \
             any UUID match would overwrite the existing cli_session_id"
        );
    }

    /// Issue #1180 — `SpawnIntent::initial_prompt` is the single source
    /// of truth for the GitHub-issue prefill. The spawn seam (`spawn_with_intent`)
    /// routes through it; so does the desktop draft response and the
    /// Autopilot watcher. Pin the wording here so any future drift would
    /// surface as a unit-test failure before the agent gets the wrong
    /// prompt.
    #[test]
    fn issue_intent_builds_its_prefill_at_the_spawn_seam() {
        let intent = SpawnIntent::Issue(GitHubWorkContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 247,
            title: "Deepen spawn pipeline".into(),
        });

        assert_eq!(
            intent
                .initial_prompt()
                .as_ref()
                .map(intent::InitialPrompt::as_str),
            Some(
                "Please work on GitHub issue #247 — Deepen spawn pipeline\n\
https://github.com/alondero/buildmesh/issues/247"
            )
        );
    }

    // -----------------------------------------------------------------------
    // Resume-skip decision surface (issue #949 regression).
    //
    // Pins the PR #1121 fix: when a Startup resume is not viable, the
    // caller must NOT write `Idle` to `agent_nodes.status` — the node
    // stays `Suspended` so the user's Resume / Regenerate affordances
    // remain reachable. `decide_startup_resume` is the single source of
    // truth for that contract; `spawn_with_intent`'s Skip arms call no
    // sink. A future refactor that re-introduces an `on_idle` write here
    // fails review by virtue of the decision being a single enum variant.
    // -----------------------------------------------------------------------

    #[test]
    fn decide_startup_resume_no_session_id_is_skipped() {
        let d = decide_startup_resume(None, ResumeCause::Startup, true);
        assert_eq!(d, ResumeSkipDecision::SkipSuspended);
    }

    #[test]
    fn decide_startup_resume_empty_session_id_is_skipped() {
        // Empty-string defense — `db::list_suspended_nodes`'s SQL filter
        // only catches NULL; legacy writes could leave an empty string
        // behind, so the empty case must be filtered here.
        let d = decide_startup_resume(Some(""), ResumeCause::Startup, true);
        assert_eq!(d, ResumeSkipDecision::SkipSuspended);
    }

    #[test]
    fn decide_startup_resume_when_adapter_declines_is_skipped() {
        let d = decide_startup_resume(
            Some("uuid"),
            ResumeCause::Startup,
            false, // OpenCode, Terminal — no --resume flag, no auto-resume
        );
        assert_eq!(
            d,
            ResumeSkipDecision::SkipAdapterDeclines,
            "OpenCode/Terminal Startup resume must skip without writing Idle"
        );
    }

    #[test]
    fn decide_startup_resume_explicit_no_session_id_is_an_error() {
        // User clicked Resume on a node that never captured a session id.
        // This is a hard error — surfacing it is the user-driven recovery
        // path; the orchestrator-side Startup path silently skips.
        let d = decide_startup_resume(None, ResumeCause::Explicit, true);
        assert_eq!(d, ResumeSkipDecision::NoSessionId);
    }

    #[test]
    fn decide_startup_resume_explicit_with_session_id_proceeds() {
        let d = decide_startup_resume(
            Some("uuid-7"),
            ResumeCause::Explicit,
            false, // explicit cause is unaffected by auto_resume_on_startup
        );
        assert_eq!(d, ResumeSkipDecision::Proceed("uuid-7".to_string()));
    }

    #[test]
    fn decide_startup_resume_startup_with_session_id_and_adapter_accepts_proceeds() {
        let d = decide_startup_resume(Some("uuid-7"), ResumeCause::Startup, true);
        assert_eq!(d, ResumeSkipDecision::Proceed("uuid-7".to_string()));
    }

    // -----------------------------------------------------------------
    // Issue #1179: capability / recipe coherence table.
    //
    // For every adapter × every session mode × every value the resolver
    // might forward, the prepared recipe must contain exactly the flags
    // the capability descriptor advertises. The single test below drives
    // the full matrix; per-adapter adapter-level tests continue to pin
    // the arg shapes directly via `*_args` helpers.
    // -----------------------------------------------------------------

    fn make_input<'a>(
        platform: Platform,
        session: SessionIdModeRef<'a>,
        config: &'a ResolvedAgentConfig,
        prefill: Option<&'a str>,
    ) -> HarnessLaunchInput<'a> {
        HarnessLaunchInput {
            platform,
            runtime: EnvType::Windows,
            session,
            config,
            prefill,
            sandbox: false,
        }
    }

    /// Coherence pin (issue #1179): for every adapter, the
    /// `HarnessCapabilities` descriptor and the recipe produced by
    /// `default_prepare` agree.
    ///
    /// 1. The recipe's model-flag presence (the flag name from
    ///    `adapter.model_args(m).first()`) matches
    ///    `caps.supports_model_override`. Kimi uses `-m`, anthropic /
    ///    codex / grok / agy / cursor use `--model`, mcode uses nothing.
    /// 2. The recipe's effort-flag presence (matched by
    ///    `caps.effort_control` shape: `Closed => "--effort"`,
    ///    `InlineConfig => key prefix`, `None => neither`) matches
    ///    `caps.effort_control != None`.
    /// 3. The recipe's prefill marker (trailing positional, `--prefill`,
    ///    or `--prompt-interactive`) matches `caps.supports_prefill`.
    #[test]
    fn capability_recipe_coherence() {
        let mut any_adapters = 0;
        for provider in crate::models::Provider::all() {
            let adapter = provider.adapter();
            let caps = adapter.capabilities();
            any_adapters += 1;

            // Build a config where every layer is populated, then verify
            // the recipe only carries what caps allow. Ask the adapter
            // itself for its model-flag shape — some harnesses use
            // short forms (Kimi `-m`) or vendor-specific names; the
            // adapter owns its flag vocabulary.
            let model_value = match adapter.id() {
                // mcode's `model` slot is no longer advertised; pick a
                // plausible value to attempt smuggling it past the mask.
                "mcode" => "minimax/MiniMax-Text-01",
                "codex" => "gpt-4o",
                "kimi" => "kimi-k2",
                "grok" => "grok-3",
                "agy" => "claude-sonnet",
                "cursor" => "claude-3-7-sonnet",
                "opencode" => "anthropic/claude-sonnet-4-5",
                "anthropic" => "claude-sonnet-4-5",
                "terminal" => "irrelevant",
                _ => "model",
            };
            let effort_value = match adapter.id() {
                "anthropic" => "high",
                "codex" => "xhigh",
                _ => "high", // other harnesses don't accept effort
            };
            let config = ResolvedAgentConfig {
                model: Some(model_value.to_string()),
                effort: Some(effort_value.to_string()),
                extra_args: None,
            };
            let prefill_text = "fix the auth bug in handler.rs";
            let input = make_input(
                Platform::Linux,
                SessionIdModeRef::None,
                &config,
                Some(prefill_text),
            );
            let prepared = crate::agent::launch::default_prepare(adapter, input);
            let args = &prepared.recipe.base_args;

            // 1. Model-flag coherence. Ask the adapter what its model-flag
            //    shape is; the recipe must contain it iff caps advertises
            //    the control. mcode (which used to advertise) now does
            //    not, so the recipe must not carry `--model` even when
            //    a value is in the resolved config.
            let model_flag = adapter
                .model_args(model_value)
                .first()
                .cloned()
                .unwrap_or_default();
            let has_model_flag = !model_flag.is_empty()
                && args.iter().any(|a| a == &model_flag);
            assert_eq!(
                has_model_flag, caps.supports_model_override,
                "model-flag / supports_model_override mismatch for {}: \
                 recipe has {} = {}, caps.supports_model_override = {}; args = {:?}",
                adapter.id(), model_flag, has_model_flag, caps.supports_model_override, args
            );

            // 2. Effort-flag coherence. Codex uses -c model_reasoning_effort=...;
            //    anthropic uses --effort; everything else must not carry either.
            //    Pin by `caps.effort_control` shape: Closed => "--effort";
            //    InlineConfig => the configured key prefix; None => neither.
            let has_effort_flag = match &caps.effort_control {
                crate::agent::capabilities::EffortControlKind::Closed { .. } => {
                    args.iter().any(|a| a == "--effort")
                }
                crate::agent::capabilities::EffortControlKind::InlineConfig { key, .. } => {
                    args.iter().any(|a| a.starts_with(key))
                }
                crate::agent::capabilities::EffortControlKind::None => false,
            };
            let has_effort_vocab =
                !matches!(caps.effort_control, crate::agent::capabilities::EffortControlKind::None);
            assert_eq!(
                has_effort_flag, has_effort_vocab,
                "effort-flag / effort_control mismatch for {}: \
                 recipe has effort flag = {}, caps.effort_control != None = {}; args = {:?}",
                adapter.id(), has_effort_flag, has_effort_vocab, args
            );

            // 3. Prefill coherence.
            let has_prefill_text = args.last().map(|a| a.as_str()) == Some(prefill_text);
            let has_prefill_flag = args.iter().any(|a| a == "--prefill");
            let has_prefill_marker = has_prefill_text || has_prefill_flag
                || args.iter().any(|a| a == "--prompt-interactive")
                || args.iter().any(|a| a == "--prompt");
            assert_eq!(
                has_prefill_marker, caps.supports_prefill,
                "prefill-marker / supports_prefill mismatch for {}: \
                 recipe has prefill marker = {}, caps.supports_prefill = {}; args = {:?}",
                adapter.id(), has_prefill_marker, caps.supports_prefill, args
            );

            // 4. Sandbox-flag coherence (issue #1287). The orchestrator's
            //    outer containment (macOS Seatbelt / Windows restricted-
            //    token) applies uniformly regardless of adapter; the
            //    adapter-level flag only applies when the adapter itself
            //    declared a `sandbox_args()` contribution. A second pass
            //    with `sandbox: true` must therefore add the flag iff
            //    `adapter.sandbox_args()` is non-empty. Any adapter that
            //    silently starts emitting `--sandbox` (or fails to emit
            //    it after overriding `sandbox_args`) trips this pin.
            let sandbox_input = make_input(
                Platform::Linux,
                SessionIdModeRef::None,
                &config,
                Some(prefill_text),
            );
            let sandbox_input = HarnessLaunchInput { sandbox: true, ..sandbox_input };
            let sandbox_prepared = crate::agent::launch::default_prepare(adapter, sandbox_input);
            let sandbox_args = sandbox_prepared
                .recipe
                .base_args
                .iter()
                .filter(|a| adapter.sandbox_args().contains(a))
                .count();
            let sandbox_vocab = adapter.sandbox_args().len();
            assert_eq!(
                sandbox_args, sandbox_vocab,
                "sandbox-flag / sandbox_args mismatch for {}: \
                 recipe should carry all {} declared sandbox args when sandbox=true, \
                 got {} matches; args = {:?}",
                adapter.id(),
                sandbox_vocab,
                sandbox_args,
                sandbox_prepared.recipe.base_args
            );
        }
        assert!(any_adapters >= 9, "expected at least 9 adapters in the matrix");
    }

    /// Codex's subcommand-style resume is the one recipe shape that
    /// diverges from the default. Pin the recipe contains the
    /// `resume <id>` shape AND not the model's regular flags when the
    /// resume is in play.
    #[test]
    fn codex_resume_recipe_uses_subcommand_shape() {
        let adapter = &crate::agent::provider::adapters::CODEX as &dyn crate::agent::provider::AgentProvider;
        let config = ResolvedAgentConfig::default();
        let input = make_input(
            Platform::Macos,
            SessionIdModeRef::Resume("sess-xyz"),
            &config,
            None,
        );
        let prepared = crate::agent::launch::default_prepare(adapter, input);
        let args = &prepared.recipe.base_args;
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"sess-xyz".to_string()));
        // Codex resume recipe is the subcommand form; no `--resume <id>`
        // flag is appended.
        assert!(!args.contains(&"--resume".to_string()));
    }

    /// Issue #1179 follow-up pin: `mcode` no longer advertises
    /// `supports_model_override`. Even with a value in the resolver
    /// config, the recipe must not contain `--model`.
    #[test]
    fn mcode_recipe_never_carries_model_arg_under_coherence_matrix() {
        let adapter = &crate::agent::provider::adapters::MCODE as &dyn crate::agent::provider::AgentProvider;
        let config = ResolvedAgentConfig {
            model: Some("minimax/MiniMax-Text-01".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = make_input(
            Platform::Macos,
            SessionIdModeRef::None,
            &config,
            Some("check the auth handler"),
        );
        let prepared = crate::agent::launch::default_prepare(adapter, input);
        let args = &prepared.recipe.base_args;
        assert!(
            !args.contains(&"--model".to_string()),
            "mcode recipe must never carry --model; got {:?}",
            args
        );
        assert!(
            args.last().map(|a| a.as_str()) == Some("check the auth handler"),
            "mcode prefill should be the trailing positional, got {:?}",
            args
        );
    }
}