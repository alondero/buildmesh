//! Auto-clear stale attention when an agent resumes on its own (issue #878;
//! idle-gap hardening in #1222).
//!
//! The hook-payload classification in `http::routes::attention` prevents most
//! false "awaiting input" marks, but it can't see everything: providers with
//! no hooks at all (Codex, OpenCode), a transcript whose format drifted, or a
//! lost task-notification. This module is the safety net for those cases: a
//! node marked `awaiting_input` whose PTY then produces a substantial burst of
//! output *without any user input* is evidently working again — flip it back
//! to `running` and broadcast `attention-cleared`.
//!
//! Three guards keep a genuine "awaiting input" mark from being falsely cleared:
//!
//! - **Grace window.** The Stop hook's POST races the terminal's end-of-turn
//!   redraw — the final message paint can land *after* the mark. Output inside
//!   the first [`GRACE_MS`] therefore doesn't count.
//! - **Burst threshold.** An idle CLI still emits the odd control sequence
//!   (cursor queries, status-line repaints). Only a cumulative
//!   [`BURST_BYTES`] of post-grace output reads as "resumed work". A resumed
//!   turn redraws far more than this within its first seconds.
//! - **Idle gap reset** (issue #1222). The first two guards model the threat
//!   as "the agent produced a lot of output." Animated CLIs at their prompt
//!   (Claude Code's continuous spinner in particular) disprove that threat
//!   model: they emit a slow drip of control sequences that accumulates past
//!   [`BURST_BYTES`] over minutes and would falsely disarm a real
//!   "awaiting input". Instead we reset the accumulator whenever the gap
//!   between successive byte chunks exceeds [`BURST_GAP_RESET_MS`]. A genuine
//!   resumed turn emits dense output whose chunks are all well under that
//!   window apart, so it still crosses within one window; a spinner at
//!   prompt repaints slower and never accumulates enough.
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

/// Ignore PTY output arriving this soon after the mark — it is the tail of the
/// turn the Stop hook just reported, not new work.
const GRACE_MS: u128 = 3_000;
/// Cumulative post-grace output that reads as "the agent resumed".
const BURST_BYTES: usize = 512;
/// A gap between successive byte chunks longer than this resets the burst
/// accumulator — closes the "animated CLI drip" loophole (issue #1222). See
/// module docs. 10s sits well above typical spinner intervals yet well below
/// the cadence of a genuine resumed turn.
const BURST_GAP_RESET_MS: u128 = 10_000;

struct Armed {
    marked_at: Instant,
    bytes_after_grace: usize,
    /// Timestamp of the most recent byte chunk the PTY reader delivered to
    /// `on_output`. Drives the gap-reset guard (issue #1222). Initialised to
    /// the mark instant so the very first post-grace chunk's "gap" is
    /// effectively zero — the accumulator only becomes stale after the
    /// stream actually goes quiet.
    last_byte_at: Instant,
}

