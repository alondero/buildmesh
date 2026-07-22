//! Autopilot launch watcher — presses Enter for the agent.
//!
//! An auto-spawned node launches its CLI with `--prefill <task>`: the prompt
//! lands in the harness's input box but nothing submits it, so the node sits
//! idle forever. This module watches a freshly spawned Autopilot node's PTY
//! output (the tail the evaluator already buffers) and, once the harness is
//! observably ready, writes the `\r` keystroke that starts the task.
//!
//! ## Readiness is two-factor, on purpose
//! 1. **The prefill is echoed on screen** — the TUI has drawn its input box
//!    with the staged prompt in it. Matching is whitespace-insensitive
//!    (`normalize_for_match`) because the input box wraps text at arbitrary
//!    columns and frames it with box-drawing characters.
//! 2. **Output has gone quiet** ([`MIN_QUIET_MS`]) — the CLI has finished
//!    booting/redrawing and is waiting for input.
//!
//! Quiescence alone would be dangerous: a first-run workspace-trust dialog
//! also sits quiet, and a blind Enter would auto-accept it. Requiring the
//! prefill echo means Enter is only ever sent at the staged prompt. If the
//! marker never appears (unexpected dialog, provider without echo), the
//! watcher gives up after [`WATCH_TIMEOUT`] with a warning — the node is
//! left for the human, never blind-driven.

use std::time::{Duration, Instant};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

/// Payload of the `autopilot-submitted` Tauri event. Emitted by the launch
/// watcher once it has confirmed the prefill is on screen and pressed Enter
/// — the agent has actually started the task (the prefill alone only stages
/// it; issue #874). The frontend surfaces a toast confirming the start.
///
/// Generated to `src/types/generated/AutopilotSubmittedPayload.ts`; the TS
/// half is imported by `src/App.tsx`. `issue` is `0` for hand-spawned
/// autopilot nodes without an originating GitHub issue.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AutopilotSubmittedPayload.ts")]
pub struct AutopilotSubmittedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "i32")]
    pub issue: i64,
}

use super::evaluator;

/// Output must be quiet this long (after the marker appears) before Enter.
pub(crate) const MIN_QUIET_MS: u128 = 1_500;

/// Cap on the normalized marker length. Long enough to be distinctive
/// against boot noise (a trust dialog, banner, model-name line), short
/// enough to fit on one TUI line even when the input box wraps — the
/// soft-wrap never moves an underlying character past its source
/// position, so the leading `MAX_MARKER_CHARS_NORMALIZED` chars are
/// always visible if the box has drawn.
const MAX_MARKER_CHARS_NORMALIZED: usize = 30;

/// Minimum normalized marker length. A loop prefill shorter than this
/// (after `normalize_for_match`) yields an empty marker — the watcher's
/// `ready_to_submit` empty-marker guard rejects it, the watcher times
/// out, the user fixes the loop config.
///
/// Why minimum at all: short markers false-positive against brand
/// strings embedded in agent-CLI boot chrome that stays on screen for
/// the session. `claude` (6), `minimax` (7), `anthropic` (9) all
/// survive `normalize_for_match` and all appear in Claude Code's
/// banner — a loop prefill of just `claude` would match the banner
/// before the input box even renders, and Enter would fire blind on a
/// trust dialog (the bug the marker gate exists to prevent).
/// 10 chars is just above `anthropic`'s normalized length but well
/// below any reasonable user-authored "do the thing" prompt, so it
/// rejects boot-chrome substrings while passing real tasks.
const MIN_MARKER_CHARS_DISTINCTIVE: usize = 10;

/// Give up watching a node that never becomes ready (spawn failed, provider
/// renders no echo, unexpected dialog). The node stays visible with its
/// prefill staged, exactly as today — a human can press Enter.
const WATCH_TIMEOUT: Duration = Duration::from_secs(300);

/// Poll cadence for the readiness check. Cheap: two map lookups + a substring
/// scan over a ≤6 KB cleaned tail.
const POLL_EVERY: Duration = Duration::from_millis(500);

/// Collapse a string to just its word characters (plus `#`) so a marker can
/// be found inside TUI output where the input box wraps text at arbitrary
/// columns and pads lines with box-drawing characters. Case-insensitive.
pub(crate) fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '#')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Derive the readiness marker that the launch watcher waits to see in the
/// harness's input box before pressing Enter. Replaces the previous
/// `submit_marker(issue_number)` heuristic (wayfinder #1027), which baked the
/// `issue #N` literal into the marker and made it impossible to match a
/// free-form prefill (loop-mode prefills from `mesh.loop_initial_prompt`
/// carry no issue reference — the literal `issue #0` cannot appear in any
/// user-authored prompt, so the launch watcher silently timed out after
/// [`WATCH_TIMEOUT`] without submitting the prefill).
///
/// Strategy: take the first ≤[`MAX_MARKER_CHARS_NORMALIZED`] characters of
/// the *normalized* prefill, gated by [`MIN_MARKER_CHARS_DISTINCTIVE`].
/// `normalize_for_match` strips the TUI's box-drawing characters and
/// whitespace, so the leading fragment survives any line wrap the input
/// box does. The fragment is by construction a substring of the normalized
/// prefill, so the watcher's `ready_to_submit` finds it the moment the
/// harness draws the input box. A prefill that normalizes below the
/// distinctiveness threshold returns an empty marker; the empty-marker
/// guard in `ready_to_submit` then defers to the [`WATCH_TIMEOUT`]
/// warning rather than firing Enter blind.
pub(crate) fn marker_hint_for_prefill(prefill: &str) -> String {
    let normalized = normalize_for_match(prefill);
    if normalized.chars().count() < MIN_MARKER_CHARS_DISTINCTIVE {
        return String::new();
    }
    normalized
        .chars()
        .take(MAX_MARKER_CHARS_NORMALIZED)
        .collect()
}

