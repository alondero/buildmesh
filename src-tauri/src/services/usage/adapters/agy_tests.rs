// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;

/// Redacted capture of a live `antigravity-oauth-token` file (agy 1.2 on
/// Windows, 2026-09-10). Field set and nesting are load-bearing; secrets
/// replaced. Proves the CLI file shares the keyring envelope
/// `{ "token": { "access_token": … }, "auth_method", "id_token"? }`.
const REAL_CLI_OAUTH_FILE_SHAPE: &[u8] = br#"{"token":{"access_token":"ya29.live-shape-fixture","token_type":"Bearer","refresh_token":"1//redacted","expiry":"2026-09-10T08:52:44.5683799+01:00"},"auth_method":"consumer","id_token":"eyJ.redacted"}"#;

#[test]
fn parse_agy_token_accepts_real_cli_oauth_file_shape() {
    assert_eq!(
        parse_agy_token(REAL_CLI_OAUTH_FILE_SHAPE).unwrap(),
        "ya29.live-shape-fixture"
    );
}

#[test]
fn parse_agy_token_extracts_nested_access_token() {
    let blob = br#"{"token":{"access_token":"ya29.agytok","token_type":"Bearer","refresh_token":"1//ref","expiry":"2026-05-31T12:00:00Z"},"auth_method":"consumer"}"#;
    assert_eq!(parse_agy_token(blob).unwrap(), "ya29.agytok");
}

#[test]
fn parse_agy_token_missing_is_no_credential() {
    match parse_agy_token(br#"{"auth_method":"consumer"}"#) {
        Err(UsageError::NoCredential(_)) => {}
        other => panic!("expected NoCredential, got {other:?}"),
    }
    match parse_agy_token(br#"{"token":{"access_token":""}}"#) {
        Err(UsageError::NoCredential(_)) => {}
        other => panic!("expected NoCredential, got {other:?}"),
    }
}

#[test]
fn parse_agy_token_corrupt_json_is_shape() {
    match parse_agy_token(br#"{not-json"#) {
        Err(UsageError::Shape(_)) => {}
        other => panic!("expected Shape, got {other:?}"),
    }
}

#[test]
fn agy_oauth_token_path_joins_agy_dir() {
    let _env_guard = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(
        agy_oauth_token_path(),
        crate::env::agy_dir().join(AGY_OAUTH_TOKEN_FILE)
    );
}

#[test]
fn collect_prefers_cli_oauth_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(AGY_OAUTH_TOKEN_FILE);
    fs::write(
        &path,
        br#"{"token":{"access_token":"ya29.fromfile","token_type":"Bearer"},"auth_method":"consumer"}"#,
    )
    .unwrap();
    let tokens = collect_agy_access_tokens(&path, || Ok("ya29.from-keyring".to_string())).unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].source, AgyTokenSource::CliFile);
    assert_eq!(tokens[0].access_token, "ya29.fromfile");
    assert_eq!(tokens[1].source, AgyTokenSource::Keyring);
    assert_eq!(tokens[1].access_token, "ya29.from-keyring");
}

#[test]
fn collect_falls_back_when_file_missing() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join("absent-oauth-token");
    let tokens = collect_agy_access_tokens(&path, || Ok("ya29.from-keyring".to_string())).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].source, AgyTokenSource::Keyring);
    assert_eq!(tokens[0].access_token, "ya29.from-keyring");
}

#[test]
fn collect_falls_back_when_file_token_empty() {
    // Valid JSON missing a bearer is NoCredential, not Shape — try keyring.
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(AGY_OAUTH_TOKEN_FILE);
    fs::write(&path, br#"{"auth_method":"consumer"}"#).unwrap();
    let tokens = collect_agy_access_tokens(&path, || Ok("ya29.from-keyring".to_string())).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].access_token, "ya29.from-keyring");
}

#[test]
fn collect_corrupt_cli_file_does_not_fall_back() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(AGY_OAUTH_TOKEN_FILE);
    fs::write(&path, br#"{not-json"#).unwrap();
    match collect_agy_access_tokens(&path, || Ok("ya29.from-keyring".to_string())) {
        Err(UsageError::Shape(_)) => {}
        other => panic!("expected Shape without keyring fallback, got {other:?}"),
    }
}

