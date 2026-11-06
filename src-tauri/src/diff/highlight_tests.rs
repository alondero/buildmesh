//! The highlighting suite — moved with the highlighting code (issue #2527).
//!
//! The test names are the ones the suite had inside `commands/diff.rs`; the
//! module they live in moved, the assertions did not.

use crate::diff::highlight::{
    ext_for_path, highlighted_or_escape, highlight_content, html_escape,
};

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
