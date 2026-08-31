use super::{engine::*, repository::*, slug::*};
use crate::models::AgentNode;

/// Serialises tests that mutate process env (PATH, USERPROFILE,
/// APPDATA) so two parallel tests can't observe each other's
/// mid-flight values. Required because the env-mutating tests in
/// this module read three different vars and a partial overlap
/// would let the resolver's `is_file()` check silently return a
/// real install path from a non-overridden var.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `f` with each `(name, value)` in `vars` set on the process
/// env, restoring the originals (or unsetting) afterwards even on
/// panic. Lets env-mutating tests stay under the `ENV_LOCK` and
/// fail without leaking global state to other tests in the binary.
fn with_env_vars<F: FnOnce()>(vars: &[(&str, Option<&std::ffi::OsStr>)], f: F) {
    let saved: Vec<(&str, Option<String>)> = vars
        .iter()
        .map(|(k, _)| (*k, std::env::var(k).ok()))
        .collect();
    for (k, v) in vars {
        match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        }
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    for (k, saved_val) in saved {
        match saved_val {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Open the buffering gate for a node so `on_output` writes immediately.
/// Real code opens the gate via `should_trigger_rename`; tests use this
/// to simulate "the agent has already had at least one turn".
fn open_gate(node_id: i64) {
    naming().entry(node_id).or_default().buffering_ready = true;
}

/// The node's accumulated buffer, or `None` if it has no naming state.
fn buffer_of(node_id: i64) -> Option<String> {
    naming().get(&node_id).map(|st| st.buffer.clone())
}

/// Whether the buffering gate is open for the node.
fn gate_open(node_id: i64) -> bool {
    naming()
        .get(&node_id)
        .map(|st| st.buffering_ready)
        .unwrap_or(false)
}

/// Whether the node has any naming state at all.
fn has_state(node_id: i64) -> bool {
    naming().contains_key(&node_id)
}

fn set_renaming_in_progress(node_id: i64) {
    naming().entry(node_id).or_default().renaming = true;
}

fn clear_renaming_in_progress(node_id: i64) {
    if let Some(st) = naming().get_mut(&node_id) {
        st.renaming = false;
    }
}

fn set_attempts(node_id: i64, n: u8) {
    naming().entry(node_id).or_default().attempts = n;
}

#[test]
fn generates_three_word_hyphenated_name() {
    let name = on_spawn();
    let parts: Vec<&str> = name.split('-').collect();
    assert_eq!(parts.len(), 3);
    assert!(parts.iter().all(|p| !p.is_empty()));
    assert!(name.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
}

// --- slugify_issue_title ---

/// Happy path: a multi-word issue title lowercases, hyphenates, and stays
/// under the 50-char limit. The result must round-trip as a valid
/// `SLUG_REGEX` match — that's the contract spawn_issue_agent relies on.
#[test]
fn slugify_issue_title_happy_path() {
    assert_eq!(
        slugify_issue_title("Fix terminal flash on resize"),
        "fix-terminal-flash-on-resize"
    );
}

/// Underscores in the title become hyphens (the spec explicitly calls
/// underscores out alongside spaces).
#[test]
fn slugify_issue_title_underscores_become_hyphens() {
    assert_eq!(
        slugify_issue_title("fix_oauth_callback"),
        "fix-oauth-callback"
    );
    assert_eq!(
        slugify_issue_title("with_mixed AND_separators"),
        "with-mixed-and-separators"
    );
}

/// Punctuation that isn't part of a word gets stripped (colons, apostrophes,
/// exclamation marks). The remaining alphanumeric runs join with hyphens.
/// The spec is "strip" not "replace with hyphen" — so punctuation between
/// two word-runs collapses them (e.g. `Foo::Bar` → `foobar`). The result
/// still matches `SLUG_REGEX` and the user can manually rename if they
/// want a different boundary.
#[test]
fn slugify_issue_title_strips_non_alphanumeric() {
    assert_eq!(
        slugify_issue_title("Bug: something's broken!"),
        "bug-somethings-broken"
    );
    // `::` between words is stripped; the two words collapse into one run.
    assert_eq!(
        slugify_issue_title("Refactor session_naming::on_turn"),
        "refactor-session-namingon-turn"
    );
}

/// Leading and trailing whitespace and hyphens are trimmed.
#[test]
fn slugify_issue_title_trims_leading_and_trailing_hyphens() {
    assert_eq!(slugify_issue_title("   hello world   "), "hello-world");
    assert_eq!(slugify_issue_title("---hello---"), "hello");
    assert_eq!(slugify_issue_title("  --fix bug--  "), "fix-bug");
}

/// Titles over 50 chars get truncated. The truncation must not leave a
/// dangling trailing hyphen (it would otherwise be one char over the cap
/// when stripped).
#[test]
fn slugify_issue_title_truncates_to_50_chars() {
    let long = "a-very-long-issue-title-that-exceeds-fifty-characters-yes-indeed";
    let result = slugify_issue_title(long);
    assert!(result.len() <= 50, "got len={}: {:?}", result.len(), result);
    assert!(!result.ends_with('-'), "trailing hyphen: {:?}", result);
    assert!(SLUG_REGEX.is_match(&result), "regex mismatch: {:?}", result);
}

/// When the input produces an invalid slug (empty, all-punctuation, starts
/// with a digit, too short), the function MUST fall back to a default
/// adj-adj-noun name. The caller relies on always getting back a valid
/// node name — an empty string would break worktree creation.
#[test]
fn slugify_issue_title_falls_back_to_default_for_empty_input() {
    let result = slugify_issue_title("");
    let parts: Vec<&str> = result.split('-').collect();
    assert_eq!(parts.len(), 3, "fallback must be adj-adj-noun");
    assert!(
        SLUG_REGEX.is_match(&result),
        "fallback must match SLUG_REGEX: {:?}",
        result
    );
}

#[test]
fn slugify_issue_title_falls_back_for_only_punctuation() {
    let result = slugify_issue_title("!!!---???");
    let parts: Vec<&str> = result.split('-').collect();
    assert_eq!(
        parts.len(),
        3,
        "fallback must be adj-adj-noun, got: {:?}",
        result
    );
}

#[test]
fn slugify_issue_title_falls_back_when_starts_with_digit() {
    // SLUG_REGEX requires the first char to be a letter, so "123-foo-bar"
    // cannot be a valid slug. Must fall back to a default name.
    let result = slugify_issue_title("123-something-here");
    let parts: Vec<&str> = result.split('-').collect();
    assert_eq!(
        parts.len(),
        3,
        "digit-prefixed must fall back, got: {:?}",
        result
    );
}

/// Contract: the result is ALWAYS a valid SLUG_REGEX match. This is the
/// invariant `services::agent_node::create` relies on when it stores the
/// name as the worktree directory.
#[test]
fn slugify_issue_title_always_produces_valid_slug() {
    let cases = [
        "Fix OAuth callback",
        "Add dark mode to settings",
        "fix terminal flash on resize",
        "  ",
        "",
        "!!!",
        "123-abc",
        "a",
        "ab",
        "---",
        "中文 issue", // non-ASCII filter-stripped; falls back
    ];
    for case in &cases {
        let result = slugify_issue_title(case);
        assert!(
            SLUG_REGEX.is_match(&result),
            "slugify({:?}) = {:?} does not match SLUG_REGEX",
            case,
            result
        );
    }
}

#[test]
fn is_default_name_positive() {
    assert!(is_default_name("bold-keen-brook"));
    assert!(is_default_name("calm-deep-oak"));
}

// --- Seed pool sizes (collision-rate guard) ---

/// Pins the minimum size of the ADJECTIVES and NOUNS lists. Together they
/// produce `ADJECTIVES.len()² × NOUNS.len()` total possible names — with
/// the birthday paradox, the 50%-collision threshold is around
/// `sqrt(2 × 0.5 × combos)` nodes. We hit collisions in production when
/// the pools were ~105 × ~125 (~1.4M combos, ~1.2k-node threshold), so any
/// future regression that shrinks them below 400 / 400 (≈64M combos,
/// ~11k-node threshold) trips this test.
///
/// Note the asymmetric leverage: combos scale quadratically with the
/// adjective count and only linearly with nouns, so when in doubt grow
/// ADJECTIVES first. The 400 / 400 floor is the *minimum-safe* threshold
/// matching the original production incident; the actual current pools
/// are an order of magnitude larger.
///
/// Bumping these floors is the primary collision-mitigation lever; adding
/// more words is a one-line change to the `ADJECTIVES` / `NOUNS` arrays
/// at the top of this file. No code logic needs to move.
#[test]
fn seed_pools_meet_minimum_size_for_low_collision_rate() {
    const MIN_ADJECTIVES: usize = 400;
    const MIN_NOUNS: usize = 400;
    assert!(
        ADJECTIVES.len() >= MIN_ADJECTIVES,
        "ADJECTIVES has {} entries; need >= {} for low collision rate",
        ADJECTIVES.len(),
        MIN_ADJECTIVES
    );
    assert!(
        NOUNS.len() >= MIN_NOUNS,
        "NOUNS has {} entries; need >= {} for low collision rate",
        NOUNS.len(),
        MIN_NOUNS
    );
}

/// Generated names must be unique enough that 500 fresh `on_spawn()` calls
/// have effectively no chance of collision. With the current pools
/// (≈3.9B combos), P(any collision in 500 picks) ≈ 0.003% — about 1 in
/// 30,000. Even with the floor pools (≈64M combos), P ≈ 0.2% — well under
/// the conventional 1% flake threshold.
///
/// If this test ever fails, the `ADJECTIVES` / `NOUNS` arrays have shrunk
/// (the size-pin test above should have caught that first) or a duplicate
/// entry was introduced; either way, the seed-word contract is broken.
#[test]
fn on_spawn_500_samples_have_no_collisions() {
    use std::collections::HashSet;
    let mut seen = HashSet::with_capacity(500);
    for _ in 0..500 {
        let name = on_spawn();
        assert!(
            seen.insert(name.clone()),
            "duplicate name generated within 500 samples: {}",
            name
        );
    }
}

/// Deterministic guards on the seed-word lists. Two failure modes are
/// checked, both of which silently degrade the collision rate without
/// tripping the size-pin test above (which counts rows, not distinct
/// entries or position-specific membership):
///
/// 1. Internal duplicates within a single list. A dup entry makes
///    `ADJECTIVES.choose()` and `NOUNS.choose()` non-uniform, lowering
///    the effective pool size.
/// 2. Cross-list overlap. A word appearing in BOTH lists lets
///    `is_default_name("X-Y-Z")` return true for non-default names where
///    every part is in either list — defeating `user_renamed_mid_flight`'s
///    guard, which then lets the LLM rename overwrite a user-typed slug.
///    The fix is position-strict: `parts[2]` must not also be in ADJECTIVES.
#[test]
fn seed_pools_have_no_duplicates() {
    use std::collections::HashSet;
    let mut seen_adj: HashSet<&str> = HashSet::new();
    for word in ADJECTIVES {
        assert!(
            seen_adj.insert(word),
            "ADJECTIVES contains internal duplicate: {:?}",
            word
        );
    }
    let mut seen_noun: HashSet<&str> = HashSet::new();
    for word in NOUNS {
        assert!(
            seen_noun.insert(word),
            "NOUNS contains internal duplicate: {:?}",
            word
        );
    }
    // Cross-list overlap is the silent one — it doesn't change len(),
    // but it makes `is_default_name` ambiguous between adj-slot and
    // noun-slot membership. Reject any word that lives in both lists.
    let overlap: Vec<&&str> = seen_adj.intersection(&seen_noun).collect();
    assert!(
        overlap.is_empty(),
        "ADJECTIVES and NOUNS share entries (breaks is_default_name's \
             position-strict assumption): {:?}",
        overlap
    );
}

// --- issue_node_name (gh{N}- prefix) ---

/// Happy path: the issue number is prepended as `gh{N}-` and the title
/// slugifies in the same way `slugify_issue_title` already documents.
/// This is the user-facing change — `fix-this-feature` becomes
/// `gh123-fix-this-feature`.
#[test]
fn issue_node_name_prefixes_with_gh_number() {
    assert_eq!(
        issue_node_name(123, "fix this feature"),
        "gh123-fix-this-feature"
    );
}

/// Underscores in the title become hyphens (inherited from
/// `slugify_issue_title`).
#[test]
fn issue_node_name_hyphenates_underscored_title() {
    assert_eq!(
        issue_node_name(7, "fix_oauth_callback"),
        "gh7-fix-oauth-callback"
    );
}

/// Punctuation in the title is stripped (inherited behaviour). The
/// `gh` prefix is preserved even when the title collapses to nothing
/// because the result still has the `gh{N}-` prefix attached to the
/// random fallback name.
#[test]
fn issue_node_name_preserves_prefix_when_title_has_punctuation() {
    let result = issue_node_name(42, "Bug: something's broken!");
    assert!(
        result.starts_with("gh42-"),
        "prefix must survive punctuation stripping: {:?}",
        result
    );
    assert!(
        SLUG_REGEX.is_match(&result),
        "must still match SLUG_REGEX: {:?}",
        result
    );
}

/// When the title is empty / unslugifyable, `slugify_issue_title` falls
/// back to a random adj-adj-noun. The prefix must still be applied so
/// the user can tell which issue the node came from.
#[test]
fn issue_node_name_keeps_prefix_when_title_is_empty() {
    let result = issue_node_name(5, "");
    assert!(
        result.starts_with("gh5-"),
        "empty title must still produce the gh-prefix: {:?}",
        result
    );
    assert!(
        SLUG_REGEX.is_match(&result),
        "must still match SLUG_REGEX: {:?}",
        result
    );
}

#[test]
fn issue_node_name_keeps_prefix_when_title_is_punctuation_only() {
    let result = issue_node_name(99, "!!!---???");
    assert!(
        result.starts_with("gh99-"),
        "punctuation-only title must still produce the gh-prefix: {:?}",
        result
    );
    assert!(
        SLUG_REGEX.is_match(&result),
        "must still match SLUG_REGEX: {:?}",
        result
    );
}

/// Total length is capped at 50 chars (matching `SLUG_REGEX`'s upper
/// bound). The truncation must not leave a trailing hyphen (a 51-char
/// name after stripping a hyphen would round-trip to 50, but a 50-char
/// name with a trailing hyphen is still rejected by `SLUG_REGEX`).
#[test]
fn issue_node_name_caps_total_length_at_50() {
    let long_title = "a-very-long-issue-title-that-exceeds-fifty-characters-yes-indeed";
    let result = issue_node_name(123, long_title);
    assert!(
        result.len() <= 50,
        "name must be <= 50 chars: len={} {:?}",
        result.len(),
        result
    );
    assert!(
        !result.ends_with('-'),
        "trailing hyphen would fail SLUG_REGEX: {:?}",
        result
    );
    assert!(
        SLUG_REGEX.is_match(&result),
        "must match SLUG_REGEX: {:?}",
        result
    );
}

/// Contract: the result is ALWAYS a valid SLUG_REGEX match. This is
/// the invariant `services::agent_node::create` relies on when it
/// stores the name as the worktree directory.
#[test]
fn issue_node_name_always_produces_valid_slug() {
    let cases = [
        ("Fix OAuth callback", 1i64),
        ("Add dark mode to settings", 4242),
        ("fix terminal flash on resize", 7),
        ("  ", 99),
        ("", 12),
        ("!!!", 3),
        ("123-abc", 5),
        ("---", 8),
        ("中文 issue", 11),
    ];
    for (title, n) in &cases {
        let result = issue_node_name(*n, title);
        assert!(
            SLUG_REGEX.is_match(&result),
            "issue_node_name({:?}, {:?}) = {:?} does not match SLUG_REGEX",
            n,
            title,
            result
        );
    }
}

/// Larger issue numbers must still produce a valid slug. Issue numbers
/// can be quite large on long-running repos; the cap is the 50-char
/// total, not the digit count.
#[test]
fn issue_node_name_handles_large_issue_numbers() {
    let result = issue_node_name(987654, "fix something");
    assert!(
        result.starts_with("gh987654-"),
        "large issue number must still appear in full: {:?}",
        result
    );
    assert!(
        SLUG_REGEX.is_match(&result),
        "must match SLUG_REGEX: {:?}",
        result
    );
}

// --- pr_node_name (issue #420, pr{N}- prefix) ---

/// PR-spawned nodes use the `pr{N}-` prefix (vs the issue-spawn
/// `gh{N}-` prefix) so the user can distinguish the two spawn sources
/// at a glance in the sidebar. Otherwise the slugification rules are
/// identical to `issue_node_name`'s.
#[test]
fn pr_node_name_prefixes_with_pr_number() {
    assert_eq!(pr_node_name(420, "spawn on PR"), "pr420-spawn-on-pr");
}

/// Empty title still produces a valid slug, keeping the `pr` prefix
/// (matches the issue-spawn behaviour for "keep the prefix" — the
/// user can always tell which PR the node came from).
#[test]
fn pr_node_name_keeps_prefix_when_title_is_empty() {
    let result = pr_node_name(5, "");
    assert!(
        result.starts_with("pr5-") || result.starts_with("pr5"),
        "empty title must keep the pr prefix: {:?}",
        result
    );
    assert!(
        SLUG_REGEX.is_match(&result) || result == on_spawn(),
        "must match SLUG_REGEX or fall back to a random default: {:?}",
        result
    );
}

/// Symmetry with `issue_node_name` — for the same input the PR variant
/// only differs in the prefix (`pr` vs `gh`). Pin so a future refactor
/// that drifts one without the other surfaces as a test failure.
#[test]
fn pr_node_name_differs_from_issue_name_only_by_prefix() {
    let title = "Add dark mode to settings";
    let issue = issue_node_name(42, title);
    let pr = pr_node_name(42, title);
    // Strip the prefix from each and the slug must match.
    let issue_slug = issue.strip_prefix("gh42-").unwrap();
    let pr_slug = pr.strip_prefix("pr42-").unwrap();
    assert_eq!(
        issue_slug, pr_slug,
        "pr and issue slugs must match for the same title"
    );
}

#[test]
fn is_default_name_negative() {
    assert!(!is_default_name("fix-auth-token-refresh"));
    assert!(!is_default_name("too-short"));
    assert!(!is_default_name("not-a-valid-one-at-all"));
}

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
    let err = slug_with_retry("fix-it").unwrap_err();
    assert!(err.contains("dash-separated tokens"));
}

#[test]
fn slug_with_retry_rejects_too_long() {
    let err = slug_with_retry("one-two-three-four-five-six").unwrap_err();
    assert!(err.contains("dash-separated tokens"));
}

#[test]
fn slug_with_retry_normalises_space_separated_words() {
    assert_eq!(
        slug_with_retry("not skip audit kirby tests").unwrap(),
        "not-skip-audit-kirby-tests"
    );
    assert_eq!(
        slug_with_retry("fix auth token flow").unwrap(),
        "fix-auth-token-flow"
    );
}

#[test]
fn slug_with_retry_detects_conversational_response() {
    let cases = [
            "it looks like there might be a terminal issue with repeated commands. how can i help you?",
            "i'm not sure what you're trying to do with that terminal output",
            "what can i help you with in the buildmesh project?",
            "that looks like terminal output from a different project. what task can i assist you with?",
        ];
    for case in &cases {
        let err = slug_with_retry(case).unwrap_err();
        assert!(
            err.contains("conversational response"),
            "expected conversational detection for: '{}', got: {}",
            case,
            err
        );
    }
}

#[test]
fn slug_with_retry_does_not_flag_short_non_slug_as_conversational() {
    let err = slug_with_retry("pond").unwrap_err();
    assert!(err.contains("dash-separated tokens"));
}

#[test]
fn buffer_caps_at_max() {
    open_gate(999);
    let big = "x".repeat(5000);
    on_output(999, &big);
    assert!(buffer_of(999).unwrap().len() <= MAX_BUFFER_CHARS);
}

#[test]
fn slug_with_retry_extracts_hyphenated_from_prose() {
    let result = slug_with_retry("Based on the session, a good name would be: improve-auth-flow");
    assert!(result.is_ok());
}

#[test]
fn stale_buffer_cleared_on_cleanup() {
    open_gate(5);
    let stale_text = "old context from a previous archived session";
    on_output(5, stale_text);
    assert!(buffer_of(5).is_some(), "precondition: buffer exists");

    cleanup(5);

    assert!(
        buffer_of(5).is_none(),
        "node 5's buffer should be cleared by cleanup"
    );
}

#[test]
fn cleanup_only_clears_target_session() {
    open_gate(5);
    open_gate(99);
    on_output(5, "session 5 output");
    on_output(99, "session 99 output");

    cleanup(5);

    assert!(buffer_of(5).is_none());
    assert!(buffer_of(99).is_some(), "session 99 buffer should survive");
    cleanup(99);
}

#[test]
fn cleanup_is_idempotent() {
    open_gate(8);
    on_output(8, "some output");

    cleanup(8);
    cleanup(8); // second call must not panic

    assert!(buffer_of(8).is_none());
}

#[test]
fn buffer_truncation_splits_multibyte_utf8_correctly() {
    let node_id = 77;
    open_gate(node_id);
    let base = "x".repeat(4000);
    let with_kanji = format!("{}{}", base, "日本");

    on_output(node_id, &with_kanji);

    let buf = buffer_of(node_id).unwrap();

    assert!(buf.len() <= MAX_BUFFER_CHARS);
    assert!(std::str::from_utf8(buf.as_bytes()).is_ok());
}

#[test]
fn on_output_drops_data_until_gate_opens() {
    // Distinct id to avoid cross-test contamination with shared statics
    let node_id = 4242;
    cleanup(node_id);

    // Before any Node Turn, all output is discarded — this is the
    // bypass-permissions-warning fix: chrome printed at startup never
    // reaches the renaming buffer.
    on_output(node_id, "chrome chrome chrome");
    on_output(node_id, "Bypass Permissions warning text...");

    assert!(
        buffer_of(node_id).is_none(),
        "buffer must stay empty before gate opens"
    );

    // Now the gate flips (simulating first idle_prompt webhook)
    open_gate(node_id);

    on_output(node_id, "real user content");
    assert_eq!(
        buffer_of(node_id).as_deref(),
        Some("real user content"),
        "post-gate output must accumulate, no chrome contamination"
    );
    cleanup(node_id);
}

#[test]
fn on_output_accumulates_after_gate_opens() {
    open_gate(42);
    on_output(42, "first output");
    on_output(42, "second output");

    assert_eq!(
        buffer_of(42).as_deref(),
        Some("first outputsecond output"),
        "on_output should accumulate once the gate is open"
    );
}

// --- Plain-terminal naming gate (issue #296) ---

/// A plain Terminal node's PTY chunks must never reach the rename
/// buffer: the buffer is consumed only by the rename LLM, which fires
/// from `on_turn` — and only the Claude stop hook calls that. Ungated,
/// every Terminal chunk would take the global NAMING mutex and retain
/// up to MAX_BUFFER_CHARS for the node's whole lifetime.
///
/// The provider gate lives in the spawn reader callback
/// (`agent::spawn::maybe_buffer_for_naming`); drive it with the node's
/// buffering gate open — the state where `on_output` WOULD write — so
/// this pin fails if the provider gate is ever bypassed.
#[test]
fn plain_terminal_output_never_reaches_rename_buffer() {
    let node_id = 70296;
    cleanup(node_id);
    open_gate(node_id);

    crate::agent::spawn::maybe_buffer_for_naming(true, node_id, "user typed: ls -la\n");

    assert!(
        buffer_of(node_id).as_deref().unwrap_or("").is_empty(),
        "terminal node output must not reach the rename buffer, got: {:?}",
        buffer_of(node_id)
    );
    cleanup(node_id);
}

/// Counterpart pin: the #296 gate must not swallow LLM providers'
/// chunks — `on_output` still fires on every chunk once the node's
/// buffering gate is open.
#[test]
fn llm_provider_output_still_reaches_rename_buffer() {
    let node_id = 71296;
    cleanup(node_id);
    open_gate(node_id);

    crate::agent::spawn::maybe_buffer_for_naming(false, node_id, "assistant reply chunk");

    assert_eq!(
        buffer_of(node_id).as_deref(),
        Some("assistant reply chunk"),
        "LLM provider output must keep accumulating after the #296 gate"
    );
    cleanup(node_id);
}

#[test]
fn reset_buffers_removes_only_target() {
    open_gate(5);
    open_gate(99);
    on_output(5, "session 5 output");
    on_output(99, "session 99 output");

    reset_buffers(5);

    assert!(buffer_of(5).is_none());
    assert!(buffer_of(99).is_some());
    cleanup(99);
}

#[test]
fn reset_buffers_closes_gate_so_next_session_starts_fresh() {
    // After a kill/resume cycle, the gate must close so the next agent's
    // startup chrome is dropped just like the first time.
    let node_id = 5151;
    open_gate(node_id);
    on_output(node_id, "turn-1 content");
    reset_buffers(node_id);

    // After reset, gate is closed — output is dropped until a new turn.
    on_output(node_id, "resumed-agent startup chrome");
    assert!(
        buffer_of(node_id).is_none(),
        "reset_buffers must also close the gate so resume-startup chrome is dropped"
    );
}

// --- SessionNamingRepository mock tests ---

struct MockRepo {
    node_name: String,
    should_fail: bool,
    /// When true, `update_agent_node_name` returns `Err` without
    /// recording the call in `updates`. Distinct from `should_fail`
    /// (which only affects `get_agent_node_by_id`) so a test can
    /// simulate "read works, write transiently fails" — the exact
    /// shape of the issue #1223 bug.
    update_should_fail: bool,
    updates: std::sync::Mutex<Vec<(i64, String)>>,
}

impl MockRepo {
    fn with_name(name: &str) -> Self {
        Self {
            node_name: name.to_string(),
            should_fail: false,
            update_should_fail: false,
            updates: std::sync::Mutex::new(vec![]),
        }
    }

    /// Repo whose `update_agent_node_name` always errors, simulating
    /// transient write-lock contention with the pool worker.
    fn with_failing_update(name: &str) -> Self {
        Self {
            node_name: name.to_string(),
            should_fail: false,
            update_should_fail: true,
            updates: std::sync::Mutex::new(vec![]),
        }
    }
}

impl SessionNamingRepository for MockRepo {
    fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String> {
        if self.should_fail {
            return Err("mock db error".into());
        }
        // The renaming-path code only reads `node.name`; the rest
        // spreads through `..Default::default()`.
        Ok(AgentNode {
            id,
            name: self.node_name.clone(),
            ..Default::default()
        })
    }
    fn update_agent_node_name(&self, id: i64, name: &str) -> Result<(), String> {
        if self.update_should_fail {
            return Err("mock update error".into());
        }
        self.updates.lock().unwrap().push((id, name.to_string()));
        Ok(())
    }
}

#[test]
fn should_trigger_rename_skips_already_renamed_node() {
    let node_id = 70001;
    open_gate(node_id);
    on_output(node_id, &"x".repeat(2000));

    let repo = MockRepo::with_name("fix-auth-token-refresh");
    let result = should_trigger_rename(&repo, node_id);
    assert!(result.is_none(), "should skip node with custom name");

    assert!(buffer_of(node_id).is_none(), "buffer should be cleared");
    assert!(
        !gate_open(node_id),
        "gate should be closed for renamed node"
    );
}

#[test]
fn should_trigger_rename_opens_gate_on_first_call_for_default_named_node() {
    // First call (no gate yet) is the "discard startup chrome" turn.
    // It opens the gate and returns None even if the buffer would have
    // been large enough, because the buffer at this point would be chrome.
    let node_id = 70002;
    cleanup(node_id);

    let repo = MockRepo::with_name("bold-keen-brook");
    let result = should_trigger_rename(&repo, node_id);
    assert!(
        result.is_none(),
        "first call must defer rename to open the gate"
    );
    assert!(gate_open(node_id), "gate must be open after first call");
    cleanup(node_id);
}

#[test]
fn should_trigger_rename_returns_buffer_on_second_call() {
    let node_id = 70003;
    cleanup(node_id);

    let repo = MockRepo::with_name("bold-keen-brook");
    // First call opens the gate, returns None
    let first = should_trigger_rename(&repo, node_id);
    assert!(first.is_none());

    // Now real PTY output accumulates (gate is open)
    on_output(node_id, &"x".repeat(2000));

    // Second call sees the buffer and returns it for renaming
    let second = should_trigger_rename(&repo, node_id);
    assert!(second.is_some(), "second call should trigger rename");
    assert_eq!(second.as_ref().unwrap().buffer.len(), 2000);

    clear_renaming_in_progress(node_id);
    cleanup(node_id);
}

#[test]
fn should_trigger_rename_skips_insufficient_buffer() {
    let node_id = 70004;
    cleanup(node_id);
    open_gate(node_id); // pretend gate already opened by an earlier turn
    on_output(node_id, "short");

    let repo = MockRepo::with_name("bold-keen-brook");
    let result = should_trigger_rename(&repo, node_id);
    assert!(result.is_none(), "should skip when buffer too small");
    cleanup(node_id);
}

#[test]
fn should_trigger_rename_skips_when_already_in_progress() {
    let node_id = 70005;
    cleanup(node_id);
    open_gate(node_id);
    on_output(node_id, &"x".repeat(2000));
    set_renaming_in_progress(node_id);

    let repo = MockRepo::with_name("bold-keen-brook");
    let result = should_trigger_rename(&repo, node_id);
    assert!(result.is_none(), "should skip when rename in progress");

    clear_renaming_in_progress(node_id);
    cleanup(node_id);
}

#[test]
fn should_trigger_rename_skips_when_max_attempts_reached() {
    let node_id = 70006;
    cleanup(node_id);
    open_gate(node_id);
    on_output(node_id, &"x".repeat(2000));
    set_attempts(node_id, MAX_RENAME_ATTEMPTS);

    let repo = MockRepo::with_name("bold-keen-brook");
    let result = should_trigger_rename(&repo, node_id);
    assert!(result.is_none(), "should skip when max attempts reached");

    cleanup(node_id);
}

/// Issue #824 follow-up: the rename-trigger return value hands back
/// the harvested PTY buffer from the second `should_trigger_rename`
/// call onward. The LLM-call backend is now resolved from
/// `AppPreferences.naming_provider` in `on_turn_with`, so the
/// trigger no longer carries the node's `provider`.
#[test]
fn should_trigger_rename_passes_buffer_through_on_second_call() {
    let node_id = 70007;
    cleanup(node_id);

    let repo = MockRepo::with_name("bold-keen-brook");
    // First call opens the gate; no RenameTrigger yet.
    assert!(should_trigger_rename(&repo, node_id).is_none());
    on_output(node_id, &"x".repeat(2000));

    // Second call should hand back the harvested buffer.
    let trigger = should_trigger_rename(&repo, node_id).expect("second call should trigger rename");
    assert_eq!(trigger.buffer.len(), 2000);

    clear_renaming_in_progress(node_id);
    cleanup(node_id);
}

#[test]
fn end_to_end_bypass_permissions_chrome_excluded_from_rename_buffer() {
    // Regression for the bypass-permissions slug bug.
    //
    // Simulated lifecycle of a freshly-spawned agent:
    //   1. Claude Code prints its startup chrome (banner + Bypass
    //      Permissions warning + plugin listing).
    //   2. First idle_prompt arrives -> on_turn fires -> gate opens.
    //   3. User does one real turn; PTY emits the real content.
    //   4. Second idle_prompt arrives -> on_turn fires -> buffer is read.
    //
    // The buffer handed to the renaming LLM must contain only step-3 content.
    let node_id = 70999;
    cleanup(node_id);

    let chrome = "\
            ╭─ WARNING ────────────────────────────────────╮\n\
            │  Claude Code running in Bypass Permissions   │\n\
            │  mode. Only use in a sandboxed environment.  │\n\
            ╰──────────────────────────────────────────────╯\n\
            Plugins loaded: hookify, feature-dev, frontend-design\n\
            ";
    // Step 1: chrome arrives before any on_turn — must be dropped
    on_output(node_id, chrome);

    let repo = MockRepo::with_name("bold-keen-brook");
    // Step 2: first turn opens the gate
    assert!(should_trigger_rename(&repo, node_id).is_none());

    // Step 3: real user content accumulates
    let real = "User: refactor the authentication module to use JWT.\n\
                    Assistant: I'll start by reading auth/login.rs.\n";
    let bulk = real.repeat(40); // push past SUMMARIZE_BUFFER_CHARS / 2
    on_output(node_id, &bulk);

    // Step 4: second turn — buffer is harvested
    let harvested = should_trigger_rename(&repo, node_id)
        .expect("rename should trigger on second turn with real content");
    let harvested_buf = &harvested.buffer;

    assert!(
        !harvested_buf.contains("Bypass Permissions"),
        "chrome leaked into rename buffer:\n{}",
        harvested_buf
    );
    assert!(
        !harvested_buf.contains("Plugins loaded"),
        "plugin listing leaked into rename buffer:\n{}",
        harvested_buf
    );
    assert!(
        !harvested_buf.contains("hookify"),
        "skill name leaked from startup chrome:\n{}",
        harvested_buf
    );
    assert!(
        harvested_buf.contains("authentication module"),
        "real user content missing from rename buffer:\n{}",
        harvested_buf
    );

    clear_renaming_in_progress(node_id);
    cleanup(node_id);
}

#[test]
fn mock_repo_update_records_calls() {
    let repo = MockRepo::with_name("bold-keen-brook");
    repo.update_agent_node_name(42, "test-slug-name").unwrap();
    let calls = repo.updates.lock().unwrap();
    assert_eq!(calls[0], (42, "test-slug-name".to_string()));
}

// --- issue #1223: DB-write-failure path must not emit node-renamed ---

/// Regression for issue #1223: when `update_agent_node_name` errors
/// after the LLM has returned a slug, the rename-pipeline MUST
/// leave the node's naming state intact (so the next Node Turn can
/// retry) and MUST NOT call the emit callback. The previous
/// inline implementation logged a warning, then unconditionally
/// cleared state and emitted — the frontend patched its in-memory
/// node list to a name SQLite never persisted, and the retry
/// buffer was wiped.
///
/// Asserts the contract on `commit_rename` directly, since the
/// inline `Ok(slug)` arm of `on_turn_with` is otherwise hidden
/// behind a real `summarize_and_rename_with` call (the unit-test
/// cannot spawn `claude --print`). The helper is the same code
/// path the async arm executes against the production repo.
#[test]
fn commit_rename_preserves_state_on_db_write_failure() {
    let node_id = 71100;
    cleanup(node_id);
    // Seed a "rename-eligible" state: gate open, buffer present.
    // Without this, `has_state` would be false trivially and the
    // test couldn't distinguish "we preserved state" from "there
    // was no state to preserve".
    open_gate(node_id);
    on_output(node_id, "transient write-lock contention simulation");

    let repo = MockRepo::with_failing_update("bold-keen-brook");
    let mut emitted: Option<String> = None;

    commit_rename(&repo, node_id, "fix-test-slug-bug", |name| {
        emitted = Some(name.to_string())
    });

    assert!(
        emitted.is_none(),
        "commit_rename must NOT call the emit callback when the DB \
             write failed (issue #1223); the UI would otherwise patch to \
             a name that was never persisted"
    );
    assert!(
        has_state(node_id),
        "commit_rename must leave naming state intact on DB write \
             failure so the next Node Turn can retry (issue #1223)"
    );

    cleanup(node_id);
}

/// Companion to `commit_rename_preserves_state_on_db_write_failure`:
/// pin the happy path so a future regression that over-eagerly
/// swallows success (e.g. returning on every code path) trips here.
/// On a successful DB write the helper MUST emit the rename and
/// clear the buffer/gate.
#[test]
fn commit_rename_emits_and_clears_on_db_write_success() {
    let node_id = 71101;
    cleanup(node_id);
    open_gate(node_id);
    on_output(node_id, "happy path buffer");

    let repo = MockRepo::with_name("bold-keen-brook");
    let mut emitted: Option<String> = None;

    commit_rename(&repo, node_id, "fix-auth-token-refresh", |name| {
        emitted = Some(name.to_string())
    });

    assert_eq!(
        emitted.as_deref(),
        Some("fix-auth-token-refresh"),
        "commit_rename must call the emit callback with the slug \
             the DB just accepted"
    );
    assert!(
        !has_state(node_id),
        "commit_rename must drop naming state once the DB write \
             succeeded — the rename is done"
    );

    let calls = repo.updates.lock().unwrap();
    assert_eq!(
        calls.as_slice(),
        &[(node_id, "fix-auth-token-refresh".to_string())],
        "the successful write must be recorded in the repo's \
             update channel (MockRepo stand-in for the SQL UPDATE)"
    );

    cleanup(node_id);
}

/// Static guard: the `Ok(slug)` arm of `on_turn_with` MUST route
/// the rename commit through the injected `repo` (via the
/// `commit_rename` helper) — historically it called the hardcoded
/// `DbSessionNamingRepository`, which is why the mock-repo tests
/// couldn't reach the commit path at all (the production DB
/// would always have been invoked instead). This regression test
/// makes that wiring a compile-time invariant: a refactor that
/// inlines a direct `DbSessionNamingRepository.update_agent_node_name`
/// call (or skips `commit_rename`) trips this assertion.
#[test]
fn on_turn_with_ok_arm_routes_through_commit_rename_not_hardcoded_db() {
    let source = include_str!("engine.rs");
    // Find the Ok(slug) arm body — the call inside the Ok match must
    // go through `commit_rename(...)`, never through
    // `DbSessionNamingRepository.update_agent_node_name` directly.
    let ok_marker = "Ok(slug) =>";
    let ok_idx = source.find(ok_marker).expect("Ok(slug) arm must exist");
    // Limit the search to the body of the Ok arm (ends at the next
    // `Err(e) =>` arm of the same match). Brace-counting isn't needed
    // here because the Ok arm is short and the next `Err(e) =>` is a
    // unique marker.
    let err_idx = source[ok_idx..]
        .find("Err(e) =>")
        .expect("Err(e) => arm must exist after Ok(slug)");
    let arm_body = &source[ok_idx..ok_idx + err_idx];

    assert!(
        arm_body.contains("commit_rename("),
        "Ok(slug) arm must route the rename commit through the \
             `commit_rename` helper, which is what lets the mock-repo \
             tests exercise the write-or-skip path (issue #1223). A direct \
             DbSessionNamingRepository.update_agent_node_name here would \
             bypass every test in this module"
    );
    assert!(
        !arm_body.contains("DbSessionNamingRepository.update_agent_node_name"),
        "Ok(slug) arm must NOT call DbSessionNamingRepository directly — \
             route the DB write through the `repo` parameter / commit_rename \
             helper instead"
    );
}

#[test]
fn strip_claude_code_banner_removes_logo_and_cwd_lines() {
    let input = "\u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.145\n\
                     \u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.7 with xhigh effort \u{00B7} Claude Pro\n\
                     \u{0020}\u{0020}\u{2598}\u{2598} \u{259D}\u{259D}    X:\\src\\buildmesh\\.claude\\worktrees\\open-lucky-box\n\
                     \n\
                     User asked to refactor the authentication flow.\n\
                     Assistant: I'll start by reading the auth module.";

    let stripped = strip_claude_code_banner(input);

    assert!(
        !stripped.contains("Claude Code v"),
        "version line should be gone:\n{}",
        stripped
    );
    assert!(
        !stripped.contains("Opus 4.7"),
        "model line should be gone:\n{}",
        stripped
    );
    assert!(
        !stripped.contains("open-lucky-box"),
        "cwd/banner line should be gone:\n{}",
        stripped
    );
    assert!(
        stripped.contains("refactor the authentication flow"),
        "real content must survive:\n{}",
        stripped
    );
}

#[test]
fn strip_claude_code_banner_is_noop_when_no_banner_present() {
    let input = "User asked to fix a bug.\nAssistant: Looking at the code now.";
    assert_eq!(strip_claude_code_banner(input), input);
}

// --- Manual rename race guard ---

#[test]
fn user_renamed_mid_flight_returns_true_for_user_chosen_name() {
    // "Fix OAuth callback" is not a default adj-adj-noun slug — the
    // guard must return true so the LLM slug is dropped.
    let repo = MockRepo::with_name("Fix OAuth callback");
    assert!(user_renamed_mid_flight(&repo, 1));
}

#[test]
fn user_renamed_mid_flight_returns_true_for_prior_llm_rename() {
    // A node whose LLM rename already committed is also "renamed";
    // the guard skips re-renaming in that case too.
    let repo = MockRepo::with_name("fix-auth-token-refresh");
    assert!(user_renamed_mid_flight(&repo, 1));
}

#[test]
fn user_renamed_mid_flight_returns_false_for_default_name() {
    // Default adj-adj-noun slugs (e.g. "bold-keen-brook") mean the node
    // has NOT been renamed, so the LLM rename is allowed to proceed.
    let repo = MockRepo::with_name("bold-keen-brook");
    assert!(!user_renamed_mid_flight(&repo, 1));
}

#[test]
fn user_renamed_mid_flight_returns_false_on_db_error() {
    // A transient DB read failure must NOT silently disable the
    // auto-rename path; err on the side of "user has not renamed" so
    // the LLM slug still gets a chance to commit.
    let repo = MockRepo {
        node_name: String::new(),
        should_fail: true,
        update_should_fail: false,
        updates: std::sync::Mutex::new(vec![]),
    };
    assert!(!user_renamed_mid_flight(&repo, 1));
}

// --- cleanup() frees the rename-eligible state ---

#[test]
fn cleanup_removes_node_from_rename_eligible_state() {
    // The manual-rename command relies on `cleanup` to free the buffer,
    // gate, and attempt counter for a node that no longer needs a rename.
    // With the consolidated state these all live in one entry, so cleanup
    // removing the entry frees them together. Seed a fully-populated entry
    // and assert cleanup drains it.
    let node_id = 81000i64;
    cleanup(node_id); // start from a clean slate

    {
        let mut map = naming();
        let st = map.entry(node_id).or_default();
        st.buffer = "buffered output".to_string();
        st.buffering_ready = true;
        st.attempts = 2;
    }

    // Sanity: the entry exists with its fields set.
    assert!(has_state(node_id));
    assert!(gate_open(node_id));
    assert_eq!(buffer_of(node_id).as_deref(), Some("buffered output"));

    cleanup(node_id);

    // Post-cleanup: the node's entire naming state is gone.
    assert!(!has_state(node_id), "cleanup must remove the node's entry");
}

// --- gh688: async claude spawn must set kill_on_drop ---

/// Regression guard for issue #688: `summarize_and_rename_with` spawns an
/// async `claude --print` child via `tokio::process::Command`. The rename
/// is fire-and-forget (caller is `tauri::async_runtime::spawn`) and
/// bounded by a 30s `tokio::time::timeout` — but tokio timeout cancels
/// the awaiter, NOT the spawned child. Without `.kill_on_drop(true)`,
/// cancellation, timeout, or app-shutdown leave the LLM child orphaned,
/// and the leak accumulates (one per node rename, capped at
/// `MAX_RENAME_ATTEMPTS = 3`).
///
/// `tokio::process::Command::get_kill_on_drop()` (tokio ≥ 1.47) would let
/// us test this at runtime, but only against a separately-constructed
/// command — not the actual line. A static check binds the assertion to
/// the production code, so a future refactor that drops the call fails
/// this test. The substring is unique to this site; a second async
/// `tokio::process::Command` would warrant a body-scope (see #665 for the
/// setter-only `creation_flags` precedent that motivated this shape).
#[test]
fn summarize_and_rename_with_sets_kill_on_drop_on_claude_command() {
    let source = include_str!("engine.rs");
    assert!(
        source.contains(".kill_on_drop(true)"),
        "summarize_and_rename_with must call .kill_on_drop(true) on its \
             tokio::process::Command (issue #688). Without it, cancellation, \
             timeout, or app-shutdown leaves the claude child orphaned."
    );
}

// --- gh824: session auto-naming must respect the user-configured provider ---

/// Unset `naming_provider` → empty Vec → caller skips rename entirely
/// (auto-naming is off). This is the post-v2 default: the user
/// must opt in via Settings → Auto-naming.
#[test]
fn naming_backend_env_with_unset_provider_returns_empty() {
    let probed = std::sync::atomic::AtomicUsize::new(0);
    let probed_ref = &probed;
    let env = naming_backend_env_with("", |_p| {
        probed_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Vec::new()
    });
    assert!(
        env.is_empty(),
        "unset provider must not invoke the resolve closure"
    );
    assert_eq!(
        probed.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "unset provider must NOT call resolve_provider_env (avoid disk reads)"
    );
}

/// Built-in Anthropic subscription → pinned haiku tier. The point of
/// this branch is to ensure `claude --print` does NOT silently pick
/// up the user's main subscription default (issue #824 review:
/// routing through whatever model the node is on would burn
/// Opus-tier tokens on a trivial summarisation).
#[test]
fn naming_backend_env_with_anthropic_pins_haiku() {
    let probed = std::sync::atomic::AtomicUsize::new(0);
    let probed_ref = &probed;
    let env = naming_backend_env_with("anthropic", |_p| {
        probed_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Vec::new()
    });
    assert_eq!(probed.load(std::sync::atomic::Ordering::SeqCst), 0);
    let map: std::collections::HashMap<_, _> = env.into_iter().collect();
    assert!(
        map.contains_key("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
        "built-in Anthropic must pin haiku; got: {:?}",
        map.keys().collect::<Vec<_>>()
    );
    assert!(
        !map.is_empty(),
        "anthropic branch must return SOMETHING (the haiku pin)"
    );
}

/// Configured provider-account pick → forwards through resolve_provider_env.
/// Whatever the user set up in Settings → Auto-naming is what runs.
#[test]
fn naming_backend_env_with_configured_account_forwards_resolve() {
    let configured = vec![(
        "ANTHROPIC_BASE_URL".to_string(),
        "https://configured.example/anthropic".to_string(),
    )];
    let probed = std::sync::atomic::AtomicUsize::new(0);
    let probed_ref = &probed;
    let env = naming_backend_env_with("claude:minimax", |p| {
        probed_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            p, "claude:minimax",
            "provider must be passed through to resolve"
        );
        configured.clone()
    });
    assert_eq!(
        probed.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "configured-provider path must call resolve_provider_env exactly once"
    );
    assert_eq!(
        env, configured,
        "configured provider env must propagate through"
    );
}

/// Static guard (issue #824 v2): the rename call site must read
/// `preferences::naming_provider()` (user-configured) and NOT the
/// node's own `provider`. The post-review pivot — a node provider
/// can be an expensive tier like Opus with xhigh effort, and
/// auto-rename runs frequently on trivial content, so the rename
/// backend is decoupled from the node's model and lives in Settings.
/// Brace-counts `on_turn_with`'s body so a regression that routes
/// through `node.provider` surfaces immediately.
#[test]
fn rename_call_site_uses_user_naming_provider_not_node_provider() {
    let source = include_str!("engine.rs");

    // Pull out the body of `fn on_turn_with(..)` by brace-counting so
    // nested closures don't false-match the closer.
    let sig = "fn on_turn_with(";
    let sig_idx = source.find(sig).expect("on_turn_with must exist");
    let open_rel = source[sig_idx..]
        .find('{')
        .expect("on_turn_with body must open with `{`");
    let body_start = sig_idx + open_rel + 1;
    let bytes = source.as_bytes();
    let mut depth: usize = 1;
    let mut i = body_start;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    assert_eq!(depth, 0, "on_turn_with body must close");
    let body_end = i - 1;
    let body = &source[body_start..body_end];

    // Strip line comments — the body documents the rejected v1
    // design ("NOT node.provider", etc.) and we don't want that prose
    // to false-positive.
    let code_only: String = body
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // The user-configured provider is the SOLE source of the rename
    // backend. Reading node.provider here would burn whatever
    // expensive tier the spawned node is on — the v1 regression
    // that triggered the v2 design pivot.
    assert!(
        code_only.contains("preferences::naming_provider()"),
        "on_turn_with must call preferences::naming_provider() (issue \
             #824 v2). Reading node.provider here would burn the node's \
             own model — auto-rename is opt-in via Settings, decoupled \
             from the node."
    );
    assert!(
        !code_only.contains("trigger.provider"),
        "on_turn_with must NOT use the node's provider for routing \
             (issue #824 v2). Rename-backend lives in Settings → Auto-naming."
    );
    assert!(
        code_only.contains("naming_backend_env(&user_naming_provider)"),
        "rename env must come from naming_backend_env(&user_naming_provider)"
    );
}

// --- claude binary resolution: PATH + well-known install fallback ---

/// The well-known-fallback arm must surface the official installer
/// binary (`%USERPROFILE%\.local\bin\claude.exe`) when it exists,
/// even if APPDATA is unset / has no `npm\claude*`. Mirrors the
/// common install path for Claude Code on Windows (the installer
/// drops a real `.exe` at `.local\bin\`, not an npm shim).
#[test]
fn windows_install_paths_finds_local_bin_claude_exe() {
    // `tempfile::tempdir` gives each test a unique directory so
    // parallel test execution can't race on a shared `pid`-suffixed
    // path. The directory auto-cleans on drop.
    let tmp = tempfile::tempdir().expect("create tempdir");
    let bin_dir = tmp.path().join(".local").join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create temp .local/bin");
    let target = bin_dir.join("claude.exe");
    // Create a tiny placeholder file so `is_file()` returns true. The
    // content doesn't matter — the resolver checks existence, not
    // executability, so we don't need a real PE here.
    std::fs::write(&target, b"fake").expect("write placeholder");

    let resolved = resolve_from_windows_install_paths(Some(tmp.path().to_str().unwrap()), None);

    assert_eq!(
        resolved.as_deref(),
        Some(target.as_path()),
        "well-known fallback must return %USERPROFILE%\\.local\\bin\\claude.exe when present"
    );
}

/// The well-known-fallback arm must surface the npm shim when the
/// official installer is absent. The `claude.cmd` candidate is
/// checked first to match `PATHEXT`'s `cmd` precedence (the npm
/// package on Windows ships a `.cmd` shim).
#[test]
fn windows_install_paths_finds_npm_claude_cmd() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let npm_dir = tmp.path().join("npm");
    std::fs::create_dir_all(&npm_dir).expect("create temp npm");
    let target = npm_dir.join("claude.cmd");
    std::fs::write(&target, b"fake").expect("write placeholder");

    let resolved = resolve_from_windows_install_paths(None, Some(tmp.path().to_str().unwrap()));

    assert_eq!(
        resolved.as_deref(),
        Some(target.as_path()),
        "well-known fallback must return %APPDATA%\\npm\\claude.cmd when present"
    );
}

/// When neither USERPROFILE nor APPDATA is set (e.g. a service or
/// a context where the env block was scrubbed), the resolver must
/// return `None` cleanly rather than panicking. The caller maps
/// `None` to the clear "claude binary not found" error rather than
/// a generic ENOENT.
#[test]
fn windows_install_paths_returns_none_when_no_env() {
    assert!(
        resolve_from_windows_install_paths(None, None).is_none(),
        "no env => no candidates => None"
    );
}

/// When the env vars are set but point at a directory without any
/// `claude*` binary, the resolver must return `None` (not a
/// `Some` of a non-existent path). This pins the `is_file()` check
/// inside the candidate loop — a regression that dropped it would
/// return a phantom path and the spawn would then fail with the
/// very "program not found" error we're trying to make clearer.
#[test]
fn windows_install_paths_returns_none_when_no_binary_present() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    std::fs::create_dir_all(tmp.path().join(".local").join("bin")).expect("dirs");
    std::fs::create_dir_all(tmp.path().join("npm")).expect("npm dir");

    let resolved = resolve_from_windows_install_paths(
        Some(tmp.path().to_str().unwrap()),
        Some(tmp.path().to_str().unwrap()),
    );

    assert!(
        resolved.is_none(),
        "empty install dirs must return None (no phantom path); got {:?}",
        resolved
    );
}

/// The official installer path wins over the npm shim when both
/// are present (the installer's `claude.exe` shadows the npm
/// shim, which is what `where.exe` would also return). This pins
/// the candidate ordering inside `resolve_from_well_known_paths`.
#[test]
fn windows_install_paths_prefers_local_bin_over_npm() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let local = tmp.path().join(".local").join("bin");
    let npm = tmp.path().join("npm");
    std::fs::create_dir_all(&local).expect("local dir");
    std::fs::create_dir_all(&npm).expect("npm dir");
    let local_target = local.join("claude.exe");
    let npm_target = npm.join("claude.cmd");
    std::fs::write(&local_target, b"local").expect("write local");
    std::fs::write(&npm_target, b"npm").expect("write npm");

    let resolved = resolve_from_windows_install_paths(
        Some(tmp.path().to_str().unwrap()),
        Some(tmp.path().to_str().unwrap()),
    );

    assert_eq!(
        resolved.as_deref(),
        Some(local_target.as_path()),
        "official installer (USERPROFILE\\.local\\bin\\claude.exe) must be preferred over npm shim"
    );
}

