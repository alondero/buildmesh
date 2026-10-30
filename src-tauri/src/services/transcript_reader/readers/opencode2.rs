//! OpenCode 2's message/part store. Never query V1 tables for a V2 identity.
use super::opencode::{opencode_resolve, parse_opencode_messages_with_text_limit};
use crate::services::transcript_reader::{
    adapter::{LocateCtx, TranscriptAdapter},
    types::{Parsed, Turn, UnavailableReason, MAX_TURN_TEXT},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub(crate) struct OpenCode2Adapter;

impl TranscriptAdapter for OpenCode2Adapter {
    fn id(&self) -> &'static str {
        "opencode2"
    }
    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        opencode_resolve(Some(ctx.session_id), ctx.node_path)
            .ok()
            .map(|(p, _)| p)
    }
    fn parse(
        &self,
        _lines: Box<dyn Iterator<Item = String> + '_>,
        _keep: usize,
        _max_text: usize,
    ) -> Parsed {
        parse_messages(&[], 1, MAX_TURN_TEXT)
    }
    fn line_has_assistant_text(&self, _line: &str) -> bool {
        false
    }
    fn read_tail(
        &self,
        path: &Path,
        session_id: &str,
        keep: usize,
    ) -> Result<Parsed, UnavailableReason> {
        let rows = read_rows(path, session_id, keep.saturating_mul(3).max(50), false)
            .ok_or(UnavailableReason::Unreadable)?;
        Ok(parse_messages(&rows, keep, MAX_TURN_TEXT))
    }
    fn last_assistant_message(
        &self,
        path: &Path,
        session_id: &str,
    ) -> Result<Parsed, UnavailableReason> {
        self.read_tail(path, session_id, 1)
    }
    fn naming_dialogue(
        &self,
        path: &Path,
        session_id: &str,
    ) -> Result<Vec<Turn>, UnavailableReason> {
        let rows = read_rows(path, session_id, 50, true).ok_or(UnavailableReason::Unreadable)?;
        let mut turns = Vec::new();
        for row in rows {
            super::super::types::append_naming_turns(
                &mut turns,
                parse_messages(&[row], 4, MAX_TURN_TEXT).turns,
            );
            if turns.len() == 4 {
                break;
            }
        }
        Ok(turns)
    }
    fn assistant_report(
        &self,
        path: &Path,
        session_id: &str,
    ) -> Option<super::super::AssistantReport> {
        let snapshot = super::super::report_snapshot::read_opencode2_file(path, session_id).ok()?;
        Some(super::super::AssistantReport {
            text: snapshot.text,
            revision: snapshot.revision,
        })
    }
}

pub(crate) fn parse_messages(rows: &[(String, Value)], keep: usize, max_text: usize) -> Parsed {
    let messages: Vec<_> = rows
        .iter()
        .filter(|(_, v)| !matches!(v["info"]["role"].as_str(), Some("idle" | "system")))
        .map(|(_, v)| v.clone())
        .collect();
    parse_opencode_messages_with_text_limit(&messages, keep, max_text)
}

