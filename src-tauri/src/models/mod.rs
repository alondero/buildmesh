//! Data models for Buildmesh

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ts_rs::TS;

/// Re-export the wire-level Agent Harness configuration value type from
/// the private `preferences` module so [`crate::models::Mesh`] / [`MeshRow`]
/// can include it in their public type signatures without leaking the
/// `preferences` module through the public API (`preferences` stays private
/// because the rest of its surface is internal-only). The same type is
/// used by the application-level defaults map
/// (`AppPreferences.harness_defaults`), the per-Mesh overrides map
/// (`Mesh.harness_overrides`), and the spawn-config resolver
/// (`ResolvedAgentConfig`).
pub use crate::preferences::HarnessConfigValue;

/// Runtime environment — Windows or WSL
///
/// `Windows` is `#[default]` so `AgentNode::default()` matches the existing
/// `from_db_str` fallback ("unknown string → Windows"); issue #457.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "EnvType.ts")]
pub enum EnvType {
    #[default]
    Windows,
    Wsl,
}

impl std::fmt::Display for EnvType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvType::Windows => write!(f, "windows"),
            EnvType::Wsl => write!(f, "wsl"),
        }
    }
}

impl From<super::env::Environment> for EnvType {
    fn from(env: super::env::Environment) -> Self {
        match env {
            super::env::Environment::Windows => EnvType::Windows,
            super::env::Environment::Wsl => EnvType::Wsl,
        }
    }
}

impl EnvType {
    /// Parse the DB string column. Unknown strings fall back to Windows
    /// (matches the prior inline `match` behaviour scattered across db/mod.rs).
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "wsl" => EnvType::Wsl,
            _ => EnvType::Windows,
        }
    }
}

/// Agent provider type
///
/// `Anthropic` is `#[default]` so `AgentNode::default()` matches the existing
/// `from_db_str` fallback ("unknown string → Anthropic"); issue #457.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "Provider.ts")]
pub enum Provider {
    #[default]
    Anthropic,
    Agy,
    OpenCode,
    Codex,
    /// Cursor's interactive coding agent CLI.
    /// See `agent::provider::adapters::cursor`.
    Cursor,
    /// xAI's Grok Build CLI — interactive TUI-based coding agent.
    /// See `agent::provider::adapters::grok`.
    Grok,
    /// Moonshot AI's Kimi Code CLI — interactive TUI-based coding agent.
    /// See `agent::provider::adapters::kimi` (wayfinder #918).
    Kimi,
    /// MiniMax Code CLI — interactive TUI-based coding agent.
    /// See `agent::provider::adapters::mcode`.
    Mcode,
    /// Plain shell terminal (PowerShell on Windows, `sh` on macOS/Linux,
    /// routed through `wsl.exe` on WSL meshes). No LLM agent loop.
    /// See `agent::provider::adapters::terminal`.
    Terminal,
}

impl Provider {
    /// All known providers, in stable order. Used to enumerate UI listings.
    pub fn all() -> &'static [Provider] {
        &[
            Provider::Anthropic,
            Provider::Agy,
            Provider::OpenCode,
            Provider::Codex,
            Provider::Cursor,
            Provider::Grok,
            Provider::Kimi,
            Provider::Mcode,
            Provider::Terminal,
        ]
    }

    /// Parse the DB string column / Tauri arg into a typed `Provider`.
    /// Unknown strings fall back to `Anthropic` (matches previous behaviour).
    ///
    /// Inputs are trimmed and ASCII-lowercased before matching so callers
    /// that hand-edit `preferences.json` (e.g. `default_provider = "Terminal"`)
    /// get the variant they meant instead of a silent Anthropic default.
    /// Genuinely-unrecognised non-empty strings emit a `tracing::warn!` so
    /// the silent fallback shows up in the buildmesh.log file — empty strings
    /// are treated as an intentional default and not logged.
    pub fn from_db_str(s: &str) -> Provider {
        let normalized = s.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "" | "anthropic" => Provider::Anthropic,
            "agy" => Provider::Agy,
            "opencode" => Provider::OpenCode,
            "codex" => Provider::Codex,
            "cursor" => Provider::Cursor,
            "grok" => Provider::Grok,
            "kimi" => Provider::Kimi,
            "mcode" | "minimax-code" => Provider::Mcode,
            "terminal" => Provider::Terminal,
            // "minimax" is no longer a first-class executor: it is Claude Code
            // with a swapped backend, configured as a harness profile whose
            // paired provider account injects the endpoint at spawn (#538). A
            // bare legacy id with no configured profile falls through to the
            // Anthropic executor here (resolve_harness_provider checks profiles
            // first, so a configured "minimax" account resolves cleanly).
            // "kimi" USED to fall through here too — Kimi Code (#918) is now a
            // native binary executor, so it gets its own arm above.
            _ => {
                tracing::warn!(
                    "Provider::from_db_str: unrecognized provider {:?}, falling back to Anthropic",
                    s
                );
                Provider::Anthropic
            }
        }
    }

    /// Look up the behaviour adapter for this provider.
    /// All provider-specific logic (binary, args, capabilities) lives behind this seam.
    pub fn adapter(&self) -> &'static dyn crate::agent::provider::AgentProvider {
        use crate::agent::provider::adapters;
        match self {
            Provider::Anthropic => &adapters::ANTHROPIC,
            Provider::Agy => &adapters::AGY,
            Provider::OpenCode => &adapters::OPENCODE,
            Provider::Codex => &adapters::CODEX,
            Provider::Cursor => &adapters::CURSOR,
            Provider::Grok => &adapters::GROK,
            Provider::Kimi => &adapters::KIMI,
            Provider::Mcode => &adapters::MCODE,
            Provider::Terminal => &adapters::TERMINAL,
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::Agy => write!(f, "agy"),
            Provider::OpenCode => write!(f, "opencode"),
            Provider::Codex => write!(f, "codex"),
            Provider::Cursor => write!(f, "cursor"),
            Provider::Grok => write!(f, "grok"),
            Provider::Kimi => write!(f, "kimi"),
            Provider::Mcode => write!(f, "mcode"),
            Provider::Terminal => write!(f, "terminal"),
        }
    }
}

/// Session status
//
// `rename_all = "snake_case"` (not "lowercase") so the multi-word `AwaitingInput`
// variant serialises to "awaiting_input" — matching `to_db_str` and every
// frontend comparison. Under "lowercase" it became "awaitinginput", a value no
// consumer matched (issue #359). Single-word variants are identical either way.
//
// `Idle` is `#[default]` so `AgentNode::default()` matches the existing
// `from_db_str` fallback ("unknown string → Idle"); issue #457.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "SessionStatus.ts")]
pub enum SessionStatus {
    Running,
    #[default]
    Idle,
    AwaitingInput,
    Error,
    Archived,
    Suspended,
    /// Node row exists but the slow stage-2 of spawn (git sync, worktree
    /// create, PTY spawn) has not yet completed. The user sees this in
    /// the UI as a pulsing "Starting…" badge. Set on creation by
    /// `create_issue_node` / `create_pending`; flipped to `Running` on
    /// stage-2 success or `Error` on stage-2 failure.
    Pending,
    /// Issue #654 — agent process launched but the early-exit window has not
    /// elapsed. Orchestrator writes this after `start_reader` returns, then
    /// schedules a delayed conditional promotion to `Running`; no-op if the
    /// reader thread already wrote `error`. Closes the race where each
    /// writer could clobber the other, leaving a ghost-Running node.
    Spawning,
    /// Issue #485 — an Autopilot node whose wrap-up sequence finished:
    /// worktree clean, branch pushed, PR opened. Terminal for the pipeline
    /// (the node stays viewable but Autopilot no longer counts it against
    /// the mesh's concurrency limit).
    Completed,
}

/// Parse a session status from a DB string column
impl SessionStatus {
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "running" => SessionStatus::Running,
            "awaiting_input" => SessionStatus::AwaitingInput,
            "error" => SessionStatus::Error,
            "archived" => SessionStatus::Archived,
            "suspended" => SessionStatus::Suspended,
            "pending" => SessionStatus::Pending,
            "spawning" => SessionStatus::Spawning,
            "completed" => SessionStatus::Completed,
            _ => SessionStatus::Idle,
        }
    }

    pub fn to_db_str(&self) -> &'static str {
        match self {
            SessionStatus::Running => "running",
            SessionStatus::Idle => "idle",
            SessionStatus::AwaitingInput => "awaiting_input",
            SessionStatus::Error => "error",
            SessionStatus::Archived => "archived",
            SessionStatus::Suspended => "suspended",
            SessionStatus::Pending => "pending",
            SessionStatus::Spawning => "spawning",
            SessionStatus::Completed => "completed",
        }
    }
}

