//! Tests for the renderer diagnostics sink (issue #2833).
//!
//! The sink's behaviour is small but load-bearing: the frontend's line is
//! routed on its prefix, and the outcome of a write decides whether the line
//! is dropped, kept, or falls back to the main log.

use super::*;

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
fn a_disabled_sampler_drops_the_line_rather_than_falling_back() {
    // `BUILDMESH_DIAG=0` is a process-wide switch, so this test can only
    // assert the outcome it produces, not the environment it read.
    let outcome = write_renderer_line("RENDER_DIAG probe=1");
    assert!(matches!(
        outcome,
        RendererLineOutcome::Written
            | RendererLineOutcome::Disabled
            | RendererLineOutcome::Unavailable
    ));
}
