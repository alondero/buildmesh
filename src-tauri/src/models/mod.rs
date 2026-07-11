//! Data models for Buildmesh

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
            "terminal" => Provider::Terminal,
            // "minimax" / "kimi" are no longer first-class executors: they're
            // Claude Code with a swapped backend, configured as harness profiles
            // whose paired provider account injects the endpoint at spawn (#538).
            // A bare legacy id with no configured profile falls through to the
            // Anthropic executor here (resolve_harness_provider checks profiles
            // first, so a configured "minimax" account resolves cleanly).
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
    #[ts(as = "Option<i32>")]
    pub source_issue: Option<i64>,       // GitHub issue number that triggered this node
    /// GitHub PR number that triggered this node (issue #420). `None` for
    /// issue-spawned and hand-spawned nodes. When set, `spawn_agent_inner`
    /// fetches `origin/<head_ref>` and uses it as the worktree's `base_ref`
    /// instead of the mesh's `base_ref` (relates to #36 worktree adoption).
    /// Mirrors the `source_issue` field so the same resume-by-URL plumbing
    /// (issue #37) can target both spawn sources.
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
    }

    #[test]
    fn mesh_row_from_mesh_blank_name_is_none() {
        let mut mesh = sample_mesh();
        mesh.name = String::new();
        assert_eq!(MeshRow::from(&mesh).name, None);
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
    }

    /// The "produces a readable transcript" capability (#317) — the Claude-backed
    /// `anthropic` adapter (which also runs custom MiniMax/Kimi/DeepSeek profiles)
    /// writes a transcript the coordinator read API can drill into. Everything
    /// else degrades to a spine-only digest flagged `unsupported`, so this matrix
    /// is load-bearing.
    #[test]
    fn only_claude_backed_providers_produce_a_readable_transcript() {
        assert!(Provider::Anthropic.adapter().produces_readable_transcript());
        // Codex has its own (non-Claude-Code) transcript format; the rest have none.
        assert!(!Provider::Codex.adapter().produces_readable_transcript());
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
    }

    /// Hard cutover (issue #538): "minimax"/"kimi" are no longer first-class
    /// executors. With no configured harness profile they fall through to the
    /// Anthropic default — `resolve_harness_provider` checks profiles first, so a
    /// configured custom account still resolves to the right backend env.
    #[test]
    fn provider_from_db_str_legacy_minimax_kimi_fall_back_to_anthropic() {
        assert_eq!(Provider::from_db_str("minimax"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str("kimi"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str("Minimax"), Provider::Anthropic);
    }

    /// Whitespace around the value shouldn't break matching either — a
    /// hand-edited JSON file with `"Terminal "` (trailing space) should
    /// still resolve correctly.
    #[test]
    fn provider_from_db_str_trims_whitespace() {
        assert_eq!(Provider::from_db_str("  terminal  "), Provider::Terminal);
        assert_eq!(Provider::from_db_str("\tcodex\n"), Provider::Codex);
    }

    /// Capture-target for tracing events emitted inside `from_db_str`.
    /// `tracing-subscriber`'s `MakeWriter` impl that appends to a shared
    /// `Vec<u8>`, so each test can grep the captured output for the
    /// expected log line.
    #[derive(Clone, Default)]
    struct VecWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for VecWriter {
        type Writer = VecWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run `body` under a subscriber that captures WARN-level tracing events
    /// into a fresh `Vec<u8>`, returning the captured string. Used by the
    /// `provider_from_db_str_*_warning` tests to assert on log output.
    fn capture_warnings<F: FnOnce()>(body: F) -> String {
        let buf = VecWriter::default();
        let buf_for_assert = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf)
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();

        tracing::subscriber::with_default(subscriber, body);

        // Clone into a local so the MutexGuard drops at end of `.clone()`,
        // not at end of block — otherwise the lock outlives the return value.
        let bytes = buf_for_assert.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
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

    #[test]
    fn provider_display_lowercase() {
        assert_eq!(format!("{}", Provider::Anthropic), "anthropic");
        assert_eq!(format!("{}", Provider::Agy), "agy");
        assert_eq!(format!("{}", Provider::OpenCode), "opencode");
        assert_eq!(format!("{}", Provider::Codex), "codex");
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
