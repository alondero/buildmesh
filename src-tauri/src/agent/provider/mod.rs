//! Agent provider trait — the seam between "which provider" and "how do we spawn it".
//!
//! Each provider (Anthropic, Minimax, Gemini, OpenCode, ...) ships its own adapter
//! module under `adapters/`. The trait describes everything `spawn_agent_inner`
//! needs to know to launch and supervise an agent.

pub mod adapters;
pub mod compatibility;
pub mod provider_conf;

use crate::agent::capabilities::{EffortControlKind, HarnessCapabilities};
use crate::models::EnvType;

/// Built-in **Harness Profile** ids that detection populates (`claude`,
/// `codex`, `cursor`, `agy`, `opencode`, `grok`, `kimi`) plus the code-defined
/// `terminal` default (issue #536) and the legacy `anthropic` executor id.
///
/// Used by the v19 Spawn Option composite-id migration's
/// `provider NOT IN (...)` guard
/// (`db::migrate_agent_node_provider_id_custom_accounts`) to refuse
/// rewriting a `HarnessProfile` row whose id happens to collide with a
/// user-added custom account. Without this guard the resolver shim
/// could silently re-attach a `claude` harness profile as a Proxied
/// `claude:claude` row — a wire-shape collision that's impossible today
/// (custom account ids are user-chosen and don't normally match a
/// harness id) but cheap to defend against.
///
/// Kept here next to the wire type that documents these ids
/// (see `ProviderInfo` doc comment) so the SQL whitelist and the wire
/// doc can't drift apart.
pub const BUILTIN_HARNESS_IDS: &[&str] = &[
    "claude",
    "codex",
    "cursor",
    "agy",
    "grok",
    "kimi",
    "mcode",
    "dsh",
    "opencode",
    "commandcode",
    "terminal",
    "anthropic",
];

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
/// every Claude-compatible endpoint, including MiniMax and custom profiles)
/// uses `Direct` now that cwrap is absorbed — see `claude_direct_recipe`.
/// Kimi Code (#918) is a native self-auth harness that also uses `Direct`.
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
///
/// `MINIMAX_API_KEY` is included even though Claude Code itself does not read
/// it: a value exported in the user's shell still propagates to the spawn
/// child via OS env passing, and any third-party wrapper (or future claude
/// release that consults it) would silently route the rename / spawn through
/// the MiniMax endpoint — bypassing the user's configured `naming_provider`.
/// Keeping it cleared is the same defence #824 installed for the hardcoded
/// `provider_conf::minimax_backend_env` path. See issue #846.
pub const CLAUDE_BACKEND_ENV_VARS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "API_TIMEOUT_MS",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    "MINIMAX_API_KEY",
];

/// UI metadata declared by an adapter. The `id` is supplied separately via
/// `AgentProvider::id()` so the two can't diverge.
#[derive(Debug, Clone)]
pub struct UiMeta {
    pub label: String,
    pub color: String,
    pub icon: String,
}

/// Split a Spawn Option id into `(harness_id, Option<provider_id>)`.
///
/// The composite id is `<harness>` for a native option and
/// `<harness>:<provider>` for a Proxied Provider option (ADR-0016 §6,
/// issue #575). The separator is the first `:` so a provider id
/// containing `:` (theoretical today, but the id is the user-chosen
/// `ProviderAccount.id`) is preserved intact on the right side.
///
/// A bare id (no `:`) yields `(id, None)` — a native Spawn Option that
/// launches its harness directly. A composite id yields
/// `(before_colon, Some(after_colon))` — the executor comes from the
/// harness part, the credentials + endpoint from the provider part.
///
/// Pure (no globals / no I/O) — the canonical place to ask "is this a
/// native or proxied spawn, and what's the executor vs the creds?". All
/// spawn-resolver call sites route through here so the parsing rule
/// lives in one place.
pub fn parse_spawn_option_id(id: &str) -> (&str, Option<&str>) {
    match id.split_once(':') {
        Some((harness, provider)) => (harness, Some(provider)),
        None => (id, None),
    }
}

