//! Data models for Buildmesh

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Runtime environment — Windows or WSL
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    Minimax,
    Gemini,
    OpenCode,
    Codex,
}

impl Provider {
    /// All known providers, in stable order. Used to enumerate UI listings.
    pub fn all() -> &'static [Provider] {
        &[
            Provider::Anthropic,
            Provider::Minimax,
            Provider::Gemini,
            Provider::OpenCode,
            Provider::Codex,
        ]
    }

    /// Parse the DB string column / Tauri arg into a typed `Provider`.
    /// Unknown strings fall back to `Anthropic` (matches previous behaviour).
    pub fn from_db_str(s: &str) -> Provider {
        match s {
            "minimax" => Provider::Minimax,
            "gemini" => Provider::Gemini,
            "opencode" => Provider::OpenCode,
            "codex" => Provider::Codex,
            _ => Provider::Anthropic,
        }
    }

    /// Look up the behaviour adapter for this provider.
    /// All provider-specific logic (binary, args, capabilities) lives behind this seam.
    pub fn adapter(&self) -> &'static dyn crate::agent::provider::AgentProvider {
        use crate::agent::provider::adapters;
        match self {
            Provider::Anthropic => &adapters::ANTHROPIC,
            Provider::Minimax => &adapters::MINIMAX,
            Provider::Gemini => &adapters::GEMINI,
            Provider::OpenCode => &adapters::OPENCODE,
            Provider::Codex => &adapters::CODEX,
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::Minimax => write!(f, "minimax"),
            Provider::Gemini => write!(f, "gemini"),
            Provider::OpenCode => write!(f, "opencode"),
            Provider::Codex => write!(f, "codex"),
        }
    }
}

/// Session status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Running,
    Idle,
    AwaitingInput,
    Error,
    Archived,
    Suspended,
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
        }
    }
}

/// A mesh — top-level folder containing agent nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mesh {
    pub id: i64,
    pub name: String,
    pub path: String, // absolute path to mesh root
    pub layout: String, // 'grid' or 'single'
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

/// An agent node — isolated agent working directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentNode {
    pub id: i64,
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
    pub source_issue: Option<i64>,       // GitHub issue number that triggered this node
    pub created_at: DateTime<Utc>,
}

/// A checkpoint — git ref snapshot of node state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: i64,
    pub node_id: i64,
    pub git_ref: String,      // e.g., "conductor/checkpoints/c1"
    pub turn_index: i32,      // which turn this was created at
    pub message: String,     // optional description
    pub created_at: DateTime<Utc>,
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

/// Diff result between two checkpoints or files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffResult {
    pub files: Vec<FileDiff>,
}

/// A single file diff
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<DiffHunk>,
}

/// A hunk within a diff
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    /// Full highlighted HTML for the old version of this hunk
    pub old_highlighted: String,
    /// Full highlighted HTML for the new version of this hunk
    pub new_highlighted: String,
    pub lines: Vec<DiffLine>,
}

/// A single line in a diff
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub line_type: String,   // "context" | "add" | "remove"
    pub content: String,
    pub old_num: Option<usize>,
    pub new_num: Option<usize>,
}

/// Per-repo git prune info — branches, worktrees, and remote-tracking refs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoPruneInfo {
    pub path: String,
    pub local_branches: Vec<BranchInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub remote_tracking_branches: Vec<String>,
}

/// A local branch and its prune-relevant metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    /// None when the repo has no main/master branch to compare against.
    pub is_merged_into_main: Option<bool>,
    pub is_orphan: bool,
    pub has_uncommitted: bool,
    pub last_commit_date: Option<String>,
    pub ahead: u64,
    pub behind: u64,
}

/// A worktree (main or linked) and its prune-relevant metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub is_active: bool,
    pub is_stale: bool,
}

/// App settings stored in SQLite
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, serde::Serialize)]
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

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_projects_root: String::new(),
            windows_cli_path: String::new(),
            wsl_cli_path: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(Provider::Gemini.adapter().spawn_recipe(Platform::Windows).binary, "gemini");
        assert_eq!(Provider::OpenCode.adapter().spawn_recipe(Platform::Windows).binary, "opencode");
        assert_eq!(Provider::Codex.adapter().spawn_recipe(Platform::Windows).binary, "codex");
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
        assert!(!Provider::Gemini.adapter().supports_resume());
        assert!(!Provider::OpenCode.adapter().supports_resume());
        assert!(Provider::Codex.adapter().supports_resume());
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
    /// If this fallback ever becomes a real correctness concern, consider
    /// changing `from_db_str` to return `Result<Provider, _>`.
    #[test]
    fn provider_from_db_str_unknown_falls_back_to_anthropic() {
        assert_eq!(Provider::from_db_str("garbage"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str(""), Provider::Anthropic);
    }

    #[test]
    fn provider_display_lowercase() {
        assert_eq!(format!("{}", Provider::Anthropic), "anthropic");
        assert_eq!(format!("{}", Provider::Minimax), "minimax");
        assert_eq!(format!("{}", Provider::Gemini), "gemini");
        assert_eq!(format!("{}", Provider::OpenCode), "opencode");
        assert_eq!(format!("{}", Provider::Codex), "codex");
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
    fn session_status_serde_json_lowercase() {
        let json = serde_json::to_string(&SessionStatus::AwaitingInput).unwrap();
        assert_eq!(json, "\"awaitinginput\"");
        let json = serde_json::to_string(&SessionStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
    }
}