/// Discriminator the v30 autopilot poller reads to decide which spawn
/// strategy to use (wayfinder #990 / ticket #991). Persisted as TEXT on
/// `meshes.autopilot_mode` (default `'issue_driven'` so every pre-v30 mesh
/// keeps the GitHub-label behaviour byte-for-byte). The wire shape is the
/// same `snake_case` union as [`SessionStatus`] — multi-word variants keep
/// their underscore (issue #359 lesson), so `IssueDriven` round-trips as
/// `"issue_driven"` and `Looping` as `"looping"`. `#[default] = IssueDriven`
/// so `Mesh::default()` (issue #518) inherits the pre-v30 behaviour without
/// a fixture edit.
///
/// Lives in `models` (not `db`) because the `Mesh` struct embeds it as a
/// field — `db` already imports `models::*`, so a domain-cycle (models ->
/// db for the enum) would be required if the enum lived in `db`. Domain
/// concept (autopilot mode discriminator) that happens to be stored on the
/// `meshes` row, so model-resident is also semantically correct.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case", export_to = "AutopilotMode.ts")]
pub enum AutopilotMode {
    /// The default, pre-v30 behaviour: the background poller watches the
    /// mesh's GitHub repo for `autopilot_trigger_label`-tagged issues and
    /// spawns branched-worktree Agent Nodes for them as they appear.
    #[default]
    IssueDriven,
    /// The new Looping mode (tickets #992 / #993): one node per loop
    /// iteration, sequential, driven by `loop_initial_prompt` and
    /// optionally suffix-injected with `loop_suffix_prompt` between
    /// iterations. Spawns on the mesh's configured worktree strategy
    /// (`mesh.use_worktree`), NOT the autopilot-forced-branched mode.
    Looping,
}

impl AutopilotMode {
    /// The literal DB string the `meshes.autopilot_mode` column stores.
    /// Pinned here (not derived from serde) so a `serde` rename_all drift
    /// can't silently corrupt the DB column — the same rationale as
    /// [`SessionStatus::to_db_str`].
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::IssueDriven => "issue_driven",
            Self::Looping => "looping",
        }
    }

    /// Parse back from the DB column. Unknown strings degrade to
    /// `IssueDriven` (the pre-v30 default) rather than `None`, so a row an
    /// old build accidentally wrote with an unsupported value doesn't break
    /// the poller (a `None` here would be worse than a degraded default —
    /// the poller would have to special-case `Option` everywhere).
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "looping" => Self::Looping,
            _ => Self::IssueDriven,
        }
    }
}

/// A mesh — top-level folder containing agent nodes.
///
/// Generated to src/types/generated/Mesh.ts (issue #359). `i64` fields carry
/// `#[ts(as = "i32")]` so they emit `number` (serde_json sends JS numbers, not
/// the `bigint` ts-rs defaults to for 64-bit ints).
///
/// `#[derive(Default)]` (issue #518) so test fixtures and stub-only call
/// sites can spread `..Default::default()` instead of re-listing every field
/// on each new column. Follow-up to the `AgentNode` migration in #457.
/// Semantics are Option A (zero-value stub): every scalar is `0`/`""`/
/// `false` and every `Option<T>` is `None`. `created_at` defaults to
/// UNIX epoch (chrono's `DateTime::<Utc>::default()`), which is a
/// well-defined placeholder that won't accidentally match a real row.
/// Future `Option<T>` columns automatically inherit `None` with no
/// fixture edits.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "Mesh.ts")]
pub struct Mesh {
    #[ts(as = "i32")]
    pub id: i64,
    pub name: String,
    pub path: String, // absolute path to mesh root
    pub layout: String, // 'grid' or 'single'
    #[ts(as = "i32")]
    pub position: i64, // sort order in sidebar
    pub created_at: DateTime<Utc>,
    // Mesh-level config (see MeshRow for the canonical typed view)
    pub build_command: Option<String>,
    pub run_command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub use_worktree: bool, // default true
    pub worktree_mode: Option<String>,
    pub default_provider: Option<String>,
    pub base_ref: String, // default "origin/main"
    /// Free-form scratch pad text for the Probe Panel "📝 Scratch Pad"
    /// tab. Owned by Buildmesh only — never written to disk, never visible
    /// to agents. Persisted as `meshes.scratchpad TEXT NOT NULL DEFAULT ''`
    /// (schema v17) and read back as the raw `String`. Empty string is a
    /// normal, non-error state ("no notes yet").
    pub scratchpad: String,
    /// OS-level agent process sandbox toggle. When `true`, agent PTY
    /// processes spawned in this mesh are confined to the node's Git
    /// worktree — macOS Seatbelt (`sandbox-exec`, #497) and Windows
    /// AppContainer (#498) each read this flag and apply their own
    /// confinement policy. Off by default (`false`); ignored on hosts
    /// where neither native spawn is built. Persisted as
    /// `meshes.sandbox INTEGER NOT NULL DEFAULT 0` (schema v18).
    pub sandbox: bool,
    /// Per-mesh target for the pre-spawn Worktree Pool worker
    /// (`services::warm_pool`, issue #609 / v21). `0` disables the pool
    /// for this mesh (no warm entries created on startup, no refill
    /// after claim); `1..=5` is the target the worker fills to.
    /// Clamped at the IPC boundary (`update_mesh_pool_size`), not here
    /// — this field is the typed integer the worker reads. ON by
    /// default since schema v24 (`1`, ADR 0020); opted out via the
    /// Worktrees Probe's ConfigurationCard (issue #611). Persisted as
    /// `meshes.pre_spawn_pool_size INTEGER NOT NULL DEFAULT 1`
    /// (schema v22, default flipped in v24).
    #[ts(as = "i32")]
    pub pre_spawn_pool_size: i32,
    /// User-chosen accent colour for the mesh, as a `#rrggbb` hex string.
    /// Picked in the "New mesh" modal on creation and recolourable by
    /// clicking the mesh's colour swatch in the sidebar. `None` means the
    /// user never chose one, so the frontend falls back to the deterministic
    /// palette keyed on the mesh id (`src/lib/meshColors.ts`). Persisted as
    /// `meshes.color TEXT` (schema v25); empty/absent reads back as `None`.
    pub color: Option<String>,
    /// Autopilot Mode master switch (issue #481, PRD #480). When `true` the
    /// background poller (`services::autopilot`) watches this mesh's GitHub
    /// repo for issues tagged [`Mesh::autopilot_trigger_label`] and spawns
    /// branched-worktree Agent Nodes for them automatically. Persisted as
    /// `meshes.autopilot_enabled INTEGER NOT NULL DEFAULT 0` (schema v26).
    pub autopilot_enabled: bool,
    /// GitHub issue label that marks an issue as an Autopilot task. `None`
    /// falls back to [`DEFAULT_AUTOPILOT_TRIGGER_LABEL`] at poll time.
    pub autopilot_trigger_label: Option<String>,
    /// Maximum number of concurrently *active* auto-spawned nodes for this
    /// mesh. The poller only ingests new issues while the active count is
    /// below this limit (PRD #480 story 5/6). Clamped to `1..=8` at the IPC
    /// boundary; stored as `INTEGER NOT NULL DEFAULT 2`.
    #[ts(as = "i32")]
    pub autopilot_concurrency_limit: i32,
    /// Spawn Option id auto-spawned nodes use. `None` falls through the
    /// normal default-provider chain (mesh default → app default → claude).
    pub autopilot_provider: Option<String>,
    /// What Autopilot asks the agent to do once the wrap-up verification
    /// passes: `"draft_pr"` (default) opens a draft PR, `"pr"` opens a
    /// ready-for-review PR, `"none"` stops after push.
    pub autopilot_action_on_success: Option<String>,
    /// Root-context build command (issue #802). When set, a node running at
    /// the mesh root (`env::worktree_segment(node).is_none()`) runs this
    /// instead of [`build_command`](Self::build_command); Worktree Nodes keep
    /// running `build_command`. `None` falls back to `build_command` in both
    /// contexts — the historical PR #801 behaviour. Persisted as
    /// `meshes.root_build_command TEXT` (schema v27).
    pub root_build_command: Option<String>,
    /// Root-context run command (issue #802) — the run-mode sibling of
    /// [`root_build_command`](Self::root_build_command). `None` falls back to
    /// `run_command`. Persisted as `meshes.root_run_command TEXT` (schema v27).
    pub root_run_command: Option<String>,
    /// Discriminator the v30 autopilot poller reads to decide which spawn
    /// strategy to use (wayfinder #990 / ticket #991). The default
    /// `IssueDriven` matches the pre-v30 GitHub-label poller byte-for-byte;
    /// `Looping` is the new sequential prompt-driven mode implemented by
    /// tickets #992 + #993. Persisted as `meshes.autopilot_mode TEXT NOT
    /// NULL DEFAULT 'issue_driven'` (schema v30).
    pub autopilot_mode: AutopilotMode,
    /// Body of the prompt injected into every loop-iteration node when
    /// `autopilot_mode == Looping` (wayfinder #990). `None` is a "no
    /// prompt configured" state — the poller (ticket #992) treats it as
    /// "loop not ready, stay idle" rather than fabricating a prompt.
    /// Persisted as `meshes.loop_initial_prompt TEXT` (schema v30);
    /// empty/absent reads back as `None`.
    pub loop_initial_prompt: Option<String>,
    /// Optional second-turn prompt injected AFTER the issue-style wrap-up
    /// (#485) verifies green, before the next loop iteration starts
    /// (ticket #993). `None` = no suffix turn — the iteration completes
    /// as soon as wrap-up passes. Persisted as
    /// `meshes.loop_suffix_prompt TEXT` (schema v30); empty/absent reads
    /// back as `None`.
    pub loop_suffix_prompt: Option<String>,
    /// Optional hard cap on loop iterations (wayfinder #990). `None` =
    /// continuous — the user must intervene to stop the loop. `Some(n)`
    /// with `n >= 1` = stop after n iterations. Validated at the IPC
    /// boundary (`commands::mesh_properties::update_mesh_loop_config`)
    /// to `>= 1` when set. Persisted as `meshes.loop_max_iterations
    /// INTEGER` (schema v30); nullable to carry the "no cap" meaning
    /// past the row. `i32` matches the `autopilot_concurrency_limit`
    /// precedent — a sane upper bound for a user-configured cap and
    /// keeps the wire shape `number` (not `bigint`).
    pub loop_max_iterations: Option<i32>,
    /// Pause delay between consecutive loop spawns (wayfinder #990).
    /// The poller (ticket #992) re-checks after this many seconds; `0`
    /// means "spawn as soon as the previous iteration finished" (no
    /// pause). Persisted as `meshes.loop_interval_seconds INTEGER NOT
    /// NULL DEFAULT 0` (schema v30).
    pub loop_interval_seconds: i32,
    /// Consecutive-failure auto-pause threshold (wayfinder #990). When
    /// `>= this value` consecutive loop iterations wrap-up-failed, the
    /// poller stops spawning until the user clears or resets it. `0`
    /// (the default) disables the threshold. Persisted as
    /// `meshes.loop_consecutive_failures INTEGER NOT NULL DEFAULT 0`
    /// (schema v30).
    pub loop_consecutive_failures: i32,
    /// **Per-Mesh harness overrides** (issue #1151 / slice 2 of #1148) —
    /// a sparse map keyed by stable harness profile id (the same id the
    /// Spawn Menu uses, e.g. `"claude"`, `"codex"`, `"agy"`, plus any
    /// user-defined custom profile id). A present entry supplies a
    /// per-harness model and/or effort value that overrides the
    /// application-level default for that harness only on this Mesh;
    /// resolving per field follows the cascade order
    /// (explicit > mesh override > application > native). A missing key
    /// means "this Mesh inherits the application default for that
    /// harness". The map is **sparse**: an entry whose every field
    /// collapses to absent is removed entirely by the CRUD command, so
    /// a stored key is never `{model: null, effort: null}`.
    ///
    /// Persisted as `meshes.harness_overrides TEXT NOT NULL DEFAULT '{}'`
    /// (schema v33), serialised as a JSON object. The legacy
    /// `meshes.model` / `meshes.effort` columns remain physically present
    /// for positional row compatibility but are no longer read as active
    /// configuration; the v33 one-shot migration copies non-empty
    /// legacy values into a `claude` override entry.
    pub harness_overrides: HashMap<String, HarnessConfigValue>,
}

