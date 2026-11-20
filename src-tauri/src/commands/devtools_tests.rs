//! Tests for the DevTools status command's wire shape (issue #2836).
//!
//! The frontend reads this payload to decide whether to warn that a port is
//! open, so the *keys* and the *source values* are a contract, not an
//! implementation detail.

use super::*;

/// The payload is flat and snake_case, and the port is null when none is open.
#[test]
fn the_status_serializes_with_a_null_port_when_off() {
    let status = DevtoolsStatus {
        port: None,
        source: crate::diagnostics::cdp::PortSource::Off,
    };
    let json = serde_json::to_string(&status).expect("the status serializes");
    assert_eq!(json, r#"{"port":null,"source":"off"}"#);
}

/// The source names where the port came from, in the vocabulary the frontend
/// reads.
#[test]
fn the_source_values_are_stable() {
    let cases = [
        (crate::diagnostics::cdp::PortSource::Off, "off"),
        (
            crate::diagnostics::cdp::PortSource::Preference,
            "preference",
        ),
        (
            crate::diagnostics::cdp::PortSource::Environment,
            "environment",
        ),
    ];
    for (source, expected) in cases {
        let json = serde_json::to_string(&source).expect("the source serializes");
        assert_eq!(json, format!("\"{expected}\""));
    }
}

/// A process that resolved no port reports no port.
#[test]
fn an_unresolved_process_reports_no_port() {
    let status = get_devtools_status();
    if crate::diagnostics::cdp::effective().port.is_none() {
        assert_eq!(status.port, None);
        assert_eq!(status.source, crate::diagnostics::cdp::PortSource::Off);
    }
}
