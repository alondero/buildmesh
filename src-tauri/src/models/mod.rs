//! Data models for Conductor Clone

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
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::Minimax => write!(f, "minimax"),
        }
    }
}

/// Workspace status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceStatus {
    Running,
    Idle,
    Error,
    Archived,
}

/// A project — top-level folder containing workspaces
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub path: String, // absolute path to project root
    pub created_at: DateTime<Utc>,
}

/// A workspace — isolated agent working directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub path: String,         // absolute path to workspace directory
    pub branch: String,
    pub env: EnvType,         // windows or wsl
    pub provider: Provider,   // anthropic or minimax
    pub status: WorkspaceStatus,
    pub created_at: DateTime<Utc>,
}

/// A checkpoint — git ref snapshot of workspace state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: i64,
    pub workspace_id: i64,
    pub git_ref: String,      // e.g., "conductor/checkpoints/c1"
    pub turn_index: i32,      // which turn this was created at
    pub message: String,     // optional description
    pub created_at: DateTime<Utc>,
}

/// A chat message in the agent session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: i64,
    pub workspace_id: i64,
    pub role: String,         // "user" or "assistant"
    pub content: String,
    pub tool_calls: Option<String>, // JSON array of tool calls if any
    pub created_at: DateTime<Utc>,
}

/// A script attached to a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceScript {
    pub id: i64,
    pub workspace_id: i64,
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
