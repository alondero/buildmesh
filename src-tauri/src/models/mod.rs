//! Data models for Buildmesh

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Runtime environment — Windows or WSL
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "EnvType.ts")]
pub enum EnvType {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "Provider.ts")]
pub enum Provider {
    Anthropic,
    Minimax,
    Agy,
    OpenCode,
    Codex,
    Kimi,
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
            Provider::Minimax,
            Provider::Agy,
            Provider::OpenCode,
            Provider::Codex,
            Provider::Kimi,
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
            "minimax" => Provider::Minimax,
            "agy" => Provider::Agy,
            "opencode" => Provider::OpenCode,
            "codex" => Provider::Codex,
            "kimi" => Provider::Kimi,
            "terminal" => Provider::Terminal,
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
            Provider::Minimax => &adapters::MINIMAX,
            Provider::Agy => &adapters::AGY,
            Provider::OpenCode => &adapters::OPENCODE,
            Provider::Codex => &adapters::CODEX,
            Provider::Kimi => &adapters::KIMI,
            Provider::Terminal => &adapters::TERMINAL,
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::Minimax => write!(f, "minimax"),
            Provider::Agy => write!(f, "agy"),
            Provider::OpenCode => write!(f, "opencode"),
            Provider::Codex => write!(f, "codex"),
            Provider::Kimi => write!(f, "kimi"),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "SessionStatus.ts")]
pub enum SessionStatus {
    Running,
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
        }
    }
}

/// A mesh — top-level folder containing agent nodes.
///
/// Generated to src/types/generated/Mesh.ts (issue #359). `i64` fields carry
/// `#[ts(as = "i32")]` so they emit `number` (serde_json sends JS numbers, not
/// the `bigint` ts-rs defaults to for 64-bit ints).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
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
    // Mesh-level config (see MeshConfig for the canonical typed view)
    pub build_command: Option<String>,
    pub run_command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub use_worktree: bool, // default true
    pub worktree_mode: Option<String>,
    pub default_provider: Option<String>,
    pub base_ref: String, // default "origin/main"
}

/// An agent node — isolated agent working directory.
///
/// Generated to src/types/generated/AgentNode.ts (issue #359); references the
/// generated `EnvType`/`Provider`/`SessionStatus` enums. `i64` fields use
/// `#[ts(as = "i32")]` / `Option<i32>` so they emit `number` / `number | null`
/// rather than ts-rs's default `bigint`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "AgentNode.ts")]
pub struct AgentNode {
    #[ts(as = "i32")]
    pub id: i64,
    #[ts(as = "i32")]
    pub mesh_id: i64,
    pub name: String,
    pub path: String,         // absolute path to node directory
    pub branch: String,
    pub env: EnvType,         // windows or wsl
    pub provider: Provider,   // anthropic or minimax
    pub status: SessionStatus,
    pub cli_session_id: Option<String>, // Opaque ID from the agent CLI
    pub worktree_name: Option<String>,   // git worktree name (same as name for cwrap providers)
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
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "WorktreeInfo.ts")]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub is_active: bool,
    pub is_stale: bool,
}

/// App settings stored in SQLite
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub default_projects_root: String,
    pub windows_cli_path: String,
    pub wsl_cli_path: String,
}

/// Canonical mesh-level configuration, derived from a `Mesh` DB row.
///
/// This is the single typed view of mesh config used by every consumer
/// (frontend properties, agent spawning, build/run). Construct it via
/// `MeshConfig::from(&mesh)` — never hand-copy `Mesh` fields elsewhere.
///
/// Generated to src/types/generated/MeshConfig.ts (issue #404).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "MeshConfig.ts")]
pub struct MeshConfig {
    pub name: Option<String>,
    pub build_command: Option<String>,
    pub run_command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub base_ref: Option<String>,
    pub use_worktree: bool,
    pub worktree_mode: Option<String>,
    pub default_provider: Option<String>,
}