/// `resolve_claude_binary` (the public resolver) must wire the
/// `which`-miss arm to the Windows-install-paths fallback. Without
/// this, the wiring "which says no → fall through to install paths"
/// is untested.
#[test]
fn resolve_claude_binary_falls_through_to_windows_install_paths() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("create tempdir");
    let empty_path = tmp.path().join("empty_path");
    std::fs::create_dir_all(&empty_path).expect("empty PATH dir");
    let bin_dir = tmp.path().join(".local").join("bin");
    std::fs::create_dir_all(&bin_dir).expect("install dir");
    let target = bin_dir.join("claude.exe");
    std::fs::write(&target, b"fake").expect("write placeholder");
    let empty_appdata = tmp.path().join("empty_appdata");
    std::fs::create_dir_all(&empty_appdata).expect("empty APPDATA dir");

    // Force the which-arm to miss (PATH → empty) and isolate the
    // resolver from any pre-existing real install (USERPROFILE /
    // APPDATA → our temp dirs). All three vars are touched because
    // the resolver reads all three; missing any one lets a
    // parallel test's value bleed through and produce a false Ok.
    with_env_vars(
        &[
            ("PATH", Some(empty_path.as_os_str())),
            ("USERPROFILE", Some(tmp.path().as_os_str())),
            ("APPDATA", Some(empty_appdata.as_os_str())),
        ],
        || {
            let result = resolve_claude_binary();
            assert_eq!(result.as_deref().map(|p| p.to_path_buf()), Ok(target));
        },
    );
}

