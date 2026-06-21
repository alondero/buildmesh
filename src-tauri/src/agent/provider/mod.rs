//! Agent provider trait — the seam between "which provider" and "how do we spawn it".
//!
//! Each provider (Anthropic, Minimax, Gemini, OpenCode, ...) ships its own adapter
//! module under `adapters/`. The trait describes everything `spawn_agent_inner`
//! needs to know to launch and supervise an agent.

pub mod adapters;
pub mod provider_conf;

use crate::models::EnvType;

/// Build host (compile-time constant via `cfg!(target_os)`).
///
/// Distinct from `EnvType` (Windows vs WSL) which is the *runtime* spawn target.
/// `Platform` is the *host* — it tells the adapter which binary name and flags
/// to emit. `EnvType` is consumed by `spawn_environment` to wrap the resulting
/// command (wsl.exe / powershell / cmd / direct).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Macos
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// Shell to use when wrapping a Windows-native spawn.
/// Codex spawns under PowerShell so ANSI escape sequences propagate
/// correctly through ConPTY; node-shim providers (`.cmd` batch files like
/// OpenCode) use cmd.exe. The Claude-backed `anthropic` adapter (which runs
/// every Claude-compatible endpoint, including MiniMax/Kimi/custom profiles)
/// uses `Direct` now that cwrap is absorbed — see `claude_direct_recipe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsShell {
    PowerShell,
    Cmd,
    /// Spawn the binary directly — used on macOS / Linux / WSL where no wrapping is needed.
    Direct,
}

/// The provider's spawn recipe for a given host platform.
/// `spawn_environment::wrap` consumes this plus an `EnvType` to produce a `CommandBuilder`.
#[derive(Debug, Clone)]
pub struct SpawnRecipe {
    pub binary: &'static str,
    pub base_args: Vec<String>,
    pub windows_shell: WindowsShell,
}

/// The direct `claude` / `claude.exe` invocation used by the Claude-backed
/// `anthropic` adapter on every platform — the one executor behind every
/// Claude-compatible endpoint (the built-in subscription plus custom MiniMax/
/// Kimi/DeepSeek profiles, whose account env is injected separately). cwrap's
/// launcher role is absorbed into buildmesh. On Windows we target `claude.exe`
/// explicitly (the bare `claude` is a bash shim); on macOS/Linux we use the
/// `claude` shell script on PATH. Spawned directly via `spawn_environment::
/// wrap`'s `WindowsShell::Direct` branch — no PowerShell, cmd.exe, or bash
/// wrapper, so the AppContainer restriction that motivated the old sandbox-
/// only seam no longer applies.
pub fn claude_direct_recipe(platform: Platform) -> SpawnRecipe {
    let binary = match platform {
        Platform::Windows => "claude.exe",
        _ => "claude",
    };
    SpawnRecipe {
        binary,
        base_args: vec!["--dangerously-skip-permissions".into()],
        windows_shell: WindowsShell::Direct,
    }
}

/// The backend-selecting environment variables cwrap's launcher `unset` before
/// `exec claude`. Claude-backed providers reset these on every spawn so a value
/// inherited from buildmesh's own process environment (e.g. an `ANTHROPIC_*`
/// override exported in the shell that launched the app) can't leak into the
/// agent — reproducing the clean slate cwrap gave each session. Mirrors the
/// `unset ...` block in `~/.local/bin/cwrap`. See [`AgentProvider::resets_backend_env`].
pub const CLAUDE_BACKEND_ENV_VARS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "API_TIMEOUT_MS",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
];

/// UI metadata declared by an adapter. The `id` is supplied separately via
/// `AgentProvider::id()` so the two can't diverge.
#[derive(Debug, Clone)]
pub struct UiMeta {
    pub label: String,
    pub color: String,
    pub icon: String,
}

/// Frontend-facing provider listing. Composed by `commands::agent::available_providers`
/// purely from the user's dynamic harness profiles (issue #538 retired the
/// legacy enum-backed rows and the `legacy` grouping flag).
///
/// Generated to src/types/generated/ProviderInfo.ts (issue #404). `Deserialize`
/// is added so the type participates in the ts-rs `export` derive (the project
/// pattern is `Serialize + Deserialize + TS` for every generated wire type).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "ProviderInfo.ts")]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub color: String,
    pub icon: String,
}

/// Behaviour an agent provider must declare.
///
/// Implementations live as zero-sized structs under `adapters/`, exposed as
/// `&'static dyn AgentProvider` via `Provider::adapter()`.
pub trait AgentProvider: Send + Sync {
    /// Stable identifier matching the DB `provider` column ("anthropic", "minimax", etc.).
    fn id(&self) -> &'static str;

    /// UI metadata shown in the frontend provider list. The `id` is *not* part
    /// of this — it comes from `id()` to avoid stringly-typed duplication.
    fn ui(&self) -> UiMeta;