impl From<&Mesh> for MeshConfig {
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
        Mesh {
            id: 1,
            name: "demo".to_string(),
            path: "/repo".to_string(),
            layout: "grid".to_string(),
            position: 0,
            created_at: Utc::now(),
            build_command: Some("npm run build".to_string()),
            run_command: None,
            model: Some("opus".to_string()),
            effort: None,
            use_worktree: false,
            worktree_mode: Some("branched".to_string()),
            default_provider: None,
            base_ref: "origin/main".to_string(),
        }
    }

    #[test]
    fn mesh_config_from_mesh_maps_all_fields() {
        let cfg = MeshConfig::from(&sample_mesh());
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
    }

    #[test]
    fn mesh_config_from_mesh_blank_name_is_none() {
        let mut mesh = sample_mesh();
        mesh.name = String::new();
        assert_eq!(MeshConfig::from(&mesh).name, None);
    }

    #[test]
    fn session_status_round_trip_all_variants() {
        let variants = [
            SessionStatus::Pending,
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
        assert_eq!(Provider::Anthropic.adapter().spawn_recipe(Platform::Windows).binary, "cwrap");
        assert_eq!(Provider::Minimax.adapter().spawn_recipe(Platform::Windows).binary, "cwrap");
        assert_eq!(Provider::Kimi.adapter().spawn_recipe(Platform::Windows).binary, "cwrap");
        assert_eq!(Provider::Agy.adapter().spawn_recipe(Platform::Windows).binary, "agy");
        assert_eq!(Provider::OpenCode.adapter().spawn_recipe(Platform::Windows).binary, "opencode");
        assert_eq!(Provider::Codex.adapter().spawn_recipe(Platform::Windows).binary, "codex");
        // Plain terminal spawns the OS-preferred shell directly — powershell.exe on Windows
        // host, routed through wsl.exe by spawn_environment::wrap when env_type is WSL.
        assert_eq!(Provider::Terminal.adapter().spawn_recipe(Platform::Windows).binary, "powershell.exe");
    }

    #[test]
    fn provider_adapter_recipe_macos_anthropic_uses_claude() {
        use crate::agent::provider::Platform;
        assert_eq!(Provider::Anthropic.adapter().spawn_recipe(Platform::Macos).binary, "claude");
    }

    #[test]
    fn provider_capabilities_split_correctly() {
        assert!(Provider::Anthropic.adapter().supports_resume());
        assert!(Provider::Minimax.adapter().supports_resume());
        assert!(Provider::Kimi.adapter().supports_resume());
        assert!(Provider::Agy.adapter().supports_resume());
        assert!(!Provider::OpenCode.adapter().supports_resume());
        assert!(Provider::Codex.adapter().supports_resume());
    }

    /// The "produces a readable transcript" capability (#317) — only the Claude
    /// Code family (the three `cwrap` providers) writes a transcript the
    /// coordinator read API can drill into. Everything else degrades to a
    /// spine-only digest flagged `unsupported`, so this matrix is load-bearing.
    #[test]
    fn only_cwrap_providers_produce_a_readable_transcript() {
        assert!(Provider::Anthropic.adapter().produces_readable_transcript());
        assert!(Provider::Minimax.adapter().produces_readable_transcript());
        assert!(Provider::Kimi.adapter().produces_readable_transcript());
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
        assert_eq!(Provider::from_db_str("Minimax"), Provider::Minimax);
        assert_eq!(Provider::from_db_str("AGY"), Provider::Agy);
        assert_eq!(Provider::from_db_str("OpenCode"), Provider::OpenCode);
        assert_eq!(Provider::from_db_str("Codex"), Provider::Codex);
        assert_eq!(Provider::from_db_str("Kimi"), Provider::Kimi);
    }

    /// Whitespace around the value shouldn't break matching either — a
    /// hand-edited JSON file with `"Terminal "` (trailing space) should
    /// still resolve correctly.
    #[test]
    fn provider_from_db_str_trims_whitespace() {
        assert_eq!(Provider::from_db_str("  terminal  "), Provider::Terminal);
        assert_eq!(Provider::from_db_str("\tminimax\n"), Provider::Minimax);
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
            assert_eq!(Provider::from_db_str("  kimi  "), Provider::Kimi);
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
        assert_eq!(format!("{}", Provider::Minimax), "minimax");
        assert_eq!(format!("{}", Provider::Agy), "agy");
        assert_eq!(format!("{}", Provider::OpenCode), "opencode");
        assert_eq!(format!("{}", Provider::Codex), "codex");
        assert_eq!(format!("{}", Provider::Kimi), "kimi");
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
        assert!(!Provider::Minimax.adapter().self_assigns_session_id());
        assert!(!Provider::Kimi.adapter().self_assigns_session_id());
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
        assert!(!Provider::Minimax.adapter().is_plain_terminal());
        assert!(!Provider::Agy.adapter().is_plain_terminal());
        assert!(!Provider::OpenCode.adapter().is_plain_terminal());
        assert!(!Provider::Codex.adapter().is_plain_terminal());
        assert!(!Provider::Kimi.adapter().is_plain_terminal());
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
