use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

static SESSION_BUFFERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, String>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

static TURN_COUNTERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, u32>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

static RENAMED_SESSIONS: once_cell::sync::Lazy<Arc<Mutex<HashSet<i64>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashSet::new())));

static SLUG_REGEX: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[a-z][a-z0-9-]{2,50}$").unwrap());

const MAX_BUFFER_CHARS: usize = 4000;
const SUMMARIZE_BUFFER_CHARS: usize = 3000;
const RENAME_AT_TURN: u32 = 2;

pub fn append_output(session_id: i64, data: &str) {
    if RENAMED_SESSIONS.lock().unwrap().contains(&session_id) {
        return;
    }
    let mut buffers = SESSION_BUFFERS.lock().unwrap();
    let buf = buffers.entry(session_id).or_default();
    buf.push_str(data);
    if buf.len() > MAX_BUFFER_CHARS {
        let mut drain_to = buf.len() - MAX_BUFFER_CHARS;
        while !buf.is_char_boundary(drain_to) {
            drain_to += 1;
        }
        buf.drain(..drain_to);
    }
}

pub fn record_turn(session_id: i64, app: AppHandle) {
    if RENAMED_SESSIONS.lock().unwrap().contains(&session_id) {
        return;
    }

    // Check DB: if session already has a non-default name, it was renamed in a previous run
    if let Ok(session) = crate::db::get_session_by_id(session_id) {
        if !crate::naming::is_default_name(&session.name) {
            RENAMED_SESSIONS.lock().unwrap().insert(session_id);
            return;
        }
    }

    let should_rename = {
        let mut counters = TURN_COUNTERS.lock().unwrap();
        let count = counters.entry(session_id).or_insert(0);
        *count += 1;
        tracing::info!("session_namer: record_turn session={} turn={}", session_id, *count);
        *count == RENAME_AT_TURN
    };

    if !should_rename {
        return;
    }

    tracing::info!("session_namer: triggering rename for session {}", session_id);

    RENAMED_SESSIONS.lock().unwrap().insert(session_id);

    let buffer_snapshot = {
        let mut buffers = SESSION_BUFFERS.lock().unwrap();
        buffers
            .remove(&session_id)
            .map(|b| {
                let len = b.len().min(SUMMARIZE_BUFFER_CHARS);
                b[..len].to_string()
            })
            .unwrap_or_default()
    };

    if buffer_snapshot.is_empty() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        match summarize_and_rename(session_id, &buffer_snapshot).await {
            Ok(slug) => {
                let _ = app.emit(
                    "session-renamed",
                    serde_json::json!({
                        "session_id": session_id,
                        "name": slug
                    }),
                );
                tracing::info!("Session {} renamed to '{}'", session_id, slug);
            }
            Err(e) => {
                tracing::warn!("Session {} rename failed: {}", session_id, e);
            }
        }
    });
}

pub fn cleanup(session_id: i64) {
    SESSION_BUFFERS.lock().unwrap().remove(&session_id);
    TURN_COUNTERS.lock().unwrap().remove(&session_id);
    RENAMED_SESSIONS.lock().unwrap().remove(&session_id);
}

async fn summarize_and_rename(session_id: i64, _buffer: &str) -> Result<String, String> {
    let prompt = "You must output EXACTLY one line containing only 3-5 lowercase hyphenated words (e.g. fix-auth-token-refresh). No explanations, no punctuation, no quotes. Output ONLY the slug string.".to_string();

    tracing::info!("session_namer: running summarize command for session {}", session_id);

    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = tokio::process::Command::new("C:\\Windows\\System32\\cmd.exe");
        c.args(["/c", "cwrap", "--minimax", "-p", &prompt]);
        c
    } else {
        let mut c = tokio::process::Command::new("cwrap");
        c.args(["--minimax", "-p", &prompt]);
        c
    };

    let output = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run CLI: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "CLI exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let slug = slug_with_retry(&raw)?;
    crate::db::update_session_name(session_id, &slug).map_err(|e| e.to_string())?;
    Ok(slug)
}

fn slug_with_retry(raw: &str) -> Result<String, String> {
    // Extract just the first hyphenated slug-like line
    let candidate = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .find(|l| l.contains('-'))
        .map(|l| l.trim().trim_matches('"').trim_matches('`').to_lowercase())
        .unwrap_or_else(|| raw.trim().trim_matches('"').trim_matches('`').to_lowercase());

    let word_count = candidate.split('-').count();
    if word_count >= 3 && word_count <= 5 && SLUG_REGEX.is_match(&candidate) {
        return Ok(candidate);
    }

    // Fallback: extract longest run of hyphenated words
    let fallback = candidate.split_whitespace()
        .filter(|w| w.contains('-'))
        .max_by_key(|w| w.split('-').count())
        .unwrap_or(&candidate)
        .to_string();

    let fallback_count = fallback.split('-').count();
    if fallback_count >= 3 && fallback_count <= 5 && SLUG_REGEX.is_match(&fallback) {
        return Ok(fallback);
    }

    Err(format!(
        "slug has {} words (expected 3-5): '{}'",
        fallback_count.max(word_count),
        candidate
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_with_retry_accepts_valid() {
        assert_eq!(
            slug_with_retry("fix-auth-token-refresh").unwrap(),
            "fix-auth-token-refresh"
        );
        assert_eq!(
            slug_with_retry("  rendering-terminal-bug\n").unwrap(),
            "rendering-terminal-bug"
        );
    }

    #[test]
    fn slug_with_retry_rejects_too_short() {
        assert!(slug_with_retry("fix-it").is_err());
    }

    #[test]
    fn slug_with_retry_rejects_too_long() {
        assert!(slug_with_retry("one-two-three-four-five-six").is_err());
    }

    #[test]
    fn buffer_caps_at_max() {
        let big = "x".repeat(5000);
        append_output(999, &big);
        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(buffers.get(&999).unwrap().len() <= MAX_BUFFER_CHARS);
    }

    #[test]
    fn slug_with_retry_extracts_hyphenated_from_prose() {
        // If model outputs prose with a slug embedded, find the hyphenated part
        let result = slug_with_retry(
            "Based on the session, a good name would be: improve-auth-flow"
        );
        assert!(result.is_ok());
    }
}
