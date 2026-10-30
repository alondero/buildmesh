// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::preferences::BillingMode;
use crate::services::usage::types::UsageWindow;

fn account(id: &str, enabled: bool) -> ProviderAccount {
    ProviderAccount {
        id: id.to_string(),
        name: id.to_string(),
        enabled,
        billing_mode: BillingMode::Plan,
        claude_compatible: crate::preferences::is_claude_compatible_id(id),
        api_key: None,
    }
}

fn profile(id: &str, harness: &str) -> HarnessProfile {
    HarnessProfile {
        id: id.to_string(),
        name: id.to_string(),
        harness: harness.to_string(),
        runtime: None,
        wsl_distro: None,
        executable: None,
    }
}

#[test]
fn opencode_subscription_card_is_shared_by_both_harness_majors() {
    let account = account("opencode", true);
    for harness in ["opencode", "opencode2"] {
        assert!(account_visible(&account, &[profile(harness, harness)]));
        assert_eq!(
            poll_ids(std::slice::from_ref(&account), &[profile(harness, harness)]),
            ["opencode"]
        );
    }
    assert!(!account_visible(&account, &[]));
}

fn usage(provider: &str) -> ProviderUsage {
    ProviderUsage {
        provider: provider.to_string(),
        logged_in: true,
        windows: Vec::new(),
        balance: None,
        meters: vec![],
        detail: None,
        error: None,
    }
}

/// Issue #1745: tests now build an `outcomes` map alongside `usages`.
/// Each entry's variant must match what `fetch` would have returned;
/// the helpers below produce the matching outcome for each envelope
/// shape used in this module's tests.
fn reading_outcome(_provider: &str) -> UsageOutcome {
    UsageOutcome::Reading {
        windows: Vec::new(),
        balance: None,
        meters: Vec::new(),
        detail: None,
    }
}

fn no_credential_outcome(provider: &str) -> UsageOutcome {
    UsageOutcome::NoCredential {
        hint: format!("No API key configured for {provider}"),
    }
}

fn outcomes_from_usages(usages: &HashMap<String, ProviderUsage>) -> HashMap<String, UsageOutcome> {
    // For each cached usage, project the variant the gate expects. The
    // gate's keep() predicate is total on every variant, so a wrong
    // mapping here silently changes keep/drop; tests must use the
    // helpers above so the projection is explicit.
    let mut map = HashMap::new();
    for (id, usage) in usages {
        let outcome = if usage.logged_in {
            UsageOutcome::Reading {
                windows: usage.windows.clone(),
                balance: usage.balance.clone(),
                meters: usage.meters.clone(),
                detail: usage.detail.clone(),
            }
        } else {
            UsageOutcome::NoCredential {
                hint: usage.error.clone().unwrap_or_default(),
            }
        };
        map.insert(id.clone(), outcome);
    }
    map
}

// ── Detection gating (issue #574) ───────────────────────────────────────

#[test]
fn native_provider_visible_only_when_its_harness_is_detected() {
    // Claude Code installed (harness profile backed by "anthropic").
    let claude = vec![profile("claude", "anthropic")];
    assert!(account_visible(&account("anthropic", true), &claude));
    // No harness profiles → the Anthropic subscription card is hidden (AC2's
    // inverse: it only shows when Claude Code is detected).
    assert!(!account_visible(&account("anthropic", true), &[]));
    // Codex card never shows on a box without Codex even though Claude is here.
    assert!(!account_visible(&account("codex", true), &claude));
}

#[test]
fn native_provider_card_stays_visible_when_disabled_but_detected() {
    // Card visibility is detection-driven, not enable-driven: a user who
    // disables an installed provider keeps its card so they can re-enable it.
    // (`enabled` gates polling, exercised in the poll_ids tests.)
    let claude = vec![profile("claude", "anthropic")];
    assert!(account_visible(&account("anthropic", false), &claude));
}

#[test]
fn keyed_provider_always_has_a_card_regardless_of_detection_or_enable() {
    // MiniMax / a custom endpoint have no binary of their own, so their card
    // always shows — to enter a key or re-enable — even disabled, even with no
    // harness profiles present at all.
    assert!(account_visible(&account("minimax", true), &[]));
    assert!(account_visible(&account("minimax", false), &[]));
    // DeepSeek is now a first-class keyed provider (issue #1127) — its
    // card shows regardless of detection state. Disabled cards stick
    // around so the user can re-enable (see `assemble_meters_keeps_disabled_*`).
    assert!(account_visible(&account("deepseek", true), &[]));
    assert!(account_visible(&account("deepseek", false), &[]));
}

