//! Mesh config file (mesh.toml) parsing and derivation utilities.
//!
//! This module centralizes all reading of `mesh.toml` so that the data layer
//! (db/mod.rs) does not need to import command modules (build_run.rs).

use std::path::Path;

/// The subset of mesh.toml fields relevant to agent spawning decisions.
#[derive(Debug, Clone)]
pub struct MeshSpawnConfig {
    pub use_worktree: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub worktree_mode: Option<String>,
}

impl Default for MeshSpawnConfig {
    fn default() -> Self {
        // Default to use_worktree=true as per existing behavior
        Self {
            use_worktree: true,
            model: None,
            effort: None,
            worktree_mode: None,
        }
    }
}

/// Read the mesh.toml at the given path and return spawn-relevant config.
/// Returns None if the file does not exist or cannot be parsed.
/// Never returns an error — missing fields default to their sensible values.
pub fn read_mesh_spawn_config(mesh_path: &Path) -> Option<MeshSpawnConfig> {
    let config_path = mesh_path.join(super::MESH_CONFIG_FILENAME);
    let content = std::fs::read_to_string(&config_path).ok()?;

    Some(MeshSpawnConfig {
        use_worktree: derive_use_worktree_from_content(&content),
        model: extract_toml_value(&content, "agent", "model"),
        effort: extract_toml_value(&content, "agent", "effort"),
        worktree_mode: extract_toml_value(&content, "agent", "worktree_mode"),
    })
}

/// Derive `use_worktree` from a mesh.toml content string.
/// Returns true if absent or non-false — matches existing default behavior.
fn derive_use_worktree_from_content(content: &str) -> bool {
    extract_toml_value(content, "agent", "use_worktree")
        .map(|v| v == "true")
        .unwrap_or(true)
}

/// Extract a value from a TOML table: [section] key = "value"
/// Also handles bare booleans: key = true or key = false
fn extract_toml_value(content: &str, section: &str, key: &str) -> Option<String> {
    let section_pattern = format!("[{}]", section);
    let section_start = content.find(&section_pattern)?;

    let section_content = if let Some(next_section_idx) = content[section_start + section_pattern.len()..]
        .find('[')
    {
        &content[section_start..section_start + section_pattern.len() + next_section_idx]
    } else {
        &content[section_start..]
    };

    let key_pattern = format!("{} = ", key);
    let key_start = section_content.find(&key_pattern)?;

    let rest = &section_content[key_start + key_pattern.len()..];

    if let Some(quote_start) = rest.find('"') {
        let after_quote = &rest[quote_start + 1..];
        if let Some(quote_end) = after_quote.find('"') {
            return Some(after_quote[..quote_end].to_string());
        }
    }

    let trimmed = rest.trim_start();
    if trimmed.starts_with("true") || trimmed.starts_with("false") {
        let bool_str = if trimmed.starts_with("true") { "true" } else { "false" };
        return Some(bool_str.to_string());
    }

    let end = trimmed.find(|c: char| c.is_ascii_whitespace()).unwrap_or(trimmed.len());
    if end > 0 {
        return Some(trimmed[..end].to_string());
    }

    None
}

pub const MESH_CONFIG_FILENAME: &str = "mesh.toml";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_worktree_derives_true_when_absent() {
        let content = r#"
[build]
command = "npm run build"
"#;
        assert!(derive_use_worktree_from_content(content));
    }

    #[test]
    fn use_worktree_derives_true_when_explicit_true() {
        let content = r#"
[agent]
use_worktree = true
"#;
        assert!(derive_use_worktree_from_content(content));
    }

    #[test]
    fn use_worktree_derives_false_when_explicit_false() {
        let content = r#"
[agent]
use_worktree = false
"#;
        assert!(!derive_use_worktree_from_content(content));
    }

    #[test]
    fn use_worktree_derives_true_bare_true() {
        // Some TOML parsers accept bare identifiers
        let content = r#"
[agent]
use_worktree = true
"#;
        assert!(derive_use_worktree_from_content(content));
    }

    #[test]
    fn read_mesh_spawn_config_returns_none_when_file_missing() {
        let path = Path::new("/nonexistent/path");
        assert!(read_mesh_spawn_config(path).is_none());
    }

    #[test]
    fn read_mesh_spawn_config_derives_worktree_mode() {
        let temp = tempfile::tempdir().unwrap();
        let mesh_path = temp.path();
        std::fs::write(
            mesh_path.join("mesh.toml"),
            r#"
[agent]
worktree_mode = "branched"
"#,
        )
        .unwrap();

        let config = read_mesh_spawn_config(mesh_path).unwrap();
        assert_eq!(config.worktree_mode, Some("branched".to_string()));
    }

    #[test]
    fn read_mesh_spawn_config_derives_model() {
        let temp = tempfile::tempdir().unwrap();
        let mesh_path = temp.path();
        std::fs::write(
            mesh_path.join("mesh.toml"),
            r#"
[agent]
model = "opus-4"
"#,
        )
        .unwrap();

        let config = read_mesh_spawn_config(mesh_path).unwrap();
        assert_eq!(config.model, Some("opus-4".to_string()));
    }
}