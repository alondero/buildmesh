//! Agent provider trait — the seam between "which provider" and "how do we spawn it".
//!
//! Each provider (Anthropic, Minimax, Gemini, OpenCode, ...) ships its own adapter
//! module under `adapters/`. The trait describes everything `spawn_agent_inner`
//! needs to know to launch and supervise an agent.

pub mod adapters;
pub mod provider_conf;

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
/// cwrap providers spawn under powershell so ANSI escape sequences propagate
/// correctly; node-shim providers (`.cmd` batch files) use cmd.exe.
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

/// The bash-free claude invocation used by the Windows OS-sandbox path
/// ([`AgentProvider::sandbox_direct_recipe`]). Inside an AppContainer the
/// `cwrap → MSYS2 bash → claude` chain dies in the loader
/// (`STATUS_DLL_INIT_FAILED`) because MSYS2's runtime can't initialize in the
/// container's restricted object namespace — but the native `claude.exe` runs
/// fine. We target the `.exe` explicitly (the bare `claude` is itself a bash
/// shim) and spawn it directly, with no PowerShell/cmd/bash wrapper.
pub fn claude_direct_recipe() -> SpawnRecipe {
    SpawnRecipe {
        binary: "claude.exe",
        base_args: vec!["--dangerously-skip-permissions".into()],
        windows_shell: WindowsShell::Direct,
    }
}

/// UI metadata declared by an adapter. The `id` is supplied separately via
/// `AgentProvider::id()` so the two can't diverge.
#[derive(Debug, Clone)]
pub struct UiMeta {
    pub label: String,
    pub color: String,
    pub icon: String,
}

/// Frontend-facing provider listing. Composed by `commands::agent::available_providers`
/// from each adapter's `id()` + `ui()`.
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

    /// How to invoke this provider on the given host platform.
    fn spawn_recipe(&self, platform: Platform) -> SpawnRecipe;

    /// Whether to accept `--session-id <uuid>` (fresh) / `--resume <uuid>` (resume) args.
    fn supports_resume(&self) -> bool;

    /// Whether the app should attempt to auto-resume suspended sessions on startup
    /// using the stored `cli_session_id`.
    fn auto_resume_on_startup(&self) -> bool;

    /// Whether to inject the Claude-Code-style attention hook into
    /// `.claude/settings.local.json` in the spawn cwd.
    fn requires_attention_hook(&self) -> bool;

    /// Whether this provider writes a transcript the coordinator read API can
    /// parse into a Node Digest's rich layer (ADR-0008). The Claude Code family
    /// — everything launched through `cwrap` (Anthropic, MiniMax, Kimi) — runs
    /// real Claude Code with a swapped backend, so it writes Claude Code's
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

    /// Bash-free spawn recipe for the Windows AppContainer sandbox. cwrap routes
    /// through MSYS2 `bash`, which can't initialize inside an AppContainer
    /// (`STATUS_DLL_INIT_FAILED`), so cwrap-backed providers (Anthropic, MiniMax,
    /// Kimi) return [`claude_direct_recipe`] here to reach `claude.exe` directly.
    /// The provider-selecting backend env that cwrap would have exported is
    /// supplied separately by [`sandbox_provider_env`](Self::sandbox_provider_env).
    ///
    /// Returns `None` for providers that don't go through cwrap (they already
    /// spawn a native binary). Consumed by `build_spawn_command` only on a
    /// Windows host with the sandbox toggle on and a non-WSL target. First slice
    /// of absorbing cwrap into buildmesh.
    fn sandbox_direct_recipe(&self, _platform: Platform) -> Option<SpawnRecipe> {
        None
    }

    /// Backend-selecting environment that `cwrap` would export for this provider,
    /// reconstructed in-process so [`sandbox_direct_recipe`](Self::sandbox_direct_recipe)
    /// can launch `claude.exe` without bash. Empty for Anthropic (the built-in
    /// subscription needs no overrides) and for non-cwrap providers. MiniMax/Kimi
    /// read their API keys from `~/.claude/providers.conf`.
    fn sandbox_provider_env(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}