/// Frontend-facing **Spawn Option** wire type (ADR-0016, issue #575). One row
/// per clickable entry in the Spawn Menu — either a bare **Agent Harness** (a
/// native launch, e.g. clicking "Claude Code" boots Claude Code with its
/// own subscription) or an Agent Harness paired with a **Proxied Provider**
/// (a Claude-Code-backed row like "MiniMax via Claude Code"). The single
/// backend-derived list (`agent::provider_menu::available_providers`) is rendered
/// as-is on every spawn surface (sidebar, Issues/PRs probes, archived-resume,
/// mobile).
///
/// Composition (issue #538 retired the legacy enum-backed rows; the list is
/// purely the user's dynamic harness profiles + configured Claude-compatible
/// accounts):
///
/// * `harness_id` is the executor that runs the spawn — the harness profile
///   id for native rows (`"claude"`, `"codex"`, `"agy"`, `"opencode"`,
///   `"terminal"`) and the harness the proxied pair attaches to for
///   proxied rows (always `"claude"` today, but the field is generic so
///   the future multi-harness attach works without a wire change).
/// * `provider_id` is `None` for native rows and `Some("minimax")` (or a
///   custom account id) for proxied rows. The credential lookup
///   (`preferences::resolve_provider_env`) keys off this.
/// * `is_proxied` mirrors `provider_id.is_some()` for the frontend
///   convenience (avoids the `?? null` check on every render).
/// * `group_key == harness_id` so the UI's `groupBy` is a one-liner and the
///   ordering logic can cluster children under their header (a stable
///   `(is_terminal, rank_of(group_key))` sort keeps each harness's native
///   row first).
///
/// `id` is the composite spawn-option identifier, encoded as `<harness_id>`
/// for native and `<harness_id>:<provider_id>` for proxied (ADR-0016 §6).
/// The frontend hands it back to `spawn_agent` / `create_issue_node` /
/// `create_pr_node` unchanged; the backend's resolver splits on the first
/// `:` via `parse_spawn_option_id` to get `(executor, creds)`.
///
/// `resumable` is the backend-derived answer to "can this option resume an
/// archived/discovered session in-place?" — derived from the resolved
/// adapter's `supports_resume() && produces_readable_transcript()`. The
/// frontend uses it to populate the archived-node resume picker (issue
/// #550 follow-up); a custom Claude-compatible harness profile (e.g.
/// "DeepSeek via Claude") shares the `anthropic` adapter, so it
/// advertises itself correctly without the old hardcoded
/// `['anthropic','minimax','kimi']` allow-list that silently filtered
/// those profiles out.
///
/// Generated to src/types/generated/ProviderInfo.ts (issue #404).
/// `Deserialize` is added so the type participates in the ts-rs `export`
/// derive (the project pattern is `Serialize + Deserialize + TS` for
/// every generated wire type).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "ProviderInfo.ts")]
pub struct ProviderInfo {
    /// Composite spawn-option id: `<harness_id>` (native) or
    /// `<harness_id>:<provider_id>` (proxied). Round-tripped to
    /// `spawn_agent` / `create_issue_node` / `create_pr_node`.
    pub id: String,
    /// Display label for the row (harness profile name or proxied account
    /// name).
    pub label: String,
    /// Hex colour for the row dot/icon (from the adapter's `UiMeta`).
    pub color: String,
    /// Icon identifier (a single character or name) for the row.
    pub icon: String,
    /// See struct doc — backend-derived "can this resume in place?".
    pub resumable: bool,
    /// Executor that runs the spawn (harness profile id).
    pub harness_id: String,
    /// Credential/endpoint id when this is a Proxied Provider pairing
    /// (`None` for native rows).
    pub provider_id: Option<String>,
    /// `true` iff this row is a Proxied Provider pairing (i.e. has a
    /// `provider_id`); the frontend uses this to render the indented
    /// child style.
    pub is_proxied: bool,
    /// `harness_id` duplicated as a grouping key so the UI can
    /// `Array.groupBy(row => row.group_key)` without a derived field.
    pub group_key: String,
    /// Backend-derived capability contract (issue #1149). The Spawn Menu
    /// and the configuration resolver both consult this same descriptor;
    /// the frontend can use `capabilities.supports_model_override` /
    /// `supports_effort_override` / `effort_control` to render only the
    /// controls the underlying harness CLI actually accepts, instead of
    /// offering settings it would silently drop. Generated to
    /// `src/types/generated/ProviderInfo.ts`.
    pub capabilities: crate::agent::capabilities::HarnessCapabilities,
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