#[test]
fn collect_propagates_fallback_error_when_both_sources_dead() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join("absent-oauth-token");
    match collect_agy_access_tokens(&path, || {
        Err(UsageError::NoCredential("keyring-missing".into()))
    }) {
        Err(UsageError::NoCredential(msg)) => {
            assert!(
                msg.contains("antigravity-oauth-token") || msg.contains("gemini:antigravity"),
                "expected combined NoCredential message, got {msg}"
            );
        }
        other => panic!("expected NoCredential, got {other:?}"),
    }
}

#[test]
fn collect_dedupes_identical_bearers() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(AGY_OAUTH_TOKEN_FILE);
    fs::write(
        &path,
        br#"{"token":{"access_token":"ya29.same","token_type":"Bearer"},"auth_method":"consumer"}"#,
    )
    .unwrap();
    let tokens = collect_agy_access_tokens(&path, || Ok("ya29.same".to_string())).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].source, AgyTokenSource::CliFile);
}

#[cfg(not(windows))]
#[test]
fn keyring_unavailable_message_on_non_windows() {
    match read_agy_keyring_token() {
        Err(UsageError::NoCredential(msg)) => {
            assert!(msg.contains("antigravity-oauth-token"));
            assert!(msg.contains("gemini:antigravity"));
        }
        other => panic!("expected NoCredential, got {other:?}"),
    }
}

#[test]
fn agy_http_auth_failure_detects_401_and_403() {
    assert!(agy_http_auth_failure(&UsageError::Shape(
        "retrieveUserQuotaSummary HTTP 401".into()
    )));
    assert!(agy_http_auth_failure(&UsageError::Shape(
        "loadCodeAssist HTTP 403 — try re-authenticating via the Antigravity CLI".into()
    )));
    assert!(!agy_http_auth_failure(&UsageError::Shape(
        "retrieveUserQuotaSummary HTTP 500".into()
    )));
    assert!(!agy_http_auth_failure(&UsageError::NoCredential(
        "missing".into()
    )));
}

#[test]
fn agy_source_error_classifies_auth_rejection_as_rejected() {
    // Issue #1758: auth rejection on the last source means the Bearer [REDACTED]
    // gone/bad — `Rejected` so the gate drops the native row (same
    // user-visible result as the old `logged_out` envelope).
    match agy_outcome_from_source_error(UsageError::Shape(
        "retrieveUserQuotaSummary HTTP 401".into(),
    )) {
        UsageOutcome::Rejected { hint } => {
            assert!(
                hint.contains("401"),
                "hint must carry the status, got: {hint:?}"
            );
        }
        other => panic!("expected Rejected outcome, got: {other:?}"),
    }
}

