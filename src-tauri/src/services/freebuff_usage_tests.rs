use super::*;
use crate::services::usage::spawn_loopback;
use std::path::Path;

/// Helper: write a `credentials.json` with the standard three fields
/// into a tempdir. Mirrors the cursor / commandcode test fixtures.
fn write_freebuff_credentials(dir: &Path, body: &str) -> PathBuf {
    let cfg_dir = dir.join(".config").join("manicode");
    std::fs::create_dir_all(&cfg_dir).expect("mkdir");
    let path = cfg_dir.join("credentials.json");
    std::fs::write(&path, body).expect("write credentials.json");
    path
}

// ── parse_freebuff_response — issue #1438 ────────────────────────────

// ── clamp_error_body — review item #4A ────────────────────────────────

#[test]
fn clamp_error_body_returns_placeholder_for_empty_input() {
    // Empty body must not produce `format!("API error {code}: ")` —
    // the `<UsagePanel>` would render a trailing colon and look
    // broken. The placeholder matches the original `unavailable`
    // copy.
    assert_eq!(clamp_error_body(""), "usage endpoint failed");
    assert_eq!(clamp_error_body("   \n  \t  "), "usage endpoint failed");
}

#[test]
fn clamp_error_body_collapses_whitespace_to_single_line() {
    // Cloudflare / nginx emit HTML error pages with newlines and
    // runs of spaces. Collapse to a single line so the UI's
    // `<UsagePanel>` card doesn't break its layout.
    let body = "<html>\n  <body><h1>502 Bad Gateway</h1></body>\n</html>";
    let result = clamp_error_body(body);
    assert!(
        !result.contains('\n'),
        "must collapse newlines, got {result:?}"
    );
    assert!(result.starts_with("<html>"));
    assert!(result.contains("502 Bad Gateway"));
}

#[test]
fn clamp_error_body_truncates_long_bodies_with_ellipsis() {
    // A 15 KB raw upstream stack trace must NOT reach the
    // `<UsagePanel>`. Truncation adds a `…` suffix so the UI
    // renders one trailing char instead of cutting mid-glyph.
    let long_body = "a".repeat(ERROR_BODY_MAX_CHARS * 10);
    let result = clamp_error_body(&long_body);
    let char_count = result.chars().count();
    assert!(
        char_count <= ERROR_BODY_MAX_CHARS + 1,
        "got {char_count} chars"
    );
    assert!(result.ends_with('…'), "expected `…` suffix, got {result:?}");
}

#[test]
fn clamp_error_body_preserves_short_bodies_verbatim() {
    // A short body (under the cap) must NOT be truncated —
    // appending `…` to a passing string would confuse the UI.
    let body = "backend unavailable";
    assert_eq!(clamp_error_body(body), "backend unavailable");
}

#[test]
fn clamp_error_body_handles_multibyte_utf8_by_char_count() {
    // Truncation must be by *char*, not byte — a body with CJK
    // glyphs would otherwise panic on a mid-codepoint byte slice.
    // This pins the `chars().take(...)` choice.
    let body = "测".repeat(ERROR_BODY_MAX_CHARS * 2);
    let result = clamp_error_body(&body);
    let char_count = result.chars().count();
    assert!(char_count <= ERROR_BODY_MAX_CHARS + 1);
    // The body has no spaces / newlines, so it doesn't collapse
    // and truncation kicks in directly.
    assert!(result.ends_with('…'));
}

// ── freebuff_credential_paths — issue #1438 + review item #4E ────────