    /// Whether the spawn path should call [`inject_attention_hook`] before
    /// launching this provider (issue #886).
    ///
    /// [`inject_attention_hook`]: AgentProvider::inject_attention_hook
    fn requires_attention_hook(&self) -> bool;

    /// Provision this harness's attention hooks in the spawn cwd so the agent
    /// calls back to the local attention endpoint on turn end / permission
    /// prompts. Each adapter owns its harness's config format (issue #886):
    /// the Claude-backed `anthropic` adapter writes
    /// `.claude/settings.local.json`; Codex writes `.codex/config.toml` +
    /// `.codex/hooks.json`. Called from `spawn_agent_inner` (gated on
    /// [`requires_attention_hook`]) with the resolved host-side project path;
    /// implementations must be idempotent — they run on every spawn. A failure
    /// is logged and the spawn proceeds (the agent still works, only the
    /// attention callback is lost).
    ///
    /// [`requires_attention_hook`]: AgentProvider::requires_attention_hook
    fn inject_attention_hook(&self, _project_path: &std::path::Path) -> Result<(), String> {
        Ok(())
    }

    /// Whether this provider writes a transcript the coordinator read API can
    /// parse into a Node Digest's rich layer (ADR-0008). The Claude-backed
    /// `anthropic` adapter runs real Claude Code (with a swapped backend for
    /// custom MiniMax/DeepSeek profiles), so it writes Claude Code's
    /// `~/.claude/projects/<encoded-cwd>/<session>.jsonl`, which
    /// `services::transcript_reader` knows how to read. Codex's rollout format
    /// is parsed via `TranscriptFormat::Codex` (issue #887), and Cursor's
    /// workspace-scoped JSONL via `TranscriptFormat::Cursor`.
    /// Providers with no wired transcript reader (OpenCode, Agy, Terminal) return
    /// `false`; their digest degrades to spine-only with enrichment explicitly
    /// flagged `unsupported`, never silently omitted.
    fn produces_readable_transcript(&self) -> bool {
        false
    }

    /// Whether `--model <name>` / `--effort <level>` args from mesh config apply.
    fn supports_model_override(&self) -> bool;

    /// Whether the harness accepts verbatim CLI flag args from
    /// configuration (issue #1358). The orchestrator's
    /// `default_prepare` only forwards `ResolvedAgentConfig.extra_args`
    /// through `adapter.extra_args_args(...)` when this returns `true`;
    /// every interactive harness declares `true` (it owns its argv
    /// shape and the launch helper just tokenises the user's string),
    /// while the plain-shell Terminal adapter declares `false` (splicing
    /// synthetic flags into a user's interactive shell session would be
    /// a footgun). Mirrors the per-adapter opt-in pattern
    /// `supports_model_override` already uses.
    fn supports_extra_args(&self) -> bool;

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

    /// Whether the PTY reader thread should sniff a session ID from live
    /// output (the labeled-UUID regex in `session_capture`). Defaults to
    /// [`Self::self_assigns_session_id`]. Harnesses that self-assign but
    /// whose IDs are not UUID-shaped (OpenCode's `ses_…`) override this
    /// to `false` and capture in [`Self::after_fresh_spawn`] instead.
    fn captures_session_id_from_pty(&self) -> bool {
        self.self_assigns_session_id()
    }

    /// Hook after a **fresh** spawn (`SessionIdMode::None`) has registered
    /// the PTY. Default is a no-op. OpenCode uses it to poll its local
    /// SQLite store for the `ses_…` id the TUI just minted. Adapters own
    /// the capture implementation — spawn must not hard-code a provider
    /// service behind a boolean flag.
    fn after_fresh_spawn(&self, _node_id: i64, _spawn_path: &str, _env_type: EnvType) {}

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