/// Fallback for [`Mesh::autopilot_trigger_label`] when the user enables
/// Autopilot without customizing the label (PRD #480 uses this literal).
pub const DEFAULT_AUTOPILOT_TRIGGER_LABEL: &str = "buildmesh:run";

/// The folder chosen in the "New mesh" modal's location picker. Returned by
/// the `pick_mesh_folder` command so the frontend can show the selected
/// path (and derived name) before committing the create — the native folder
/// dialog is a backend-only capability, so this splits "pick a folder" from
/// "create the mesh" (which used to be fused in `add_mesh`). `None` from the
/// command means the user cancelled the dialog.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PickedFolder.ts")]
pub struct PickedFolder {
    pub path: String,
    pub name: String,
}

/// A paired mobile/admin client identified by a persistent per-device token
/// (issue #502, PRD #494). Generated to `src/types/generated/DeviceSession.ts`
/// (issue #359). This is the panel/wire view — it deliberately omits the
/// `token_hash` column so the secret never crosses the IPC/HTTP boundary.
///
/// Timestamps are the raw SQLite `datetime('now')` text (`YYYY-MM-DD HH:MM:SS`),
/// surfaced as opaque `String`s the UI renders directly — not `DateTime<Utc>`,
/// which would only force an RFC3339 parse-or-fall-back-to-epoch round-trip for
/// a value the backend never does date math on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "DeviceSession.ts")]
pub struct DeviceSession {
    #[ts(as = "i32")]
    pub id: i64,
    /// Human-friendly name derived from the client's `User-Agent` at pairing
    /// (e.g. "Safari on iPhone"). `None` when no usable header was present.
    pub label: Option<String>,
    /// Last IP the device was seen from. Demoted from an auth factor (the
    /// device token now identifies the client, supporting roaming) to a
    /// displayed attribute. `None` until the first activity touch records one.
    pub last_ip: Option<String>,
    /// When the device first paired.
    pub created_at: String,
    /// When the device last authenticated (login refresh or WS-ticket mint).
    pub last_active_at: String,
}

/// An agent node — isolated agent working directory.
///
/// Generated to src/types/generated/AgentNode.ts (issue #359); references the
/// generated `EnvType`/`SessionStatus` enums. `provider` is an opaque harness
/// id `String` (issue #535), not the legacy `Provider` enum. `i64` fields use
/// `#[ts(as = "i32")]` / `Option<i32>` so they emit `number` / `number | null`
/// rather than ts-rs's default `bigint`.
///
/// `#[derive(Default)]` (issue #457) so test fixtures and stub-only call
/// sites can spread `..Default::default()` instead of re-listing every field
/// on each new column. The enum defaults match each `from_db_str` fallback
/// (Windows / Idle); `provider` defaults to `""` (treated as Anthropic by the
/// resolver); scalars are zero/empty/false. Future `Option<T>` columns
/// automatically inherit `None` with no fixture edits.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "AgentNode.ts")]
pub struct AgentNode {
    #[ts(as = "i32")]
    pub id: i64,
    #[ts(as = "i32")]
    pub mesh_id: i64,
    pub name: String,
    pub path: String,         // absolute path to node directory
    /// Branch the worktree was cut from — **overloaded** based on spawn source.
    ///
    /// For issue-spawned, hand-spawned, and handover-spawned nodes, this holds
    /// the mesh's `base_ref` (resolved via `commands::git::get_default_branch`,
    /// typically `origin/main`). For PR-spawned nodes (`source_pr.is_some()`,
    /// issue #420), this holds the PR's `head_ref` instead, and
    /// `spawn_agent_inner` fetches `origin/<head_ref>` (or
    /// `fork-<owner>/<head_ref>` for fork PRs, issue #443) to cut the worktree
    /// from the same commits the PR is built on.
    ///
    /// Disambiguate with `source_pr.is_some()` — when set, treat this field
    /// as the PR head ref, otherwise as the mesh's base ref. The canonical
    /// reader is `spawn_agent_inner` in `agent/spawn.rs` (see
    /// `worktree_base_ref` derivation around `if node.source_pr.is_some()`);
    /// new readers should not reimplement the overload decision.
    pub branch: String,
    pub env: EnvType,         // windows or wsl
    /// Stored harness/profile id (e.g. "anthropic", "minimax", "terminal", or a
    /// user-defined profile id). Kept as an opaque `String` rather than the
    /// legacy [`Provider`] enum so user-defined harness profiles survive the
    /// DB round-trip — `Provider::from_db_str` would flatten any unknown id to
    /// Anthropic. Resolved to a concrete executor at the spawn seam via
    /// `preferences::resolve_harness_provider` (ADR-0014 / issue #535). Empty
    /// string is treated as "anthropic" by the resolver, so `Default` is a
    /// behaviour-preserving stub.
    pub provider: String,
    pub status: SessionStatus,
    pub cli_session_id: Option<String>, // Opaque ID from the agent CLI
    pub worktree_name: Option<String>,   // git worktree name (same as name for claude-backed providers)
    pub use_worktree: bool,  // true = commands run in worktree, false = repo root
    /// Whether the user has pinned this node for the Pinned Grid view
    /// (wayfinder #982). Persisted so a pinned node survives app restarts
    /// and stays in the user's focus set across sessions. Independent of
    /// `status` — a pinned node can be `running`, `idle`, `awaiting_input`,
    /// etc.; the view switcher reads `is_pinned` to render the Pinned Grid,
    /// not to filter by lifecycle state. Default is `false` (the column has
    /// a `NOT NULL DEFAULT 0`, so a node inserted before the column existed
    /// reads back as unpinned).
    pub is_pinned: bool,
    #[ts(as = "Option<i32>")]
    pub source_issue: Option<i64>,       // GitHub issue number that triggered this node
    /// GitHub PR number that triggered this node (issue #420). `None` for
    /// issue-spawned and hand-spawned nodes. When set, `spawn_agent_inner`
    /// fetches `origin/<head_ref>` and uses it as the worktree's `base_ref`
    /// instead of the mesh's `base_ref` (relates to #36 worktree adoption).
    /// Mirrors the `source_issue` field so the same plumbing can target
    /// both spawn sources.
    #[ts(as = "Option<i32>")]
    pub source_pr: Option<i64>,
    /// GitHub owner login of the PR's head repo (issue #443). Only set for
    /// PR-spawned nodes where the head lives on a fork — when `Some`,
    /// `spawn_agent_inner` runs `git remote add fork-<owner> <clone_url>` and
    /// fetches `fork-<owner>/<head_ref>` instead of `origin/<head_ref>`. For
    /// same-repo PRs the head's `repo.owner.login` is the destination owner,
    /// and the column stays `None` so the spawn path takes the #420 branch.
    /// `None` for issue-spawned and hand-spawned nodes.
    pub head_repo_owner: Option<String>,
    /// Clone URL of the PR's head repo (issue #443). Paired with
    /// [`head_repo_owner`](Self::head_repo_owner) — only set for fork PRs, used
    /// as the URL when registering `fork-<owner>` as a remote so the head ref
    /// can be fetched without the user pre-configuring it. `None` for
    /// same-repo PRs and for issue-spawned / hand-spawned nodes.
    pub head_repo_clone_url: Option<String>,
    /// PR's head commit SHA at spawn time (issue #444). Exact-pinning handle:
    /// `spawn_agent_inner` reads the local `origin/<head_ref>` SHA after
    /// `git fetch` and emits a `pr_sha_drift` warning via `mesh-sync-warning`
    /// if it no longer matches (force-push / rebase). `None` for v15 and
    /// earlier PR-spawned rows (the SHA wasn't known at insert time), and
    /// `None` for issue-spawned / hand-spawned nodes. The drift-check path
    /// branches on `Some(_)` so `None` skips the comparison rather than
    /// failing — same fail-open semantics as the `pr_head_unfetchable`
    /// fallback introduced in #420.
    pub source_pr_pinned_sha: Option<String>,
    #[ts(as = "i32")]
    pub position: i64,        // grid order within the mesh (drag-to-reorder); lower = earlier
    pub created_at: DateTime<Utc>,
}

