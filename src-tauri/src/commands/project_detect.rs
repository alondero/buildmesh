//! Project-type detection for mesh directories.
//!
//! Inspects a mesh path for marker files (Cargo.toml, package.json, etc.) and
//! returns a structured preset suggestion the frontend can surface in
//! Mesh Properties. Pure read — no state mutation.

use std::path::Path;
use serde::{Deserialize, Serialize};
use tauri::command;
use crate::env;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeScripts {
    pub build: Option<String>,
    pub run: Option<String>,
    pub has_tauri_cli: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedProject {
    pub preset_id: Option<String>,
    pub label: Option<String>,
    pub node_scripts: Option<NodeScripts>,
}

impl DetectedProject {
    fn empty() -> Self {
        Self { preset_id: None, label: None, node_scripts: None }
    }

    fn from(preset_id: &str, label: &str) -> Self {
        Self {
            preset_id: Some(preset_id.to_string()),
            label: Some(label.to_string()),
            node_scripts: None,
        }
    }
}

fn parse_node_scripts(package_json_path: &Path) -> NodeScripts {
    let Ok(content) = std::fs::read_to_string(package_json_path) else {
        return NodeScripts { build: None, run: None, has_tauri_cli: false };
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return NodeScripts { build: None, run: None, has_tauri_cli: false };
    };

    let scripts = v.get("scripts").and_then(|s| s.as_object());

    let has_script = |name: &str| -> bool {
        scripts.map(|m| m.contains_key(name)).unwrap_or(false)
    };

    let has_tauri_cli = ["dependencies", "devDependencies"]
        .iter()
        .any(|k| {
            v.get(k)
                .and_then(|d| d.as_object())
                .map(|m| m.contains_key("@tauri-apps/cli"))
                .unwrap_or(false)
        });

    let build = if has_script("build") { Some("build".to_string()) } else { None };

    let run = if has_script("dev") {
        Some("dev".to_string())
    } else if has_script("start") {
        Some("start".to_string())
    } else if has_script("serve") {
        Some("serve".to_string())
    } else {
        None
    };

    NodeScripts { build, run, has_tauri_cli }
}

fn detect_in_dir(dir: &Path) -> DetectedProject {
    if !dir.is_dir() {
        return DetectedProject::empty();
    }

    let pkg = dir.join("package.json");
    let cargo = dir.join("Cargo.toml");
    let tauri_cargo = dir.join("src-tauri").join("Cargo.toml");

    if pkg.exists() {
        let scripts = parse_node_scripts(&pkg);
        let is_tauri = scripts.has_tauri_cli || tauri_cargo.exists();
        let preset_id = if is_tauri { "node-tauri" } else { "node" };
        let label = if is_tauri { "Tauri" } else { "Node" };
        return DetectedProject {
            preset_id: Some(preset_id.to_string()),
            label: Some(label.to_string()),
            node_scripts: Some(scripts),
        };
    }

    if cargo.exists() {
        return DetectedProject::from("rust", "Rust");
    }

    if dir.join("build.gradle").exists() || dir.join("build.gradle.kts").exists() {
        return DetectedProject::from("jvm-gradle", "JVM (Gradle)");
    }

    if dir.join("pom.xml").exists() {
        return DetectedProject::from("jvm-maven", "JVM (Maven)");
    }

    if dir.join("go.mod").exists() {
        return DetectedProject::from("go", "Go");
    }

    if dir.join("pyproject.toml").exists() {
        return DetectedProject::from("python", "Python");
    }

    DetectedProject::empty()
}

// Offloaded via `run_blocking` (issue #762 convention): the marker-file
// probes are filesystem stats that can stall on WSL UNC paths; a stalled
// probe must not park a bounded tokio worker.
#[command]
pub async fn detect_mesh_project(mesh_path: String) -> Result<DetectedProject, String> {
    crate::commands::run_blocking("detect_mesh_project", move || {
        let host_path = env::to_host_path(&mesh_path);
        let path_obj = Path::new(&host_path);

        if !path_obj.exists() {
            return Err(format!(
                "Path does not exist: {} (mapped from {})",
                host_path, mesh_path
            ));
        }

        Ok(detect_in_dir(path_obj))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, "").unwrap();
    }

    #[test]
    fn empty_dir_returns_no_preset() {
        let tmp = TempDir::new().unwrap();
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id, None);
        assert_eq!(got.label, None);
    }

    #[test]
    fn cargo_only_detects_rust() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("rust"));
    }

    #[test]
    fn package_json_detects_node() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("package.json"), r#"{ "scripts": { "build": "tsc", "dev": "vite" } }"#).unwrap();
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("node"));
        let scripts = got.node_scripts.expect("node_scripts populated");
        assert_eq!(scripts.build.as_deref(), Some("build"));
        assert_eq!(scripts.run.as_deref(), Some("dev"));
        assert!(!scripts.has_tauri_cli);
    }

    #[test]
    fn package_json_plus_src_tauri_cargo_detects_tauri() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("package.json"), r#"{ "scripts": { "tauri": "tauri" } }"#).unwrap();
        touch(tmp.path(), "src-tauri/Cargo.toml");
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("node-tauri"));
    }

    #[test]
    fn package_json_with_tauri_dep_detects_tauri() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "devDependencies": { "@tauri-apps/cli": "^2" } }"#,
        ).unwrap();
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("node-tauri"));
        assert!(got.node_scripts.unwrap().has_tauri_cli);
    }

    #[test]
    fn node_falls_back_to_serve_when_no_dev() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "scripts": { "serve": "webpack serve" } }"#,
        ).unwrap();
        let got = detect_in_dir(tmp.path());
        let scripts = got.node_scripts.expect("node_scripts populated");
        assert_eq!(scripts.run.as_deref(), Some("serve"));
        assert_eq!(scripts.build, None);
    }

    #[test]
    fn gradle_detected() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "build.gradle.kts");
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("jvm-gradle"));
    }

    #[test]
    fn maven_detected() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "pom.xml");
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("jvm-maven"));
    }

    #[test]
    fn go_detected() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "go.mod");
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("go"));
    }

    #[test]
    fn python_detected() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "pyproject.toml");
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("python"));
    }

    #[test]
    fn node_wins_over_rust_when_both_present() {
        // Tauri-style repo: top-level package.json + src-tauri/Cargo.toml.
        // Without explicit @tauri-apps/cli dep but with the src-tauri/Cargo.toml signal,
        // this should still detect as node-tauri (not bare rust).
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        touch(tmp.path(), "src-tauri/Cargo.toml");
        let got = detect_in_dir(tmp.path());
        assert_eq!(got.preset_id.as_deref(), Some("node-tauri"));
    }
}
