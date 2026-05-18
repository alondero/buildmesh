//! Agent provider trait — the seam between "which provider" and "how do we spawn it".
//!
//! Each provider (Anthropic, Minimax, Gemini, OpenCode, ...) ships its own adapter
//! module under `adapters/`. The trait describes everything `spawn_agent_inner`
//! needs to know to launch and supervise an agent.

pub mod adapters;

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
#[derive(Debug, Clone, serde::Serialize)]
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

    /// Whether `--model <name>` / `--effort <level>` args from mesh config apply.
    fn supports_model_override(&self) -> bool;

    /// Whether `--prefill <text>` is accepted (used by `spawn_issue_agent` to seed
    /// the agent with a GitHub issue's title + body on first turn).
    fn supports_prefill(&self) -> bool;

    /// Platforms where this provider is available. Used to filter `list_providers`.
    fn available_on(&self) -> &'static [Platform];
}