/// A worktree whose node is already closed but whose on-disk directory still
/// needs removing. Recording the intent durably lets the slow, retry-prone
/// removal run in the background (or resume on next launch) without the node
/// lingering in the UI while it grinds. Drained by `process_pending_removals`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWorktreeRemoval {
    pub worktree_path: String,
    pub node_name: String,
}

/// A chat message in the agent session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: i64,
    pub session_id: i64,
    pub role: String,         // "user" or "assistant"
    pub content: String,
    pub tool_calls: Option<String>, // JSON array of tool calls if any
    pub created_at: DateTime<Utc>,
}

/// A script attached to a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionScript {
    pub id: i64,
    pub session_id: i64,
    pub script_type: String,  // "setup" | "run" | "archive"
    pub content: String,
}

/// File change event from the watcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: String, // "created" | "modified" | "deleted"
}

/// Diff result — a set of per-file diffs.
///
/// Generated to src/types/generated/DiffResult.ts (issue #404). `usize`
/// counters carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "DiffResult.ts")]
pub struct DiffResult {
    pub files: Vec<FileDiff>,
}

/// A single file diff.
///
/// Generated to src/types/generated/FileDiff.ts (issue #404). `usize`
/// counters carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[ts(export, export_to = "FileDiff.ts")]
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<DiffHunk>,
    /// Change kind: "added" | "modified" | "deleted" | "renamed" | "untracked".
    /// Matches the vocabulary `GitStatus.status` uses on the frontend.
    #[serde(default)]
    pub status: String,
    /// For renames, the path the file moved *from*; `None` otherwise.
    #[serde(default)]
    pub old_path: Option<String>,
    /// Added / removed line counts across the whole file (not just the
    /// context-bounded hunks), so summaries match `git diff --stat`.
    #[serde(default)]
    #[ts(as = "i32")]
    pub additions: usize,
    #[serde(default)]
    #[ts(as = "i32")]
    pub deletions: usize,
    /// True for binary files — `hunks` is empty and the UI shows a placeholder
    /// instead of dumping bytes.
    #[serde(default)]
    pub binary: bool,
}

/// A hunk within a diff.
///
/// Generated to src/types/generated/DiffHunk.ts (issue #404). `usize` hunk
/// line counters carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[ts(export, export_to = "DiffHunk.ts")]
pub struct DiffHunk {
    #[ts(as = "i32")]
    pub old_start: usize,
    #[ts(as = "i32")]
    pub old_lines: usize,
    #[ts(as = "i32")]
    pub new_start: usize,
    #[ts(as = "i32")]
    pub new_lines: usize,
    /// Full highlighted HTML for the old version of this hunk (side-by-side view)
    pub old_highlighted: String,
    /// Full highlighted HTML for the new version of this hunk (side-by-side view)
    pub new_highlighted: String,
    pub lines: Vec<DiffLine>,
    /// Per-line highlighted inline HTML, aligned 1:1 with `lines`. Lets the
    /// unified view colour each row with syntax highlighting while keeping its
    /// own add/remove background and gutter. Empty for producers that only feed
    /// the side-by-side view.
    #[serde(default)]
    pub lines_highlighted: Vec<String>,
}

/// A single line in a diff.
///
/// Generated to src/types/generated/DiffLine.ts (issue #404). `Option<usize>`
/// line numbers carry `#[ts(as = "Option<i32>")]` so they emit `number | null`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "DiffLine.ts")]
pub struct DiffLine {
    pub line_type: String,   // "context" | "add" | "remove"
    pub content: String,
    #[ts(as = "Option<i32>")]
    pub old_num: Option<usize>,
    #[ts(as = "Option<i32>")]
    pub new_num: Option<usize>,
}

/// Per-repo git prune info — branches, worktrees, and remote-tracking refs.
///
/// Generated to src/types/generated/GitRepoPruneInfo.ts (issue #404).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitRepoPruneInfo.ts")]
pub struct GitRepoPruneInfo {
    pub path: String,
    pub local_branches: Vec<BranchInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub remote_tracking_branches: Vec<String>,
}

/// A local branch and its prune-relevant metadata.
///
/// Generated to src/types/generated/BranchInfo.ts (issue #404). `u64` counters
/// carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "BranchInfo.ts")]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    /// None when the repo has no main/master branch to compare against.
    pub is_merged_into_main: Option<bool>,
    pub is_orphan: bool,
    /// `true` when a non-archived agent node has this branch checked out
    /// (sibling of `WorktreeInfo.is_active`). Mirrors the worktree's
    /// protection in the prune UI: the row's checkbox is disabled and a
    /// backend guard in `commands::prune::delete_branches` refuses to
    /// drop the branch — the user must close the node first.
    pub is_active: bool,
    /// `Some(worktree_path)` when this branch is the HEAD of *any* working
    /// tree on disk — main or linked worktree, agent-attached or orphan.
    /// Orthogonal to `is_active`: that field is about a live agent node;
    /// this one is about the on-disk git state. An orphan agent worktree
    /// (its node was deleted/archived but its directory survives) reads
    /// `is_active: false` here, so the user can no longer hit libgit2's
    /// "current HEAD of a linked repository" error from the prune UI.
    /// The UI uses this to disable the branch checkbox and direct the
    /// user to the worktree row above (whose delete already cascades to
    /// the branch via `remove_one_worktree_and_branch`).
    pub checked_out_in_worktree: Option<String>,
    pub has_uncommitted: bool,
    pub last_commit_date: Option<String>,
    #[ts(as = "i32")]
    pub ahead: u64,
    #[ts(as = "i32")]
    pub behind: u64,
}

/// A worktree (main or linked) and its prune-relevant metadata.
///
/// Generated to src/types/generated/WorktreeInfo.ts (issue #404).
///
/// `is_pool` distinguishes a pre-spawn pool entry from a normal worktree
/// (issue #611). Pool entries are detached-HEAD worktrees under
/// `{mesh.path}/.claude/worktrees/<slug>` whose path matches a row in
/// the `warm_worktrees` table; the Worktree Manager tab shows them with
/// a "Pre-spawn Pool" badge and disables the delete action so the
/// background worker can refill on demand. Always `false` for the
/// primary (repo-root) worktree — pool entries are always linked.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "WorktreeInfo.ts")]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub is_active: bool,
    pub is_stale: bool,
    pub is_pool: bool,
}

/// App settings stored in SQLite
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub default_projects_root: String,
    pub windows_cli_path: String,
    pub wsl_cli_path: String,
}

