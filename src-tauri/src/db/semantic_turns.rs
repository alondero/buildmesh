//! Semantic-turn persistence (app_settings keys keyed by agent node).

use rusqlite::params;

use super::{read_conn, write_conn, SqlResult};

pub(crate) const SEMANTIC_TURN_KEY_PREFIX: &str = "semantic_turn:";

pub fn persist_semantic_turn(node_id: i64, value: Option<&str>) -> SqlResult<()> {
    let conn = write_conn();
    let key = format!("{SEMANTIC_TURN_KEY_PREFIX}{node_id}");
    match value {
        Some(value) => conn.execute("INSERT OR REPLACE INTO app_settings (key,value) VALUES (?1,?2)", params![key, value])?,
        None => conn.execute("DELETE FROM app_settings WHERE key=?1", params![key])?,
    };
    Ok(())
}

pub fn list_semantic_turns() -> SqlResult<Vec<(i64, String)>> {
    let conn = read_conn();
    let mut stmt = conn.prepare("SELECT key,value FROM app_settings WHERE key LIKE ?1")?;
    let rows = stmt.query_map(params![format!("{SEMANTIC_TURN_KEY_PREFIX}%")], |row| {
        let key: String = row.get(0)?;
        let id = key[SEMANTIC_TURN_KEY_PREFIX.len()..].parse::<i64>().unwrap_or(0);
        Ok((id, row.get(1)?))
    })?;
    rows.collect()
}