pub(crate) fn read_rows(
    path: &Path,
    session_id: &str,
    limit: usize,
    oldest: bool,
) -> Option<Vec<(String, Value)>> {
    crate::http::request::parse_opencode_session_id(session_id)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(100))
        .ok()?;
    // One statement observes messages and parts from the same SQLite snapshot.
    let order = if oldest { "ASC" } else { "DESC" };
    let sql = format!("WITH recent AS (SELECT id, session_id, type, seq, data FROM session_message WHERE session_id=?1 ORDER BY seq {order} LIMIT ?2) SELECT m.id,m.type,m.data,p.type,p.call_id,p.data FROM recent m LEFT JOIN session_part p ON p.message_id=m.id AND p.session_id=m.session_id ORDER BY m.seq {order},p.ordinal ASC");
    let mut stmt = conn.prepare(&sql).ok()?;
    let mut rows = stmt
        .query(rusqlite::params![
            session_id,
            limit.min(i64::MAX as usize) as i64
        ])
        .ok()?;
    let mut result: Vec<(String, Value)> = Vec::new();
    while let Some(row) = rows.next().ok()? {
        let id: String = row.get(0).ok()?;
        let role: String = row.get(1).ok()?;
        if result.last().is_none_or(|(prev, _)| prev != &id) {
            let data: String = row.get(2).ok()?;
            let data: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
            let valid_shape = data.is_object()
                && (role != "user" || data.get("text").is_some_and(Value::is_string))
                && (role != "assistant" || data.get("content").is_none_or(Value::is_array));
            let message = if valid_shape {
                let mut info = data.clone();
                info["role"] = role.clone().into();
                // V2 user text and migrated assistant content may be inline.
                let parts = if role == "user" {
                    json!([{"type":"text", "text":data["text"]}])
                } else {
                    data.get("content").cloned().unwrap_or(json!([]))
                };
                json!({"info":info,"parts":parts})
            } else {
                json!({"info":null,"parts":[]})
            };
            result.push((id, message));
        }
        if let Some(raw) = row.get::<_, Option<String>>(5).ok()? {
            let message = &mut result.last_mut()?.1;
            match serde_json::from_str::<Value>(&raw) {
                Ok(mut part) if part.is_object() => {
                    let typ: String = row.get(3).ok()?;
                    part["type"] = typ.into();
                    if let Some(call) = row.get::<_, Option<String>>(4).ok()? {
                        part["callID"] = call.into();
                    }
                    // Native part rows supersede any inline migrated content.
                    if message.get("native_parts").is_none() {
                        message["parts"] = json!([]);
                        message["native_parts"] = true.into();
                    }
                    message["parts"].as_array_mut()?.push(part);
                }
                _ => message["info"] = Value::Null,
            }
        }
    }
    if !oldest {
        result.reverse();
    }
    Some(result)
}

/// Discover V2 root conversations with explicit provenance; never scan a sibling
/// repository simply because it shares the mesh's directory-name prefix.
pub(crate) fn discover(
    path: &Path,
    mesh: &str,
    worktrees: &str,
    tracked: &std::collections::HashSet<String>,
) -> Result<Vec<crate::services::agent_node_discovery::ArchivedAgentNode>, String> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_millis(100))
        .map_err(|e| e.to_string())?;
    let has_v2: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_v2')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !has_v2 {
        return Ok(vec![]);
    }
    let normalize = |s: &str| {
        let s = s.replace('\\', "/").trim_end_matches('/').to_string();
        if s.as_bytes().get(1) == Some(&b':') || s.starts_with("//") {
            s.to_ascii_lowercase()
        } else {
            s
        }
    };
    let root = normalize(mesh);
    let container = normalize(worktrees);
    let mut stmt = conn.prepare("SELECT id,directory,time_updated FROM session_v2 WHERE parent_id IS NULL AND (lower(replace(directory,char(92),'/'))=lower(?1) OR (?2!='/' AND instr(lower(replace(directory,char(92),'/')),lower(?2))=1)) ORDER BY time_updated DESC LIMIT 1000").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![root, format!("{container}/")], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for row in rows {
        let (id, directory, time) = row.map_err(|e| e.to_string())?;
        let normalized = normalize(&directory);
        if tracked.contains(&id)
            || crate::http::request::parse_opencode_session_id(&id).is_none()
            || (normalized != root
                && (container.is_empty() || !normalized.starts_with(&format!("{container}/"))))
        {
            continue;
        }
        let data: Option<String> = conn.query_row("SELECT data FROM session_message WHERE session_id=?1 AND type='user' ORDER BY seq ASC LIMIT 1", [&id], |row| row.get(0)).optional().map_err(|e| e.to_string())?;
        let first = match data {
            Some(data) => serde_json::from_str::<Value>(&data)
                .map_err(|e| format!("Malformed OpenCode 2 archive message: {e}"))?["text"]
                .as_str()
                .map(str::to_owned)
                .ok_or("Malformed OpenCode 2 user message")?,
            None => continue,
        };
        result.push(crate::services::agent_node_discovery::ArchivedAgentNode {
            session_id: id,
            first_message: super::super::types::truncate(&first, 500),
            branch: None,
            cwd: Some(directory),
            timestamp: chrono::DateTime::from_timestamp_millis(time).map(|t| t.to_rfc3339()),
            worktree_name: if normalized == root {
                None
            } else {
                Path::new(&normalized)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            },
            resumable: true,
            provider: Some("opencode2".into()),
            node_id: None,
        });
    }
    Ok(result)
}

#[cfg(test)]
#[path = "opencode2_tests.rs"]
mod tests;