static ARMED: Lazy<Mutex<HashMap<i64, Armed>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Arm auto-clear for a node that was just marked `awaiting_input`.
pub fn on_marked(node_id: i64) {
    let now = Instant::now();
    ARMED.lock().unwrap().insert(
        node_id,
        Armed {
            marked_at: now,
            bytes_after_grace: 0,
            last_byte_at: now,
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
    if take_crossed(node_id, byte_len, Instant::now()) {
        clear_now(node_id);
    }
}

/// The state transition alone (no DB/emit side effects, so tests can drive
/// it): accumulate the chunk and report whether the burst threshold was
/// crossed — in which case the node is disarmed here, exactly once.
fn take_crossed(node_id: i64, byte_len: usize, now: Instant) -> bool {
    let mut armed = ARMED.lock().unwrap();
    let Some(state) = armed.get_mut(&node_id) else {
        return false;
    };
    let gap_ms = now.duration_since(state.last_byte_at).as_millis();
    let crossed = accumulate(
        &mut state.bytes_after_grace,
        state.marked_at.elapsed().as_millis(),
        gap_ms,
        byte_len,
    );
    // Advance the timestamp regardless of outcome — even when a stale gap
    // reset the accumulator, this chunk starts a fresh burst window.
    state.last_byte_at = now;
    if !crossed {
        return false;
    }
    armed.remove(&node_id);
    true
}

/// The pure arming logic: output inside the grace window is ignored; after
/// it, bytes accumulate until the burst threshold is crossed — but a gap
/// wider than [`BURST_GAP_RESET_MS`] between this chunk and the previous
/// one resets the accumulator first (issue #1222).
fn accumulate(
    bytes_after_grace: &mut usize,
    elapsed_ms: u128,
    gap_ms: u128,
    byte_len: usize,
) -> bool {
    if elapsed_ms < GRACE_MS {
        return false;
    }
    if gap_ms > BURST_GAP_RESET_MS {
        *bytes_after_grace = 0;
    }
    *bytes_after_grace += byte_len;
    *bytes_after_grace >= BURST_BYTES
}

/// Flip the node back to `running` and broadcast `attention-cleared` — the
/// same fan-out every other clear path performs (`http::ws`,
/// `coordinator::drive`, `autopilot::pipeline`). Routes through
/// `SessionLifecycle` (issue #132) for the DB write + desktop emit; the
/// mobile broadcast (`http::events`) is a separate channel kept here.
fn clear_now(node_id: i64) {
    tracing::info!(
        "Node {} attention auto-cleared: agent resumed output without user input (issue #878)",
        node_id
    );
    if let Some(app) = crate::http::app_handle() {
        let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app };
        let _ = crate::agent::session_lifecycle::on_attention_cleared(&sink, node_id);
    } else {
        // No app handle — write the status but skip the emit (matches
        // pre-refactor behaviour where the `if let Some(app)` branch
        // guarded the emit).
        let _ = crate::agent::session_lifecycle::on_attention_cleared(
            &crate::agent::session_lifecycle::DbOnlySink,
            node_id,
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
        assert!(!accumulate(&mut bytes, 0, 0, BURST_BYTES * 10));
        assert!(!accumulate(&mut bytes, GRACE_MS - 1, 0, BURST_BYTES * 10));
        assert_eq!(bytes, 0, "grace-window output must not count");
    }

    #[test]
    fn post_grace_burst_crosses_threshold_cumulatively() {
        let mut bytes = 0;
        assert!(!accumulate(&mut bytes, GRACE_MS, 100, BURST_BYTES / 2));
        assert!(
            accumulate(&mut bytes, GRACE_MS + 100, 100, BURST_BYTES / 2),
            "chunks accumulate — a resumed turn's redraw arrives in many small writes"
        );
    }

    #[test]
    fn trickle_below_threshold_never_clears() {
        // An idle CLI's occasional control sequences stay below the burst bar.
        let mut bytes = 0;
        assert!(!accumulate(&mut bytes, GRACE_MS + 500, 500, 16));
        assert!(!accumulate(&mut bytes, GRACE_MS + 900, 500, 16));
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
        // real grace window. Set last_byte_at far enough into the past that
        // gap > BURST_GAP_RESET_MS would NOT reset (we want the dense path).
        let node = 990_003;
        let now = Instant::now();
        ARMED.lock().unwrap().insert(
            node,
            Armed {
                marked_at: now - Duration::from_millis((GRACE_MS + 1000) as u64),
                bytes_after_grace: 0,
                last_byte_at: now,
            },
        );
        assert!(
            take_crossed(node, BURST_BYTES, now),
            "post-grace burst must cross the threshold"
        );
        assert!(
            !ARMED.lock().unwrap().contains_key(&node),
            "crossing the burst threshold disarms the node"
        );
        assert!(
            !take_crossed(node, BURST_BYTES, Instant::now()),
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

    // Issue #1222 — gap-reset guard. The signature here is `(bytes,
    // elapsed_ms, gap_ms, byte_len)`; the new `gap_ms` parameter is what
    // powers the staleness reset. Tests below exercise the contract.

    #[test]
    fn stale_gap_resets_accumulator() {
        // Two post-grace chunks below threshold with small gaps keep
        // accumulating: 80 + 80 = 160 (still under).
        let mut bytes = 0;
        assert!(!accumulate(&mut bytes, GRACE_MS + 100, 100, 80));
        assert!(!accumulate(&mut bytes, GRACE_MS + 200, 100, 80));
        assert_eq!(bytes, 160, "tied chunks within window accumulate");
        // Now a gap wider than the reset window: the next chunk starts fresh.
        assert!(!accumulate(&mut bytes, GRACE_MS + 12_000, 12_000, 80));
        assert_eq!(
            bytes, 80,
            "stale gap → accumulator reset to just the new chunk"
        );
    }

    #[test]
    fn spinner_spread_over_60s_never_crosses_threshold() {
        // 20-byte chunks every 15 seconds — well within the spurious-clear
        // scenario from issue #1222. Without the gap-reset, 26+ chunks would
        // accumulate past BURST_BYTES. With it, every chunk resets because
        // each gap exceeds BURST_GAP_RESET_MS.
        let mut bytes = 0;
        let mut crossed = false;
        for i in 0..30 {
            let elapsed = GRACE_MS + i as u128 * 15_000;
            crossed = accumulate(&mut bytes, elapsed, 15_000, 20);
            if crossed {
                break;
            }
        }
        assert!(
            !crossed && bytes < BURST_BYTES,
            "spinner with gaps > reset window must not cross the burst threshold (got {bytes} bytes)"
        );
    }

    #[test]
    fn dense_burst_within_one_window_crosses() {
        // 100-byte chunks every 100ms — a genuine resumed turn's redraw.
        // After 6 chunks (600 bytes) we cross the 512 threshold.
        let mut bytes = 0;
        let mut crossed = false;
        for i in 0..10 {
            let elapsed = GRACE_MS + i * 100;
            crossed = accumulate(&mut bytes, elapsed, 100, 100);
            if crossed {
                break;
            }
        }
        assert!(
            crossed,
            "dense burst inside one window must cross the threshold"
        );
    }

    #[test]
    fn gap_exactly_at_reset_boundary_keeps_accumulating() {
        // Strict `>` comparison: a gap exactly equal to BURST_GAP_RESET_MS
        // does NOT reset. Useful so a slow continuous producer (with gaps
        // hovering at the boundary) still accumulates.
        let mut bytes = 0;
        assert!(!accumulate(
            &mut bytes,
            GRACE_MS + BURST_GAP_RESET_MS,
            BURST_GAP_RESET_MS,
            100
        ));
        assert!(!accumulate(
            &mut bytes,
            GRACE_MS + 2 * BURST_GAP_RESET_MS,
            BURST_GAP_RESET_MS,
            100
        ));
        assert_eq!(
            bytes, 200,
            "boundary gap == reset window keeps accumulating"
        );
        // A gap strictly larger: resets.
        assert!(!accumulate(
            &mut bytes,
            GRACE_MS + 2 * BURST_GAP_RESET_MS + 1,
            BURST_GAP_RESET_MS + 1,
            100
        ));
        assert_eq!(bytes, 100, "gap strictly > reset window starts fresh");
    }
}