    /// Args appended verbatim from a circuit author's
    /// `SpawnAgentNode.extra_args` (issue #1358). The orchestrator's
    /// `default_prepare` only forwards `ResolvedAgentConfig.extra_args`
    /// through this helper when the adapter's
    /// `capabilities().supports_extra_args == true` — Terminal is the
    /// standing example of a harness that opts out, so its invocation
    /// never gets a synthetic flag splice.
    ///
    /// Default impl: tokenise with shell-style quoting so that
    /// `--append "fix the bug"` keeps the quoted phrase as a single
    /// argv element instead of splitting inside the quotes (a real
    /// footgun with naive `split_whitespace` — flagged in PR #1362
    /// code review). Backslashes and the canonical POSIX quotes are
    /// honoured.
    ///
    /// Returns `Err(shell_words::Error)` on malformed input
    /// (unclosed quote, dangling escape) **rather than panicking**:
    /// the spawn worker thread must not abort on a user typo.
    /// `default_prepare` logs the parse error and falls back to
    /// whitespace tokenisation, which gracefully drops the malformed
    /// token list rather than crashing the worker.
    /// Per-adapter overrides can special-case (e.g. Codex may want a
    /// `--` separator before raw tokens in a future slice) — the
    /// seam is wide-open without breaking call sites.
    fn extra_args_args(&self, raw: &str) -> Result<Vec<String>, shell_words::ParseError> {
        shell_words::split(raw)
    }

    /// Args appended when the parent mesh has its `sandbox` toggle on.
    /// The orchestrator consults this independently for its own
    /// platform-level containment (macOS Seatbelt — see
    /// [`crate::agent::sandbox`]; Windows restricted-token in
    /// `spawn::sandbox_spawn`); this method is the *adapter-level* knob,
    /// so a harness whose CLI exposes a native sandbox flag (e.g.
    /// Antigravity's `--sandbox`, issue #1287) can opt into forwarding
    /// it without the orchestrator knowing the flag vocabulary.
    ///
    /// Default is empty — every harness that doesn't override inherits
    /// "no native sandbox flag"; the orchestrator's outer wrapper is the
    /// sole containment layer. Appended in `default_prepare` between
    /// `effort_args` and `prefill_args` so security-shaped flags land
    /// with the capability-driven contributions, ahead of the trailing
    /// prefill text.
    fn sandbox_args(&self) -> Vec<String> {
        Vec::new()
    }

    /// True for plain shell providers (e.g. a node whose PTY runs
    /// `powershell.exe` / `sh` directly, with no LLM agent loop).
    /// When `true`, `start_reader` skips the LLM-specific EOF tail:
    /// PTY exit becomes `SessionStatus::Idle` (never `Error`), and the
    /// 3-second "resume-failed" early-exit warning and event are
    /// suppressed. `start_reader` also never buffers the node's PTY
    /// output for session auto-naming (`session_naming::on_output`,
    /// issue #296): a terminal's rename buffer would never be consumed —
    /// the rename LLM only fires from `on_turn`, which only the Claude
    /// stop hook calls. Other LLM-specific skips in `spawn_agent_inner`
    /// are already gated by the existing capability flags this adapter
    /// also returns `false` for.
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