    /// How to invoke this provider on the given host platform, for a node
    /// running in the given runtime environment. The `env_type` parameter
    /// lets adapters customise their spawn for WSL meshes — the Terminal
    /// adapter uses it to pick up the user's WSL login shell (e.g. zsh)
    /// instead of the system `sh`. The Claude-Code family ignores it; the
    /// `claude` / `claude.exe` binary is the same regardless of env.
    fn spawn_recipe(&self, platform: Platform, env_type: EnvType) -> SpawnRecipe;

    /// Whether to accept `--session-id <uuid>` (fresh) / `--resume <uuid>` (resume) args.
    fn supports_resume(&self) -> bool;

    /// Whether the app should attempt to auto-resume suspended sessions on startup
    /// using the stored `cli_session_id`.
    fn auto_resume_on_startup(&self) -> bool;

    /// Whether to inject the Claude-Code-style attention hook into
    /// `.claude/settings.local.json` in the spawn cwd.
    fn requires_attention_hook(&self) -> bool;

    /// Whether this provider writes a transcript the coordinator read API can
    /// parse into a Node Digest's rich layer (ADR-0008). The Claude-backed
    /// `anthropic` adapter runs real Claude Code (with a swapped backend for
    /// custom MiniMax/Kimi/DeepSeek profiles), so it writes Claude Code's
    /// `~/.claude/projects/<encoded-cwd>/<session>.jsonl`, which
    /// `services::transcript_reader` knows how to read. Providers with their own
    /// transcript format (Codex) or none (OpenCode, Agy, Terminal) return
    /// `false`; their digest degrades to spine-only with enrichment explicitly
    /// flagged `unsupported`, never silently omitted.
    fn produces_readable_transcript(&self) -> bool {
        false
    }

    /// Whether `--model <name>` / `--effort <level>` args from mesh config apply.
    fn supports_model_override(&self) -> bool;

    /// Whether `--prefill <text>` is accepted. Used by both spawn flows:
    /// `spawn_issue_agent` (URL + title hint, ~150 bytes; never the full body —
    /// see memory: buildmesh-issue-spawn-url-only) and `spawn_handover_agent`
    /// (free-form selected text from a parent terminal, often multi-line).
    fn supports_prefill(&self) -> bool;

    /// Platforms where this provider is available. Used to filter `list_providers`.
    fn available_on(&self) -> &'static [Platform];

    /// Whether this provider auto-assigns session IDs (captured from PTY output)
    /// rather than accepting one via CLI flag.
    fn self_assigns_session_id(&self) -> bool {
        false
    }

    /// Alternative recipe for resume (subcommand-style providers like Codex).
    /// If Some, `build_spawn_command()` uses this instead of `spawn_recipe()` + `resume_args()`.
    fn spawn_recipe_for_resume(&self, _platform: Platform, _session_id: &str) -> Option<SpawnRecipe> {
        None
    }

    /// Args appended when assigning a fresh session ID.
    fn session_assign_args(&self, id: &str) -> Vec<String> {
        vec!["--session-id".into(), id.into()]
    }

    /// Args appended when resuming an existing session.
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--resume".into(), id.into()]
    }

    /// Args for model override.
    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    /// Args for effort override.
    fn effort_args(&self, effort: &str) -> Vec<String> {
        vec!["--effort".into(), effort.into()]
    }

    /// Args for prefill/prompt text.
    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec!["--prefill".into(), text.into()]
    }

    /// True for plain shell providers (e.g. a node whose PTY runs
    /// `powershell.exe` / `sh` directly, with no LLM agent loop).
    /// When `true`, `start_reader` skips the LLM-specific EOF tail:
    /// PTY exit becomes `SessionStatus::Idle` (never `Error`), and the
    /// 3-second "resume-failed" early-exit warning and event are
    /// suppressed. Other LLM-specific skips in `spawn_agent_inner` are
    /// already gated by the existing capability flags this adapter also
    /// returns `false` for.
    fn is_plain_terminal(&self) -> bool {
        false
    }

    /// Whether this provider's launcher resets [`CLAUDE_BACKEND_ENV_VARS`] before
    /// the spawn path applies the per-profile backend env
    /// ([`crate::preferences::resolve_provider_env`]). True for the Claude-backed
    /// `anthropic` adapter (the executor for every Claude-compatible endpoint):
    /// cwrap `unset` those vars before `exec claude`, so any value inherited from
    /// buildmesh's environment is cleared first to give the agent the same clean
    /// slate. The built-in Anthropic subscription exports nothing of its own, so
    /// the reset alone keeps it on the default endpoint; a custom profile's
    /// account env is then layered on top. False for native-binary providers
    /// (Codex, OpenCode, Agy) that never went through cwrap and don't read these
    /// vars.
    fn resets_backend_env(&self) -> bool {
        false
    }
}
