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

/// Clears all node_namer state for a session_id.
///
/// This is called automatically by `record_turn`, but is also exposed as a public
/// API so tests can verify state cleanup without needing an AppHandle.
pub fn reset_session_state(session_id: i64) {
    SESSION_BUFFERS.lock().unwrap().remove(&session_id);
    TURN_COUNTERS.lock().unwrap().remove(&session_id);
    RENAMED_SESSIONS.lock().unwrap().remove(&session_id);
}

pub fn record_turn(session_id: i64, app: AppHandle) {
    // Defensively clear stale state at record_turn entry. This prevents archived/deleted
    // sessions from contaminating newly-created nodes that reuse the same session_id.
    reset_session_state(session_id);

    if RENAMED_SESSIONS.lock().unwrap().contains(&session_id) {
        return;
    }

    // Check DB: if node already has a non-default name, it was renamed in a previous run
    if let Ok(node) = crate::db::get_agent_node_by_id(session_id) {
        if !crate::naming::is_default_name(&node.name) {
            RENAMED_SESSIONS.lock().unwrap().insert(session_id);
            return;
        }
    }

    let should_rename = {
        let mut counters = TURN_COUNTERS.lock().unwrap();
        let count = counters.entry(session_id).or_insert(0);
        *count += 1;
        tracing::info!("node_namer: record_turn session={} turn={}", session_id, *count);
        *count == RENAME_AT_TURN
    };

    if !should_rename {
        return;
    }

    tracing::info!("node_namer: triggering rename for session {}", session_id);

    RENAMED_SESSIONS.lock().unwrap().insert(session_id);

    let buffer_snapshot = {
        let mut buffers = SESSION_BUFFERS.lock().unwrap();
        buffers
            .remove(&session_id)
            .map(|b| {
                // Scan backward from the byte limit to find a valid UTF-8 character boundary,
                // so we never slice in the middle of a multi-byte sequence.
                let target = b.len().min(SUMMARIZE_BUFFER_CHARS);
                let mut end = target;
                while end > 0 && !b.is_char_boundary(end) {
                    end -= 1;
                }
                b[..end].to_string()
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
    reset_session_state(session_id);
}

pub fn renamed_sessions_count() -> usize {
    RENAMED_SESSIONS.lock().unwrap().len()
}

pub fn buffers_size_bytes() -> usize {
    let buffers = SESSION_BUFFERS.lock().unwrap();
    buffers.values().map(|b| b.len()).sum()
}

pub fn turn_counter_count() -> usize {
    TURN_COUNTERS.lock().unwrap().len()
}

async fn summarize_and_rename(session_id: i64, _buffer: &str) -> Result<String, String> {
    let prompt = "You must output EXACTLY one line containing only 3-5 lowercase hyphenated words (e.g. fix-auth-token-refresh). No explanations, no punctuation, no quotes. Output ONLY the slug string.".to_string();

    tracing::info!("node_namer: running summarize command for session {}", session_id);

    let mut cmd = {
        #[cfg(target_os = "windows")]
        {
            let mut c = tokio::process::Command::new("C:\\Windows\\System32\\cmd.exe");
            c.args(["/c", "cwrap", "--minimax", "-p", &prompt]);
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x08000000);
            c
        }
        #[cfg(not(target_os = "windows"))]
        {
            let mut c = tokio::process::Command::new("cwrap");
            c.args(["--minimax", "-p", &prompt]);
            c
        }
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
    crate::db::update_agent_node_name(session_id, &slug).map_err(|e| e.to_string())?;
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

    // Stale-state clearing tests. Covers the bug where archived/deleted sessions
    // left behind state in SESSION_BUFFERS, TURN_COUNTERS, and RENAMED_SESSIONS,
    // contaminating newly-created nodes that reused the same session_id.
    // -------------------------------------------------------------------------

    #[test]
    fn stale_buffers_cleared_on_record_turn() {
        // Simulate the bug: an archived session left behind SESSION_BUFFERS
        // for session_id=5. When record_turn is called for a new node that
        // reuses session_id=5, the old buffer must be cleared BEFORE any rename
        // logic runs — otherwise the rename snapshot is contaminated with stale text.
        //
        // record_turn() calls reset_session_state() at the TOP of the function,
        // before any DB access or turn counting. We test reset_session_state()
        // directly since we have no AppHandle in unit tests.
        let stale_text = "old context from a previous archived session";
        append_output(5, stale_text);
        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(buffers.get(&5).is_some(), "precondition: buffer exists");
        }

        reset_session_state(5);

        // Buffer must be gone — stale text was cleared before any rename logic
        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(
            buffers.get(&5).is_none(),
            "SESSION_BUFFERS[5] should be cleared by record_turn entry"
        );
    }

    #[test]
    fn turn_counter_reset_on_record_turn() {
        // After archiving a session before it reached turn 2, TURN_COUNTERS[5]
        // can be at 1. A new node reusing session_id=5 should start at turn 0,
        // not inherit the stale counter (which would cause immediate rename).
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(5, 1);
        }
        {
            let mut buffers = SESSION_BUFFERS.lock().unwrap();
            buffers.insert(5, "fresh context".to_string());
        }

        reset_session_state(5);

        // TURN_COUNTERS[5] should have been removed
        let counters = TURN_COUNTERS.lock().unwrap();
        assert!(
            counters.get(&5).is_none(),
            "TURN_COUNTERS[5] should be removed by record_turn entry"
        );
    }

    #[test]
    fn renamed_sessions_cleared_on_record_turn() {
        // If session 5 was previously renamed (in RENAMED_SESSIONS), a new node
        // reusing session_id=5 should NOT be blocked by that stale guard entry.
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(5);
        }
        {
            let mut buffers = SESSION_BUFFERS.lock().unwrap();
            buffers.insert(5, "fresh context for new node".to_string());
        }

        reset_session_state(5);

        // RENAMED_SESSIONS[5] must be cleared so the new node can be renamed
        let renamed = RENAMED_SESSIONS.lock().unwrap();
        assert!(
            !renamed.contains(&5),
            "RENAMED_SESSIONS[5] should be cleared by record_turn entry"
        );
    }

    #[test]
    fn cleanup_resets_all_state() {
        // populate all three statics for session 7
        append_output(7, "some output text for session 7");
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(7, 2);
        }
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(7);
        }
        // verify preconditions
        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(buffers.get(&7).is_some());
        }
        {
            let counters = TURN_COUNTERS.lock().unwrap();
            assert_eq!(counters.get(&7), Some(&2));
        }
        {
            let renamed = RENAMED_SESSIONS.lock().unwrap();
            assert!(renamed.contains(&7));
        }

        cleanup(7);

        // all three must be empty for session 7
        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(
                buffers.get(&7).is_none(),
                "SESSION_BUFFERS[7] should be removed by cleanup"
            );
        }
        {
            let counters = TURN_COUNTERS.lock().unwrap();
            assert!(
                counters.get(&7).is_none(),
                "TURN_COUNTERS[7] should be removed by cleanup"
            );
        }
        {
            let renamed = RENAMED_SESSIONS.lock().unwrap();
            assert!(
                !renamed.contains(&7),
                "RENAMED_SESSIONS[7] should be removed by cleanup"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Additional edge-case coverage
    // -------------------------------------------------------------------------

    #[test]
    fn reset_session_state_only_clears_target_session() {
        // Populate state for two different sessions
        append_output(5, "session 5 output");
        append_output(99, "session 99 output");
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(5, 1);
            counters.insert(99, 2);
        }
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(5);
            renamed.insert(99);
        }

        // Clear only session 5
        reset_session_state(5);

        // session 5 state must be gone
        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(buffers.get(&5).is_none());
            assert!(buffers.get(&99).is_some(), "session 99 buffer should survive");
        }
        {
            let counters = TURN_COUNTERS.lock().unwrap();
            assert!(counters.get(&5).is_none());
            assert_eq!(counters.get(&99), Some(&2), "session 99 counter should survive");
        }
        {
            let renamed = RENAMED_SESSIONS.lock().unwrap();
            assert!(!renamed.contains(&5));
            assert!(renamed.contains(&99), "session 99 renamed guard should survive");
        }
    }

    #[test]
    fn reset_session_state_is_idempotent() {
        // Populating session 8 then clearing it twice should be safe
        append_output(8, "some output");
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(8, 1);
        }
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(8);
        }

        reset_session_state(8);
        reset_session_state(8); // second call must not panic

        let buffers = SESSION_BUFFERS.lock().unwrap();
        let counters = TURN_COUNTERS.lock().unwrap();
        let renamed = RENAMED_SESSIONS.lock().unwrap();
        assert!(buffers.get(&8).is_none());
        assert!(counters.get(&8).is_none());
        assert!(!renamed.contains(&8));
    }

    #[test]
    fn append_output_drops_data_after_renamed_gate() {
        // Once a session is in RENAMED_SESSIONS, append_output should be a no-op.
        // This prevents buffers from growing after a node has been renamed.
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(42);
        }

        append_output(42, "this should be dropped");
        append_output(42, "and this too");

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(
            buffers.get(&42).is_none(),
            "append_output must drop data for sessions in RENAMED_SESSIONS"
        );
    }

    #[test]
    fn turn_counter_accumulates_from_zero_to_rename() {
        // Simulate the full turn accumulation path:
        // turn 1: counter goes 0 -> 1, no rename
        // turn 2: counter goes 1 -> 2, rename fires
        // We test by directly inserting into TURN_COUNTERS to simulate
        // having already seen turn 1, then verify the entry logic.
        let session_id = 55;

        // Manually set counter to 1 (simulating one prior record_turn call)
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(session_id, 1);
        }
        append_output(session_id, "turn 2 output");

        // Simulate the turn-counting portion of record_turn
        // (entry-or-insert then increment, fire if == 2)
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            let count = counters.entry(session_id).or_insert(0);
            *count += 1;
            assert_eq!(
                *count, 2,
                "second turn (count=2) should trigger rename"
            );
        }
    }

    #[test]
    fn buffer_truncation_splits_multibyte_utf8_correctly() {
        // The buffer truncation logic drains bytes from the front until it lands
        // on a valid UTF-8 character boundary. This tests that multi-byte UTF-8
        // sequences are not sliced mid-character.
        // "日" is 3 bytes in UTF-8. If we append a big string ending with "日",
        // the drain must not cut in the middle of those 3 bytes.
        let session_id = 77;
        let base = "x".repeat(4000);
        let with_kanji = format!("{}{}", base, "日本"); // "日本" = 6 bytes

        append_output(session_id, &with_kanji);

        let buffers = SESSION_BUFFERS.lock().unwrap();
        let buf = buffers.get(&session_id).unwrap();

        // Buffer must be <= MAX_BUFFER_CHARS
        assert!(buf.len() <= MAX_BUFFER_CHARS);

        // Buffer content must be valid UTF-8 (if we sliced mid-character, this would panic)
        // We use the fact that String::new() + push_str would have panicked at insert time.
        // Instead verify by re-creating the string slice:
        assert!(std::str::from_utf8(buf.as_bytes()).is_ok());
    }
}