/// Typed view of a `Mesh` row — a 1:1 mirror of the user-tunable columns on
/// the `meshes` SQLite row (name, build/run commands, model, effort,
/// base_ref, use_worktree, worktree_mode, default_provider). This is the
/// single typed view of mesh config used by every consumer (frontend
/// properties, agent spawning, build/run). Construct it via
/// `MeshRow::from(&mesh)` — never hand-copy `Mesh` fields elsewhere.
///
/// **There is no `mesh.toml` file.** This struct is a thin DTO over a
/// `meshes` SQLite row (see `db::get_mesh_by_path`); every field on it
/// is a column on that row. The "config" in the previous name is
/// historical — before the DB columns existed, mesh settings lived in a
/// TOML file at the mesh root; that file was deleted when the columns
/// were added (see `docs/adr/` and `docs/specs/build-run-system.md` for
/// the migration history). New contributors reading `MeshRow` should
/// read it as "the DTO that mirrors a `meshes` row" and treat the
/// `meshes` table as the single source of truth. The `base_ref` field
/// is *also* mirrored into `.claude/settings.json` at the mesh root
/// (see `commands::mesh_properties::update_worktree_base_ref`) for
/// Claude Code to read; that mirror is an output, not an input to
/// spawn-time resolution.
///
/// Generated to src/types/generated/MeshRow.ts (issue #404 / issue #474).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "MeshRow.ts")]
pub struct MeshRow {
    pub name: Option<String>,
    pub build_command: Option<String>,
    pub run_command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub base_ref: Option<String>,
    pub use_worktree: bool,
    pub worktree_mode: Option<String>,
    pub default_provider: Option<String>,
    /// OS-level sandbox toggle (macOS Seatbelt #497, Windows AppContainer
    /// #498) — see [`Mesh::sandbox`]. The column is one; the OS-specific
    /// spawn policy is decided at `spawn_environment::wrap` time.
    pub sandbox: bool,
    /// Per-mesh pre-spawn pool target — see [`Mesh::pre_spawn_pool_size`].
    /// `0` = pool off, `1..=5` = target the worker fills to. Surfaced in
    /// the Worktrees Probe's ConfigurationCard (issue #611).
    #[ts(as = "i32")]
    pub pre_spawn_pool_size: i32,
    /// Autopilot Policy (issue #481) — see the matching [`Mesh`] fields.
    pub autopilot_enabled: bool,
    pub autopilot_trigger_label: Option<String>,
    #[ts(as = "i32")]
    pub autopilot_concurrency_limit: i32,
    pub autopilot_provider: Option<String>,
    pub autopilot_action_on_success: Option<String>,
    /// Per-context build/run commands (issue #802). When set, a Root Node
    /// runs these instead of `build_command` / `run_command`; `None` falls
    /// back to those. See the matching [`Mesh`] fields.
    pub root_build_command: Option<String>,
    pub root_run_command: Option<String>,
    /// Looping Autopilot configuration (wayfinder #990 / ticket #991).
    /// See the matching [`Mesh`] fields — every `loop_*` column is
    /// surfaced here so the dedicated Autopilot Probe UI tab (ticket #994)
    /// reads & writes them through the same `get_mesh_properties` IPC
    /// boundary. `loop_consecutive_failures` IS the configured auto-pause
    /// threshold (default `0` = feature off), NOT a runtime failure count —
    /// the running count lives in process state (`#992` follow-up).
    pub autopilot_mode: AutopilotMode,
    pub loop_initial_prompt: Option<String>,
    pub loop_suffix_prompt: Option<String>,
    pub loop_max_iterations: Option<i32>,
    pub loop_interval_seconds: i32,
    pub loop_consecutive_failures: i32,
    /// **Per-Mesh harness overrides** (issue #1151 / slice 2 of #1148) —
    /// see the matching [`Mesh`] field. Surface for the Mesh Properties
    /// "Per-harness overrides" experience; the legacy `model` / `effort`
    /// fields stay here so a pre-v33 reading client doesn't crash, but
    /// the new UI ignores them.
    pub harness_overrides: HashMap<String, HarnessConfigValue>,
}

impl From<&Mesh> for MeshRow {
    fn from(mesh: &Mesh) -> Self {
        Self {
            name: if mesh.name.is_empty() { None } else { Some(mesh.name.clone()) },
            build_command: mesh.build_command.clone(),
            run_command: mesh.run_command.clone(),
            model: mesh.model.clone(),
            effort: mesh.effort.clone(),
            base_ref: Some(mesh.base_ref.clone()),
            use_worktree: mesh.use_worktree,
            worktree_mode: mesh.worktree_mode.clone(),
            default_provider: mesh.default_provider.clone(),
            sandbox: mesh.sandbox,
            pre_spawn_pool_size: mesh.pre_spawn_pool_size,
            autopilot_enabled: mesh.autopilot_enabled,
            autopilot_trigger_label: mesh.autopilot_trigger_label.clone(),
            autopilot_concurrency_limit: mesh.autopilot_concurrency_limit,
            autopilot_provider: mesh.autopilot_provider.clone(),
            autopilot_action_on_success: mesh.autopilot_action_on_success.clone(),
            root_build_command: mesh.root_build_command.clone(),
            root_run_command: mesh.root_run_command.clone(),
            autopilot_mode: mesh.autopilot_mode,
            loop_initial_prompt: mesh.loop_initial_prompt.clone(),
            loop_suffix_prompt: mesh.loop_suffix_prompt.clone(),
            loop_max_iterations: mesh.loop_max_iterations,
            loop_interval_seconds: mesh.loop_interval_seconds,
            loop_consecutive_failures: mesh.loop_consecutive_failures,
            harness_overrides: mesh.harness_overrides.clone(),
        }
    }
}

/// A worktree that currently holds the Base Ref's branch checked out,
/// blocking `git checkout <base>` from the Mesh root. Returned by
/// `get_mesh_health` as part of `MeshHealth.base_branch_holder`.
///
/// `path` is the on-disk worktree path (host-normalised). `name` is the
/// basename for display. `is_active` reflects whether a non-archived
/// agent node currently points at the path (active worktrees can't be
/// freely deleted but can be safely detached via `free_base_branch`).
///
/// Generated to src/types/generated/HoldingWorktree.ts (issue #404).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "HoldingWorktree.ts")]
pub struct HoldingWorktree {
    pub path: String,
    pub name: String,
    pub is_active: bool,
}

/// A single-snapshot read of a Mesh's Git health — what the Base Ref
/// resolved to, where HEAD actually is, and whether recovery is
/// currently safe. Computed by `commands::git::compute_mesh_health` and
/// returned over IPC by the `get_mesh_health` Tauri command.
///
/// Detection rules (see the `MeshHealth` doc on the command side for the
/// full reasoning):
/// - `local_base_branch` is derived from `base_ref` (e.g. `origin/main` → `main`).
/// - `is_drifted = true` when `current_branch != local_base_branch` or HEAD
///   is detached on a non-base OID. Detached at the base branch's OID is
///   not drifted — close enough to base that no badge is needed.
/// - `unpushed_ahead` is 0 when there is no upstream; a no-upstream branch
///   with local commits still triggers the "unpushed" guard because the
///   branch ref is the only handle to those commits.
/// - `base_branch_holder` is `Some` when the Base Ref's branch is checked
///   out in any of the Mesh's worktrees (main or linked).
///
/// Generated to src/types/generated/MeshHealth.ts (issue #404). `u32`
/// counter carries `#[ts(as = "i32")]` so it emits `number`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "MeshHealth.ts")]
pub struct MeshHealth {
    pub base_ref: String,
    pub local_base_branch: Option<String>,
    pub current_branch: Option<String>,
    pub current_short_sha: String,
    pub is_detached: bool,
    pub is_dirty: bool,
    #[ts(as = "i32")]
    pub unpushed_ahead: u32,
    pub has_upstream: bool,
    pub is_drifted: bool,
    pub base_branch_holder: Option<HoldingWorktree>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample_mesh() -> Mesh {
        // Spread `..Default::default()` for every field this fixture doesn't
        // intentionally exercise (issue #518 follow-up to #457). Keeping only
        // the fields the consumer tests assert on: name + base_ref + the
        // `Option<T>` columns that need a non-default value, plus the two
        // scalar toggles (`use_worktree`, `sandbox`) whose values the tests
        // pin explicitly. A future `Mesh` column just needs to be added to
        // the struct — this fixture stays unchanged.
        Mesh {
            id: 1,
            name: "demo".to_string(),
            path: "/repo".to_string(),
            build_command: Some("npm run build".to_string()),
            model: Some("opus".to_string()),
            worktree_mode: Some("branched".to_string()),
            base_ref: "origin/main".to_string(),
            use_worktree: false,
            sandbox: true,
            ..Default::default()
        }
    }

    #[test]
    fn mesh_row_from_mesh_maps_all_fields() {
        let cfg = MeshRow::from(&sample_mesh());
        assert_eq!(cfg.name.as_deref(), Some("demo"));
        assert_eq!(cfg.build_command.as_deref(), Some("npm run build"));
        assert_eq!(cfg.run_command, None);
        assert_eq!(cfg.model.as_deref(), Some("opus"));
        assert_eq!(cfg.effort, None);
        // base_ref always carries a value (DB COALESCEs to 'origin/main')
        assert_eq!(cfg.base_ref.as_deref(), Some("origin/main"));
        assert!(!cfg.use_worktree);
        assert_eq!(cfg.worktree_mode.as_deref(), Some("branched"));
        assert_eq!(cfg.default_provider, None);
        assert!(cfg.sandbox, "sandbox toggle must map through MeshRow::from");
        // #802 — root_* commands are None on a mesh that never set them, so
        // the build_run resolver falls back to build_command / run_command.
        assert_eq!(cfg.root_build_command, None);
        assert_eq!(cfg.root_run_command, None);
        // Wayfinder #990 / ticket #991 — looping autopilot config mirrors
        // through MeshRow::from exactly like the other Mesh fields.
        assert_eq!(cfg.autopilot_mode, AutopilotMode::IssueDriven);
        assert_eq!(cfg.loop_initial_prompt, None);
        assert_eq!(cfg.loop_suffix_prompt, None);
        assert_eq!(cfg.loop_max_iterations, None);
        assert_eq!(cfg.loop_interval_seconds, 0);
        assert_eq!(cfg.loop_consecutive_failures, 0);
    }

