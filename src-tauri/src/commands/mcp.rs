//! MCP server configuration listing

use crate::db;
use crate::models::{EnvType, McpServer};
use std::process::Command;
use tauri::command;

/// List MCP servers configured for a session
#[command]
pub async fn list_mcp_servers(session_id: i64) -> Result<Vec<McpServer>, String> {
    let node = db::get_agent_node_by_id(session_id)
        .map_err(|e| e.to_string())?;

    let output = if node.env == EnvType::Wsl {
        Command::new("wsl.exe")
            .args(["--cd", &node.path, "--", "claude", "mcp", "list"])
            .output()
    } else {
        Command::new("claude")
            .args(["mcp", "list"])
            .output()
    };

    let output = match output {
        Ok(o) => o,
        Err(_) => return Ok(vec![]),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut servers = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            servers.push(McpServer {
                name: parts[0].to_string(),
                command: parts[1].to_string(),
                args: parts[2..].iter().map(|s| s.to_string()).collect(),
            });
        }
    }

    Ok(servers)
}