/// Pure readiness decision: the (normalized) marker is on screen and output
/// has been quiet at least [`MIN_QUIET_MS`].
///
/// Defense in depth: an empty `normalized_marker` must NEVER be reported
/// ready. `str::contains("")` is unconditionally `true`, so without this
/// guard a degenerate prefill (one whose normalization yields no
/// alphanumeric chars) would re-introduce exactly the workspace-trust-dialog
/// blind-Enter bug the marker gate exists to prevent — the user typed
/// `loop_initial_prompt = "---"`, the marker normalizes to `""`, the tail
/// of a quiet harness matches, Enter fires before any prefill is drawn.
/// Rejecting empty markers degrades gracefully to the [`WATCH_TIMEOUT`]
/// warning instead, which lets the user fix their loop config.
pub(crate) fn ready_to_submit(
    normalized_tail: &str,
    normalized_marker: &str,
    quiet_ms: u128,
) -> bool {
    !normalized_marker.is_empty()
        && quiet_ms >= MIN_QUIET_MS
        && normalized_tail.contains(normalized_marker)
}

/// Watch a just-spawned Autopilot node and press Enter once it's ready.
/// Spawns its own OS thread (the poller thread must not block on one node's
/// slow spawn). Exits when: submitted, node unregistered/killed, or timeout.
///
/// The `prefill` is the text that was staged into the harness's input box
/// by `commands::agent::start_node_background`; the marker is derived from
/// it (see [`marker_hint_for_prefill`]) so the readiness gate matches the
/// *actual* prompt on screen — both the issue-driven prefill and the
/// loop-mode prefill are substrings of their own `prefill` by construction.
///
/// `issue_number` is preserved for the `autopilot-submitted` toast payload
/// so the existing wire shape stays intact; it is NOT used for the marker
/// (wayfinder #1027: that's what broke loop-mode prefills).
pub fn watch_and_submit(
    app: AppHandle,
    node_id: i64,
    issue_number: i64,
    prefill: &str,
) {
    // Owned copy for the spawned thread (`'static`); the prefill is a
    // small string (<= a few KB even for verbose loop prompts) but the
    // thread must outlive any stack frame, so a borrow is not enough.
    let prefill = prefill.to_string();
    std::thread::spawn(move || {
        // `marker_hint_for_prefill` already normalizes; no need to
        // re-normalize here (idempotent but a needless pass over the
        // string each tick). The tail is normalized inside the poll
        // loop because it's a fresh evaluator read every iteration.
        let marker = marker_hint_for_prefill(&prefill);
        let deadline = Instant::now() + WATCH_TIMEOUT;
        loop {
            std::thread::sleep(POLL_EVERY);
            if Instant::now() >= deadline {
                tracing::warn!(
                    "autopilot launch({}): harness never became ready within {:?} — \
                     leaving the prefilled prompt for a human to submit",
                    node_id,
                    WATCH_TIMEOUT
                );
                return;
            }
            // Node closed / pipeline aborted while we waited.
            if !evaluator::is_piloted(node_id) {
                return;
            }
            // Stage-2 spawn may still be provisioning the worktree/PTY.
            if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
                continue;
            }
            let Some(quiet_ms) = evaluator::millis_since_last_output(node_id) else {
                continue; // no output captured yet
            };
            let tail = normalize_for_match(&evaluator::cleaned_tail(node_id));
            if !ready_to_submit(&tail, &marker, quiet_ms) {
                continue;
            }
            // The pipeline's shared submit helper: Enter as its own write,
            // acknowledged by PTY output, retried if swallowed — a swallowed
            // Enter stalls a prefilled launch exactly like an injection (#874).
            match crate::autopilot::pipeline::press_enter_until_output(node_id) {
                Ok(attempt) => {
                    tracing::info!(
                        "autopilot launch({}): harness ready — submitted prefilled prompt \
                         for issue #{} (Enter attempt {})",
                        node_id,
                        issue_number,
                        attempt
                    );
                    let _ = app.emit(
                        "autopilot-submitted",
                        AutopilotSubmittedPayload {
                            node_id,
                            issue: issue_number,
                        },
                    );
                }
                Err(e) => tracing::warn!(
                    "autopilot launch({}): prefilled prompt was never submitted: {}",
                    node_id,
                    e
                ),
            }
            return;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_wrapping_and_frame_noise() {
        // A TUI input box wraps the prefill and frames it with box-drawing
        // characters — normalization must see through all of it.
        let screen = "│ Please work on GitHub issu │\n│ e #123 — Fix the login flow │";
        assert!(normalize_for_match(screen).contains(&normalize_for_match("issue #123")));
    }

    #[test]
    fn normalize_is_case_insensitive() {
        assert_eq!(normalize_for_match("Issue #7"), "issue#7");
    }

    #[test]
    fn ready_requires_both_marker_and_quiescence() {
        let tail = normalize_for_match("Please work on GitHub issue #42 — thing");
        let marker = normalize_for_match("issue #42");
        assert!(ready_to_submit(&tail, &marker, MIN_QUIET_MS));
        assert!(!ready_to_submit(&tail, &marker, MIN_QUIET_MS - 1));
        let boot_noise = normalize_for_match("Do you trust the files in this folder?");
        assert!(!ready_to_submit(&boot_noise, &marker, 10_000));
    }

    // The marker MUST be a substring of the normalized prefill, by
    // construction: `marker_hint_for_prefill` takes a substring, so
    // `ready_to_submit`'s `contains` check finds it once the harness
    // draws the input box. Wayfinder #1027: this invariant is what
    // loop autopilot was missing before — the previous `submit_marker(0)`
    // literal `issue #0` couldn't be a substring of any user-authored
    // loop prefill, so the watcher silently timed out.
    #[test]
    fn marker_hint_for_prefill_is_a_substring_of_normalized_prefill() {
        // Issue prefill keeps the baseline covered; loop prefill is
        // the regression shape. Both paths go through `watch_and_submit`.
        for prefill in [
            crate::commands::agent::format_issue_prefill(
                "alondero",
                "buildmesh",
                358,
                "Fix the login flow",
            ),
            "Iterate on the failing test cases".to_string(),
        ] {
            let marker = marker_hint_for_prefill(&prefill);
            let normalized_prefill = normalize_for_match(&prefill);
            assert!(
                !marker.is_empty(),
                "marker_hint_for_prefill({:?}) must be non-empty",
                prefill,
            );
            assert!(
                normalized_prefill.contains(&marker),
                "marker {:?} must be a substring of normalized prefill {:?}",
                marker,
                normalized_prefill,
            );
        }
    }

    /// `word ` × 100 → 400 alphanumerics → cap at MAX_MARKER_CHARS_NORMALIZED.
    /// The constant name is asserted (not just `30`) so a future tuning
    /// of the cap is forced to update both the helper and the pin in
    /// one review.
    #[test]
    fn marker_hint_for_prefill_truncates_a_long_prefill_to_max_marker_chars() {
        let long = "word ".repeat(100);
        let marker = marker_hint_for_prefill(&long);
        assert_eq!(marker.chars().count(), MAX_MARKER_CHARS_NORMALIZED);
        assert!(marker.chars().all(|c| c == 'w' || c == 'o' || c == 'r' || c == 'd'));
    }

    // Short prompts like `"claude"`, `"grok"`, or `"minimax"` survive
    // `normalize_for_match` and match brand strings embedded in agent-CLI
    // boot chrome that stays on screen for the session — the watcher
    // would fire Enter on a quiet trust dialog before the prefill is
    // ever drawn. Below `MIN_MARKER_CHARS_DISTINCTIVE`, the helper
    // returns empty so `ready_to_submit` rejects it (degrades to the
    // WATCH_TIMEOUT warning instead).
    #[test]
    fn marker_hint_for_prefill_rejects_a_short_brand_string() {
        for short in ["claude", "grok", "kimi", "minimax", "anthropic", "Fix X"] {
            assert!(
                marker_hint_for_prefill(short).is_empty(),
                "{short:?} normalizes to < {} chars and must produce an empty marker",
                MIN_MARKER_CHARS_DISTINCTIVE,
            );
        }
    }

    // Defense in depth (Spec review, wayfinder #1027 follow-up):
    // `str::contains("")` is unconditionally true, so without this
    // guard a degenerate prefill (`loop_initial_prompt = "---"`)
    // would re-enable blind Enter on a workspace-trust dialog.
    // Rejecting empty markers degrades to the WATCH_TIMEOUT warning
    // instead, which lets the user fix their loop config.
    #[test]
    fn ready_rejects_an_empty_marker_to_prevent_blind_enter() {
        for (tail, quiet_ms) in [
            ("any tail text", MIN_QUIET_MS),
            ("", MIN_QUIET_MS),
            ("x", 10_000),
        ] {
            assert!(
                !ready_to_submit(tail, "", quiet_ms),
                "empty marker must never report ready (tail={:?}, quiet={}ms)",
                tail,
                quiet_ms,
            );
        }
    }
}