#[test]
fn agy_source_error_classifies_transport_failure_as_unavailable() {
    // Issue #1758: a non-auth source failure (e.g. loadCodeAssist
    // transport error) is transport-class — `Unavailable` keeps the row
    // visible with red error copy instead of silently dropping it.
    match agy_outcome_from_source_error(UsageError::Shape(
        "loadCodeAssist failed: connection refused".into(),
    )) {
        UsageOutcome::Unavailable { reason } => {
            assert!(
                reason.contains("loadCodeAssist"),
                "reason must carry the cause, got: {reason:?}"
            );
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

#[test]
fn parse_agy_quota_summary_puts_five_hour_before_weekly_in_each_group() {
    // retrieveUserQuotaSummary returns weekly then 5-hour in each group.
    // Every other Usage Meter (Anthropic, Codex, OpenCode) lists the 5-hour
    // window first; keep Antigravity in that order.
    let json = r#"{
        "groups": [
            {
                "displayName": "Gemini Models",
                "buckets": [
                    {"bucketId":"gemini-weekly","displayName":"Weekly Limit Remaining","window":"weekly","remainingFraction":0.5,"resetTime":"2026-08-29T17:48:59Z"},
                    {"bucketId":"gemini-5h","displayName":"Five Hour Limit Remaining","window":"5h","remainingFraction":0.75,"resetTime":"2026-08-28T00:03:11Z"}
                ]
            },
            {
                "displayName": "Claude and GPT models",
                "buckets": [
                    {"bucketId":"3p-weekly","displayName":"Weekly Limit Remaining","window":"weekly","remainingFraction":0.25,"resetTime":"2026-09-03T16:17:33Z"},
                    {"bucketId":"3p-5h","displayName":"Five Hour Limit Remaining","window":"5h","remainingFraction":0,"resetTime":"2026-08-27T21:17:33Z"}
                ]
            }
        ]
    }"#;

    let (windows, detail) = parse_agy_quota_summary(json).unwrap();

    assert_eq!(windows.len(), 4);
    assert_eq!(windows[0].label, "Gemini Models — 5-hour");
    assert_eq!(windows[0].used_percent, Some(25.0));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-08-28T00:03:11Z")
    );
    assert_eq!(windows[1].label, "Gemini Models — Weekly");
    assert_eq!(windows[1].used_percent, Some(50.0));
    assert_eq!(
        windows[1].resets_at.as_deref(),
        Some("2026-08-29T17:48:59Z")
    );
    assert_eq!(windows[2].label, "Claude and GPT models — 5-hour");
    // remainingFraction 0 is a real exhausted window, not "missing".
    assert_eq!(windows[2].used_percent, Some(100.0));
    assert_eq!(windows[3].label, "Claude and GPT models — Weekly");
    assert_eq!(windows[3].used_percent, Some(75.0));
    assert_eq!(detail, None);
}

#[test]
fn parse_agy_quota_summary_empty_result_falls_back_to_model_api() {
    // An empty but successful response is transient upstream state, not a
    // meaningful zero-quota reading. Returning an error lets `agy_usage`
    // retain the older model-API meter instead of caching a blank card.
    let json = r#"{"groups":[]}"#;
    assert!(parse_agy_quota_summary(json).is_err());
}

#[test]
fn parse_agy_quota_summary_missing_group_name_is_shape() {
    // Fail loudly: a missing displayName is not an unnamed meter.
    let json = r#"{"groups":[{"buckets":[{"window":"5h","remainingFraction":0.5}]}]}"#;
    assert!(parse_agy_quota_summary(json).is_err());
}

#[test]
fn parse_agy_quota_summary_flags_a_missing_gemini_group() {
    // Regression 2026-10-16: a below-gate User-Agent is served HTTP 200 with
    // the "Gemini Models" group silently stripped, so the Gemini meters
    // vanish with no error. Keep the surviving windows, but attach a detail
    // so a re-gate (or a stale pin) is visible instead of silent.
    let json = r#"{
        "groups": [
            {
                "displayName": "Claude and GPT models",
                "buckets": [
                    {"bucketId":"3p-5h","window":"5h","remainingFraction":0.5,"resetTime":"2026-10-16T14:03:39Z"}
                ]
            }
        ]
    }"#;
    let (windows, detail) = parse_agy_quota_summary(json).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "Claude and GPT models — 5-hour");
    assert!(
        detail
            .as_deref()
            .is_some_and(|d| d.contains("no Gemini quota group")),
        "expected a missing-Gemini detail, got {detail:?}"
    );
}

#[test]
fn agy_usage_does_not_spawn_cli() {
    // #1324 spawned `agy --print /usage` (≈6s CLI boot) to reach the weekly
    // Gemini bucket. retrieveUserQuotaSummary is the same payload over HTTP
    // (~250ms). Spawning the CLI stalls the whole Usage Probe.
    //
    // The fetch moved into this file (issue #2528), so the guard reads
    // this file — and only its production half, because the assertions
    // below quote the very strings they look for.
    let src = include_str!("agy.rs");
    let production = src.split("#[cfg(test)]").next().unwrap_or_default();
    assert!(
        !production.contains("command_no_window(\"agy\")"),
        "do not spawn the agy CLI for usage; call retrieveUserQuotaSummary instead"
    );
    assert!(
        !production.contains("\"/usage\""),
        "do not spawn agy --print /usage; call retrieveUserQuotaSummary instead"
    );
}

