//! Bridge that lets the frontend forward `console.*`, `window.error`, and
//! `unhandledrejection` into the same tracing pipeline as the backend so they
//! land in `buildmesh.log`. Without this the WebView2 console is invisible to
//! anyone debugging headlessly.
//!
//! The command is the security boundary for that bridge. Whatever the webview
//! sends is masked with [`SecretScrubber::scrub_for_persistence`] before it is
//! handed to tracing, then capped. The log writer scrubs again on the way to
//! disk, so a subscriber line that never passed through this command is held
//! to the same rule.

use crate::secret_scrubber::SecretScrubber;
use tauri::command;

/// Persisted frontend lines stay at the historical 8 KiB cap. Scrubbing runs
/// on the whole input first, so a secret is masked before the cap can split it.
const PERSISTED_CAP: usize = 8192;

/// Inputs larger than this are dropped whole. Truncating first would leave a
/// prefix of a secret that the masker can no longer recognize, and a hostile
/// webview can hand the command a multi-megabyte string.
const SCRUB_INPUT_CAP: usize = 64 * 1024;

#[command]
pub async fn log_frontend(level: String, message: String) {
    let message = prepare_frontend_message(&message);
    match level.as_str() {
        "error" => tracing::error!(target: "frontend", "{}", message),
        "warn" => tracing::warn!(target: "frontend", "{}", message),
        "info" => tracing::info!(target: "frontend", "{}", message),
        "debug" => tracing::debug!(target: "frontend", "{}", message),
        other => {
            let other = prepare_frontend_message(other);
            tracing::info!(target: "frontend", "[level={}] {}", other, message);
        }
    }
}

/// The string `log_frontend` passes to tracing.
///
/// Order is load-bearing: refuse an oversized payload, mask secrets in what
/// remains, then cap the masked text. Capping before masking can cut a token
/// in half so the masker misses it and the surviving prefix is written.
pub(crate) fn prepare_frontend_message(message: &str) -> String {
    if message.len() > SCRUB_INPUT_CAP {
        return format!("<frontend log omitted: {} bytes>", message.len());
    }
    let scrubbed = SecretScrubber::scrub_for_persistence(message);
    truncate_log(&scrubbed, PERSISTED_CAP)
}

fn truncate_log(message: &str, cap: usize) -> String {
    if message.len() <= cap {
        return message.to_string();
    }
    // The marker is charged to the cap, so a persisted line stays inside the
    // documented budget instead of overrunning it by the marker's length. The
    // marker built from the whole length is an upper bound on the one built
    // from the truncated remainder, so the reservation cannot come up short.
    let reservation = format!("…<truncated {} bytes>", message.len());
    let end = message.floor_char_boundary(cap.saturating_sub(reservation.len()));
    format!(
        "{}…<truncated {} bytes>",
        &message[..end],
        message.len() - end
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_message() -> (String, Vec<&'static str>) {
        let provider = "sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB";
        let root = "0123456789abcdef0123456789abcdef";
        let device = "fedcba9876543210fedcba9876543210";
        let pairing = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let bearer = "abc123def456ghi789";
        let pem_body = "MIIEpAIBAAKCAQEA";
        let nested = "plain-secret-no-shape";
        let message = format!(
            "spawn_timing: session=42 checkpoint=xterm_mount elapsed=17ms\n\
             Error: provider rejected {provider}\n\
             at login (app.ts:10)\n\
             {{\"status\":500,\"author\":\"octocat\",\"auth\":{{\"rootToken\":\"{root}\",\"deviceToken\":\"{device}\"}},\"api_keys\":[\"{nested}\"]}}\n\
             Authorization: Bearer {bearer}\n\
             https://127.0.0.1/#pair={pairing}\n\
             ticket={pairing}\n\
             -----BEGIN RSA PRIVATE KEY-----\n{pem_body}\nabc/def+ghi=\n-----END RSA PRIVATE KEY-----"
        );
        (
            message,
            vec![provider, root, device, pairing, bearer, pem_body, nested],
        )
    }

    #[test]
    fn prepared_frontend_message_keeps_diagnostics_and_drops_secrets() {
        let (message, secrets) = sample_message();
        let prepared = prepare_frontend_message(&message);
        for secret in secrets {
            assert!(!prepared.contains(secret), "leaked {secret} in {prepared}");
        }
        assert!(
            prepared.contains("spawn_timing: session=42 checkpoint=xterm_mount elapsed=17ms"),
            "benign timing text must survive: {prepared}"
        );
        assert!(prepared.contains("at login (app.ts:10)"), "{prepared}");
        assert!(
            prepared.contains("octocat"),
            "a non-secret field must survive: {prepared}"
        );
        assert!(prepared.contains("500"), "{prepared}");
        assert!(prepared.contains("[REDACTED]"), "{prepared}");
        assert!(
            !prepared.contains("BEGIN RSA PRIVATE KEY"),
            "the private key block must be masked whole: {prepared}"
        );
    }

    #[test]
    fn oversized_frontend_message_is_omitted_without_its_secret() {
        let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";
        let message = secret.repeat(5_000);
        let prepared = prepare_frontend_message(&message);
        assert!(!prepared.contains("ghp_"), "{prepared}");
        assert!(prepared.contains("omitted"), "{prepared}");
        assert!(prepared.contains(&message.len().to_string()));
    }

    #[test]
    fn long_frontend_message_caps_on_a_char_boundary_after_masking() {
        let secret = "sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB";
        let mut message = format!("{secret} ");
        while message.len() < 9_000 {
            message.push('你');
        }
        message.push_str(" tail-marker");
        let prepared = prepare_frontend_message(&message);
        assert!(!prepared.contains(secret), "{prepared}");
        assert!(prepared.contains("[REDACTED]"), "{prepared}");
        assert!(prepared.contains("truncated"), "{prepared}");
        assert!(!prepared.contains("tail-marker"), "{prepared}");
    }

    #[test]
    fn the_truncation_marker_counts_against_the_cap() {
        // The marker used to be appended on top of the cap, so a persisted line
        // could exceed the budget the doc states by the marker's length.
        for len in [PERSISTED_CAP + 1, 9_000, 20_000] {
            let prepared = prepare_frontend_message(&"x".repeat(len));
            assert!(
                prepared.len() <= PERSISTED_CAP,
                "{len} bytes produced {} bytes, over the {PERSISTED_CAP}-byte cap",
                prepared.len()
            );
            assert!(prepared.contains("truncated"), "{len}");
        }
    }

    #[test]
    fn unknown_level_is_masked_before_it_is_interpolated() {
        let raw = "token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";
        let prepared = prepare_frontend_message(raw);
        assert!(
            !prepared.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"),
            "{prepared}"
        );
        assert!(prepared.contains("token="), "{prepared}");
    }
}