    /// #802 — a mesh that DID configure per-context commands must round-trip
    /// both new columns through `MeshRow::from`.
    #[test]
    fn mesh_row_from_mesh_maps_root_commands() {
        let mut mesh = sample_mesh();
        mesh.root_build_command = Some("cargo build --workspace".to_string());
        mesh.root_run_command = Some("cargo run -p app".to_string());
        let cfg = MeshRow::from(&mesh);
        assert_eq!(cfg.root_build_command.as_deref(), Some("cargo build --workspace"));
        assert_eq!(cfg.root_run_command.as_deref(), Some("cargo run -p app"));
    }

    #[test]
    fn mesh_row_from_mesh_blank_name_is_none() {
        let mut mesh = sample_mesh();
        mesh.name = String::new();
        assert_eq!(MeshRow::from(&mesh).name, None);
    }

    /// Wayfinder #990 / ticket #991 — a mesh that DID configure looping
    /// autopilot must round-trip ALL six columns through `MeshRow::from`.
    #[test]
    fn mesh_row_from_mesh_maps_loop_config() {
        let mut mesh = sample_mesh();
        mesh.autopilot_mode = AutopilotMode::Looping;
        mesh.loop_initial_prompt = Some("iterate the planner".to_string());
        mesh.loop_suffix_prompt = Some("now write tests".to_string());
        mesh.loop_max_iterations = Some(7);
        mesh.loop_interval_seconds = 60;
        mesh.loop_consecutive_failures = 2;
        let cfg = MeshRow::from(&mesh);
        assert_eq!(cfg.autopilot_mode, AutopilotMode::Looping);
        assert_eq!(cfg.loop_initial_prompt.as_deref(), Some("iterate the planner"));
        assert_eq!(cfg.loop_suffix_prompt.as_deref(), Some("now write tests"));
        assert_eq!(cfg.loop_max_iterations, Some(7));
        assert_eq!(cfg.loop_interval_seconds, 60);
        assert_eq!(cfg.loop_consecutive_failures, 2);
    }

    /// Regression test for issue #457: `AgentNode::default()` exists so future
    /// optional columns only need to be added to the struct, not to 8 test
    /// fixtures. Defaults are chosen to match each enum's `from_db_str`
    /// fallback semantics (`EnvType::Windows`, `Provider::Anthropic`,
    /// `SessionStatus::Idle`) so an AgentNode built from `..Default::default()`
    /// is a meaningful "no row loaded" stub — not a value that would silently
    /// masquerade as a real row.
    #[test]
    fn agent_node_default_matches_fallback_semantics() {
        let n = AgentNode::default();
        assert_eq!(n.id, 0);
        assert_eq!(n.mesh_id, 0);
        assert_eq!(n.name, "");
        assert_eq!(n.path, "");
        assert_eq!(n.branch, "");
        assert_eq!(n.env, EnvType::Windows);
        // provider is now an opaque String; default is "" (resolver treats it
        // as anthropic), so the fallback semantics are preserved (issue #535).
        assert_eq!(n.provider, "");
        assert_eq!(n.status, SessionStatus::Idle);
        assert_eq!(n.cli_session_id, None);
        assert_eq!(n.worktree_name, None);
        assert!(!n.use_worktree);
        assert_eq!(n.source_issue, None);
        assert_eq!(n.source_pr, None);
        assert_eq!(n.head_repo_owner, None);
        assert_eq!(n.head_repo_clone_url, None);
        assert_eq!(n.source_pr_pinned_sha, None);
        assert_eq!(n.position, 0);
        // DateTime<Utc>::default() == UNIX epoch — not "now", but a
        // well-defined placeholder that won't accidentally match a real row.
        assert_eq!(n.created_at, chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap());
    }

    /// Companion to the above: a partially-overridden literal must compile
    /// cleanly via `..Default::default()`, which is the migration pattern the
    /// 8 fixtures will adopt. Pins the spread-syntax contract so a future
    /// `non_exhaustive` or similar can't quietly break it.
    #[test]
    fn agent_node_partial_with_default_spread_works() {
        let n = AgentNode {
            id: 7,
            path: "/tmp/fix-login".to_string(),
            ..Default::default()
        };
        assert_eq!(n.id, 7);
        assert_eq!(n.path, "/tmp/fix-login");
        // Untouched fields keep their defaults.
        assert_eq!(n.env, EnvType::Windows);
        assert_eq!(n.provider, "");
        assert_eq!(n.status, SessionStatus::Idle);
    }

    /// Regression test for issue #518: `Mesh::default()` exists so future
    /// optional columns only need to be added to the struct, not to
    /// `sample_mesh()`. Follow-up to #457 (which did the same for
    /// `AgentNode`).
    ///
    /// Option A semantics (zero-value stub — see issue body): every field
    /// at its mechanical zero. There is no enum on `Mesh` to align with a
    /// `from_db_str` fallback, so the choice is uniform. A `Mesh` built
    /// from `..Default::default()` is a "no row loaded" placeholder — not
    /// a value that would silently masquerade as a real DB row.
    #[test]
    fn mesh_default_matches_fallback_semantics() {
        let m = Mesh::default();
        assert_eq!(m.id, 0);
        assert_eq!(m.name, "");
        assert_eq!(m.path, "");
        assert_eq!(m.layout, "");
        assert_eq!(m.position, 0);
        assert_eq!(
            m.created_at,
            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap()
        );
        assert_eq!(m.build_command, None);
        assert_eq!(m.run_command, None);
        assert_eq!(m.model, None);
        assert_eq!(m.effort, None);
        assert!(!m.use_worktree);
        assert_eq!(m.worktree_mode, None);
        assert_eq!(m.default_provider, None);
        assert_eq!(m.base_ref, "");
        assert_eq!(m.scratchpad, "");
        assert!(!m.sandbox);
        assert_eq!(m.root_build_command, None);
        assert_eq!(m.root_run_command, None);
        // Wayfinder #990 / ticket #991 — looping autopilot config: zero /
        // default per Option A (issue #518), with the `#[default]` enum
        // variant on the autopilot_mode carrying its pre-v30 behaviour.
        assert_eq!(m.autopilot_mode, AutopilotMode::IssueDriven);
        assert_eq!(m.loop_initial_prompt, None);
        assert_eq!(m.loop_suffix_prompt, None);
        assert_eq!(m.loop_max_iterations, None);
        assert_eq!(m.loop_interval_seconds, 0);
        assert_eq!(m.loop_consecutive_failures, 0);
    }

    /// Companion to the above: a partially-overridden literal must compile
    /// cleanly via `..Default::default()`, which is the migration pattern
    /// `sample_mesh()` will adopt. Pins the spread-syntax contract so a
    /// future `non_exhaustive` or similar can't quietly break it.
    #[test]
    fn mesh_partial_with_default_spread_works() {
        let m = Mesh {
            id: 7,
            name: "fixture".to_string(),
            path: "/repo".to_string(),
            ..Default::default()
        };
        assert_eq!(m.id, 7);
        assert_eq!(m.name, "fixture");
        assert_eq!(m.path, "/repo");
        // Untouched fields keep their defaults.
        assert!(!m.use_worktree);
        assert!(!m.sandbox);
        assert_eq!(m.base_ref, "");
    }

    #[test]
    fn session_status_round_trip_all_variants() {
        let variants = [
            SessionStatus::Pending,
            SessionStatus::Spawning,
            SessionStatus::Running,
            SessionStatus::Idle,
            SessionStatus::AwaitingInput,
            SessionStatus::Error,
            SessionStatus::Archived,
            SessionStatus::Suspended,
        ];
        for status in variants {
            let db_str = status.to_db_str();
            let parsed = SessionStatus::from_db_str(db_str);
            assert_eq!(parsed, status, "round-trip failed for {:?}", status);
        }
    }

    #[test]
    fn session_status_serializes_to_wire_as_its_db_string() {
        // `AgentNode` is serialised straight from the serde derive on both the
        // Tauri and HTTP transports, so the JSON wire value of `status` MUST
        // equal the DB string (which the frontend also compares against).
        // `rename_all = "lowercase"` silently emitted "awaitinginput" (no
        // underscore) for the one multi-word variant — a value no consumer
        // ever matched, masked because the UI sets that status client-side.
        // Issue #359.
        let variants = [
            SessionStatus::Pending,
            SessionStatus::Spawning,
            SessionStatus::Running,
            SessionStatus::Idle,
            SessionStatus::AwaitingInput,
            SessionStatus::Error,
            SessionStatus::Archived,
            SessionStatus::Suspended,
        ];
        for status in variants {
            let wire = serde_json::to_value(status).unwrap();
            assert_eq!(
                wire,
                serde_json::Value::String(status.to_db_str().to_string()),
                "wire serialization of {status:?} must match its DB string",
            );
        }
    }

    #[test]
    fn session_status_unknown_string_defaults_to_idle() {
        assert_eq!(SessionStatus::from_db_str("garbage"), SessionStatus::Idle);
        assert_eq!(SessionStatus::from_db_str(""), SessionStatus::Idle);
        assert_eq!(SessionStatus::from_db_str("RUNNING"), SessionStatus::Idle);
    }