#[test]
fn agy_user_agent_meets_gemini_quota_version_gate() {
    // Regression 2026-10-16: `retrieveUserQuotaSummary` gates the
    // "Gemini Models" (weekly + 5-hour) group on the CLI version advertised
    // in the User-Agent. An `antigravity/cli/1.0.3` UA still got HTTP 200
    // but with the Gemini group stripped, so both Gemini Usage Meters
    // vanished. Bisected live 2026-10-16 varying only the UA: `1.1.5`
    // returns only "Claude and GPT models", `1.2.0` adds "Gemini Models".
    // The pin is `1.4.0` — the current `agy --version`, also exercised live
    // (it additionally returns the "Gemini 4 Argon" group). Guard the pin
    // against drifting back below the gate.
    let version_token = AGY_USER_AGENT
        .strip_prefix("antigravity/cli/")
        .and_then(|rest| rest.split_whitespace().next())
        .expect("AGY_USER_AGENT must read `antigravity/cli/<major.minor.patch> <os>/<arch>`");
    let version: Vec<u32> = version_token
        .split('.')
        .map(|part| {
            part.parse()
                .unwrap_or_else(|_| panic!("non-numeric version segment in {version_token:?}"))
        })
        .collect();
    assert_eq!(
        version.len(),
        3,
        "AGY_USER_AGENT must advertise a major.minor.patch version, got {version_token:?}"
    );
    assert!(
        (version[0], version[1], version[2]) >= (1, 2, 0),
        "the quota API returns the Gemini (weekly + 5-hour) group only for \
         User-Agent cli versions >= 1.2.0, but AGY_USER_AGENT advertises \
         {version_token:?}; bump the pin"
    );
}

#[test]
fn parse_agy_models_follows_sort_order_and_inverts_fraction() {
    // Two ranked models + one with no quota (must be skipped). The sort order
    // (claude before flash) must be preserved regardless of map iteration.
    let json = r#"{
        "models": {
            "m-flash": {"displayName":"Gemini 3.5 Flash (Medium)","quotaInfo":{"remainingFraction":0.8,"resetTime":"2026-05-31T12:22:46Z"}},
            "m-claude": {"displayName":"Claude Sonnet 4.6 (Thinking)","quotaInfo":{"remainingFraction":1.0,"resetTime":"2026-05-31T16:51:02Z"}},
            "m-hidden": {"displayName":"No Quota Model"}
        },
        "agentModelSorts": [{"displayName":"Recommended","groups":[{"modelIds":["m-claude","m-flash"]}]}]
    }"#;
    let (windows, detail) = parse_agy_models(json).unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].label, "Claude Sonnet 4.6 (Thinking)");
    assert_eq!(windows[0].used_percent, Some(0.0));
    // Single Gemini entry is still relabeled — the collapsed label applies
    // unconditionally so the row always reads as "this is your Gemini budget".
    assert_eq!(windows[1].label, "Gemini (all models)");
    // Same float expression the parser uses (0.8 remaining → ~20% used).
    assert_eq!(windows[1].used_percent, Some((1.0 - 0.8) * 100.0));
    assert_eq!(
        windows[1].resets_at.as_deref(),
        Some("2026-05-31T12:22:46Z")
    );
    assert_eq!(detail, None);
}

#[test]
fn parse_agy_models_falls_back_to_all_when_no_sorts() {
    let json =
        r#"{"models":{"a":{"displayName":"Model A","quotaInfo":{"remainingFraction":0.5}}}}"#;
    let (windows, _) = parse_agy_models(json).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].used_percent, Some(50.0));
}

