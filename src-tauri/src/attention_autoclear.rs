//! Auto-clear stale attention when an agent resumes on its own (issue #878).
//!
//! The hook-payload classification in `http::routes::attention` prevents most
//! false "awaiting input" marks, but it can't see everything: providers with
//! no hooks at all (Codex, OpenCode), a transcript whose format drifted, or a
//! lost task-notification. This module is the safety net for those cases: a
//! node marked `awaiting_input` whose PTY then produces a substantial burst of
//! output *without any user input* is evidently working again — flip it back
//! to `running` and broadcast `attention-cleared`.
//!
//! Two guards keep a genuine "awaiting input" mark from being falsely cleared:
//!
//! - **Grace window.** The Stop hook's POST races the terminal's end-of-turn
//!   redraw — the final message paint can land *after* the mark. Output inside
//!   the first [`GRACE_MS`] therefore doesn't count.
//! - **Burst threshold.** An idle CLI still emits the odd control sequence
//!   (cursor queries, status-line repaints). Only a cumulative
//!   [`BURST_BYTES`] of post-grace output reads as "resumed work". A resumed
//!   turn redraws far more than this within its first seconds.
//!
//! A node is *armed* when attention is marked and *disarmed* by any user
//! keystroke into it (the user is engaged; the existing Enter-driven clear in
//! `write_to_agent` owns the status flip from there) or by any other path
//! that clears attention itself (coordinator drive, autopilot injection,
//! mobile input).

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tauri::Emitter;

/// Ignore PTY output arriving this soon after the mark — it is the tail of the
/// turn the Stop hook just reported, not new work.
const GRACE_MS: u128 = 3_000;
/// Cumulative post-grace output that reads as "the agent resumed".
const BURST_BYTES: usize = 512;

struct Armed {
    marked_at: Instant,
    bytes_after_grace: usize,
}

static ARMED: Lazy<Mutex<HashMap<i64, Armed>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Arm auto-clear for a node that was just marked `awaiting_input`.
pub fn on_marked(node_id: i64) {
    ARMED.lock().unwrap().insert(
        node_id,
        Armed {
            marked_at: Instant::now(),
            bytes_after_grace: 0,
        },
    );
}

/// Disarm: the user typed into the node (or another path cleared attention
/// itself) — the stale-mark hypothesis no longer holds.
pub fn disarm(node_id: i64) {
    ARMED.lock().unwrap().remove(&node_id);
}

/// PTY reader hook — called for every output chunk. Returns fast for unarmed
/// nodes (one map lookup). When the post-grace burst threshold is crossed the
/// node is disarmed and its attention cleared.
pub fn on_output(node_id: i64, byte_len: usize) {
    if take_crossed(node_id, byte_len) {
        clear_now(node_id);
    }
}

/// The state transition alone (no DB/emit side effects, so tests can drive
/// it): accumulate the chunk and report whether the burst threshold was
/// crossed — in which case the node is disarmed here, exactly once.
fn take_crossed(node_id: i64, byte_len: usize) -> bool {
    let mut armed = ARMED.lock().unwrap();
    let Some(state) = armed.get_mut(&node_id) else {
        return false;
    };
    if !accumulate(
        &mut state.bytes_after_grace,
        state.marked_at.elapsed().as_millis(),
        byte_len,
    ) {
        return false;
    }
    armed.remove(&node_id);
    true
}

/// The pure arming logic: output inside the grace window is ignored; after it,
/// bytes accumulate until the burst threshold is crossed.
fn accumulate(bytes_after_grace: &mut usize, elapsed_ms: u128, byte_len: usize) -> bool {
    if elapsed_ms < GRACE_MS {
        return false;
    }
    *bytes_after_grace += byte_len;
    *bytes_after_grace >= BURST_BYTES
}

/// Flip the node back to `running` and broadcast `attention-cleared` — the
/// same fan-out every other clear path performs (`http::ws`,
/// `coordinator::drive`, `autopilot::pipeline`).
fn clear_now(node_id: i64) {
    tracing::info!(
        "Node {} attention auto-cleared: agent resumed output without user input (issue #878)",
        node_id
    );
    let _ = crate::db::update_agent_node_status(node_id, crate::models::SessionStatus::Running);
    if let Some(app) = crate::http::app_handle() {
        let _ = app.emit(
            "attention-cleared",
            crate::commands::attention::AttentionClearedPayload { session_id: node_id },
        );
    }
    crate::http::events::emit(crate::http::events::EventMsg::AttentionCleared {
        session_id: node_id,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn output_inside_grace_window_never_accumulates() {
        // The end-of-turn redraw race: a huge paint right after the mark must
        // not clear a genuine "awaiting input".
        let mut bytes = 0;
        assert!(!accumulate(&mut bytes, 0, BURST_BYTES * 10));
        assert!(!accumulate(&mut bytes, GRACE_MS - 1, BURST_BYTES * 10));
        assert_eq!(bytes, 0, "grace-window output must not count");
    }

    #[test]
    fn post_grace_burst_crosses_threshold_cumulatively() {
        let mut bytes = 0;
        assert!(!accumulate(&mut bytes, GRACE_MS, BURST_BYTES / 2));
        assert!(
            accumulate(&mut bytes, GRACE_MS + 100, BURST_BYTES / 2),
            "chunks accumulate — a resumed turn's redraw arrives in many small writes"
        );
    }

    #[test]
    fn trickle_below_threshold_never_clears() {
        // An idle CLI's occasional control sequences stay below the burst bar.
        let mut bytes = 0;
        assert!(!accumulate(&mut bytes, GRACE_MS + 500, 16));
        assert!(!accumulate(&mut bytes, GRACE_MS + 900, 16));
        assert!(bytes < BURST_BYTES);
    }

    #[test]
    fn unarmed_node_output_is_a_no_op() {
        // No mark → nothing to clear, whatever the node prints.
        on_output(990_001, BURST_BYTES * 10);
        assert!(!ARMED.lock().unwrap().contains_key(&990_001));
    }

    #[test]
    fn disarm_stops_accumulation() {
        let node = 990_002;
        on_marked(node);
        disarm(node);
        on_output(node, BURST_BYTES * 10);
        assert!(!ARMED.lock().unwrap().contains_key(&node));
    }

    #[test]
    fn armed_node_clears_after_grace_and_burst() {
        // Inject an already-elapsed mark so the test doesn't sleep through the
        // real grace window.
        let node = 990_003;
        ARMED.lock().unwrap().insert(
            node,
            Armed {
                marked_at: Instant::now() - Duration::from_millis((GRACE_MS + 1000) as u64),
                bytes_after_grace: 0,
            },
        );
        assert!(
            take_crossed(node, BURST_BYTES),
            "post-grace burst must cross the threshold"
        );
        assert!(
            !ARMED.lock().unwrap().contains_key(&node),
            "crossing the burst threshold disarms the node"
        );
        assert!(
            !take_crossed(node, BURST_BYTES),
            "the clear fires exactly once — a disarmed node never re-crosses"
        );
    }

    #[test]
    fn re_mark_rearms_with_fresh_state() {
        let node = 990_004;
        on_marked(node);
        {
            let mut armed = ARMED.lock().unwrap();
            let state = armed.get_mut(&node).unwrap();
            state.bytes_after_grace = BURST_BYTES - 1;
        }
        on_marked(node);
        assert_eq!(
            ARMED.lock().unwrap().get(&node).unwrap().bytes_after_grace,
            0,
            "a fresh mark resets the accumulator"
        );
        disarm(node);
    }
}
