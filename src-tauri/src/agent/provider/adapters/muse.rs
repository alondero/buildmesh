//! Muse Code 1.1.1 interactive CLI contract, checked against the installed
//! Linux CLI. See docs/learning/windows-wsl-harness-interop.md.
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct MuseAdapter;
pub static MUSE: MuseAdapter = MuseAdapter;

impl AgentProvider for MuseAdapter {
    fn id(&self) -> &'static str {
        "muse"
    }
    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Meta Muse".into(),
            color: "#0866ff".into(),
            icon: "M".into(),
        }
    }
    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "muse",
            base_args: vec![],
            trailing_args: vec![],
            windows_shell: WindowsShell::Direct,
        }
    }
    fn supports_resume(&self) -> bool {
        true
    }
    fn auto_resume_on_startup(&self) -> bool {
        true
    }
    fn self_assigns_session_id(&self) -> bool {
        true
    }
    fn captures_session_id_from_pty(&self) -> bool {
        false
    }
    fn requires_attention_hook(&self) -> bool {
        false
    }
    fn produces_readable_transcript(&self) -> bool {
        false
    }
    fn supports_model_override(&self) -> bool {
        true
    }
    fn supports_extra_args(&self) -> bool {
        true
    }
    fn supports_prefill(&self) -> bool {
        true
    }
    // Resume accepts a session UUID but does not document a positional prompt.
    fn prefill_requires_pty(&self, _text: &str) -> bool {
        true
    }
    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Linux, Platform::Macos]
    }
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["resume".into(), id.into()]
    }
    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }

    fn recover_suspended_session_id(
        &self,
        spawn_path: &str,
        _env_type: EnvType,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        let native = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(std::path::PathBuf::from)?
            .join(".local/share/muse");
        let home = crate::env::cli_dir_for_spawn(native, ".local/share/muse", spawn_path)?;
        find_session(
            &home.join("session-index.db"),
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    fn after_fresh_spawn(
        &self,
        node_id: i64,
        _spawn_path: &str,
        _env_type: EnvType,
        _app: &tauri::AppHandle,
    ) {
        tauri::async_runtime::spawn(async move {
            for delay in [200, 500, 1000, 2000, 4000, 8000] {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                    break;
                }
                let result = crate::blocking::run_blocking("muse_session_capture", move || {
                    crate::services::session_recovery::recover_live_node(node_id)
                })
                .await;
                if matches!(result, Ok(Some(_))) {
                    break;
                }
            }
        });
    }
}

fn find_session(
    database: &std::path::Path,
    workspace: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    connection
        .busy_timeout(std::time::Duration::from_millis(200))
        .ok()?;
    // Muse's index can leave workspace/timestamp columns NULL. Read only
    // the session metadata frame from the indexed log, never transcript text.
    let mut statement = connection.prepare("SELECT session_id, session_log_path FROM sessions ORDER BY session_log_path DESC LIMIT 128").ok()?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok()?;
    let indexed: Vec<_> = rows.filter_map(Result::ok).collect();
    drop(statement);
    drop(connection);
    let candidates = indexed.into_iter().filter_map(|(id, path)| {
        let path = crate::env::to_host_path(&path);
        let (recorded_id, cwd, timestamp) = session_metadata(std::path::Path::new(&path))?;
        (id == recorded_id && crate::env::directories_match(&cwd, workspace))
            .then_some((id, timestamp))
    });
    crate::services::session_recovery::select_recovery_identity(
        candidates,
        anchor_ms,
        recorded_start,
    )
}

fn session_metadata(path: &std::path::Path) -> Option<(String, String, i64)> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file.take(262_144)).lines().take(64) {
        let line = line.ok()?;
        let frame: serde_json::Value = serde_json::from_str(&line).ok()?;
        if let Some(metadata) = metadata_record(&frame) {
            return Some(metadata);
        }
        let Some(children) = frame.get("children").and_then(|c| c.as_array()) else {
            continue;
        };
        for child in children {
            let Some(json) = child.get("record_json").and_then(|r| r.as_str()) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<serde_json::Value>(json) else {
                continue;
            };
            if let Some(metadata) = metadata_record(&record) {
                return Some(metadata);
            }
        }
    }
    None
}

fn metadata_record(record: &serde_json::Value) -> Option<(String, String, i64)> {
    if record.get("payload_type").and_then(|p| p.as_str()) != Some("runtime.session.metadata")
        || record.pointer("/stream/kind").and_then(|s| s.as_str()) != Some("session")
    {
        return None;
    }
    let id = record.pointer("/stream/id")?.as_str()?;
    uuid::Uuid::parse_str(id).ok()?;
    let cwd = record.pointer("/payload/record/workspace_root")?.as_str()?;
    let timestamp = record.get("recorded_at")?.as_i64()?;
    Some((id.into(), cwd.into(), timestamp / 1000))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn muse_uses_documented_interactive_arguments() {
        assert_eq!(
            MUSE.spawn_recipe(Platform::Linux, EnvType::Wsl).binary,
            "muse"
        );
        assert_eq!(MUSE.resume_args("session-uuid"), ["resume", "session-uuid"]);
        assert_eq!(MUSE.prefill_args("fix the bug"), ["fix the bug"]);
        assert_eq!(MUSE.model_args("model-id"), ["--model", "model-id"]);
        assert!(!MUSE.captures_session_id_from_pty());
        assert!(MUSE.prefill_requires_pty("follow-up"));
    }

    #[test]
    fn muse_session_index_matches_workspace_and_refuses_ambiguous_identity() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("session-index.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("CREATE TABLE sessions (session_id TEXT, session_log_path TEXT);")
            .unwrap();
        let insert = |id: &str, workspace: &str, timestamp: i64, wrapped: bool| {
            let log = directory.path().join(format!("{id}.jsonl"));
            let record = serde_json::json!({"payload_type": "runtime.session.metadata", "stream": {"kind":"session", "id":id}, "recorded_at":timestamp, "payload":{"record":{"workspace_root":workspace}}});
            let frame = if wrapped {
                serde_json::json!({"retained_frame":"session_permission_transaction","children":[{"record_json":record.to_string()}]})
            } else {
                record
            };
            std::fs::write(&log, format!("{{}}\n{frame}\n")).unwrap();
            connection
                .execute(
                    "INSERT INTO sessions VALUES (?1, ?2)",
                    rusqlite::params![id, log.to_str().unwrap()],
                )
                .unwrap();
        };
        insert(
            "12345678-1234-4234-8234-123456789abc",
            "/workspace",
            10000000,
            false,
        );
        insert(
            "22345678-1234-4234-8234-123456789abc",
            "/other",
            10000000,
            true,
        );
        assert_eq!(
            find_session(&database, "/workspace", 10000, true).as_deref(),
            Some("12345678-1234-4234-8234-123456789abc")
        );
        assert_eq!(find_session(&database, "/workspace", 20000, true), None);
        insert(
            "32345678-1234-4234-8234-123456789abc",
            "/workspace",
            11000000,
            true,
        );
        assert_eq!(find_session(&database, "/workspace", 10000, true), None);
    }
}