#[test]
fn freebuff_credential_paths_includes_xdg_default() {
    // The XDG-style path is always present regardless of platform
    // — it's the primary candidate the upstream CLI writes to.
    //
    // `freebuff_credential_paths` reads the ambient environment, and the
    // process environment is shared state: the sibling override test
    // exports `XDG_CONFIG_HOME` while holding `ENV_LOCK`. A reader that
    // skipped the lock therefore saw that test's tempdir as candidate #0,
    // and failed only under parallel libtest (issue #2285). Hold the
    // crate-wide lock and pin the variable to "unset" for the read, so the
    // assertion sees the default rather than whichever writer happened to
    // run alongside it.
    let _env = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let paths = crate::env::with_env_vars(&[("XDG_CONFIG_HOME", None)], freebuff_credential_paths);
    assert!(!paths.is_empty(), "must have at least the XDG candidate");
    let first = paths[0].to_string_lossy();
    assert!(
        first.ends_with(".config/manicode/credentials.json")
            || first.ends_with(".config\\manicode\\credentials.json"),
        "first candidate must be the XDG-style credentials.json, got {first}"
    );
}

#[test]
fn freebuff_credential_paths_respects_xdg_config_home_override() {
    // XDG_CONFIG_HOME overrides the default — a user who sets
    // `XDG_CONFIG_HOME=/custom/cfg` must see their directory in
    // the candidate list ahead of `<home>/.config`. We use a
    // tempdir so the test is hermetic regardless of the caller's
    // actual XDG_CONFIG_HOME (production code reads it via
    // `env::var_os`).
    // The process environment is shared state: this mutation has to
    // exclude every other test that mutates it *and* every test that
    // snapshots it, so it takes the one crate-wide lock (issue #2048).
    // `with_env_vars` restores the previous value through
    // `catch_unwind`, so a failing assertion cannot leave this tempdir
    // exported to the rest of the suite.
    let _env = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::env::with_env_vars(
        &[("XDG_CONFIG_HOME", Some(dir.path().as_os_str()))],
        freebuff_credential_paths,
    );
    let first = paths[0].to_string_lossy();
    assert!(
        first.starts_with(dir.path().to_string_lossy().as_ref()),
        "first candidate must honour XDG_CONFIG_HOME, got {first}"
    );
}

// ── read_freebuff_credentials — issue #1438 + review item #4C ────────

// ── freebuff_usage_with — issue #1438 ────────────────────────────────

/// Helper used by the loopback tests that need exactly one
/// candidate file.
fn one_candidate(path: &std::path::Path) -> Vec<PathBuf> {
    vec![path.to_path_buf()]
}

#[test]
fn freebuff_usage_no_credential_returns_no_credential() {
    // No candidate path that exists → `NoCredential` so the gate
    // drops the row rather than rendering a blank gauge. The
    // loopback server below would fail loudly if hit — no request
    // may fire without a credential.
    let dir = tempfile::tempdir().unwrap();
    let missing = vec![dir.path().join("nope").join("credentials.json")];
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(204));
    });
    match freebuff_usage_with(&missing, &format!("http://127.0.0.1:{port}/usage")) {
        UsageOutcome::NoCredential { hint } => {
            assert!(
                hint.contains("No credential"),
                "expected No credential hint, got {hint:?}"
            );
        }
        other => panic!("expected NoCredential outcome, got: {other:?}"),
    }
}

#[test]
fn freebuff_usage_401_returns_rejected_with_relogin_copy() {
    // 401 (Unauthorized) signals a bad credential — surface as
    // `Rejected` with a copy that points the user at
    // `freebuff login`, matching the kimi_usage 401/403 branch
    // (`Rejected` so the UI shows the re-enter affordance, not
    // a generic failure card).
    let dir = tempfile::tempdir().unwrap();
    let path = write_freebuff_credentials(
        dir.path(),
        r#"{
            "authToken": "fb_revoked_token",
            "fingerprintId": "fb_fp",
            "email": "user@example.com"
        }"#,
    );
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(401));
    });
    match freebuff_usage_with(
        &one_candidate(&path),
        &format!("http://127.0.0.1:{port}/usage"),
    ) {
        UsageOutcome::Rejected { hint } => {
            assert!(
                hint.contains("freebuff login"),
                "401 prompt must reference 'freebuff login', got {hint:?}"
            );
        }
        other => panic!("expected Rejected outcome, got: {other:?}"),
    }
}

