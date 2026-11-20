//! Tests for the renderer diagnostics sink (issue #2833).
//!
//! The sink's behaviour is small but load-bearing: the frontend's line is
//! routed on its prefix, and the outcome of a write decides whether the line
//! is dropped, kept, or falls back to the main log.

use super::*;
use crate::diagnostics::RotatingWriter;

/// A scratch path per test, so cargo's parallel runner cannot collide. The
/// PID is in the name too: the shard runner executes several binaries against
/// the same temp directory at once.
fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("bm-renderer-{}-{name}.log", std::process::id()))
}

#[test]
fn only_a_prefixed_line_is_a_renderer_health_line() {
    assert!(is_renderer_health_line("RENDER_DIAG age_ms=10 dom=3"));
    assert!(!is_renderer_health_line("spawn_timing: session=42"));
    assert!(!is_renderer_health_line(""));
    // The prefix must be at the start, not merely present: a frontend
    // message that quotes the marker in the middle is not a health line.
    assert!(!is_renderer_health_line("see RENDER_DIAG for details"));
}

#[test]
fn the_frontend_and_native_prefixes_agree() {
    // The frontend constant lives in
    // `src/lib/diagnostics/rendererHealth.ts`. The two files cannot
    // share a value across the language boundary, so the agreement is
    // pinned here against the literal the frontend uses.
    let frontend_prefix = "RENDER_DIAG";
    assert_eq!(RENDER_HEALTH_PREFIX, frontend_prefix);
}

#[test]
fn a_line_without_a_sink_is_unavailable() {
    // No sink means the caller falls back to the main log rather than
    // dropping the line.
    assert_eq!(
        route_renderer_line(None, "RENDER_DIAG probe=1"),
        RendererLineOutcome::Unavailable
    );
}

#[test]
fn a_line_with_a_sink_is_written_to_the_file() {
    let path = scratch("written");
    let _ = std::fs::remove_file(&path);
    let mut writer =
        RotatingWriter::with_limits(path.clone(), 1024 * 1024, 1).expect("the scratch sink opens");

    let outcome = route_renderer_line(Some(&mut writer), "RENDER_DIAG probe=1");

    assert_eq!(outcome, RendererLineOutcome::Written);
    let contents = std::fs::read_to_string(&path).expect("the line is on disk");
    assert!(
        contents.contains("RENDER_DIAG probe=1"),
        "the written file holds the line: {contents:?}"
    );
    let _ = std::fs::remove_file(&path);
}
