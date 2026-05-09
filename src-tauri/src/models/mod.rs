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

/// Agent provider type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    Minimax,
    Gemini,
    OpenCode,
}

impl Provider {
    /// Returns just the binary name (without args)
    pub fn binary(&self) -> &'static str {
        match self {
            Provider::Anthropic | Provider::Minimax => "cwrap",
            Provider::Gemini => "gemini",
            Provider::OpenCode => "opencode",
        }
    }

    /// Returns the argument flag for this provider
    pub fn cli_flag(&self) -> &'static str {
        match self {
            Provider::Anthropic => "--anthropic",
            Provider::Minimax => "--minimax",
            Provider::Gemini | Provider::OpenCode => "",
        }
    }

    /// Returns true if this provider uses cwrap (Anthropic or Minimax)
    pub fn is_cwrap(&self) -> bool {
        matches!(self, Provider::Anthropic | Provider::Minimax)
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::Minimax => write!(f, "minimax"),
            Provider::Gemini => write!(f, "gemini"),
            Provider::OpenCode => write!(f, "opencode"),
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

/// MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
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

/// App settings stored in SQLite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub default_projects_root: String,
    pub windows_cli_path: String,
    pub wsl_cli_path: String,
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
    fn provider_binary_cwrap_for_anthropic_minimax() {
        assert_eq!(Provider::Anthropic.binary(), "cwrap");
        assert_eq!(Provider::Minimax.binary(), "cwrap");
    }

    #[test]
    fn provider_binary_direct_for_gemini_opencode() {
        assert_eq!(Provider::Gemini.binary(), "gemini");
        assert_eq!(Provider::OpenCode.binary(), "opencode");
    }

    #[test]
    fn provider_cli_flag_correct() {
        assert_eq!(Provider::Anthropic.cli_flag(), "--anthropic");
        assert_eq!(Provider::Minimax.cli_flag(), "--minimax");
        assert_eq!(Provider::Gemini.cli_flag(), "");
        assert_eq!(Provider::OpenCode.cli_flag(), "");
    }

    #[test]
    fn provider_display_lowercase() {
        assert_eq!(format!("{}", Provider::Anthropic), "anthropic");
        assert_eq!(format!("{}", Provider::Minimax), "minimax");
        assert_eq!(format!("{}", Provider::Gemini), "gemini");
        assert_eq!(format!("{}", Provider::OpenCode), "opencode");
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