#[test]
fn freebuff_usage_403_with_unparseable_body_returns_unavailable() {
    // 403 with an unparseable body (HTML error page from a WAF
    // / CDN, or an empty body) MUST NOT reject the credential --
    // doing so would wipe their login state on a Cloudflare
    // block, which is the wrong UX (PR #1443 review round-4
    // item #2). The fetcher treats unparseable 403 as
    // `Unavailable`.
    let dir = tempfile::tempdir().unwrap();
    let path = write_freebuff_credentials(
        dir.path(),
        r#"{
            "authToken": "fb_token",
            "fingerprintId": "fb_fp",
            "email": "user@example.com"
        }"#,
    );
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(403));
    });
    match freebuff_usage_with(
        &one_candidate(&path),
        &format!("http://127.0.0.1:{port}/usage"),
    ) {
        UsageOutcome::Unavailable { reason } => {
            assert_eq!(reason, "Access forbidden (HTTP 403)");
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

#[test]
fn freebuff_usage_403_with_country_blocked_body_returns_unavailable() {
    // 403 with the documented `{"status":"country_blocked"}` body
    // -- the user IS logged in (token valid), just not available
    // in this region. Surface as `Unavailable`, not `Rejected`.
    let dir = tempfile::tempdir().unwrap();
    let path = write_freebuff_credentials(
        dir.path(),
        r#"{
            "authToken": "fb_token",
            "fingerprintId": "fb_fp",
            "email": "user@example.com"
        }"#,
    );
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(
            tiny_http::Response::from_string(r#"{"status":"country_blocked"}"#)
                .with_status_code(403),
        );
    });
    match freebuff_usage_with(
        &one_candidate(&path),
        &format!("http://127.0.0.1:{port}/usage"),
    ) {
        UsageOutcome::Unavailable { reason } => {
            assert_eq!(reason, "Freebuff is not available in this region");
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

#[test]
fn freebuff_usage_403_with_banned_body_returns_rejected() {
    // 403 with a valid JSON `{"status":"banned"}` body -- the
    // upstream IS actively informing us the account is suspended,
    // NOT a WAF block (a Cloudflare HTML block fails JSON parsing
    // and falls into the unparseable 403 arm). Log out so the
    // user sees the suspension and re-authenticates -- mirror the
    // 401 + banned behaviour. PR #1443 round-6 review item #4.
    let dir = tempfile::tempdir().unwrap();
    let path = write_freebuff_credentials(
        dir.path(),
        r#"{
            "authToken": "fb_token",
            "fingerprintId": "fb_fp",
            "email": "user@example.com"
        }"#,
    );
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(
            tiny_http::Response::from_string(r#"{"status":"banned"}"#).with_status_code(403),
        );
    });
    match freebuff_usage_with(
        &one_candidate(&path),
        &format!("http://127.0.0.1:{port}/usage"),
    ) {
        UsageOutcome::Rejected { hint } => {
            assert_eq!(hint, "Freebuff account suspended");
        }
        other => panic!("expected Rejected outcome, got: {other:?}"),
    }
}

#[test]
fn freebuff_usage_404_with_html_body_surfaces_unavailable() {
    // Generic 404 (HTML page from a retired route or missing
    // reverse-proxy entry) must surface as `unavailable`, NOT as
    // a logged-in "no active session" card. PR #1443 review
    // round-4 item #4.
    let dir = tempfile::tempdir().unwrap();
    let path = write_freebuff_credentials(
        dir.path(),
        r#"{
            "authToken": "fb_token",
            "fingerprintId": "fb_fp",
            "email": "user@example.com"
        }"#,
    );
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(
            tiny_http::Response::from_string("<html>not found</html>").with_status_code(404),
        );
    });
    match freebuff_usage_with(
        &one_candidate(&path),
        &format!("http://127.0.0.1:{port}/usage"),
    ) {
        UsageOutcome::Unavailable { reason } => {
            assert!(
                reason.starts_with("API error 404:"),
                "generic 404 must surface as Unavailable, got: {reason:?}"
            );
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

#[test]
fn freebuff_usage_429_returns_rate_limited() {
    // 429 is transient (rate limit). We surface `RateLimited`
    // so the user doesn't think their session is bad — same
    // contract as Kimi / OpenRouter / DeepSeek.
    let dir = tempfile::tempdir().unwrap();
    let path = write_freebuff_credentials(
        dir.path(),
        r#"{
            "authToken": "fb_test_token",
            "fingerprintId": "fb_fp",
            "email": "user@example.com"
        }"#,
    );
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(429));
    });
    match freebuff_usage_with(
        &one_candidate(&path),
        &format!("http://127.0.0.1:{port}/usage"),
    ) {
        UsageOutcome::RateLimited { reason } => {
            assert!(
                reason.contains("Rate limited"),
                "429 copy must say 'Rate limited', got {reason:?}"
            );
        }
        other => panic!("expected RateLimited outcome, got: {other:?}"),
    }
}