    // Wayfinder #990 / ticket #991 — AutopilotMode is a wire-shape mirror of
    // SessionStatus: same snake_case rename, same "DB string == wire value"
    // contract, same fail-open unknown-strings-degrade-to-default semantics.
    // The defensive tests below pin all three.

    #[test]
    fn autopilot_mode_round_trip_all_variants() {
        for &mode in [AutopilotMode::IssueDriven, AutopilotMode::Looping].iter() {
            let db_str = match mode {
                AutopilotMode::IssueDriven => "issue_driven",
                AutopilotMode::Looping => "looping",
            };
            assert_eq!(
                AutopilotMode::from_db_str(db_str),
                mode,
                "round-trip failed for {mode:?}"
            );
        }
    }

    #[test]
    fn autopilot_mode_serializes_to_wire_as_its_snake_case_string() {
        // Wire shape MUST equal the DB string the column stores, the same
        // way SessionStatus does (issue #359). The `rename_all = "snake_case"`
        // serde attribute is the bridge; without it, `Looping` would land
        // on the wire as `"Looping"` and the frontend enum-compare would
        // silently miss every Looping-mode mesh.
        for mode in [AutopilotMode::IssueDriven, AutopilotMode::Looping] {
            let wire = serde_json::to_value(mode).unwrap();
            let expected = match mode {
                AutopilotMode::IssueDriven => "issue_driven",
                AutopilotMode::Looping => "looping",
            };
            assert_eq!(
                wire,
                serde_json::Value::String(expected.to_string()),
                "wire serialization of {mode:?} must match its snake_case DB string"
            );
        }
    }

    #[test]
    fn autopilot_mode_unknown_string_defaults_to_issue_driven() {
        // Fail-open: a row written by a future build with an unknown mode
        // string degrades to IssueDriven (the pre-v30 behaviour), so the
        // poller keeps working — the alternative (degrading to Looping)
        // would silently spin up a configured-but-failed Looping mesh.
        assert_eq!(AutopilotMode::from_db_str("garbage"), AutopilotMode::IssueDriven);
        assert_eq!(AutopilotMode::from_db_str(""), AutopilotMode::IssueDriven);
        assert_eq!(AutopilotMode::from_db_str("Looping"), AutopilotMode::IssueDriven);
    }

    #[test]
    fn provider_adapter_recipe_windows() {
        use crate::agent::provider::Platform;
        // MiniMax and Kimi were retired from the legacy enum (#538) — Claude-compatible
        // endpoints are now harness profiles whose per-account env is injected separately
        // by the unified `anthropic` adapter via `claude_direct_recipe`.
        assert_eq!(Provider::Anthropic.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "claude.exe");
        assert_eq!(Provider::Agy.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "agy");
        assert_eq!(Provider::OpenCode.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "opencode");
        assert_eq!(Provider::Codex.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "codex");
        // Plain terminal spawns the OS-preferred shell directly — powershell.exe on Windows
        // host, routed through wsl.exe by spawn_environment::wrap when env_type is WSL.
        assert_eq!(Provider::Terminal.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "powershell.exe");
    }

    #[test]
    fn provider_adapter_recipe_macos_anthropic_uses_claude() {
        use crate::agent::provider::Platform;
        assert_eq!(Provider::Anthropic.adapter().spawn_recipe(Platform::Macos, EnvType::Windows).binary, "claude");
    }

    #[test]
    fn provider_capabilities_split_correctly() {
        assert!(Provider::Anthropic.adapter().supports_resume());
        assert!(Provider::Agy.adapter().supports_resume());
        assert!(!Provider::OpenCode.adapter().supports_resume());
        assert!(Provider::Codex.adapter().supports_resume());
        // Kimi Code (wayfinder #918) is a native TUI harness like Codex/Grok
        // — its adapter declares resume + model override, no prefill, no
        // Claude-style attention hook. Pin the matrix so a future adapter
        // refactor that drops Kimi from the resume path trips this test.
        assert!(Provider::Kimi.adapter().supports_resume());
        assert!(Provider::Kimi.adapter().supports_model_override());
        assert!(!Provider::Kimi.adapter().requires_attention_hook());
        assert!(Provider::Mcode.adapter().supports_resume());
        assert!(Provider::Mcode.adapter().supports_model_override());
        assert!(!Provider::Mcode.adapter().requires_attention_hook());
    }

    /// The "produces a readable transcript" capability (#317) — the Claude-backed
    /// `anthropic` adapter (which also runs custom MiniMax/DeepSeek profiles)
    /// and Codex (whose rollout format the reader parses since #887) write
    /// transcripts the coordinator read API can drill into. Kimi Code's
    /// `wire.jsonl` is standard JSONL (#911 research), but the reader's
    /// path resolver isn't wired for `~/.kimi/` yet — Kimi returns `false`
    /// and the Node Digest rich layer degrades to spine-only with
    /// `unsupported`. Everything else degrades the same way; this matrix
    /// is load-bearing.
    #[test]
    fn only_transcript_writing_providers_produce_a_readable_transcript() {
        assert!(Provider::Anthropic.adapter().produces_readable_transcript());
        // Codex's rollout format is parsed via TranscriptFormat::Codex (#887).
        assert!(Provider::Codex.adapter().produces_readable_transcript());
        // Kimi Code (#918) — reader wiring is the follow-up; capability
        // claim is honest at `false` until then.
        assert!(!Provider::Kimi.adapter().produces_readable_transcript());
        // Cursor writes compatible JSONL, but its ~/.cursor/projects layout
        // still needs reader/archive-discovery wiring.
        assert!(!Provider::Cursor.adapter().produces_readable_transcript());
        assert!(!Provider::Mcode.adapter().produces_readable_transcript());
        assert!(!Provider::Agy.adapter().produces_readable_transcript());
        assert!(!Provider::OpenCode.adapter().produces_readable_transcript());
        assert!(!Provider::Terminal.adapter().produces_readable_transcript());
    }

    #[test]
    fn provider_from_db_str_round_trip_for_known_values() {
        for &p in Provider::all() {
            let s = p.to_string();
            assert_eq!(Provider::from_db_str(&s), p);
        }
    }

