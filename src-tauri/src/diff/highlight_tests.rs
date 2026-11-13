//! The highlighting suite — moved with the highlighting code (issue #2527).
//!
//! The test names are the ones the suite had inside `commands/diff.rs`; the
//! module they live in moved, the assertions did not.

use crate::diff::highlight::{ext_for_path, highlight_content, highlighted_or_escape, html_escape};

#[test]
fn ext_for_path_extracts_extension() {
    assert_eq!(ext_for_path("foo/bar.rs"), "rs");
    assert_eq!(ext_for_path("Cargo.toml"), "toml");
    assert_eq!(ext_for_path("no-extension"), "");
    assert_eq!(ext_for_path("src/main.tsx"), "tsx");
}

#[test]
fn highlight_content_returns_non_empty_for_code() {
    let result = highlight_content("fn main() {}", "test.rs");
    assert!(!result.is_empty());
    assert!(result.contains("main"));
}

#[test]
fn highlight_content_handles_unknown_extension() {
    let result = highlight_content("random text", "file.xyz123");
    assert!(result.contains("random text"));
}

/// Issue #2152: when syntect fails, `highlight_content` used to hand the raw
/// file contents back as HTML. The diff view injects these highlight fields
/// with `dangerouslySetInnerHTML` under a `null` CSP (#830), so markup in a
/// file became live script. Forcing the error arm is the only way to reach it
/// — a real syntect failure needs a corrupt syntax set or theme.
#[test]
fn highlight_content_failure_fallback_escapes_file_text() {
    let forced: Result<String, syntect::Error> =
        Err(syntect::Error::LoadingError(syntect::LoadingError::BadPath));
    let result = highlighted_or_escape("<script>alert(1)</script>\n", forced);

    assert!(
        !result.contains('<'),
        "a highlighting failure must not emit raw markup, got: {result}"
    );
    assert_eq!(result, "&lt;script&gt;alert(1)&lt;/script&gt;\n");
}

/// The escaped fallback renders identically to the per-line path's, so
/// markup stays inert whether or not the whole-string highlight succeeded.
#[test]
fn highlight_content_failure_fallback_matches_line_escape() {
    let forced: Result<String, syntect::Error> =
        Err(syntect::Error::LoadingError(syntect::LoadingError::BadPath));
    let content = "if a < b && c > d { \"<b>\" }";

    assert_eq!(highlighted_or_escape(content, forced), html_escape(content));
}

/// Guards the success arm too: syntect escapes the source text itself, so a
/// normal highlight must not carry a raw tag either. This is why only the
/// error arm needed the fix.
#[test]
fn highlight_content_success_path_leaves_markup_escaped_by_syntect() {
    let result = highlight_content("<script>alert(1)</script>\n", "attack.html");

    assert!(
        !result.contains("<script"),
        "syntect's own HTML must not carry a raw <script tag, got: {result}"
    );
    assert!(
        result.contains("<span"),
        "expected the escaped source text inside syntect's markup, got: {result}"
    );
}

#[test]
fn html_escape_neutralises_markup() {
    assert_eq!(
        html_escape("a < b && c > d"),
        "a &lt; b &amp;&amp; c &gt; d"
    );
}

/// The regex engine must compile every regex the parser can hand it
/// (issue #2757, bundle size).
///
/// syntect compiles a grammar's regexes lazily and `.expect()`s on failure
/// (`syntect-5.3.0/src/parsing/regex.rs:70`), so a pattern the engine
/// rejects is a panic in the diff view rather than a language that quietly
/// loses its highlighting. `fancy-regex` rejects backreferences, and the
/// shipped default syntax set contains patterns that use them, so this test
/// holds the engine to the contract syntect actually implements:
///
/// * a pattern **without** capture groups is compiled exactly as written, so
///   it must compile;
/// * a pattern **with** capture groups is compiled with its backreferences
///   substituted for the matched text (`MatchPattern::regex_with_refs`), so
///   the substituted form must compile — and its context must carry
///   `uses_backrefs`, which is what makes syntect substitute the pattern
///   instead of compiling it raw.
///
/// A grammar that breaks either rule fails here with the offending pattern
/// and its context, instead of panicking in the diff view.
///
/// The runtime contexts come from `SyntaxSet::into_builder`, which
/// reconstructs them from the same linked contexts the parser reads, so the
/// `uses_backrefs` flag checked here is the one the parser sees.
#[test]
fn every_runtime_regex_compiles_with_the_active_engine() {
    use syntect::parsing::syntax_definition::Pattern;
    use syntect::parsing::{Regex, SyntaxSet};

    let syntax_set = SyntaxSet::load_defaults_newlines();
    let mut patterns = 0usize;
    let mut capture_patterns = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for definition in syntax_set.into_builder().syntaxes() {
        for (context_name, context) in &definition.contexts {
            for pattern in &context.patterns {
                let Pattern::Match(match_pattern) = pattern else {
                    continue;
                };
                patterns += 1;
                let raw = match_pattern.regex.regex_str();

                if !match_pattern.has_captures {
                    // Compiled as written, on every line the grammar matches.
                    if let Some(error) = Regex::try_compile(raw) {
                        failures.push(format!(
                            "{} / {context_name}: {raw:?} does not compile: {error}",
                            definition.name
                        ));
                    }
                    continue;
                }

                capture_patterns += 1;
                if !context.uses_backrefs {
                    // syntect only substitutes a backreference when the level
                    // carries captures, and it decides that from this flag.
                    // Without it the raw pattern would be compiled, and the
                    // raw pattern is exactly what the engine rejects.
                    failures.push(format!(
                        "{} / {context_name}: {raw:?} captures a backreference but its context \
                         does not use backreferences, so syntect would compile it raw: {raw:?}",
                        definition.name
                    ));
                    continue;
                }
                if let Some(error) = Regex::try_compile(&substitute_backrefs(raw)) {
                    failures.push(format!(
                        "{} / {context_name}: {raw:?} does not compile once its backreference is \
                         substituted: {error}",
                        definition.name
                    ));
                }
            }
        }
    }

    assert!(
        patterns >= 1000,
        "the enumeration found only {patterns} patterns, so the check is vacuous"
    );
    assert!(
        capture_patterns > 0,
        "the shipped syntax set no longer has a backreference pattern, so the substitution path \
         is no longer exercised by this test"
    );
    assert!(
        failures.is_empty(),
        "the shipped regex engine cannot compile {} of the {patterns} patterns the parser can \
         hand it:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// syntect's own backreference substitution
/// (`syntax_definition.rs::substitute_backrefs_in_regex`), with the matched
/// text replaced by a placeholder.
///
/// syntect escapes the text it substitutes, so the result is always a literal;
/// a placeholder is a literal too, which is what the compile check needs.
fn substitute_backrefs(regex: &str) -> String {
    let mut out = String::with_capacity(regex.len());
    let mut last_was_escape = false;
    for character in regex.chars() {
        if last_was_escape && character.is_ascii_digit() {
            let value = character.to_digit(10).unwrap() as usize;
            out.push_str(&format!("<placeholder_{value}>"));
        } else if last_was_escape {
            out.push('\\');
            out.push(character);
        } else if character != '\\' {
            out.push(character);
        }
        last_was_escape = character == '\\' && !last_was_escape;
    }
    if last_was_escape {
        out.push('\\');
    }
    out
}
