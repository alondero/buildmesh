//! Auto-trust agent workspace paths in Claude Code and Antigravity settings.
//!
//! When buildmesh spawns an agent in a worktree, the agent's CLI prompts the
//! user with a workspace-trust dialog the first time it sees the path — and
//! that dialog blocks `CLAUDE.md` and `.claude/` skill loading until the
//! user accepts. Spawning is automated, so we pre-add the path to the CLI's
//! `trustedWorkspaces` array before launch.
//!
//! The two CLIs we target each keep their trust state in a JSON file under
//! `$HOME` (Claude Code at `~/.claude/settings.json`, Antigravity at
//! `~/.gemini/antigravity-cli/settings.json`). This module mutates both files
//! best-effort: a missing file, unreadable file, or malformed JSON is a
//! silent no-op. Writes are atomic (temp + rename) so two concurrent spawns
//! can't lose a path or see a half-written file. Trailing-newline style is
//! preserved; indent style is normalised to serde's default 2 spaces.
//!
//! WSL-guest paths are intentionally skipped: the Windows-side
//! `~/.claude/settings.json` is interpreted by the Windows Claude install,
//! which can't trust a WSL path; the WSL guest's own `~/.claude/settings.json`
//! is a separate concern that lives in the WSL env.

use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::io;
use std::path::{Path, PathBuf};

/// Relative paths under `$HOME` whose `trustedWorkspaces` array we manage.
/// `(relative, log_label)` — the label is used in `tracing` messages so a
/// failure log distinguishes the two CLIs.
const TARGETS: &[(&str, &str)] = &[
    (".gemini/antigravity-cli/settings.json", "antigravity"),
    (".claude/settings.json", "claude"),
];

/// Add `resolved.host_path` to the `trustedWorkspaces` array in both Claude
/// Code and Antigravity settings files under `$USERPROFILE`/`$HOME`. Called
/// from `spawn_agent_inner` immediately before `inject_attention_hook`.
///
/// Best-effort: a missing `$HOME`, missing settings file, unreadable file,
/// or malformed JSON are all silent no-ops. The function returns `()`; the
/// call site cannot fail.
pub fn ensure_trusted(resolved: &ResolvedPath) {
    if resolved.env_type == EnvType::Wsl {
        // The Windows-side settings file would hold a UNC WSL path for a
        // WSL-guest trust decision, but the guest's own CLI reads from its
        // own `~/.claude/settings.json` — separate trust entry. Leave the
        // Windows file alone.
        return;
    }
    let Some(home) = home_dir() else { return };
    ensure_trusted_with_home(resolved, &home);
}

/// Testable core: same as `ensure_trusted` but takes the home directory
/// explicitly. WSL skip is mirrored here so tests can drive the policy
/// without going through the process env.
fn ensure_trusted_with_home(resolved: &ResolvedPath, home: &Path) {
    if resolved.env_type == EnvType::Wsl {
        return;
    }
    for (relative, label) in TARGETS {
        // Errors are logged inside `update_settings_file`; a broken trust
        // update is never fatal to a spawn.
        update_settings_file(&home.join(relative), label, &resolved.host_path);
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(PathBuf::from)
}

/// Read-modify-write one settings file. Atomic write on success; silent
/// no-op on missing file, IO error, or malformed JSON. If the existing
/// `trustedWorkspaces` value is a non-array (e.g. a string), we refuse to
/// overwrite it and log a warning — better to be loud than to silently
/// destroy user data.
fn update_settings_file(path: &Path, label: &str, project_path: &str) {
    if !path.exists() {
        return;
    }
    let original = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut settings: serde_json::Value = match serde_json::from_str(&original) {
        Ok(v) => v,
        Err(_) => return,
    };
    let arr = match trusted_workspaces_array(&mut settings) {
        Some(arr) => arr,
        None => {
            tracing::warn!(
                "ensure_trusted: {}: 'trustedWorkspaces' is not an array; refusing to overwrite",
                label
            );
            return;
        }
    };

    let normalized = normalize_path(project_path);
    if arr.iter().any(|v| matches_existing(v, &normalized)) {
        return;
    }
    arr.push(serde_json::Value::String(normalized.clone()));
    tracing::info!("ensure_trusted: added {:?} to {}", normalized, label);

    let body = match serde_json::to_string_pretty(&settings) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("ensure_trusted: serialize {} failed: {}", label, e);
            return;
        }
    };
    let with_newline = if original.ends_with('\n') && !body.ends_with('\n') {
        format!("{}\n", body)
    } else {
        body
    };
    if let Err(e) = atomic_write(path, &with_newline) {
        tracing::warn!("ensure_trusted: write {} failed: {}", label, e);
    }
}

