use std::collections::HashSet;

use super::{
    engine::*,
    repository::*,
    slug::*,
    words::{ADJECTIVES, NOUNS},
};
use crate::models::AgentNode;

#[test]
fn naming_accepts_native_codex_launch_configuration() {
    let prefs = crate::preferences::AppPreferences::default();
    let plan = crate::preferences::launch_configurations::capture(
        &prefs,
        "codex",
        &crate::preferences::launch_configurations::LaunchOverrides::default(),
    )
    .unwrap();
    let result = naming_backend_env_from_plan(plan, &prefs);
    assert!(
        result.is_ok(),
        "native Codex supports background inference: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "requires an installed and authenticated harness; set BUILDMESH_BACKGROUND_HARNESS"]
async fn live_background_inference() {
    let harness =
        std::env::var("BUILDMESH_BACKGROUND_HARNESS").expect("select the live harness explicitly");
    let prefs = crate::preferences::AppPreferences::default();
    let plan = crate::preferences::launch_configurations::capture(
        &prefs,
        &harness,
        &crate::preferences::launch_configurations::LaunchOverrides::default(),
    )
    .unwrap();
    let launch = naming_backend_env_from_plan(plan, &prefs).unwrap();
    let slug = summarize_and_rename_with(
        0,
        "The user asked to fix background naming. Reply only with fix-background-naming.",
        launch,
    )
    .await
    .unwrap();
    assert_eq!(slug, "fix-background-naming");
}

#[tokio::test]
async fn codex_background_naming_reads_the_final_file_and_preserves_saved_settings() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join(if cfg!(windows) {
        "naming.ps1"
    } else {
        "naming.sh"
    });
    let args_file = directory.path().join("args.txt");
    if cfg!(windows) {
        std::fs::write(&script, format!(
            "$promptText = [Console]::In.ReadToEnd()\n\
             if (-not $promptText.Contains('Conversation messages (JSON):')) {{ exit 9 }}\n\
             if (-not $promptText.Contains('fix background naming')) {{ exit 8 }}\n\
             if ($env:OPENAI_API_KEY -or $env:OPENAI_BASE_URL -or $env:BUILDMESH_SESSION_ID) {{ exit 7 }}\n\
             [IO.File]::WriteAllLines({}, [string[]]$args)\n\
             $index = [Array]::IndexOf([object[]]$args, '--output-last-message')\n\
             [IO.File]::WriteAllText($args[$index + 1], 'fix-background-naming')\n\
             [Console]::Out.WriteLine('wrong-stdout-answer')\nexit 0\n",
            crate::env::powershell_literal(&args_file.to_string_lossy()),
        )).unwrap();
    } else {
        std::fs::write(&script, format!(
            "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *'Conversation messages (JSON):'*'fix background naming'*) ;; *) exit 9;; esac\n\
             [ -z \"$OPENAI_API_KEY$OPENAI_BASE_URL$BUILDMESH_SESSION_ID\" ] || exit 7\n\
             printf '%s\\n' \"$@\" > {}\nwhile [ \"$#\" -gt 0 ]; do\n\
             if [ \"$1\" = '--output-last-message' ]; then shift; printf fix-background-naming > \"$1\"; break; fi\n\
             shift\ndone\nprintf wrong-stdout-answer\n",
            shell_words::quote(&args_file.to_string_lossy()),
        )).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let prefs = crate::preferences::AppPreferences::default();
    let mut plan = crate::preferences::launch_configurations::capture(
        &prefs,
        "codex",
        &crate::preferences::launch_configurations::LaunchOverrides {
            model: Some("gpt-6-luna".into()),
            effort: Some("low".into()),
            extra_args: None,
        },
    )
    .unwrap();
    plan.harness.executable = Some(script);
    let launch = naming_backend_env_from_plan(plan, &prefs).unwrap();
    assert_eq!(
        summarize_and_rename_with(0, "fix background naming", launch)
            .await
            .unwrap(),
        "fix-background-naming"
    );
    let args = std::fs::read_to_string(args_file).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--model", "gpt-6-luna"]));
    assert!(args.contains(&"model_reasoning_effort=\"low\""));
    assert!(args.contains(&"--ephemeral"));
    assert_eq!(args.last(), Some(&"-"));
}

fn has_state(node_id: i64) -> bool {
    naming().contains_key(&node_id)
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

// --- pr_reviewer_node_name + disambiguate_node_name ---

/// The reviewer sibling carries a `review` token so it reads as distinct from
/// the implementation node for the same PR.
#[test]
fn pr_reviewer_node_name_marks_review_and_prefixes_pr_number() {
    assert_eq!(
        pr_reviewer_node_name(123, "add pr chip"),
        "pr123-review-add-pr-chip"
    );
}

/// The whole point of the separate helper: a reviewer must NOT collide with the
/// implementation node's name, or the two would share one worktree.
#[test]
fn pr_reviewer_node_name_differs_from_implementation_name() {
    let title = "add pr chip";
    assert_ne!(pr_reviewer_node_name(123, title), pr_node_name(123, title));
}

// --- issue_reviewer_node_name + agent_reviewer_node_name ---

#[test]
fn issue_reviewer_node_name_marks_review_and_prefixes_issue_number() {
    assert_eq!(
        issue_reviewer_node_name(123, "fix it"),
        "gh123-review-fix-it"
    );
}

#[test]
fn agent_reviewer_node_name_prefixes_review_to_the_source_name() {
    assert_eq!(
        agent_reviewer_node_name("gh5-add-chip"),
        "review-gh5-add-chip"
    );
}

#[test]
fn issue_and_agent_reviewer_node_names_are_not_default_names() {
    assert!(!is_default_name(&issue_reviewer_node_name(123, "fix it")));
    assert!(!is_default_name(&agent_reviewer_node_name("gh5-add-chip")));
}

#[test]
fn disambiguate_node_name_returns_base_when_free() {
    assert_eq!(
        disambiguate_node_name("pr1-review-x", &HashSet::new()),
        "pr1-review-x"
    );
}

/// Repeat reviewer spawns append the next free numeric suffix so each gets its
/// own worktree directory.
#[test]
fn disambiguate_node_name_appends_next_free_suffix() {
    let taken = HashSet::from(["pr1-review-x".to_string(), "pr1-review-x-2".to_string()]);
    assert_eq!(
        disambiguate_node_name("pr1-review-x", &taken),
        "pr1-review-x-3"
    );
}

/// A base already at the 50-char cap must still yield a valid slug after the
/// suffix is appended — the suffix is carved out of the base, not past the cap.
#[test]
fn disambiguate_node_name_stays_within_the_slug_cap() {
    let base = format!("pr1-review-{}", "a".repeat(39));
    assert_eq!(base.len(), 50);
    let taken = HashSet::from([base.clone()]);
    let result = disambiguate_node_name(&base, &taken);
    assert!(
        result.ends_with("-2"),
        "expected a -2 suffix, got {:?}",
        result
    );
    assert!(result.len() <= 50, "must stay within the cap: {:?}", result);
    assert!(
        SLUG_REGEX.is_match(&result),
        "must stay valid: {:?}",
        result
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
fn slug_with_retry_truncates_too_long() {
    assert_eq!(
        slug_with_retry("one-two-three-four-five-six").unwrap(),
        "one-two-three-four-five"
    );
    // Seven words is past the prompt's own range, but the answer is already
    // hyphen-joined, so it is a name and only its tail is over-long.
    assert_eq!(
        slug_with_retry("fix-rust-dead-code-warnings-raise-pr").unwrap(),
        "fix-rust-dead-code-warnings"
    );
}

#[test]
fn slug_with_retry_truncates_at_the_character_cap() {
    // Five words, 66 chars: the word-boundary cut keeps three whole words and
    // never leaves a trailing hyphen.
    let slug = slug_with_retry("extraordinarily-complicated-configuration-normalization-refactor")
        .unwrap();
    assert_eq!(slug, "extraordinarily-complicated-configuration");
    assert!(slug.len() <= 50);
    assert!(!slug.ends_with('-'));
}

#[test]
fn slug_with_retry_collapses_empty_tokens_before_clamping() {
    assert_eq!(
        slug_with_retry("fix--rust--dead--code--warnings--raise--pr").unwrap(),
        "fix-rust-dead-code-warnings"
    );
}

#[test]
fn slug_with_retry_still_rejects_prose() {
    // Clamping is for length only: a response that is not slug-shaped must
    // still spend an attempt so the backend gets another go.
    for prose in [
        "the task: improve auth flow",
        "the task: improve auth flow and the tests too",
    ] {
        let err = slug_with_retry(prose).unwrap_err();
        assert!(
            err.contains("lowercase words joined by hyphens"),
            "expected a shape rejection for: '{prose}', got: {err}"
        );
    }
    let err = slug_with_retry("fix: it").unwrap_err();
    assert!(err.contains("dash-separated tokens"), "got: {err}");
}

#[test]
fn slug_with_retry_refuses_a_sentence_rather_than_truncating_it() {
    // Nothing is trimmed out of the model's own words: dropping the tail of a
    // sentence yields a plausible-looking name that describes nothing, whether
    // the cut lands on the word count or on the character cap.
    for sentence in [
        "please rename it to fix auth flow",     // seven words
        "could you please rename this node now", // seven words
        "i think the best name would be tidy",   // eight words
        // Five words, but 51 characters once joined: the word count alone lets
        // this through, so only the cap refuses it.
        "configuration refactor documentation should mention",
    ] {
        let err = slug_with_retry(sentence).unwrap_err();
        assert!(
            err.contains("lowercase words joined by hyphens"),
            "expected a refusal for: '{sentence}', got: {err}"
        );
    }
    // A word run inside the range is still normalised, including the ambiguous
    // short ones — the boundary the user guide states.
    assert_eq!(
        slug_with_retry("fix auth token flow").unwrap(),
        "fix-auth-token-flow"
    );
    assert_eq!(
        slug_with_retry("please rename it").unwrap(),
        "please-rename-it"
    );
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
fn slug_with_retry_extracts_hyphenated_from_prose() {
    let result = slug_with_retry("Based on the session, a good name would be: improve-auth-flow");
    assert!(result.is_ok());
}

// --- SessionNamingRepository mock tests ---

struct MockRepo {
    node_name: String,
    context: Option<String>,
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
            context: Some(r#"[{"role":"user","text":"Fix login"}]"#.into()),
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
            context: Some(r#"[{"role":"user","text":"Fix login"}]"#.into()),
            should_fail: false,
            update_should_fail: true,
            updates: std::sync::Mutex::new(vec![]),
        }
    }
}

impl SessionNamingRepository for MockRepo {
    fn naming_context(&self, _node: &AgentNode) -> Option<String> {
        self.context.clone()
    }

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
fn naming_uses_short_dialogue_on_first_turn() {
    let id = 70999;
    cleanup(id);
    let repo = MockRepo::with_name("bold-keen-brook");
    let trigger = should_trigger_rename(&repo, id).unwrap();
    assert_eq!(
        trigger.dialogue,
        "[{\"role\":\"user\",\"text\":\"Fix login\"}]"
    );
    assert!(
        should_trigger_rename(&repo, id).is_none(),
        "one in-flight rename"
    );
    cleanup(id);
}

#[test]
fn unavailable_dialogue_retries_without_spending_attempts() {
    let id = 71000;
    cleanup(id);
    let mut repo = MockRepo::with_name("bold-keen-brook");
    repo.context = None;
    for _ in 0..5 {
        assert!(should_trigger_rename(&repo, id).is_none());
        assert_eq!(naming().get(&id).unwrap().attempts, 0);
        assert!(!naming().get(&id).unwrap().renaming);
    }
    repo.context = Some("actual task".into());
    assert_eq!(
        should_trigger_rename(&repo, id).unwrap().dialogue,
        "actual task"
    );
    cleanup(id);
}

#[test]
fn named_nodes_and_failed_reads_never_start_inference() {
    let id = 71001;
    cleanup(id);
    assert!(should_trigger_rename(&MockRepo::with_name("my-task-name"), id).is_none());
    assert!(!has_state(id));
    let mut repo = MockRepo::with_name("bold-keen-brook");
    repo.should_fail = true;
    assert!(should_trigger_rename(&repo, id).is_none());
    assert!(!has_state(id));
}

#[test]
fn inference_failures_lock_out_until_reset() {
    let id = 71002;
    cleanup(id);
    let repo = MockRepo::with_name("bold-keen-brook");
    for attempt in 1..=MAX_RENAME_ATTEMPTS {
        let trigger = should_trigger_rename(&repo, id).unwrap();
        assert_eq!(record_failed_attempt(&trigger.reservation), Some(attempt));
    }
    assert!(should_trigger_rename(&repo, id).is_none());
    reset_buffers(id);
    assert!(should_trigger_rename(&repo, id).is_some());
    cleanup(id);
}

#[test]
fn stale_reservation_cannot_release_or_charge_resumed_naming() {
    let id = 71004;
    cleanup(id);
    let repo = MockRepo::with_name("bold-keen-brook");
    let old = should_trigger_rename(&repo, id).unwrap();
    reset_buffers(id);
    let current = should_trigger_rename(&repo, id).unwrap();
    assert!(!old.reservation.is_current());
    assert_eq!(record_failed_attempt(&old.reservation), None);
    commit_rename(&repo, &old.reservation, "stale-task-name", |_| {
        panic!("stale completion emitted")
    });
    assert!(repo.updates.lock().unwrap().is_empty());
    drop(old);
    assert!(current.reservation.is_current());
    assert!(should_trigger_rename(&repo, id).is_none());
    assert_eq!(naming().get(&id).unwrap().attempts, 0);
    drop(current);
    assert!(should_trigger_rename(&repo, id).is_some());
    cleanup(id);
}

#[test]
fn old_completion_after_new_success_cannot_restore_stale_name() {
    let id = 71006;
    cleanup(id);
    let repo = MockRepo::with_name("bold-keen-brook");
    let old = should_trigger_rename(&repo, id).unwrap();
    reset_buffers(id);
    let current = should_trigger_rename(&repo, id).unwrap();
    let mut names = Vec::new();
    commit_rename(&repo, &current.reservation, "new-task-name", |name| {
        names.push(name.to_owned())
    });
    commit_rename(&repo, &old.reservation, "old-task-name", |_| {
        panic!("old completion emitted")
    });
    assert_eq!(names, ["new-task-name"]);
    assert_eq!(
        *repo.updates.lock().unwrap(),
        [(id, "new-task-name".to_owned())]
    );
    assert_eq!(record_failed_attempt(&old.reservation), None);
    assert!(!has_state(id));
}

#[test]
fn reset_during_transcript_read_discards_the_prepared_rename() {
    struct ResetDuringRead(MockRepo);
    impl SessionNamingRepository for ResetDuringRead {
        fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String> {
            self.0.get_agent_node_by_id(id)
        }
        fn update_agent_node_name(&self, id: i64, name: &str) -> Result<(), String> {
            self.0.update_agent_node_name(id, name)
        }
        fn naming_context(&self, node: &AgentNode) -> Option<String> {
            reset_buffers(node.id);
            self.0.naming_context(node)
        }
    }
    let id = 71005;
    cleanup(id);
    let repo = ResetDuringRead(MockRepo::with_name("bold-keen-brook"));
    assert!(prepare_rename_with(&repo, id, || panic!(
        "stale read must not resolve an inference backend"
    ))
    .unwrap()
    .is_none());
    assert!(!has_state(id));
}

#[test]
fn failed_backend_resolution_releases_rename_ownership() {
    let id = 71003;
    cleanup(id);
    let repo = MockRepo::with_name("bold-keen-brook");
    let err = prepare_rename_with(&repo, id, || Err("repair configuration".into()))
        .err()
        .unwrap();
    assert_eq!(err, "repair configuration");
    assert_eq!(naming().get(&id).unwrap().attempts, 0);
    assert!(should_trigger_rename(&repo, id).is_some());
    cleanup(id);
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
/// dialogue was wiped.
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
    // Seed an in-flight rename so persistence failures must preserve state.
    // Without this, `has_state` would be false trivially and the
    // test couldn't distinguish "we preserved state" from "there
    // was no state to preserve".

    let repo = MockRepo::with_failing_update("bold-keen-brook");
    let trigger = should_trigger_rename(&repo, node_id).unwrap();
    let mut emitted: Option<String> = None;

    commit_rename(&repo, &trigger.reservation, "fix-test-slug-bug", |name| {
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
/// clear the dialogue/gate.
#[test]
fn commit_rename_emits_and_clears_on_db_write_success() {
    let node_id = 71101;
    cleanup(node_id);

    let repo = MockRepo::with_name("bold-keen-brook");
    let trigger = should_trigger_rename(&repo, node_id).unwrap();
    let mut emitted: Option<String> = None;

    commit_rename(
        &repo,
        &trigger.reservation,
        "fix-auth-token-refresh",
        |name| emitted = Some(name.to_string()),
    );

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
        context: None,
        should_fail: true,
        update_should_fail: false,
        updates: std::sync::Mutex::new(vec![]),
    };
    assert!(!user_renamed_mid_flight(&repo, 1));
}

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

/// `resolve_claude_binary_in` (the resolver behind `resolve_claude_binary`)
/// must wire the `which`-miss arm to the Windows-install-paths fallback.
/// Without this, the wiring "which says no → fall through to install paths"
/// is untested.
///
/// The lookup inputs are passed in, never written to the process environment:
/// a rewritten `PATH` makes every concurrent test that spawns `git`, `node` or
/// `powershell.exe` fail with "program not found" (issue #2109).
#[test]
fn resolve_claude_binary_falls_through_to_windows_install_paths() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let empty_path = tmp.path().join("empty_path");
    std::fs::create_dir_all(&empty_path).expect("empty PATH dir");
    let bin_dir = tmp.path().join(".local").join("bin");
    std::fs::create_dir_all(&bin_dir).expect("install dir");
    let target = bin_dir.join("claude.exe");
    std::fs::write(&target, b"fake").expect("write placeholder");
    let empty_appdata = tmp.path().join("empty_appdata");
    std::fs::create_dir_all(&empty_appdata).expect("empty APPDATA dir");

    // PATH misses, and USERPROFILE / APPDATA point at our temp dirs so a real
    // install on the machine running the tests cannot bleed through.
    let result = resolve_claude_binary_in(&ClaudeSearch {
        path: Some(empty_path.into_os_string()),
        userprofile: Some(tmp.path().to_string_lossy().into_owned()),
        appdata: Some(empty_appdata.to_string_lossy().into_owned()),
    });
    assert_eq!(result, Ok(target));
}

/// When both arms miss, the error must point users at the
/// actionable fix (install Claude Code) and NOT at the misleading
/// "pick a different provider in Settings → Auto-naming" — the
/// rename backend is always `claude --print`, so changing the
/// provider setting doesn't unblock a missing binary.
#[test]
fn resolve_claude_binary_error_does_not_mislead_to_settings() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let empty_path = tmp.path().join("empty_path");
    std::fs::create_dir_all(&empty_path).expect("empty PATH dir");
    let empty_appdata = tmp.path().join("empty_appdata");
    std::fs::create_dir_all(&empty_appdata).expect("empty APPDATA dir");

    let err = resolve_claude_binary_in(&ClaudeSearch {
        path: Some(empty_path.into_os_string()),
        userprofile: Some(tmp.path().to_string_lossy().into_owned()),
        appdata: Some(empty_appdata.to_string_lossy().into_owned()),
    })
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
}
