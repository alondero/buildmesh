//! Syntax highlighting and the HTML escaping rules for the diff view.
//!
//! The diff view injects server-rendered strings with
//! `dangerouslySetInnerHTML` (`src/components/Diff/Diff.tsx`) and the CSP is
//! `null` (#830), so **every** arm that yields HTML must escape the text
//! itself (#2152). syntect escapes its own success output, so only the
//! fallback arms need escaping here — but they need it.
//!
//! The themes and the syntax set are process-wide and loaded once: they are
//! the expensive half of a highlight, and a diff of a busy worktree runs three
//! highlight passes per hunk.

use crate::models::DiffLine;
use once_cell::sync::Lazy;
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;

#[cfg(test)]
#[path = "highlight_tests.rs"]
mod tests;

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

/// Highlight a string with syntect, returning HTML
pub fn highlight_content(content: &str, path: &str) -> String {
    let ext = ext_for_path(path);
    let syntax = SYNTAX_SET
        .find_syntax_by_extension(&ext)
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

    let theme = &THEME_SET.themes["base16-ocean.dark"];

    highlighted_or_escape(
        content,
        highlighted_html_for_string(content, &SYNTAX_SET, syntax, theme),
    )
}

/// Resolve a whole-string highlight result, rendering the file text as text
/// when syntect failed.
///
/// `old_highlighted` / `new_highlighted` are server-generated HTML on the
/// `DiffHunk` wire type, consumed by a diff view that injects sibling
/// highlight fields with `dangerouslySetInnerHTML` under a `null` CSP (#830).
/// So this arm must escape: returning raw file contents turns markup in any
/// opened file into live script. The success arm is safe alone — syntect
/// escapes its own output — which is why only the error arm needs escaping.
/// Same helper as the per-line fallback (#2152).
pub fn highlighted_or_escape(content: &str, highlighted: Result<String, syntect::Error>) -> String {
    highlighted.unwrap_or_else(|_| html_escape(content))
}

/// Minimal HTML escape for the highlighting fallback path, so a line we
/// couldn't tokenise still renders as text rather than injecting markup.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Highlight each line of a hunk independently, returning inline HTML aligned
/// 1:1 with `group`. Two stateful highlighters (one per side) are advanced in
/// lockstep so multi-line constructs — block comments, multi-line strings —
/// stay coloured correctly: a context line advances both sides, an add advances
/// only the new side, a remove only the old side.
pub fn highlight_group_lines(group: &[DiffLine], path: &str) -> Vec<String> {
    use syntect::easy::HighlightLines;
    use syntect::html::{styled_line_to_highlighted_html, IncludeBackground};

    let ext = ext_for_path(path);
    let syntax = SYNTAX_SET
        .find_syntax_by_extension(&ext)
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
    let theme = &THEME_SET.themes["base16-ocean.dark"];

    let mut old_h = HighlightLines::new(syntax, theme);
    let mut new_h = HighlightLines::new(syntax, theme);

    let render = |line: &DiffLine, ranges: Result<Vec<(syntect::highlighting::Style, &str)>, _>| {
        ranges
            .ok()
            .and_then(|r| styled_line_to_highlighted_html(&r, IncludeBackground::No).ok())
            .unwrap_or_else(|| html_escape(&line.content))
    };

    group
        .iter()
        .map(|line| {
            // HighlightLines expects a trailing newline to terminate the line;
            // the stray `\n` lands inside a span and collapses harmlessly in
            // the single-line row the frontend renders.
            let with_nl = format!("{}\n", line.content);
            match line.line_type.as_str() {
                "add" => render(line, new_h.highlight_line(&with_nl, &SYNTAX_SET)),
                "remove" => render(line, old_h.highlight_line(&with_nl, &SYNTAX_SET)),
                _ => {
                    // context: keep both sides' parser state in sync, emit new.
                    let _ = old_h.highlight_line(&with_nl, &SYNTAX_SET);
                    render(line, new_h.highlight_line(&with_nl, &SYNTAX_SET))
                }
            }
        })
        .collect()
}

/// Get file extension from path
pub fn ext_for_path(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}
