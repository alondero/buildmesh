//! Native Windows caption-button affordances for the bespoke title bar.
//!
//! Windows 11 only shows the Snap Layouts flyout to a window whose
//! `WM_NCHITTEST` answers `HTMAXBUTTON` for the maximise button's rectangle.
//! Our page lives in a WebView2 child HWND covering the entire client area, so
//! *that* child answers the hit test and the Tauri window's procedure is never
//! consulted — no DOM API, CSS property, or Tauri config value can produce the
//! return value. Upstream is a dead end (tauri#4531 is `status: upstream`,
//! winit#3884 is open with no workaround), because both describe the framework
//! shipping a first-class API, not what an application can do.
//!
//! The working technique — the same one Windows Terminal, VS Code, Electron,
//! and five independent Tauri plugins use — is a transparent native child HWND
//! parked over the maximise button whose window procedure returns
//! `HTMAXBUTTON` unconditionally. It never paints, so the design shows through.
//!
//! That overlay owns the mouse in its rectangle, which is the price: the
//! button's DOM `:hover` and `onClick` stop firing, so the overlay emits events
//! and the frontend drives hover, press, and click from them. Keyboard
//! activation is unaffected (it still runs the DOM handler). See
//! `docs/adr/0035-native-windows-caption-button-affordances.md`.
//!
//! Geometry is deliberately **measured from the DOM** by the frontend rather
//! than hardcoded here. The overlay's position depends on the button's real box,
//! and a constant that drifts from a Tailwind class would silently kill the
//! flyout with no other symptom — the single most expensive failure mode in
//! this problem space.

use parking_lot::Mutex;

/// The maximise button's box as measured from the DOM, in logical (CSS) pixels.
///
/// Logical rather than physical because the frontend cannot know the window's
/// DPI scale and Rust can (`GetDpiForWindow`), so the conversion happens on the
/// side that actually has the information.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaximizeMetrics {
    /// Distance from the right edge of the client area to the button's right
    /// edge. Stored as an inset, not an absolute x, so the overlay tracks a live
    /// resize drag without a round trip to the frontend.
    pub right_inset: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// Last metrics reported by the frontend; `None` until React has mounted and
/// measured. That is what makes installation lazy — there is nothing to do on
/// macOS or Linux, and no chance of a mispositioned overlay before layout
/// settles. `snap_overlay` reads this both on install and on every `WM_SIZE`.
static MAXIMIZE_METRICS: Mutex<Option<MaximizeMetrics>> = Mutex::new(None);

#[cfg(target_os = "windows")]
mod snap_overlay;

/// Scale a logical-pixel measurement to physical pixels for `dpi`.
fn scale(value: f64, dpi: u32) -> i32 {
    (value * dpi as f64 / 96.0).round() as i32
}

/// The overlay's physical-pixel rect `(x, y, width, height)` inside a client
/// area `client_width` wide.
///
/// Pure, so the arithmetic is testable on any host. The Win32 half cannot be
/// unit-tested and the flyout cannot be captured by a screenshot (it is a
/// separate OS window owned by the shell), which makes this the one part of the
/// geometry that gets a real regression test.
fn overlay_rect(metrics: &MaximizeMetrics, client_width: i32, dpi: u32) -> (i32, i32, i32, i32) {
    // A zero extent would leave the overlay nothing to hit-test, so a
    // degenerate measurement (taken before layout settles) still yields a
    // 1px window rather than a broken one.
    let width = scale(metrics.width, dpi).max(1);
    let height = scale(metrics.height, dpi).max(1);
    // Floored at the client's left edge. A client narrower than the caption
    // cluster — a minimized window reports an empty client area — would
    // otherwise yield a negative x. `snap_overlay::reposition` hides the overlay
    // outright in that state, so this is the arithmetic's own floor rather than
    // the defence that matters; but a position the parent cannot contain is
    // never a useful one, and the floor is the part that can be unit-tested off
    // Windows.
    let x = (client_width - scale(metrics.right_inset, dpi) - width).max(0);
    (x, scale(metrics.top, dpi), width, height)
}

