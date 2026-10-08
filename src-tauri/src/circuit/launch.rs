//! Circuit launch watcher — presses Enter for the agent.
//!
//! An auto-spawned node launches its CLI with `--prefill <task>`: the prompt
//! lands in the harness's input box but nothing submits it, so the node sits
//! idle forever. This module watches a freshly spawned Circuit agent's PTY
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
//!
//! The echo is matched on the end of the prefill the composer keeps in view,
//! not always its start — see [`marker_hint_for_prefill`].

use super::delivery::{TAIL_ANCHOR_CHARS, VISIBLE_PASTE_TEXT_LIMIT};
use super::evaluator;
use std::time::{Duration, Instant};

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
/// Strategy: confirm the *normalized* prefill against the harness's drawn
/// input box, gated by [`MIN_MARKER_CHARS_DISTINCTIVE`].
/// `normalize_for_match` strips the TUI's box-drawing characters and
/// whitespace, so the fragment survives any line wrap the input box does. A
/// prefill that normalizes below the distinctiveness threshold returns an
/// empty marker; the empty-marker guard in `ready_to_submit` then defers to
/// the [`WATCH_TIMEOUT`] warning rather than firing Enter blind.
///
/// # Which end of the prefill confirms readiness
///
/// A prefill the composer draws in full is confirmed by its head: a fragment of
/// ≤[`MAX_MARKER_CHARS_NORMALIZED`] characters, distinctive against boot chrome
/// yet short enough to survive any wrap. A prefill longer than the input box
/// scrolls — the harness keeps the text nearest the cursor in view and the head
/// leaves the frame — so it is confirmed by its [`TAIL_ANCHOR_CHARS`]-character
/// tail instead. See the section below.
///
/// The marker is by construction a substring of the normalized prefill, so the
/// watcher's `ready_to_submit` matches the moment the harness draws the
/// corresponding span.
///
/// # Long prefills anchor on their tail, not their head
///
/// The leading fragment is only *drawn* while the composer shows the start of
/// the staged text. Claude Code's input box scrolls, and once a prefill is
/// longer than the box it keeps the text nearest the cursor — its tail — in
/// view and scrolls the head out of frame. Head-anchoring such a prefill waits
/// for a marker the harness will never draw, so the watcher burns
/// [`WATCH_TIMEOUT`] and leaves a perfectly good prompt staged but unsubmitted
/// (run 393: the Circuit PR reviewer's review policy, delivery line, and
/// `BUILDMESH_REVIEW_V1` contract together are ~1.5 KB, comfortably past the
/// limit, and a human had to press Enter for every review round).
///
/// `circuit::delivery` already hit this for PTY pastes and settled on the tail
/// for the same reason — a composer that scrolls keeps the tail in view
/// ([`TAIL_ANCHOR_CHARS`], issue #2061/#2108). This helper applies that one
/// policy to the prefill path so both transports anchor on the span that is
/// actually rendered.
pub(crate) fn marker_hint_for_prefill(prefill: &str) -> String {
    let normalized = normalize_for_match(prefill);
    let chars = normalized.chars().count();
    if chars < MIN_MARKER_CHARS_DISTINCTIVE {
        return String::new();
    }
    if chars <= VISIBLE_PASTE_TEXT_LIMIT {
        return normalized
            .chars()
            .take(MAX_MARKER_CHARS_NORMALIZED)
            .collect();
    }
    let skip = chars - TAIL_ANCHOR_CHARS;
    normalized.chars().skip(skip).collect()
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
/// warning instead, which lets the user inspect the staged prompt.
pub(crate) fn ready_to_submit(
    normalized_tail: &str,
    normalized_marker: &str,
    quiet_ms: u128,
) -> bool {
    !normalized_marker.is_empty()
        && quiet_ms >= MIN_QUIET_MS
        && normalized_tail.contains(normalized_marker)
}

/// Submit a staged Circuit prefill after its prompt echo and quiet output prove readiness.
/// The watcher exits after submission, ownership removal, or the bounded timeout.
pub(crate) fn watch_and_submit_for_circuit(_app: tauri::AppHandle, node_id: i64, prefill: &str) {
    // Owned copy for the spawned thread (`'static`); the prefill is a
    // small string (<= a few KB even for verbose loop prompts) but the
    // thread must outlive any stack frame, so a borrow is not enough.
    let prefill = prefill.to_string();
    std::thread::spawn(move || {
        // `marker_hint_for_prefill` already normalizes; no need to
        // re-normalize here (idempotent but a needless pass over the
        // string each tick). The tail is normalized inside the poll
        // loop because it's a fresh evaluator read every iteration.
        // The marker is the span the composer keeps in view — the head
        // of a short draft, the tail of one long enough to scroll.
        let marker = marker_hint_for_prefill(&prefill);
        let deadline = Instant::now() + WATCH_TIMEOUT;
        loop {
            std::thread::sleep(POLL_EVERY);
            if Instant::now() >= deadline {
                tracing::warn!(
                    "circuit launch({}): harness never became ready within {:?} — \
                     leaving the prefilled prompt for a human to submit",
                    node_id,
                    WATCH_TIMEOUT
                );
                return;
            }
            // Node closed / pipeline aborted while we waited.
            if !evaluator::is_circuit_piloted(node_id) {
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
            match crate::circuit::delivery::press_enter_until_output(node_id) {
                Ok(attempt) => {
                    tracing::info!(node_id, attempt, "circuit prefill submitted");
                }
                Err(e) => tracing::warn!(
                    "circuit launch({}): prefilled prompt was never submitted: {}",
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
        // Issue #1180 — the issue prefill here is the EXACT string the
        // watcher will be handed (built from `SpawnIntent::Issue` →
        // `initial_prompt()`); the loop prefill is taken verbatim, just
        // like `SpawnIntent::Loop`. Routing through the source of truth
        // keeps the marker test honest if anyone changes the wording.
        let issue_prefill =
            crate::agent::spawn::SpawnIntent::Issue(crate::agent::spawn::IssueContext {
                owner: "alondero".into(),
                repo: "buildmesh".into(),
                number: 358,
                title: "Fix the login flow".into(),
                template: None,
            })
            .initial_prompt()
            .expect("issue intent always has a prompt")
            .into_string();
        for prefill in [
            issue_prefill,
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

    /// A prompt the composer draws whole keeps the head anchor, capped at
    /// MAX_MARKER_CHARS_NORMALIZED. The constant name is asserted (not just
    /// `30`) so a future tuning of the cap is forced to update both the
    /// helper and the pin in one review.
    #[test]
    fn marker_hint_for_prefill_caps_a_visible_prefill_at_max_marker_chars() {
        let visible = "word ".repeat(20);
        assert!(
            normalize_for_match(&visible).chars().count() <= VISIBLE_PASTE_TEXT_LIMIT,
            "fixture precondition: this prefill is drawn inline, so its head is on screen"
        );
        let marker = marker_hint_for_prefill(&visible);
        assert_eq!(marker.chars().count(), MAX_MARKER_CHARS_NORMALIZED);
        assert!(marker
            .chars()
            .all(|c| c == 'w' || c == 'o' || c == 'r' || c == 'd'));
    }

    /// A prefill longer than the composer can draw scrolls: the head leaves
    /// the frame and only the tail stays visible. The marker must therefore
    /// come from the tail, or the watcher waits out `WATCH_TIMEOUT` for a
    /// marker the harness never paints (run 393's reviewer prompt).
    #[test]
    fn marker_hint_for_prefill_anchors_on_the_tail_of_a_long_prefill() {
        let long = "word ".repeat(100);
        let normalized = normalize_for_match(&long);
        assert!(
            normalized.chars().count() > VISIBLE_PASTE_TEXT_LIMIT,
            "fixture precondition: this prefill scrolls its head out of the input box"
        );
        let marker = marker_hint_for_prefill(&long);
        assert_eq!(marker.chars().count(), TAIL_ANCHOR_CHARS);
        // Drawn from the end, not the start.
        assert_eq!(
            marker,
            normalized
                .chars()
                .skip(normalized.chars().count() - TAIL_ANCHOR_CHARS)
                .collect::<String>(),
        );
    }

    /// The regression itself: the real Circuit PR reviewer prompt is ~1.5 KB,
    /// so a head-anchored watcher waits for a marker Claude Code scrolls out
    /// of its input box. Its marker must be the tail, and that tail must be
    /// reachable in a tail-only echo of the drawn input box.
    #[test]
    fn circuit_reviewer_prefill_marker_survives_a_scrolled_input_box() {
        let reviewer = crate::circuit::model::CircuitGraph::pr_review_prompt();
        let normalized = normalize_for_match(&reviewer);
        assert!(
            normalized.chars().count() > VISIBLE_PASTE_TEXT_LIMIT,
            "fixture precondition: the reviewer prompt scrolls ({} normalized chars)",
            normalized.chars().count(),
        );
        let marker = marker_hint_for_prefill(&reviewer);
        assert_eq!(marker.chars().count(), TAIL_ANCHOR_CHARS);
        // What a scrolled composer actually draws: the tail of the staged
        // prompt, wrapped and framed by the input box, with its head scrolled
        // out of frame. The head-anchored marker is absent from that frame.
        let visible_tail = format!(
            "╭─╮\n│ {} │\n╰─╯",
            &normalized[normalized.chars().count() - TAIL_ANCHOR_CHARS..]
        );
        let head_marker = &normalized[..MAX_MARKER_CHARS_NORMALIZED];
        assert!(
            !normalize_for_match(&visible_tail).contains(head_marker),
            "a scrolled input box must not contain the head fragment the old marker waited for",
        );
        assert!(ready_to_submit(
            &normalize_for_match(&visible_tail),
            &marker,
            MIN_QUIET_MS
        ));
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
    // instead, which lets the user inspect the staged prompt.
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
