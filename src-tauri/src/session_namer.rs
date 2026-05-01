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

async fn summarize_and_rename(session_id: i64, buffer: &str) -> Result<String, String> {
    let prompt = format!(
        "Summarize this terminal session in exactly 3-5 lowercase hyphenated words as a slug \
         (e.g. fix-auth-token-refresh). Only output the slug, nothing else:\n\n{}",
        buffer
    );

    tracing::info!("session_namer: running summarize command for session {}", session_id);

    let output = build_summarize_command(&prompt)
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
    let slug = validate_slug(&raw)?;

    crate::db::update_session_name(session_id, &slug).map_err(|e| e.to_string())?;

    Ok(slug)
}

fn build_summarize_command(prompt: &str) -> tokio::process::Command {
    let is_macos = cfg!(target_os = "macos");

    if is_macos {
        let claude_path = which_claude().unwrap_or_else(|| "claude".into());
        let mut cmd = tokio::process::Command::new(claude_path);
        cmd.args(["-p", prompt, "--output-format", "text"]);
        cmd
    } else {
        let mut cmd = tokio::process::Command::new("C:\\Windows\\System32\\cmd.exe");
        cmd.args(["/c", "cwrap", "anthropic", "-p", prompt]);
        cmd
    }
}

fn which_claude() -> Option<String> {
    let paths = [
        "/opt/homebrew/bin/claude",
        "/usr/local/bin/claude",
    ];
    for p in paths {
        if std::path::Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    None
}

fn validate_slug(raw: &str) -> Result<String, String> {
    let slug = raw
        .lines()
        .last()
        .unwrap_or(raw)
        .trim()
        .trim_matches('"')
        .trim_matches('`')
        .to_lowercase();

    let word_count = slug.split('-').count();
    if word_count < 3 || word_count > 5 {
        return Err(format!(
            "slug has {} words (expected 3-5): '{}'",
            word_count, slug
        ));
    }

    if !SLUG_REGEX.is_match(&slug) {
        return Err(format!("slug failed validation: '{}'", slug));
    }

    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_slug_accepts_valid() {
        assert_eq!(
            validate_slug("fix-auth-token-refresh").unwrap(),
            "fix-auth-token-refresh"
        );
        assert_eq!(
            validate_slug("  rendering-terminal-bug\n").unwrap(),
            "rendering-terminal-bug"
        );
    }

    #[test]
    fn validate_slug_rejects_too_short() {
        assert!(validate_slug("fix-it").is_err());
    }

    #[test]
    fn validate_slug_rejects_too_long() {
        assert!(validate_slug("one-two-three-four-five-six").is_err());
    }

    #[test]
    fn buffer_caps_at_max() {
        let big = "x".repeat(5000);
        append_output(999, &big);
        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(buffers.get(&999).unwrap().len() <= MAX_BUFFER_CHARS);
    }
}