/// When both arms miss, the error must point users at the
/// actionable fix (install Claude Code) and NOT at the misleading
/// "pick a different provider in Settings → Auto-naming" — the
/// rename backend is always `claude --print`, so changing the
/// provider setting doesn't unblock a missing binary.
#[test]
fn resolve_claude_binary_error_does_not_mislead_to_settings() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("create tempdir");
    let empty_path = tmp.path().join("empty_path");
    std::fs::create_dir_all(&empty_path).expect("empty PATH dir");
    let empty_appdata = tmp.path().join("empty_appdata");
    std::fs::create_dir_all(&empty_appdata).expect("empty APPDATA dir");

    with_env_vars(
        &[
            ("PATH", Some(empty_path.as_os_str())),
            ("USERPROFILE", Some(tmp.path().as_os_str())),
            ("APPDATA", Some(empty_appdata.as_os_str())),
        ],
        || {
            let err = resolve_claude_binary()
                .expect_err("with no binary anywhere, the resolver must Err");
            assert!(
                err.contains("claude binary not found"),
                "error must say what was missing; got: {}",
                err
            );
            assert!(
                err.contains("install Claude Code"),
                "error must point at the actionable fix; got: {}",
                err
            );
            assert!(
                !err.contains("pick a different provider"),
                "error must not blame Settings → Auto-naming for a missing binary; got: {}",
                err
            );
        },
    );
}