/// Get-or-create the `trustedWorkspaces` array, validating its type. Returns
/// `None` if the existing value is a non-array (we won't destroy user data).
fn trusted_workspaces_array(
    settings: &mut serde_json::Value,
) -> Option<&mut Vec<serde_json::Value>> {
    let obj = settings.as_object_mut()?;
    if let Some(existing) = obj.get("trustedWorkspaces") {
        if !existing.is_array() {
            return None;
        }
    } else {
        obj.insert(
            "trustedWorkspaces".to_string(),
            serde_json::Value::Array(Vec::new()),
        );
    }
    obj.get_mut("trustedWorkspaces")
        .and_then(|v| v.as_array_mut())
}

/// Windows uses `\` as separator; macOS/Linux use `/`. The `cfg!` is the
/// *target* triple so a Windows build always normalizes regardless of the
/// runtime environment.
fn normalize_path(p: &str) -> String {
    if cfg!(target_os = "windows") {
        p.replace('/', "\\")
    } else {
        p.to_string()
    }
}

/// Path comparison: case-insensitive on Windows (NTFS is case-insensitive),
/// case-sensitive on macOS/Linux. (macOS HFS+ is technically case-insensitive
/// by default, but the *trust list* is stored as raw text so we treat it as
/// the user wrote it — matching what Claude Code itself does on those
/// platforms.)
fn matches_existing(existing: &serde_json::Value, candidate: &str) -> bool {
    let Some(s) = existing.as_str() else { return false };
    if cfg!(target_os = "windows") {
        s.eq_ignore_ascii_case(candidate)
    } else {
        s == candidate
    }
}