#[test]
fn usage_tracked_recognizes_registered_kinds_and_rejects_generic_provider() {
    assert!(usage_tracked("anthropic"), "representative native meter");
    assert!(usage_tracked("minimax"), "representative keyed meter");
    assert!(!usage_tracked("glm"), "unregistered Generic provider");
}

#[test]
fn poll_ids_require_enabled_visible_and_fetchable() {
    let claude = vec![profile("claude", "anthropic")];
    let accounts = vec![
        account("anthropic", true), // enabled + detected + tracked → in
        account("codex", true),     // tracked but harness undetected → out
        account("minimax", true),   // enabled keyed tracked → in
        account("kimi", true),      // enabled keyed tracked → in (wallet meter)
        // OpenRouter joins the tracked keyed set — opt-in via missing key
        // (defaults to enabled, but `account_visible` lets it through; the
        // fetcher's empty-key path returns `logged_out` until the user adds
        // a key). The `poll_ids` gate here is purely enable+visible+tracked.
        account("openrouter", true),
        // DeepSeek is first-class keyed tracked (issue #1127); the
        // configured-key gate in `assemble_meters` (not `poll_ids`) drops
        // the row until the user adds a key.
        account("deepseek", true),
    ];
    assert_eq!(
        poll_ids(&accounts, &claude),
        vec!["anthropic", "minimax", "kimi", "openrouter", "deepseek"]
    );
}

#[test]
fn poll_ids_excludes_a_disabled_but_visible_tracked_provider() {
    // A disabled MiniMax keeps its card (keyed → visible) but must not be
    // polled — the enable toggle gates the network fetch, not the card.
    assert!(poll_ids(&[account("minimax", false)], &[]).is_empty());
    // A disabled-but-detected Anthropic likewise isn't polled.
    let claude = vec![profile("claude", "anthropic")];
    assert!(poll_ids(&[account("anthropic", false)], &claude).is_empty());
}

#[test]
fn assemble_meters_drops_undetected_natives_and_marks_generic_untracked() {
    let claude = vec![profile("claude", "anthropic")];
    let accounts = vec![
        account("anthropic", true),
        account("codex", true), // undetected harness → excluded entirely (AC1)
        account("minimax", true),
        // A *true* Generic provider (custom id, no first-class fetcher)
        // — included with `usage_tracked = false` (AC4). `glm` stands in
        // for the class: `deepseek` was a Generic before issue #1127
        // promoted it to first-class keyed tracked, so we can't reuse it
        // here without confusing the AC4 contract.
        account("glm", true),
    ];
    let mut usages = HashMap::new();
    usages.insert("anthropic".to_string(), usage("anthropic"));
    usages.insert("minimax".to_string(), usage("minimax"));

    let rows = assemble_meters(
        &accounts,
        &claude,
        &usages,
        &outcomes_from_usages(&usages),
        &HashSet::new(),
        &HashMap::new(),
    );
    let ids: Vec<_> = rows.iter().map(|r| r.provider.as_str()).collect();
    assert_eq!(ids, vec!["anthropic", "minimax", "glm"]);

    let anthropic = &rows[0];
    assert!(anthropic.usage_tracked);
    assert!(anthropic.usage.is_some());

    let glm = rows.iter().find(|r| r.provider == "glm").unwrap();
    assert!(!glm.usage_tracked);
    assert!(glm.usage.is_none());
}