/// Record the maximise button's measured box and install/refresh the overlay.
#[cfg(target_os = "windows")]
pub fn set_maximize_metrics(
    window: &tauri::WebviewWindow,
    metrics: MaximizeMetrics,
) -> Result<(), String> {
    *MAXIMIZE_METRICS.lock() = Some(metrics);
    snap_overlay::install_or_update(window).map_err(|e| e.to_string())
}

/// No-op off Windows: Snap Layouts and `HTMAXBUTTON` are Windows shell features.
#[cfg(not(target_os = "windows"))]
pub fn set_maximize_metrics(
    _window: &tauri::WebviewWindow,
    _metrics: MaximizeMetrics,
) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three 46px backplates flush to the right edge, so the maximise button's
    /// right edge sits 46px in from the right of the client area.
    fn metrics() -> MaximizeMetrics {
        MaximizeMetrics {
            right_inset: 46.0,
            top: 0.0,
            width: 46.0,
            height: 32.0,
        }
    }

    #[test]
    fn at_100_percent_dpi_lands_on_the_maximise_button() {
        // A 138px cluster at the right of a 1280px client area starts at 1142
        // (minimise); the maximise button is the next 46px from 1188.
        assert_eq!(overlay_rect(&metrics(), 1280, 96), (1188, 0, 46, 32));
    }

    #[test]
    fn scales_with_the_window_dpi() {
        // 150%: 46 → 69 physical px, and the inset scales with it.
        assert_eq!(overlay_rect(&metrics(), 1920, 144), (1782, 0, 69, 48));
    }

    #[test]
    fn tracks_the_client_width_through_a_resize() {
        // The stored inset is constant, so a resize only moves x.
        let (narrow, _, _, _) = overlay_rect(&metrics(), 900, 96);
        let (wide, _, _, _) = overlay_rect(&metrics(), 2560, 96);
        assert_eq!(narrow, 900 - 92);
        assert_eq!(wide, 2560 - 92);
    }

    #[test]
    fn a_degenerate_measurement_still_yields_a_hittable_rect() {
        let degenerate = MaximizeMetrics {
            right_inset: 0.0,
            top: 0.0,
            width: 0.0,
            height: 0.0,
        };
        let (_, _, width, height) = overlay_rect(&degenerate, 1280, 96);
        assert_eq!((width, height), (1, 1));
    }

    #[test]
    fn a_client_narrower_than_the_cluster_never_yields_a_negative_x() {
        // A minimized window reports an empty client area; a client narrower than
        // the maximise backplate's own 92px of run-up is the same arithmetic case
        // (its 46px width plus the 46px Close backplate to its right). 92 is the
        // boundary — exactly 0 — so the floor starts biting above it.
        // `reposition` hides the overlay in that state, so this pins only the
        // floor: the maths must not emit a position the parent cannot contain.
        for client_width in [0, 40, 91, 92] {
            let (x, _, width, _) = overlay_rect(&metrics(), client_width, 96);
            assert_eq!(x, 0, "client_width {client_width}");
            assert_eq!(width, 46, "client_width {client_width}");
        }
        // Above the floor the right-inset is honoured exactly: 200 − 46 − 46.
        assert_eq!(overlay_rect(&metrics(), 200, 96).0, 108);
    }

    #[test]
    fn top_offset_is_preserved() {
        // The bar is taller than the native 32px caption strip, so the measured
        // box may not start at y=0; the overlay must follow it rather than
        // assuming a top-anchored strip.
        let offset = MaximizeMetrics {
            top: 6.0,
            height: 33.5,
            ..metrics()
        };
        let (_, y, _, height) = overlay_rect(&offset, 1280, 96);
        assert_eq!((y, height), (6, 34));
    }
}