#[test]
fn freebuff_usage_500_returns_unavailable_with_clamped_body() {
    // 500 is `unavailable` (logged_in stays true because the
    // credential may be valid; the upstream just failed). The
    // body is clamped to ~120 chars + `…` so a 15 KB
    // HTML error page from Cloudflare does NOT reach the
    // `<UsagePanel>` card. Pin the truncation behaviour here.
    let dir = tempfile::tempdir().unwrap();
    let path = write_freebuff_credentials(
        dir.path(),
        r#"{
            "authToken": "fb_test_token",
            "fingerprintId": "fb_fp",
            "email": "user@example.com"
        }"#,
    );
    // Long body designed to exceed ERROR_BODY_MAX_CHARS so the
    // truncation branch fires.
    let long_body = "stack trace begins ".repeat(20);
    let expected_detail = clamp_error_body(&long_body);
    let port = spawn_loopback(1, move |req| {
        // `long_body` is moved into the `Fn` closure via `move`; `Fn`
        // closures can be called multiple times so `tiny_http::Response::from_string`
        // consuming it would require `Clone` (one clone per request).
        let _ =
            req.respond(tiny_http::Response::from_string(long_body.clone()).with_status_code(500));
    });
    match freebuff_usage_with(
        &one_candidate(&path),
        &format!("http://127.0.0.1:{port}/usage"),
    ) {
        UsageOutcome::Unavailable { reason } => {
            assert!(
                reason.starts_with("API error 500: "),
                "expected leading 'API error 500: ' prefix, got {reason:?}"
            );
            assert!(
                reason.ends_with('…'),
                "long upstream body must be truncated with '…' suffix, got {reason:?}"
            );
            assert!(
                reason.contains(&expected_detail),
                "expected the clamped detail fragment in the error, got {reason:?}"
            );
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

#[test]
fn freebuff_usage_malformed_live_body_returns_unavailable_with_parse_error() {
    // 200 + garbage body → `unavailable` (logged_in stays true;
    // the credential works, just not for this endpoint shape).
    // Same contract as codex / kimi shape-failure.
    let dir = tempfile::tempdir().unwrap();
    let path = write_freebuff_credentials(
        dir.path(),
        r#"{
            "authToken": "fb_test_token",
            "fingerprintId": "fb_fp",
            "email": "user@example.com"
        }"#,
    );
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::from_string("not-json"));
    });
    match freebuff_usage_with(
        &one_candidate(&path),
        &format!("http://127.0.0.1:{port}/usage"),
    ) {
        UsageOutcome::Unavailable { reason } => {
            assert!(
                reason.contains("Failed to parse response"),
                "expected parse-error copy, got {reason:?}"
            );
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}
