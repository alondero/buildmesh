use rand::seq::IndexedRandom;
use super::words::{ADJECTIVES, NOUNS};

// ---------------------------------------------------------------------------
// Random name generation (word lists + combinatorics)
// ---------------------------------------------------------------------------

#[rustfmt::skip]
pub(super) static SLUG_REGEX: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[a-z][a-z0-9-]{2,50}$").unwrap());

// ---------------------------------------------------------------------------
// Public lifecycle API
// ---------------------------------------------------------------------------

/// Generate an initial random name for a newly spawned agent node.
/// Returns a three-word hyphenated slug (e.g. "bold-keen-brook").
pub fn on_spawn() -> String {
    let mut rng = rand::rng();
    let adj1 = ADJECTIVES.choose(&mut rng).unwrap();
    let adj2 = ADJECTIVES.choose(&mut rng).unwrap();
    let noun = NOUNS.choose(&mut rng).unwrap();
    format!("{}-{}-{}", adj1, adj2, noun)
}

/// Build the initial node name for a node spawned from a GitHub issue.
///
/// Composes `gh{issue_number}-{slug_from_title}` so the user can spot the
/// origin in the mesh list from the moment the spawn modal closes (e.g.
/// issue #123 "fix this feature" → `gh123-fix-this-feature`). The prefix is
/// the same on both spawn paths (`spawn_issue_agent` for the mobile HTTP
/// route and `create_issue_node` for the desktop two-stage flow).
///
/// Behaviour:
/// - Re-uses `slugify_issue_title` for the title → slug step, so all the
///   rules there (lowercase, hyphenation, 50-char cap, fallback to a
///   random default when the title is unslugifyable) carry over.
/// - Prepends `gh{n}-`. If the combined string is still under 50 chars the
///   result is final; otherwise it's truncated to 50 with a trailing-hyphen
///   re-trim (mirroring `slugify_issue_title`'s truncation step).
/// - Validates the final result against `SLUG_REGEX`. The only realistic
///   failure mode is a truncation that lands mid-token and yields something
///   that no longer matches (e.g. trailing punctuation in the digit run);
///   in that case we fall back to a plain random name so the caller always
///   gets a valid slug (an empty string would break worktree creation).
pub fn issue_node_name(issue_number: i64, title: &str) -> String {
    prefixed_node_name("gh", issue_number, title)
}

/// Build the initial node name for a node spawned from a GitHub pull request
/// (issue #420). `pr{N}-` prefix distinguishes PR-spawned nodes from
/// issue-spawned `gh{N}-` ones in the sidebar.
pub fn pr_node_name(pr_number: i64, title: &str) -> String {
    prefixed_node_name("pr", pr_number, title)
}

/// Shared core for `issue_node_name` / `pr_node_name` — both flows just
/// pick a prefix and the rules below apply uniformly. Centralising the
/// 50-char cap, trailing-hyphen re-trim, and `SLUG_REGEX` / `on_spawn()`
/// fallback in one place means a future tweak (e.g. bumping the cap) lands
/// for every spawn source at once.
///
/// Wire format: `{prefix}{number}-{slug}` (no dash between prefix and
/// number — `gh123-fix-this-feature`, not `gh-123-fix-this-feature`).
/// This matches what existing users have on disk from previous builds
/// and what the existing issue-spawn tests pin.
pub(super) fn prefixed_node_name(prefix: &str, number: i64, title: &str) -> String {
    let slug = slugify_issue_title(title);
    let mut full = format!("{}{}-{}", prefix, number, slug);

    if full.len() > 50 {
        full.truncate(50);
        while full.ends_with('-') {
            full.pop();
        }
    }

    if SLUG_REGEX.is_match(&full) {
        full
    } else {
        on_spawn()
    }
}