    /// The effort-control vocabulary this harness accepts.
    ///
    /// Replaces the adapter-id switch in the previous
    /// `agent::capabilities::effort_control_for`. Adapters whose CLI
    /// exposes a reasoning-effort knob override this with the matching
    /// `EffortControlKind::Closed { allowed }` (e.g. Claude Code's
    /// `--effort low|medium|high`) or
    /// `EffortControlKind::InlineConfig { key, allowed }` (e.g. Codex's
    /// `-c model_reasoning_effort=…`); every other adapter inherits the
    /// `None` default and the resolver drops the effort layer.
    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::None
    }

    /// The capability contract this harness advertises for its normal
    /// launch mode (issue #1149, refactored in #1179).
    ///
    /// **Invariant:** the values here must match the recipe produced by
    /// [`AgentProvider::prepare_launch`] for the same platform; the
    /// coherence is regression-pinned by
    /// `crate::agent::spawn::tests::capability_recipe_coherence`. The
    /// default implementation composes the existing `*_flags` methods
    /// so every adapter that does not need a custom descriptor inherits
    /// a correct one for free; the resolver and Spawn Menu consult
    /// this single source of truth.
    fn capabilities(&self) -> HarnessCapabilities {
        let platforms: Vec<String> = self
            .available_on()
            .iter()
            .map(|p| crate::agent::capabilities::platform_name(*p).to_string())
            .collect();
        let effort_control = self.effort_control();
        HarnessCapabilities {
            harness_id: self.id().to_string(),
            supports_resume: self.supports_resume(),
            auto_resume_on_startup: self.auto_resume_on_startup(),
            requires_attention_hook: self.requires_attention_hook(),
            produces_readable_transcript: self.produces_readable_transcript(),
            supports_model_override: self.supports_model_override(),
            supports_effort_override: !matches!(effort_control, EffortControlKind::None),
            supports_extra_args: self.supports_extra_args(),
            supports_prefill: self.supports_prefill(),
            is_plain_terminal: self.is_plain_terminal(),
            effort_control,
            available_on: platforms,
        }
    }

    /// Build the coherent launch contribution for one spawn (issue #1179).
    ///
    /// Pure (no I/O, no DB, no globals). The shared default
    /// implementation in `agent::launch::default_prepare` composes the
    /// existing `*_args` methods into a single
    /// [`crate::agent::launch::PreparedHarnessLaunch`] — recipe +
    /// capability contract + env policy — so `build_spawn_command_prepared`
    /// no longer has to reassemble per-adapter semantics from many
    /// independent trait methods. Adapters that diverge (e.g. Codex's
    /// subcommand-style resume) can override; nothing in the current
    /// set needs to.
    ///
    /// `where Self: Sized` because the default coerces `&Self` to a
    /// `&dyn AgentProvider` to call the shared helper. Object-safe
    /// callers (the orchestrator's `&'static dyn AgentProvider`) route
    /// through the free function `agent::launch::default_prepare`
    /// directly; the trait method exists so future adapter overrides
    /// can be invoked through the same call site.
    fn prepare_launch(
        &self,
        input: crate::agent::launch::HarnessLaunchInput<'_>,
    ) -> crate::agent::launch::PreparedHarnessLaunch
    where
        Self: Sized,
    {
        crate::agent::launch::default_prepare(self, input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #846 — "cover shell-injected `MINIMAX_API_KEY`".
    ///
    /// The rename path (`session_naming::summarize_and_rename_with`) iterates
    /// over `CLAUDE_BACKEND_ENV_VARS` and `env_remove`s each from the spawned
    /// `claude --print` child. Without `MINIMAX_API_KEY` in the list, a value
    /// exported in the user's shell (e.g. `export MINIMAX_API_KEY=sk-...` in
    /// `~/.bashrc`) is inherited by the child via OS env passing — silently
    /// routing the rename through the MiniMax backend regardless of the user's
    /// configured `naming_provider`. Pinning it here means any future refactor
    /// that shrinks the list trips this test, surfacing the regression instead
    /// of leaking a "why is my Anthropic-rename going to MiniMax?" surprise
    /// (the exact class of bug issue #824 closed for the hardcoded
    /// `provider_conf::minimax_backend_env` path).
    #[test]
    fn claude_backend_env_vars_clears_shell_injected_minimax_api_key() {
        assert!(
            CLAUDE_BACKEND_ENV_VARS.contains(&"MINIMAX_API_KEY"),
            "CLAUDE_BACKEND_ENV_VARS must clear MINIMAX_API_KEY before the \
             rename child spawns, otherwise a shell-exported key silently \
             routes the rename through MiniMax (issue #846). Current list: {:?}",
            CLAUDE_BACKEND_ENV_VARS
        );
    }
}
