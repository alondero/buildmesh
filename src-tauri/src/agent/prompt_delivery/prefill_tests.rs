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
/// marker the harness never paints.
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

/// The regression itself, pinned to a real prompt rather than a synthetic
/// string: the Circuit PR reviewer prompt is ~1.5 KB, well past the limit,
/// so a head-anchored watcher waits for a marker the harness scrolls out of
/// its input box. Its marker must be the tail, and that tail must be
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