/// Static guard: the rename call site must go through
/// `resolve_claude_binary` rather than the previous literal
/// `Command::new("claude")`. A regression that re-introduces the
/// literal would re-trigger the "program not found" toast for
/// users with a stale buildmesh PATH.
///
/// Brace-counts the function body instead of a file-level
/// `source.contains("...")`, because the assertion message itself
/// contains the literal being checked (a file-level check would
/// always pass). Matches the established gh824 test shape at
/// `session_naming.rs:2377`.
#[test]
fn summarize_and_rename_uses_resolved_claude_path_not_literal() {
    let source = include_str!("engine.rs");

    // Pull out the body of `fn summarize_and_rename_with(..)` by
    // brace-counting so nested closures don't false-match.
    let sig = "async fn summarize_and_rename_with(";
    let sig_idx = source
        .find(sig)
        .expect("summarize_and_rename_with must exist");
    let open_rel = source[sig_idx..]
        .find('{')
        .expect("summarize_and_rename_with body must open with `{`");
    let body_start = sig_idx + open_rel + 1;
    let bytes = source.as_bytes();
    let mut depth: usize = 1;
    let mut i = body_start;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    assert_eq!(depth, 0, "summarize_and_rename_with body must close");
    let body_end = i - 1;
    let body = &source[body_start..body_end];

    // Strip line comments so the explanatory prose in the body
    // (the rejected-v1 design note) doesn't false-positive.
    let code_only: String = body
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // The call site must go through the resolver so the
    // well-known install-location fallback applies when the
    // process's PATH is stale (the "program not found" toast).
    assert!(
        code_only.contains("resolve_claude_binary()?"),
        "summarize_and_rename_with must call resolve_claude_binary() \
             (PATH-stale spawn failure). A direct `Command::new(\"claude\")` \
             falls back to the process's inherited PATH, which on Windows \
             can be stale if Claude Code was installed after buildmesh \
             launched."
    );

    // And the call site must NOT still spawn the literal "claude"
    // string — that would re-introduce the bug. The
    // `command_no_window("claude")` shape is unique to the old
    // call site (the regular Claude Code spawn goes through
    // `claude_direct_recipe` / `spawn_environment`, not
    // `command_no_window`).
    assert!(
        !code_only.contains("command_no_window(\"claude\")"),
        "summarize_and_rename_with must NOT spawn the literal \
             \"claude\" anymore — use resolve_claude_binary() so the \
             well-known install-location fallback applies."
    );
}