#[test]
fn assemble_meters_keeps_a_disabled_detected_native_card_with_no_usage() {
    // Disabling a detected provider hides its meter (not polled) but keeps its
    // card so the user can re-enable it — the row stays, with usage None.
    let claude = vec![profile("claude", "anthropic")];
    let rows = assemble_meters(
        &[account("anthropic", false)],
        &claude,
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
        &HashMap::new(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider, "anthropic");
    assert!(rows[0].usage_tracked);
    assert!(rows[0].usage.is_none());
    assert_eq!(
        rows[0].usage_page.as_ref().unwrap().url,
        "https://claude.ai/settings/usage"
    );
}

#[test]
fn assemble_meters_attaches_usage_pages_to_live_failed_and_remembered_rows() {
    let accounts = [account("codex", true)];
    let profiles = [profile("codex", "codex")];
    for (outcome, previous) in [
        (reading_outcome("codex"), HashMap::new()),
        (
            UsageOutcome::Unavailable {
                reason: "Offline".to_string(),
            },
            HashMap::new(),
        ),
        (
            no_credential_outcome("codex"),
            [remembered("codex", 45.0, 1_700_000_000)]
                .into_iter()
                .collect(),
        ),
        (
            UsageOutcome::Unavailable {
                reason: "Offline".to_string(),
            },
            [remembered("codex", 45.0, 1_700_000_000)]
                .into_iter()
                .collect(),
        ),
    ] {
        let usages = [("codex".to_string(), outcome.clone().into_usage("codex"))]
            .into_iter()
            .collect();
        let outcomes = [("codex".to_string(), outcome)].into_iter().collect();
        let rows = assemble_meters(
            &accounts,
            &profiles,
            &usages,
            &outcomes,
            &HashSet::new(),
            &previous,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].usage_page.as_ref().unwrap().url,
            "https://chatgpt.com/codex/settings/usage"
        );
        let value = serde_json::to_value(&rows).unwrap();
        assert_eq!(
            value[0]["usagePage"]["url"],
            "https://chatgpt.com/codex/settings/usage"
        );
    }
}

