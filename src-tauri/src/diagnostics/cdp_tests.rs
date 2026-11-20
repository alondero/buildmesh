//! Tests for the opt-in DevTools port resolution (issue #2836).
//!
//! The acceptance criterion this module has to keep is the *negative* one:
//! with the preference off, no port is open. That is a statement about the
//! browser arguments this process starts WebView2 with, so it is asserted
//! here on the arguments themselves rather than on a running app.

use super::*;

/// With no port asked for, the browser arguments stay as they were.
///
/// This is the "preference off, no port is open" case: no argument is added,
/// so WebView2 starts without a debugging port.
#[test]
fn no_port_adds_no_browser_argument() {
    let resolution = resolve(None, None, None);
    assert_eq!(resolution.effective.port, None);
    assert_eq!(resolution.effective.source, PortSource::Off);
    assert_eq!(resolution.browser_args, None);
}

/// An existing argument string survives a run with the port off.
#[test]
fn no_port_leaves_existing_arguments_alone() {
    let resolution = resolve(None, None, Some("--disable-gpu"));
    assert_eq!(resolution.effective.port, None);
    assert_eq!(resolution.effective.source, PortSource::Off);
    assert_eq!(resolution.browser_args.as_deref(), Some("--disable-gpu"));
}

/// The preference opens the port, and the argument names the port.
#[test]
fn the_preference_opens_the_port() {
    let resolution = resolve(Some(9222), None, None);
    assert_eq!(resolution.effective.port, Some(9222));
    assert_eq!(resolution.effective.source, PortSource::Preference);
    assert_eq!(
        resolution.browser_args.as_deref(),
        Some("--remote-debugging-port=9222")
    );
}

/// The environment override wins over the preference: it is the more
/// specific act for this launch.
#[test]
fn the_environment_override_wins_over_the_preference() {
    let resolution = resolve(Some(9222), Some(9333), None);
    assert_eq!(resolution.effective.port, Some(9333));
    assert_eq!(resolution.effective.source, PortSource::Environment);
    assert_eq!(
        resolution.browser_args.as_deref(),
        Some("--remote-debugging-port=9333")
    );
}

/// A launcher that already opened a port keeps it.
///
/// Two `--remote-debugging-port` arguments in one string would leave
/// Chromium using the first one, so the effective port has to be the one
/// that is already there — and the app must not claim a different number.
#[test]
fn an_existing_port_wins_and_is_not_duplicated() {
    let resolution = resolve(Some(9222), None, Some("--remote-debugging-port=9223"));
    assert_eq!(resolution.effective.port, Some(9223));
    assert_eq!(resolution.effective.source, PortSource::Environment);
    assert_eq!(
        resolution.browser_args.as_deref(),
        Some("--remote-debugging-port=9223")
    );
}

/// The preference is appended to whatever the launcher already asked for.
#[test]
fn the_preference_is_appended_to_existing_arguments() {
    let resolution = resolve(Some(9222), None, Some("--disable-gpu"));
    assert_eq!(
        resolution.browser_args.as_deref(),
        Some("--disable-gpu --remote-debugging-port=9222")
    );
}

/// A blank or non-numeric value is not a port.
#[test]
fn an_unusable_value_is_not_a_port() {
    assert_eq!(parse_port(""), None);
    assert_eq!(parse_port("   "), None);
    assert_eq!(parse_port("not-a-port"), None);
    assert_eq!(parse_port("-1"), None);
    assert_eq!(parse_port("70000"), None);
}

/// A privileged port is not a port this module opens.
#[test]
fn a_privileged_port_is_refused() {
    assert_eq!(parse_port("80"), None);
    assert_eq!(parse_port("1023"), None);
    assert_eq!(parse_port("1024"), Some(1024));
    assert_eq!(parse_port(" 9222 "), Some(9222));
}

/// A port outside the usable range is not a port, whoever asked for it.
///
/// The resolver is total: the callers filter their inputs too, but a value
/// that slipped through must not become a `--remote-debugging-port`
/// argument.
#[test]
fn an_out_of_range_port_does_not_reach_the_browser_arguments() {
    let resolution = resolve(Some(80), None, None);
    assert_eq!(resolution.effective.port, None);
    assert_eq!(resolution.effective.source, PortSource::Off);
    assert_eq!(resolution.browser_args, None);

    // An unusable environment override falls through to the usable
    // preference rather than to "off".
    let resolution = resolve(Some(9222), Some(80), None);
    assert_eq!(resolution.effective.port, Some(9222));
    assert_eq!(resolution.effective.source, PortSource::Preference);
}

/// The port an argument string already opens is read back verbatim.
///
/// The range check is deliberately not applied here: a launcher that asked
/// for port 80 got port 80, and the app has to report what is open.
#[test]
fn an_existing_port_is_read_back_as_written() {
    assert_eq!(
        port_in_browser_args("--remote-debugging-port=9223"),
        Some(9223)
    );
    assert_eq!(port_in_browser_args("--disable-gpu"), None);
    assert_eq!(port_in_browser_args(""), None);
    assert_eq!(
        port_in_browser_args("--remote-debugging-port=80"),
        Some(80),
        "a port the launcher actually opened is reported as it is"
    );
}

/// The process reports no port before the startup path has resolved one.
#[test]
fn an_unresolved_process_reports_no_port() {
    // The `OnceLock` is only filled by `apply_startup_port`, which a unit
    // test does not call — so this reads the "not resolved" answer.
    let effective = effective();
    if EFFECTIVE.get().is_none() {
        assert_eq!(effective.port, None);
        assert_eq!(effective.source, PortSource::Off);
    }
}