/// Write via a sibling temp file + rename. `std::fs::rename` on Windows uses
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` and is atomic for same-volume
/// NTFS — so two concurrent spawns can't lose a path or see a half-written
/// file. On a rename failure the `.tmp` file is cleaned up and the caller
/// surfaces a `warn!`.
fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, content)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — drive the inner functions directly with an explicit `home` so we
// never touch the process env (which is global and races across parallel
// `cargo test` threads).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, body).unwrap();
        p
    }

    fn windows_resolved(host_path: &str) -> ResolvedPath {
        ResolvedPath {
            host_path: host_path.to_string(),
            spawn_path: host_path.to_string(),
            raw_path: host_path.to_string(),
            env_type: EnvType::Windows,
        }
    }

    fn wsl_resolved(host_path: &str) -> ResolvedPath {
        ResolvedPath {
            host_path: host_path.to_string(),
            spawn_path: host_path.to_string(),
            raw_path: host_path.to_string(),
            env_type: EnvType::Wsl,
        }
    }

    /// Drive the testable inner function with an explicit `home`. We avoid
    /// `ensure_trusted` here because that reads `$USERPROFILE` from the
    /// process env (a global), which races with parallel test threads.
    fn drive(home: &Path, resolved: &ResolvedPath) {
        ensure_trusted_with_home(resolved, home);
    }

    fn read_trusted(home: &Path, rel: &str) -> Vec<String> {
        let body = fs::read_to_string(home.join(rel)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["trustedWorkspaces"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|item| item.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn missing_file_is_silent_noop() {
        let home = TempDir::new().unwrap();
        // No settings file written
        drive(home.path(), &windows_resolved("C:/work/proj"));
        assert!(!home.path().join(".claude/settings.json").exists());
        assert!(!home
            .path()
            .join(".gemini/antigravity-cli/settings.json")
            .exists());
    }

    #[test]
    fn adds_to_fresh_file_with_existing_keys() {
        let home = TempDir::new().unwrap();
        write_file(home.path(), ".claude/settings.json", r#"{"hooks":[]}"#);
        drive(home.path(), &windows_resolved("C:/work/proj"));

        let v: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let arr = v["trustedWorkspaces"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert!(arr[0].as_str().unwrap().contains("work"));
        // Other keys preserved
        assert!(v["hooks"].is_array());
    }

    #[test]
    fn writes_to_both_target_files() {
        let home = TempDir::new().unwrap();
        write_file(home.path(), ".claude/settings.json", "{}");
        write_file(home.path(), ".gemini/antigravity-cli/settings.json", "{}");

        drive(home.path(), &windows_resolved("C:/work/proj"));

        for rel in [
            ".claude/settings.json",
            ".gemini/antigravity-cli/settings.json",
        ] {
            assert_eq!(read_trusted(home.path(), rel), vec!["C:\\work\\proj"]);
        }
    }

    #[test]
    fn already_present_no_duplicate() {
        let home = TempDir::new().unwrap();
        write_file(
            home.path(),
            ".claude/settings.json",
            r#"{"trustedWorkspaces":["C:\\work\\proj"]}"#,
        );
        drive(home.path(), &windows_resolved("C:/work/proj"));

        assert_eq!(
            read_trusted(home.path(), ".claude/settings.json"),
            vec!["C:\\work\\proj"]
        );
    }

    #[test]
    fn case_insensitive_dedup_on_windows() {
        let home = TempDir::new().unwrap();
        // Existing entry is upper-cased; new entry is lower-cased. On Windows
        // these are the same path and we should not duplicate.
        write_file(
            home.path(),
            ".claude/settings.json",
            r#"{"trustedWorkspaces":["C:\\WORK\\PROJ"]}"#,
        );
        drive(home.path(), &windows_resolved("c:/work/proj"));

        let list = read_trusted(home.path(), ".claude/settings.json");
        assert_eq!(list.len(), 1, "got: {:?}", list);
    }

    #[test]
    fn existing_non_array_value_is_not_silently_destroyed() {
        let home = TempDir::new().unwrap();
        write_file(
            home.path(),
            ".claude/settings.json",
            r#"{"trustedWorkspaces":"some-user-string"}"#,
        );
        drive(home.path(), &windows_resolved("C:/work/proj"));

        // File unchanged — we refused to overwrite a non-array.
        let v: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            v["trustedWorkspaces"].as_str().unwrap(),
            "some-user-string"
        );
    }

    #[test]
    fn malformed_json_is_silent_noop() {
        let home = TempDir::new().unwrap();
        write_file(home.path(), ".claude/settings.json", "{not json");
        drive(home.path(), &windows_resolved("C:/work/proj"));

        let body = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
        assert_eq!(body, "{not json");
    }

    #[test]
    fn trailing_newline_preserved_when_present() {
        let home = TempDir::new().unwrap();
        write_file(home.path(), ".claude/settings.json", "{\"hooks\":[]}\n");
        drive(home.path(), &windows_resolved("C:/work/proj"));

        let body = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
        assert!(
            body.ends_with('\n'),
            "expected trailing newline, got: {:?}",
            body
        );
    }

    #[test]
    fn no_trailing_newline_added_if_not_present() {
        let home = TempDir::new().unwrap();
        write_file(home.path(), ".claude/settings.json", "{\"hooks\":[]}");
        drive(home.path(), &windows_resolved("C:/work/proj"));

        let body = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
        assert!(!body.ends_with("\n\n"));
    }

    #[test]
    fn wsl_guest_path_is_skipped() {
        let home = TempDir::new().unwrap();
        write_file(home.path(), ".claude/settings.json", "{}");
        // The Windows-side `~/.claude/settings.json` is not the right place to
        // store WSL-guest trust — the guest CLI reads from its own home.
        let resolved = wsl_resolved("\\\\wsl$\\Ubuntu\\home\\user\\proj"); // allow-wsl-path
        drive(home.path(), &resolved);

        let body = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
        assert_eq!(body, "{}");
    }

    #[test]
    fn atomic_write_leaves_no_tmp_residue_on_success() {
        let home = TempDir::new().unwrap();
        write_file(home.path(), ".claude/settings.json", "{}");
        drive(home.path(), &windows_resolved("C:/work/proj"));

        let tmp = home.path().join(".claude/settings.json.tmp");
        assert!(!tmp.exists(), "tmp file leaked: {:?}", tmp);
    }

    #[test]
    fn normalize_path_windows_target() {
        if cfg!(target_os = "windows") {
            assert_eq!(normalize_path("C:/work/proj"), "C:\\work\\proj");
        } else {
            // Cross-compile check: when building for non-windows the path
            // is preserved as-is.
            assert_eq!(normalize_path("C:/work/proj"), "C:/work/proj");
        }
    }

    #[test]
    fn multiple_paths_accumulate() {
        let home = TempDir::new().unwrap();
        write_file(home.path(), ".claude/settings.json", "{}");
        drive(home.path(), &windows_resolved("C:/work/proj1"));
        drive(home.path(), &windows_resolved("C:/work/proj2"));

        let list = read_trusted(home.path(), ".claude/settings.json");
        assert_eq!(list.len(), 2);
        assert!(list.contains(&"C:\\work\\proj1".to_string()));
        assert!(list.contains(&"C:\\work\\proj2".to_string()));
    }
}