#[test]
fn one_credential_proxied_through_many_harnesses_is_one_meter() {
    // Usage follows the credential, not the harness pairing (AC3): the same
    // Anthropic account reachable via two harness profiles still yields a
    // single row — there's one account, so it's counted once.
    let profiles = vec![
        profile("claude", "anthropic"),
        profile("claude-alt", "anthropic"),
    ];
    let accounts = vec![account("anthropic", true)];
    // The no-credential gate now requires a logged-in usage entry for
    // the row to surface; an empty usages map would drop it (matching
    // the user contract for unconfigured providers).
    let mut usages = HashMap::new();
    usages.insert("anthropic".to_string(), usage("anthropic"));
    let rows = assemble_meters(
        &accounts,
        &profiles,
        &usages,
        &outcomes_from_usages(&usages),
        &HashSet::new(),
        &HashMap::new(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider, "anthropic");
}

// ── No-credential gate (user contract: "I shouldn't see a meter for a
//    provider I haven't added credentials for") ────────────────────────

fn logged_out_usage(provider: &str) -> ProviderUsage {
    ProviderUsage {
        provider: provider.to_string(),
        logged_in: false,
        windows: Vec::new(),
        balance: None,
        meters: vec![],
        detail: None,
        error: Some(format!("No API key configured for {provider}")),
    }
}

#[test]
fn assemble_meters_drops_enabled_keyed_provider_without_credentials() {
    // User contract: "If we haven't added credentials for a provider I
    // shouldn't see the usage meter for that provider at all in the
    // usage Probe pane." A fresh user has every built-in enabled but
    // no API keys; MiniMax/OpenRouter must NOT show up as "No API key"
    // cards on the glance surface.
    let mut usages = HashMap::new();
    usages.insert("minimax".to_string(), logged_out_usage("minimax"));
    usages.insert("openrouter".to_string(), logged_out_usage("openrouter"));
    let mut outcomes = HashMap::new();
    outcomes.insert("minimax".to_string(), no_credential_outcome("minimax"));
    outcomes.insert(
        "openrouter".to_string(),
        no_credential_outcome("openrouter"),
    );
    let rows = assemble_meters(
        &[account("minimax", true), account("openrouter", true)],
        &[],
        &usages,
        &outcomes,
        &HashSet::new(),
        &HashMap::new(),
    );
    assert!(
        rows.is_empty(),
        "keyed providers without credentials must be dropped, got: {rows:?}"
    );
}

#[test]
fn assemble_meters_drops_enabled_native_provider_when_not_logged_in() {
    // Same contract for self-auth native providers: if the credential
    // file is absent (e.g. user hasn't run `claude login` yet) the
    // meter row must be dropped, not rendered as "Not logged in".
    // Detection gating already excludes uninstalled harnesses; this
    // is the second half — exclude detected-but-not-logged-in.
    let claude = vec![profile("claude", "anthropic")];
    let mut usages = HashMap::new();
    usages.insert("anthropic".to_string(), logged_out_usage("anthropic"));
    let mut outcomes = HashMap::new();
    outcomes.insert("anthropic".to_string(), no_credential_outcome("anthropic"));
    let rows = assemble_meters(
        &[account("anthropic", true)],
        &claude,
        &usages,
        &outcomes,
        &HashSet::new(),
        &HashMap::new(),
    );
    assert!(
        rows.is_empty(),
        "native providers without credentials must be dropped, got: {rows:?}"
    );
}

#[test]
fn assemble_meters_keeps_keyed_provider_with_rejected_key() {
    // CRITICAL regression guard (code review finding [0]): Kimi and
    // OpenRouter return `logged_in = false` on HTTP 401/403 — i.e.
    // when the stored key has been revoked, expired, or mistyped. A
    // naive `drop_on_logged_in_false` gate would silently hide the row
    // for those users with no in-tab signal to re-enter their key.
    // The configured_keys parameter lets us keep the row when the
    // account HAS a key configured (so <UsagePanel> can render the
    // existing "Invalid API key" copy and the user has a path back
    // to Settings).
    let mut usages = HashMap::new();
    usages.insert("kimi".to_string(), logged_out_usage("kimi"));
    usages.insert("openrouter".to_string(), logged_out_usage("openrouter"));
    // Issue #1745: the kept rows are `Rejected` outcomes (the wire
    // envelope is `logged_out`, but the projection of a `Rejected` is
    // identical and the test cares about the affordance being visible, not
    // about which variant was minted). The gate's keep() predicate keeps
    // `Rejected` when a key is configured.
    let mut outcomes = HashMap::new();
    outcomes.insert(
        "kimi".to_string(),
        UsageOutcome::Rejected {
            hint: "Invalid API key".into(),
        },
    );
    outcomes.insert(
        "openrouter".to_string(),
        UsageOutcome::Rejected {
            hint: "Invalid API key".into(),
        },
    );
    let mut configured_keys = HashSet::new();
    configured_keys.insert("kimi".to_string());
    configured_keys.insert("openrouter".to_string());
    let rows = assemble_meters(
        &[account("kimi", true), account("openrouter", true)],
        &[],
        &usages,
        &outcomes,
        &configured_keys,
        &HashMap::new(),
    );
    assert_eq!(
        rows.len(),
        2,
        "keyed providers WITH a configured key must keep their row even when the API rejected the key (got: {rows:?})"
    );
    let ids: Vec<&str> = rows.iter().map(|r| r.provider.as_str()).collect();
    assert!(ids.contains(&"kimi"));
    assert!(ids.contains(&"openrouter"));
    // The logged_out usage is forwarded so the UI can render
    // "Invalid API key" — the user-facing discoverability signal.
    for row in &rows {
        assert!(!row.usage.as_ref().unwrap().logged_in);
        assert!(row.usage.as_ref().unwrap().error.is_some());
    }
}

#[test]
fn assemble_meters_drops_enabled_tracked_provider_missing_from_usages_map() {
    // Regression guard for the `?` operator branch: a polled provider
    // whose entry is absent from `usages` (e.g. a transient fetch
    // failure that didn't even produce a `logged_out` envelope) is
    // dropped silently rather than rendered as "Unable to load usage
    // data". Without this test, a future refactor that swapped `?`
    // back to `usages.get(&a.id).cloned()` would silently reintroduce
    // the stale "Unable to load usage data" fallback path.
    let rows = assemble_meters(
        &[account("minimax", true)],
        &[],
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
        &HashMap::new(),
    );
    assert!(
        rows.is_empty(),
        "enabled+tracked with no usages entry must be dropped, got: {rows:?}"
    );
}

#[test]
fn assemble_meters_keeps_disabled_keyed_provider_as_a_card() {
    // Regression guard for the Settings-side "Disabled" card. A user
    // who explicitly disables a keyed provider still needs its card
    // visible so they can re-enable — but the meter row carries
    // `usage = None` so <UsagePanel> renders "Disabled" (not "No API
    // key"). This is the orthogonal axis to the no-credential gate:
    // disabled = "user choice, keep the card", no-credential = "drop
    // entirely".
    let rows = assemble_meters(
        &[account("minimax", false)],
        &[],
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
        &HashMap::new(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider, "minimax");
    assert!(rows[0].usage.is_none(), "disabled card carries no usage");
}

#[test]
fn muse_code_card_is_gated_on_the_muse_harness() {
    let muse = vec![profile("muse", "muse")];
    assert!(account_visible(&account("muse-code", true), &muse));
    assert!(!account_visible(&account("muse-code", true), &[]));
    assert!(usage_tracked("muse-code"));
    assert_eq!(
        poll_ids(&[account("muse-code", true)], &muse),
        vec!["muse-code"]
    );
    assert!(poll_ids(&[account("muse-code", true)], &[]).is_empty());
}

#[test]
fn muse_code_no_credential_outcome_drops_the_row_just_like_grok_and_agy() {
    // Issue #1745 acceptance criterion #3: this is the load-bearing
    // test that pins the bug pre-#1745 (Muse's unconfigured row
    // stayed on the usage surface and rendered red error text) and
    // pins the fix post-#1745 (Muse's no-credential outcome drops
    // the row, identical to Grok and Agy's no-credential case).
    let muse = vec![profile("muse", "muse")];
    let no_credential = UsageOutcome::NoCredential {
        hint: "Muse login missing. Run muse login again.".into(),
    };

    // The projection of `NoCredential` is the same wire envelope that
    // `logged_out()` would have produced — `logged_in: false,
    // error: Some(hint), meters: []`. Pin it here so a future change
    // to the projection cannot silently re-add the `UsageMeter::Unavailable`
    // sentinel (the dead-branch combination pre-#1745).
    let projected = no_credential.clone().into_usage("muse-code");
    assert!(!projected.logged_in);
    assert_eq!(
        projected.error.as_deref(),
        Some("Muse login missing. Run muse login again.")
    );
    assert!(projected.meters.is_empty());

    let mut usages = HashMap::new();
    usages.insert("muse-code".to_string(), projected);
    let mut outcomes = HashMap::new();
    outcomes.insert("muse-code".to_string(), no_credential);
    let rows = assemble_meters(
        &[account("muse-code", true)],
        &muse,
        &usages,
        &outcomes,
        &HashSet::new(),
        &HashMap::new(),
    );
    assert!(
        rows.is_empty(),
        "Muse Code's no-credential case must drop the row (issue #1745), got: {rows:?}"
    );
}

/// Issue #1745 acceptance criterion #3: cross-provider equality.
/// The no-credential wire envelope for muse-code, grok, and agy must
/// be identical — same field set, same values. Without this, a
/// future adapter drift re-introduces the cross-provider inconsistency
/// the seam exists to prevent.
#[test]
fn no_credential_envelope_is_identical_across_providers() {
    let muse = UsageOutcome::NoCredential {
        hint: "Muse login missing.".into(),
    }
    .into_usage("muse-code");
    let grok = UsageOutcome::NoCredential {
        hint: "Grok auth missing.".into(),
    }
    .into_usage("grok");
    let agy = UsageOutcome::NoCredential {
        hint: "Antigravity OAuth missing.".into(),
    }
    .into_usage("agy");

    // Compare the structural shape, not the provider-specific copy.
    for ((muse_field, muse_value), (grok_field, grok_value), (agy_field, agy_value)) in [
        (
            ("logged_in", muse.logged_in),
            ("logged_in", grok.logged_in),
            ("logged_in", agy.logged_in),
        ),
        (
            ("windows", muse.windows.is_empty()),
            ("windows", grok.windows.is_empty()),
            ("windows", agy.windows.is_empty()),
        ),
        (
            ("balance", muse.balance.is_none()),
            ("balance", grok.balance.is_none()),
            ("balance", agy.balance.is_none()),
        ),
        (
            ("meters", muse.meters.is_empty()),
            ("meters", grok.meters.is_empty()),
            ("meters", agy.meters.is_empty()),
        ),
        (
            ("detail", muse.detail.is_none()),
            ("detail", grok.detail.is_none()),
            ("detail", agy.detail.is_none()),
        ),
        (
            ("error_is_some", muse.error.is_some()),
            ("error_is_some", grok.error.is_some()),
            ("error_is_some", agy.error.is_some()),
        ),
    ] {
        assert_eq!(
            muse_field, grok_field,
            "field name must match across providers"
        );
        assert_eq!(
            muse_field, agy_field,
            "field name must match across providers"
        );
        assert_eq!(
            muse_value, grok_value,
            "field {muse_field} must match across providers: muse {muse_value:?} vs grok {grok_value:?}"
        );
        assert_eq!(
            muse_value, agy_value,
            "field {muse_field} must match across providers: muse {muse_value:?} vs agy {agy_value:?}"
        );
    }
}

// ── Last-known fallback (ADR-0037) ─────────────────────────────────────

/// A reading the provider reported previously, as the durable store hands it
/// back: the wire triple plus the instant it was fetched.
fn remembered(provider: &str, used_percent: f64, cached_at: i64) -> (String, LastKnown) {
    let mut cached = usage(provider);
    cached.windows.push(UsageWindow {
        label: "Weekly".to_string(),
        used_percent: Some(used_percent),
        resets_at: None,
    });
    (
        provider.to_string(),
        LastKnown {
            usage: cached,
            cached_at,
        },
    )
}

fn unavailable_usage(provider: &str, reason: &str) -> ProviderUsage {
    ProviderUsage {
        provider: provider.to_string(),
        logged_in: true,
        windows: Vec::new(),
        balance: None,
        meters: vec![],
        detail: None,
        error: Some(reason.to_string()),
    }
}

/// The load-bearing case: Grok/Antigravity/Muse are not logged into yet
/// today, so the fetch returns `NoCredential` and the row would be hidden —
/// instead it shows the last reading the provider did report, stamped so the
/// UI can label it as last known.
#[test]
fn assemble_meters_serves_the_last_known_reading_when_the_row_would_be_hidden() {
    let grok = vec![profile("grok", "grok")];
    let mut usages = HashMap::new();
    usages.insert("grok".to_string(), logged_out_usage("grok"));
    let mut outcomes = HashMap::new();
    outcomes.insert("grok".to_string(), no_credential_outcome("grok"));
    let previous: HashMap<String, LastKnown> = [remembered("grok", 72.5, 1_700_000_000)]
        .into_iter()
        .collect();

    let rows = assemble_meters(
        &[account("grok", true)],
        &grok,
        &usages,
        &outcomes,
        &HashSet::new(),
        &previous,
    );

    assert_eq!(
        rows.len(),
        1,
        "a remembered reading must keep the row visible, got: {rows:?}"
    );
    let row = &rows[0];
    assert_eq!(
        row.cached_at,
        Some(1_700_000_000),
        "the row must be stamped with when the reading was fetched, not now"
    );
    let shown = row.usage.as_ref().expect("fallback usage");
    assert!(shown.logged_in);
    assert!(
        shown.error.is_none(),
        "the failure envelope must not leak through"
    );
    assert_eq!(shown.windows[0].used_percent, Some(72.5));
}

#[test]
fn assemble_meters_drops_the_row_when_nothing_was_remembered() {
    // The documented no-cache behaviour: nothing remembered → hidden, exactly
    // as before the fallback existed.
    let grok = vec![profile("grok", "grok")];
    let mut usages = HashMap::new();
    usages.insert("grok".to_string(), logged_out_usage("grok"));
    let mut outcomes = HashMap::new();
    outcomes.insert("grok".to_string(), no_credential_outcome("grok"));

    let rows = assemble_meters(
        &[account("grok", true)],
        &grok,
        &usages,
        &outcomes,
        &HashSet::new(),
        &HashMap::new(),
    );
    assert!(rows.is_empty(), "nothing remembered means no row: {rows:?}");
}

/// A transient failure with a remembered reading serves the cached row:
/// a slightly outdated meter beats a bare error (the Muse
/// "did not report subscription usage" case). The row is stamped so the
/// UI labels it as last known rather than live.
#[test]
fn assemble_meters_serves_last_known_for_a_transient_failure_when_remembered() {
    for outcome in [
        UsageOutcome::Unavailable {
            reason: "Muse did not report subscription usage.".into(),
        },
        UsageOutcome::RateLimited {
            reason: "Rate limited.".into(),
        },
    ] {
        let muse = vec![profile("muse", "muse")];
        let mut usages = HashMap::new();
        usages.insert(
            "muse-code".to_string(),
            unavailable_usage("muse-code", "Muse did not report subscription usage."),
        );
        let mut outcomes = HashMap::new();
        outcomes.insert("muse-code".to_string(), outcome);
        let previous: HashMap<String, LastKnown> = [remembered("muse-code", 72.5, 1_700_000_000)]
            .into_iter()
            .collect();

        let rows = assemble_meters(
            &[account("muse-code", true)],
            &muse,
            &usages,
            &outcomes,
            &HashSet::new(),
            &previous,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cached_at, Some(1_700_000_000));
        let shown = rows[0].usage.as_ref().unwrap();
        assert_eq!(shown.windows[0].used_percent, Some(72.5));
    }
}

/// A transient failure with nothing remembered is a live signal the user
/// can act on; the row keeps showing the error (the with-cache case is
/// pinned by the test above).
#[test]
fn assemble_meters_keeps_the_live_error_for_a_transient_failure() {
    let grok = vec![profile("grok", "grok")];
    let mut usages = HashMap::new();
    usages.insert(
        "grok".to_string(),
        unavailable_usage("grok", "API error 500: upstream down"),
    );
    let mut outcomes = HashMap::new();
    outcomes.insert(
        "grok".to_string(),
        UsageOutcome::Unavailable {
            reason: "API error 500: upstream down".into(),
        },
    );
    let previous: HashMap<String, LastKnown> = HashMap::new();

    let rows = assemble_meters(
        &[account("grok", true)],
        &grok,
        &usages,
        &outcomes,
        &HashSet::new(),
        &previous,
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].cached_at, None, "a kept row is not a fallback row");
    let shown = rows[0].usage.as_ref().unwrap();
    assert_eq!(shown.error.as_deref(), Some("API error 500: upstream down"));
    assert!(
        shown.windows.is_empty(),
        "the remembered reading must not replace the live error"
    );
}

/// The "Invalid API key" prompt is the user's route back to Settings. A
/// remembered reading must never stand in for it.
#[test]
fn assemble_meters_does_not_cover_a_rejected_key_with_a_stale_reading() {
    let mut usages = HashMap::new();
    let mut rejected = logged_out_usage("kimi");
    rejected.error = Some("Invalid API key".to_string());
    usages.insert("kimi".to_string(), rejected);
    let mut outcomes = HashMap::new();
    outcomes.insert(
        "kimi".to_string(),
        UsageOutcome::Rejected {
            hint: "Invalid API key".into(),
        },
    );
    let configured_keys: HashSet<String> = ["kimi".to_string()].into_iter().collect();
    let previous: HashMap<String, LastKnown> = [remembered("kimi", 12.0, 1_700_000_000)]
        .into_iter()
        .collect();

    let rows = assemble_meters(
        &[account("kimi", true)],
        &[],
        &usages,
        &outcomes,
        &configured_keys,
        &previous,
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].cached_at, None,
        "a rejected key is not a fallback row"
    );
    let shown = rows[0].usage.as_ref().unwrap();
    assert_eq!(shown.error.as_deref(), Some("Invalid API key"));
    assert!(
        shown.windows.is_empty(),
        "the remembered reading must not stand in for the re-entry prompt"
    );
}

/// Issue #1073: a cache entry must never gate or discard a successful live
/// probe.
#[test]
fn assemble_meters_never_prefers_a_remembered_reading_over_a_live_one() {
    let grok = vec![profile("grok", "grok")];
    let (_, stale) = remembered("grok", 99.0, 1_700_000_000);
    let mut usages = HashMap::new();
    usages.insert("grok".to_string(), usage("grok"));
    usages.get_mut("grok").unwrap().windows.push(UsageWindow {
        label: "Weekly".to_string(),
        used_percent: Some(5.0),
        resets_at: None,
    });
    let mut outcomes = HashMap::new();
    outcomes.insert("grok".to_string(), reading_outcome("grok"));
    let previous: HashMap<String, LastKnown> = [("grok".to_string(), stale)].into_iter().collect();

    let rows = assemble_meters(
        &[account("grok", true)],
        &grok,
        &usages,
        &outcomes,
        &HashSet::new(),
        &previous,
    );

    assert_eq!(rows[0].cached_at, None, "a live row is never stamped stale");
    assert_eq!(
        rows[0].usage.as_ref().unwrap().windows[0].used_percent,
        Some(5.0),
        "the live reading wins over the remembered one"
    );
}