    /// Documents the intentional silent fallback: unknown DB values default to
    /// Anthropic rather than erroring. This preserves pre-refactor behaviour
    /// where the inline `match` in db/mod.rs had a `_ => Provider::Anthropic` arm.
    /// The fallback now also emits a `tracing::warn!` (covered by
    /// `provider_from_db_str_unknown_logs_warning`).
    #[test]
    fn provider_from_db_str_unknown_falls_back_to_anthropic() {
        assert_eq!(Provider::from_db_str("garbage"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str(""), Provider::Anthropic);
    }

    /// Regression test for #297: hand-edited `preferences.json` with a
    /// capitalised value like `"Terminal"` previously silently resolved to
    /// `Anthropic` while the DB persisted the capitalised string — a
    /// particularly nasty mismatch. The fix lowercases + trims the input
    /// so every recognised name resolves regardless of case.
    #[test]
    fn provider_from_db_str_is_case_insensitive() {
        assert_eq!(Provider::from_db_str("Terminal"), Provider::Terminal);
        assert_eq!(Provider::from_db_str("TERMINAL"), Provider::Terminal);
        assert_eq!(Provider::from_db_str("TeRmInAl"), Provider::Terminal);
        assert_eq!(Provider::from_db_str("ANTHROPIC"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str("AGY"), Provider::Agy);
        assert_eq!(Provider::from_db_str("OpenCode"), Provider::OpenCode);
        assert_eq!(Provider::from_db_str("Codex"), Provider::Codex);
        assert_eq!(Provider::from_db_str("Mcode"), Provider::Mcode);
    }

    /// Hard cutover (issue #538): "minimax" is no longer a first-class executor.
    /// With no configured harness profile it falls through to the Anthropic
    /// default — `resolve_harness_provider` checks profiles first, so a
    /// configured custom account still resolves to the right backend env.
    ///
    /// "kimi" USED to fall through here too — Kimi Code (wayfinder #918) is now
    /// a native binary executor, so it resolves to `Provider::Kimi` directly.
    /// See `provider_from_db_str_kimi_resolves_to_native_harness` below.
    #[test]
    fn provider_from_db_str_legacy_minimax_falls_back_to_anthropic() {
        assert_eq!(Provider::from_db_str("minimax"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str("Minimax"), Provider::Anthropic);
    }

    /// Kimi Code (wayfinder #918) ships a native CLI on PATH as `kimi` — the
    /// legacy "Kimi LLM endpoint via Claude Code" interpretation has been
    /// retired (the `kimi` ProviderAccount is now self_auth, matching `grok`).
    /// A bare `"kimi"` in `AgentNode.provider` (or a user `default_provider`
    /// setting) now resolves to the native Kimi Code executor directly, no
    /// profile lookup needed.
    #[test]
    fn provider_from_db_str_kimi_resolves_to_native_harness() {
        assert_eq!(Provider::from_db_str("kimi"), Provider::Kimi);
        assert_eq!(Provider::from_db_str("Kimi"), Provider::Kimi);
        assert_eq!(Provider::from_db_str("KIMI"), Provider::Kimi);
    }

    /// MiniMax Code CLI (`mcode`) is a native binary executor on PATH as `mcode`.
    #[test]
    fn provider_from_db_str_mcode_resolves_to_native_harness() {
        assert_eq!(Provider::from_db_str("mcode"), Provider::Mcode);
        assert_eq!(Provider::from_db_str("Mcode"), Provider::Mcode);
        assert_eq!(Provider::from_db_str("MCODE"), Provider::Mcode);
        assert_eq!(Provider::from_db_str("minimax-code"), Provider::Mcode);
    }

    /// Whitespace around the value shouldn't break matching either — a
    /// hand-edited JSON file with `"Terminal "` (trailing space) should
    /// still resolve correctly.
    #[test]
    fn provider_from_db_str_trims_whitespace() {
        assert_eq!(Provider::from_db_str("  terminal  "), Provider::Terminal);
        assert_eq!(Provider::from_db_str("\tcodex\n"), Provider::Codex);
    }

    // Per-thread capture buffer for tracing events emitted inside
    // `from_db_str`. `thread_local!` (rather than a per-test
    // `Arc<Mutex<Vec<u8>>>`) guarantees events from other test threads
    // can't bleed into this thread's buffer under parallel `cargo test` —
    // issue #1007. The buffer persists across tests scheduled on the same
    // OS thread, so `capture_warnings` drains it on entry.
    thread_local! {
        static WARN_BUFFER: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// `MakeWriter` that appends to the thread-local `WARN_BUFFER`. A unit
    /// struct because there's nothing to clone — the address lives in the
    /// `thread_local!`.
    struct ThreadLocalWriter;

    impl Write for ThreadLocalWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            WARN_BUFFER.with(|cell| cell.borrow_mut().extend_from_slice(buf));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLocalWriter {
        type Writer = ThreadLocalWriter;
        fn make_writer(&'a self) -> Self::Writer {
            ThreadLocalWriter
        }
    }

    /// Run `body` under a subscriber that captures WARN-level tracing events
    /// into the thread-local `WARN_BUFFER`, returning the captured string.
    /// Used by the `provider_from_db_str_*_warning` tests to assert on log
    /// output.
    fn capture_warnings<F: FnOnce()>(body: F) -> String {
        // Drain in case a prior test on this OS thread left bytes behind.
        WARN_BUFFER.with(|cell| cell.borrow_mut().clear());

        let subscriber = tracing_subscriber::fmt()
            .with_writer(ThreadLocalWriter)
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();

        tracing::subscriber::with_default(subscriber, body);

        let result = String::from_utf8(WARN_BUFFER.with(|cell| cell.borrow().clone())).unwrap();
        // Tidy up so the buffer doesn't accumulate across tests on this OS thread.
        WARN_BUFFER.with(|cell| cell.borrow_mut().clear());
        result
    }

    /// Issue #297: a misspelled provider id in `preferences.json` (e.g.
    /// `"antropic"`, `"claude"`, `"gpt"`) used to be invisible. After the
    /// fix the function emits a `warn!` so the silent fallback shows up in
    /// `buildmesh.log` next to the offending value.
    #[test]
    fn provider_from_db_str_unknown_logs_warning() {
        // Typo: missing the second 'h' — must fall back AND warn.
        let captured = capture_warnings(|| {
            assert_eq!(Provider::from_db_str("antropic"), Provider::Anthropic);
        });
        assert!(
            captured.contains("unrecognized provider") && captured.contains("antropic"),
            "expected warn! mentioning 'antropic', got: {}",
            captured
        );
    }

    /// Known values — even when capitalised — should NOT emit a warning;
    /// the case-insensitive match is the intended path, not a fallback.
    #[test]
    fn provider_from_db_str_known_value_does_not_warn() {
        let captured = capture_warnings(|| {
            assert_eq!(Provider::from_db_str("Terminal"), Provider::Terminal);
            assert_eq!(Provider::from_db_str("ANTHROPIC"), Provider::Anthropic);
            assert_eq!(Provider::from_db_str("  codex  "), Provider::Codex);
        });
        assert!(
            !captured.contains("unrecognized provider"),
            "expected no warn! for known (case-insensitive) value, got: {}",
            captured
        );
    }

    /// Regression test for #1007: the previous `VecWriter` (with per-test
    /// `Arc<Mutex<Vec<u8>>>`) allowed events from other test threads to
    /// bleed into the capture buffer under parallel `cargo test`. This test
    /// runs `capture_warnings` on 16 threads simultaneously via a `Barrier`
    /// and asserts each thread's captured output contains ONLY its own
    /// marker — under `ThreadLocalWriter` this passes deterministically.
    ///
    /// Markers use fixed-width zero-padded suffixes (`_00`…`_15`) so that
    /// `_1` cannot be a substring of `_10`…`_15` (a false positive that
    /// bit the first version of this test).
    #[test]
    fn capture_warnings_isolates_buffers_per_thread() {
        use std::sync::{Arc, Barrier};
        use std::thread;
        const THREADS: usize = 16;
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);
        for t in 0..THREADS {
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let marker = format!("capture_warnings_thread_marker_{t:02}");
                barrier.wait();
                capture_warnings(|| {
                    tracing::warn!("{marker}");
                })
            }));
        }
        let results: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().expect("worker thread panicked"))
            .collect();
        for (t, captured) in results.iter().enumerate() {
            let my_marker = format!("capture_warnings_thread_marker_{t:02}");
            assert!(
                captured.contains(&my_marker),
                "thread {t} must see its own marker, got: {captured}"
            );
            for other in 0..THREADS {
                if other == t {
                    continue;
                }
                let other_marker = format!("capture_warnings_thread_marker_{other:02}");
                assert!(
                    !captured.contains(&other_marker),
                    "thread {t} saw thread {other}'s marker — cross-thread bleed: {captured}"
                );
            }
        }
    }

    #[test]
    fn provider_display_lowercase() {
        assert_eq!(format!("{}", Provider::Anthropic), "anthropic");
        assert_eq!(format!("{}", Provider::Agy), "agy");
        assert_eq!(format!("{}", Provider::OpenCode), "opencode");
        assert_eq!(format!("{}", Provider::Codex), "codex");
        assert_eq!(format!("{}", Provider::Cursor), "cursor");
        assert_eq!(format!("{}", Provider::Grok), "grok");
        assert_eq!(format!("{}", Provider::Kimi), "kimi");
        assert_eq!(format!("{}", Provider::Mcode), "mcode");
        assert_eq!(format!("{}", Provider::Terminal), "terminal");
    }

    #[test]
    fn codex_uses_subcommand_resume_recipe() {
        use crate::agent::provider::Platform;
        let adapter = Provider::Codex.adapter();
        let resume = adapter.spawn_recipe_for_resume(Platform::Macos, "abc-123");
        assert!(resume.is_some());
        let recipe = resume.unwrap();
        assert_eq!(recipe.binary, "codex");
        assert_eq!(recipe.base_args[0], "resume");
        assert_eq!(recipe.base_args[1], "abc-123");
    }

    #[test]
    fn codex_self_assigns_session_ids() {
        let adapter = Provider::Codex.adapter();
        assert!(adapter.self_assigns_session_id());
        assert!(adapter.session_assign_args("test-id").is_empty());
        assert!(adapter.resume_args("test-id").is_empty());
    }

    #[test]
    fn other_providers_do_not_self_assign() {
        assert!(!Provider::Anthropic.adapter().self_assigns_session_id());
        assert!(!Provider::OpenCode.adapter().self_assigns_session_id());
        assert!(!Provider::Terminal.adapter().self_assigns_session_id());
    }

    /// `is_plain_terminal` is the single trait method that switches the
    /// spawn pipeline's reader EOF handling. Only the Terminal provider
    /// overrides the default `false`. This test guards against accidental
    /// flipping by future refactors — if any LLM provider were ever to
    /// claim "plain terminal" semantics, the spawn path would silently
    /// stop emitting `resume-failed` events for it, breaking a real
    /// LLM-resume signal.
    #[test]
    fn is_plain_terminal_only_for_terminal() {
        assert!(Provider::Terminal.adapter().is_plain_terminal());
        assert!(!Provider::Anthropic.adapter().is_plain_terminal());
        assert!(!Provider::Agy.adapter().is_plain_terminal());
        assert!(!Provider::OpenCode.adapter().is_plain_terminal());
        assert!(!Provider::Codex.adapter().is_plain_terminal());
    }

    #[test]
    fn codex_prefill_is_positional() {
        let adapter = Provider::Codex.adapter();
        let args = adapter.prefill_args("fix the auth bug");
        assert_eq!(args, vec!["fix the auth bug"]);
    }

    #[test]
    fn env_type_display_lowercase() {
        assert_eq!(format!("{}", EnvType::Windows), "windows");
        assert_eq!(format!("{}", EnvType::Wsl), "wsl");
    }

    #[test]
    fn session_status_serde_json_snake_case() {
        // Was `session_status_serde_json_lowercase`, which asserted the buggy
        // "awaitinginput" — it pinned the wire format without ever checking it
        // against the "awaiting_input" the DB and frontend use. Issue #359
        // switched the enum to snake_case; see
        // `session_status_serializes_to_wire_as_its_db_string` for the full
        // per-variant guard.
        let json = serde_json::to_string(&SessionStatus::AwaitingInput).unwrap();
        assert_eq!(json, "\"awaiting_input\"");
        let json = serde_json::to_string(&SessionStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
    }
}