#[test]
fn parse_agy_models_empty_reports_detail() {
    let (windows, detail) = parse_agy_models(r#"{"models":{}}"#).unwrap();
    assert!(windows.is_empty());
    assert_eq!(detail.as_deref(), Some("No active model quotas found"));
}

#[test]
fn parse_agy_models_collapses_gemini_effort_levels() {
    // Antigravity returns one quota row per Gemini effort level (Low / Medium /
    // High, Pro Low / High) but they all read off one shared bucket on
    // Google's side, so we render them as a single "Gemini (all models)" row
    // at the first Gemini entry's position in the sort. Non-Gemini models
    // pass through unchanged. In production all Gemini entries carry the
    // SAME remainingFraction (verified live 2026-06-04).
    let json = r#"{
        "models": {
            "m-flash-low":   {"displayName":"Gemini 3.5 Flash (Low)",   "quotaInfo":{"remainingFraction":0.7,"resetTime":"2026-06-04T12:00:00Z"}},
            "m-flash-med":   {"displayName":"Gemini 3.5 Flash (Medium)","quotaInfo":{"remainingFraction":0.7,"resetTime":"2026-06-04T12:00:00Z"}},
            "m-flash-high":  {"displayName":"Gemini 3.5 Flash (High)",  "quotaInfo":{"remainingFraction":0.7,"resetTime":"2026-06-04T12:00:00Z"}},
            "m-claude":      {"displayName":"Claude Sonnet 4.6 (Thinking)","quotaInfo":{"remainingFraction":1.0,"resetTime":"2026-06-04T16:00:00Z"}},
            "m-gpt":         {"displayName":"GPT-OSS 120B","quotaInfo":{"remainingFraction":0.5,"resetTime":"2026-06-04T18:00:00Z"}}
        },
        "agentModelSorts":[{"groups":[{"modelIds":["m-claude","m-flash-med","m-flash-low","m-flash-high","m-gpt"]}]}]
    }"#;
    let (windows, detail) = parse_agy_models(json).unwrap();
    // 5 input models → 3 windows out (the 3 Gemini rows collapse to 1).
    assert_eq!(
        windows.len(),
        3,
        "Gemini effort levels should collapse to a single row"
    );
    // Sort order preserved: claude → (collapsed gemini at first gemini's slot) → gpt.
    assert_eq!(windows[0].label, "Claude Sonnet 4.6 (Thinking)");
    assert_eq!(windows[1].label, "Gemini (all models)");
    assert_eq!(windows[1].used_percent, Some((1.0 - 0.7) * 100.0));
    assert_eq!(
        windows[1].resets_at.as_deref(),
        Some("2026-06-04T12:00:00Z")
    );
    assert_eq!(windows[2].label, "GPT-OSS 120B");
    assert_eq!(detail, None);
}

#[test]
fn agy_load_project_extracts_companion_project() {
    let resp: AgyLoadResp = serde_json::from_str(
        r#"{"currentTier":{"id":"standard-tier"},"cloudaicompanionProject":"sinuous-strategy-j3z18"}"#,
    )
    .unwrap();
    assert_eq!(
        resp.cloudaicompanion_project.as_deref(),
        Some("sinuous-strategy-j3z18")
    );
}

/// The rendered meter output is pinned (issue #2528).
///
/// The `fetchAvailableModels` reply and the exact wire triple it must
/// project to are both fixtures: any change to agy's window labels,
/// percentages or reset times shows up here as a byte difference rather
/// than as a quietly different meter.
#[test]
fn agy_rendered_output_matches_the_pinned_fixture() {
    const RESPONSE: &str = include_str!("fixtures/agy-models.json");
    const RENDERED: &str = include_str!("fixtures/agy-models-rendered.json");

    let (windows, detail) = parse_agy_models(RESPONSE).unwrap();
    let usage = UsageReading {
        windows,
        balance: None,
        meters: Vec::new(),
        detail,
    }
    .into_outcome()
    .into_usage("agy");
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        // The fixture file ends with a newline; the payload does not.
        RENDERED.trim_end(),
        "agy's rendered meter output must stay byte-identical"
    );
}

/// The quota-summary path is agy's primary read, so its rendered meter
/// output is pinned too (issue #2528).
///
/// `retrieveUserQuotaSummary` is the endpoint behind `agy /usage`: the
/// 5-hour and weekly shared buckets for each model group. The reply and
/// the exact wire triple it must project to are both fixtures, so a change
/// to agy's group labels, window order, percentages or reset times shows up
/// here as a byte difference rather than as a quietly different meter.
#[test]
fn agy_quota_summary_rendered_output_matches_the_pinned_fixture() {
    const RESPONSE: &str = include_str!("fixtures/agy-quota-summary.json");
    const RENDERED: &str = include_str!("fixtures/agy-quota-summary-rendered.json");

    let (windows, detail) = parse_agy_quota_summary(RESPONSE).unwrap();
    let usage = UsageReading {
        windows,
        balance: None,
        meters: Vec::new(),
        detail,
    }
    .into_outcome()
    .into_usage("agy");
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        // The fixture file ends with a newline; the payload does not.
        RENDERED.trim_end(),
        "agy's quota-summary rendered meter output must stay byte-identical"
    );
}
