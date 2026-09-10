//! Agent-node and session wire types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
    /// Windows executable reached through interoperability from a Linux WSL host.
    WindowsInterop,
}

impl std::fmt::Display for EnvType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvType::Windows => write!(f, "windows"),
            EnvType::Wsl => write!(f, "wsl"),
            EnvType::WindowsInterop => write!(f, "windowsinterop"),
        }
    }
}

impl From<crate::env::Environment> for EnvType {
    fn from(env: crate::env::Environment) -> Self {
        match env {
            crate::env::Environment::Windows => EnvType::Windows,
            crate::env::Environment::Wsl => EnvType::Wsl,
        }
    }
}

impl EnvType {
    /// Parse the DB string column. Unknown strings fall back to Windows
    /// (matches the prior inline `match` behaviour scattered across db/mod.rs).
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "wsl" => EnvType::Wsl,
            "windowsinterop" => EnvType::WindowsInterop,
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
    /// DeepSeek Harness CLI (`dsh`) — interactive agent harness.
    /// See `agent::provider::adapters::dsh`.
    Dsh,
    /// Command Code CLI (`commandcode`) — interactive agent harness.
    /// See `agent::provider::adapters::commandcode` (wayfinder #1394).
    CommandCode,
    /// Freebuff CLI (`freebuff`) — interactive AI coding agent harness.
    /// See `agent::provider::adapters::freebuff` (issue #1437).
    Freebuff,
    /// Meta Muse Code, executed in a Unix runtime.
    Muse,
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
            Provider::Dsh,
            Provider::CommandCode,
            Provider::Freebuff,
            Provider::Muse,
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
            "dsh" | "deepseek-harness" | "deepseek" => Provider::Dsh,
            "commandcode" | "command-code" | "cmdc" | "cmd" => Provider::CommandCode,
            "freebuff" => Provider::Freebuff,
            "muse" => Provider::Muse,
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
            Provider::Dsh => &adapters::DSH,
            Provider::CommandCode => &adapters::COMMANDCODE,
            Provider::Freebuff => &adapters::FREEBUFF,
            Provider::Muse => &adapters::MUSE,
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
            Provider::Dsh => write!(f, "dsh"),
            Provider::CommandCode => write!(f, "commandcode"),
            Provider::Freebuff => write!(f, "freebuff"),
            Provider::Muse => write!(f, "muse"),
            Provider::Terminal => write!(f, "terminal"),
        }
    }
}
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
    /// Issue #1364 — an ordinary turn finished and the agent is at its
    /// prompt, ready for another prompt. The process is alive; the user is
    /// NOT needed (unlike `AwaitingInput`) and this is NOT Autopilot's
    /// PR-opened terminal state (`Completed`). Written by
    /// `session_lifecycle::on_turn_completed`.
    Ready,
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
            "ready" => SessionStatus::Ready,
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
            SessionStatus::Ready => "ready",
        }
    }
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
    /// Hook/attention signal health (issue #1364 §3). Layered on top of
    /// `status`, never a status itself: `Some(Ok)` once provisioning
    /// succeeded or the first hook callback arrived, `Some(Degraded)` for
    /// an unparseable/unknown payload, `Some(Unavailable)` when the
    /// attention hook could not be installed/trusted/reached, and `None`
    /// before the first provisioning outcome or callback. Lets the UI
    /// distinguish "the harness has not produced an event yet" from "the
    /// hook is broken".
    pub signal_health: Option<crate::agent::session_lifecycle::SignalHealth>,
    #[ts(as = "i32")]
    pub position: i64,        // grid order within the mesh (drag-to-reorder); lower = earlier
    pub created_at: DateTime<Utc>,
    /// Exact resolved Worktree Node directory for this node (issue #1519).
    /// `Some(raw_path)` for Worktree Nodes created after the configurable
    /// directory landed — the effective `<worktree_dir>/<trimmed_name>`
    /// computed at creation from Mesh override → app default →
    /// `.claude/worktrees`. `None` for Root Nodes and for pre-#1519 rows,
    /// which retain the legacy `<mesh>/.claude/worktrees/<name>` fallback
    /// via `env::node_working_path`. Immutable — changing a directory
    /// setting affects future nodes without moving live worktrees.
    /// Persisted as `agent_nodes.worktree_path TEXT` (schema v37).
    pub worktree_path: Option<String>,
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