/// Derive a node name from a GitHub issue title (issue #111).
///
/// Pipeline: lowercase → spaces/underscores become hyphens → strip
/// non-alphanumeric characters (keeping hyphens) → trim leading and trailing
/// hyphens → cap at 50 chars (with another trailing-hyphen trim in case
/// truncation lands on one). If the result is empty, starts with a digit, or
/// otherwise fails `SLUG_REGEX`, fall back to a random default name from
/// `on_spawn()` so the caller always gets a valid name — an empty string
/// would break the worktree directory creation downstream.
///
/// Pure function — exposed for `spawn_issue_agent` / `create_issue_node` to
/// give issue-spawned nodes a meaningful initial identifier, and unit-tested
/// here so the rules stay pinned.
///
/// Note on rename interaction: the result is a non-default slug, so the LLM
/// rename path's `is_default_name` guard will see it as already-named and
/// skip the LLM rename for issue-spawned nodes. The user can still manually
/// rename via `rename_session`; making the LLM fire on issue-derived names
/// is tracked as a follow-up (would need a separate "rename_locked" column
/// to distinguish user/prior-LLM renames from issue-derived slugs).
pub fn slugify_issue_title(title: &str) -> String {
    // 1. lowercase, 2. spaces + underscores → hyphens
    let mut s: String = title.to_lowercase().replace([' ', '_'], "-");

    // 3. strip non-alphanumeric, keeping hyphens
    s.retain(|c| c.is_ascii_alphanumeric() || c == '-');

    // 4. trim leading and trailing hyphens
    s = s.trim_matches('-').to_string();

    // 5. cap at 50 chars (re-trim trailing hyphens if truncation split a word)
    if s.len() > 50 {
        s.truncate(50);
        while s.ends_with('-') {
            s.pop();
        }
    }

    // 6. validate against SLUG_REGEX; fall back to a random default if invalid
    if SLUG_REGEX.is_match(&s) {
        s
    } else {
        on_spawn()
    }
}

/// Check if a name matches the random default pattern (adj-adj-noun).
pub fn is_default_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    ADJECTIVES.contains(&parts[0]) && ADJECTIVES.contains(&parts[1]) && NOUNS.contains(&parts[2])
}

pub(super) fn is_conversational_response(s: &str) -> bool {
    if s.contains('?') {
        return true;
    }
    let lower = s.to_lowercase();
    let conversational_prefixes = [
        "it looks like",
        "i'm not sure",
        "what can i",
        "how can i",
        "that looks like",
        "i don't",
        "this looks like",
        "it seems like",
        "i can help",
        "let me help",
    ];
    conversational_prefixes.iter().any(|p| lower.starts_with(p))
}

pub(super) fn slug_with_retry(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches('"').trim_matches('`');
    if trimmed.split_whitespace().nth(5).is_some() && is_conversational_response(trimmed) {
        let preview = trimmed
            .char_indices()
            .nth(80)
            .map_or(trimmed, |(i, _)| &trimmed[..i]);
        return Err(format!(
            "naming LLM returned conversational response: '{}'",
            preview
        ));
    }

    // Extract just the first hyphenated slug-like line
    let candidate = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .find(|l| l.contains('-'))
        .map(|l| l.trim().trim_matches('"').trim_matches('`').to_lowercase())
        .unwrap_or_else(|| {
            raw.trim()
                .trim_matches('"')
                .trim_matches('`')
                .to_lowercase()
        });

    // Normalise space-separated words to hyphens when no hyphens present
    let candidate = if !candidate.contains('-') {
        candidate.split_whitespace().collect::<Vec<_>>().join("-")
    } else {
        candidate
    };

    let token_count = candidate.split('-').count();
    if (3..=5).contains(&token_count) && SLUG_REGEX.is_match(&candidate) {
        return Ok(candidate);
    }

    // Fallback: extract longest run of hyphenated words
    let fallback = candidate
        .split_whitespace()
        .filter(|w| w.contains('-'))
        .max_by_key(|w| w.split('-').count())
        .unwrap_or(&candidate)
        .to_string();

    let fallback_count = fallback.split('-').count();
    if (3..=5).contains(&fallback_count) && SLUG_REGEX.is_match(&fallback) {
        return Ok(fallback);
    }

    Err(format!(
        "slug has {} dash-separated tokens (expected 3-5): '{}'",
        fallback_count.max(token_count),
        candidate
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
