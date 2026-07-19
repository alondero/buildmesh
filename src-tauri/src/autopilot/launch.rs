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

/// The fragment of the prefill we require on screen before submitting.
/// `issue #N` is short enough to survive normalization intact and unique
/// enough not to appear in boot noise.
pub(crate) fn submit_marker(issue_number: i64) -> String {
    format!("issue #{}", issue_number)
}

/// Pure readiness decision: the (normalized) marker is on screen and output
/// has been quiet at least [`MIN_QUIET_MS`].
pub(crate) fn ready_to_submit(
    normalized_tail: &str,
    normalized_marker: &str,
    quiet_ms: u128,
) -> bool {
    quiet_ms >= MIN_QUIET_MS && normalized_tail.contains(normalized_marker)
}

/// Watch a just-spawned Autopilot node and press Enter once it's ready.
/// Spawns its own OS thread (the poller thread must not block on one node's
/// slow spawn). Exits when: submitted, node unregistered/killed, or timeout.
pub fn watch_and_submit(app: AppHandle, node_id: i64, issue_number: i64) {
    std::thread::spawn(move || {
        let marker = normalize_for_match(&submit_marker(issue_number));
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
        let marker = normalize_for_match(&submit_marker(42));
        assert!(ready_to_submit(&tail, &marker, MIN_QUIET_MS));
        // Still streaming output → not ready even with the marker on screen.
        assert!(!ready_to_submit(&tail, &marker, MIN_QUIET_MS - 1));
        // Quiet but no marker (e.g. a trust dialog) → never submit blind.
        let boot_noise = normalize_for_match("Do you trust the files in this folder?");
        assert!(!ready_to_submit(&boot_noise, &marker, 10_000));
    }

    #[test]
    fn marker_is_the_issue_reference_from_the_prefill() {
        // Pin the coupling to `format_issue_prefill`'s shape: the prefill
        // always contains `issue #N`, so the marker must too.
        let prefill = crate::commands::agent::format_issue_prefill(
            "alondero",
            "buildmesh",
            358,
            "Fix the thing",
        );
        assert!(
            normalize_for_match(&prefill).contains(&normalize_for_match(&submit_marker(358))),
            "marker must appear in the prefill it is derived from"
        );
    }
}
