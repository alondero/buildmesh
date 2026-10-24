// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::services::usage::types::UsageAmount;

/// Pin the projection table. One test per variant with the exact wire
/// triple the panel renders against, compared as serialized JSON so the
/// check covers each field rather than a summary of it. If a future change
/// adds a variant without extending this test, the gate's behaviour for
/// that variant is undefined — fail here, not in production.
#[test]
fn into_usage_table_per_variant() {
    let amount = UsageAmount {
        used: 25.0,
        limit: Some(100.0),
        remaining: Some(75.0),
        unit: "USD".to_string(),
        used_percent: Some(25.0),
        resets_at: Some("2026-10-01T00:00:00Z".to_string()),
    };
    let cases = vec![
        (
            "reading windows pass through",
            UsageOutcome::Reading {
                windows: vec![UsageWindow {
                    label: "5-hour".into(),
                    used_percent: Some(41.0),
                    resets_at: None,
                }],
                balance: None,
                meters: vec![],
                detail: None,
            },
            ProviderUsage {
                provider: "minimax".into(),
                logged_in: true,
                windows: vec![UsageWindow {
                    label: "5-hour".into(),
                    used_percent: Some(41.0),
                    resets_at: None,
                }],
                balance: None,
                meters: vec![],
                detail: None,
                error: None,
            },
        ),
        (
            "reading balance + meters + detail pass through",
            UsageOutcome::Reading {
                windows: vec![],
                balance: Some(BillingBalance {
                    remaining: 12.34,
                    monthly_spend: Some(1.5),
                    currency: "USD".into(),
                }),
                meters: vec![UsageMeter::Metered {
                    amount: amount.clone(),
                }],
                detail: Some("tier name".into()),
            },
            ProviderUsage {
                provider: "cursor".into(),
                logged_in: true,
                windows: vec![],
                balance: Some(BillingBalance {
                    remaining: 12.34,
                    monthly_spend: Some(1.5),
                    currency: "USD".into(),
                }),
                meters: vec![UsageMeter::Metered {
                    amount: amount.clone(),
                }],
                detail: Some("tier name".into()),
                error: None,
            },
        ),
        (
            "no credential -> logged_out envelope (no meter sentinel)",
            UsageOutcome::NoCredential {
                hint: "Muse login missing. Run muse login again.".into(),
            },
            ProviderUsage {
                provider: "muse-code".into(),
                logged_in: false,
                windows: vec![],
                balance: None,
                meters: vec![],
                detail: None,
                error: Some("Muse login missing. Run muse login again.".into()),
            },
        ),
        (
            "rejected -> logged_out envelope",
            UsageOutcome::Rejected {
                hint: "Invalid API key".into(),
            },
            ProviderUsage {
                provider: "kimi".into(),
                logged_in: false,
                windows: vec![],
                balance: None,
                meters: vec![],
                detail: None,
                error: Some("Invalid API key".into()),
            },
        ),
        (
            "rate limited -> unavailable envelope with rate message",
            UsageOutcome::RateLimited {
                reason: "Rate limited — usage data temporarily unavailable".into(),
            },
            ProviderUsage {
                provider: "anthropic".into(),
                logged_in: true,
                windows: vec![],
                balance: None,
                meters: vec![],
                detail: None,
                error: Some("Rate limited — usage data temporarily unavailable".into()),
            },
        ),
        (
            "unavailable -> unavailable envelope with reason",
            UsageOutcome::Unavailable {
                reason: "API error 500: upstream down".into(),
            },
            ProviderUsage {
                provider: "codex".into(),
                logged_in: true,
                windows: vec![],
                balance: None,
                meters: vec![],
                detail: None,
                error: Some("API error 500: upstream down".into()),
            },
        ),
        (
            "degraded -> logged_in with detail (ADR-0026 sk-proj- contract)",
            UsageOutcome::Degraded {
                detail:
                    "Monthly spend tracking requires an Organization Admin API Key (sk-admin-...)"
                        .into(),
            },
            ProviderUsage {
                provider: "openai".into(),
                logged_in: true,
                windows: vec![],
                balance: None,
                meters: vec![],
                detail: Some(
                    "Monthly spend tracking requires an Organization Admin API Key (sk-admin-...)"
                        .into(),
                ),
                error: None,
            },
        ),
        (
            "managed externally -> managed_externally meter",
            UsageOutcome::ManagedExternally {
                platform: "AWS Bedrock".into(),
            },
            ProviderUsage {
                provider: "anthropic".into(),
                logged_in: true,
                windows: vec![],
                balance: None,
                meters: vec![UsageMeter::ManagedExternally {
                    platform: "AWS Bedrock".into(),
                }],
                detail: None,
                error: None,
            },
        ),
    ];

    for (name, outcome, expected) in cases {
        let usage = outcome.into_usage(&expected.provider);
        // Compare the serialized wire triple rather than a summary tuple, so
        // the check covers each field — a window's label and percentage, the
        // balance's amount and currency, the meter's amount — and the
        // projection is pinned at the bytes the panel receives.
        assert_eq!(
            serde_json::to_string(&usage).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "{name}: projection mismatch"
        );
    }
}

/// The projection must never emit both `error` and a `UsageMeter::Unavailable`.
/// That's the dead-branch class Muse Code's pre-#1745 envelope exposed
/// (the panel's error branch precedes the meters branch in render order).
/// Pin every variant against this invariant.
#[test]
fn projection_never_emits_error_and_unavailable_meter_together() {
    // Touch every variant of UsageOutcome — Reading is included because
    // a future adapter bug could construct one with both fields.
    let amount = UsageAmount {
        used: 0.0,
        limit: None,
        remaining: None,
        unit: "USD".into(),
        used_percent: None,
        resets_at: None,
    };
    let outcomes = vec![
        UsageOutcome::Reading {
            windows: vec![],
            balance: None,
            meters: vec![UsageMeter::Unavailable],
            detail: None,
        },
        UsageOutcome::NoCredential { hint: "x".into() },
        UsageOutcome::Rejected { hint: "x".into() },
        UsageOutcome::RateLimited { reason: "x".into() },
        UsageOutcome::Unavailable { reason: "x".into() },
        UsageOutcome::Degraded { detail: "x".into() },
        UsageOutcome::ManagedExternally {
            platform: "AWS Bedrock".into(),
        },
        UsageOutcome::Enriched {
            reading: Box::new(UsageOutcome::Reading {
                windows: vec![],
                balance: None,
                meters: vec![],
                detail: None,
            }),
        },
    ];
    for outcome in outcomes {
        let usage = outcome.into_usage("p");
        let has_unavailable_meter = usage
            .meters
            .iter()
            .any(|m| matches!(m, UsageMeter::Unavailable));
        let has_error = usage.error.is_some();
        assert!(
            !(has_error && has_unavailable_meter),
            "projection must never emit both error and UsageMeter::Unavailable; got: {usage:?}"
        );
        // Silence the unused warning on `amount` — it documents the
        // shape of a future Reading case but the Reading case here is
        // intentionally a degraded one without the amount field.
        let _ = amount;
    }
}

/// `keep` table: one row per variant × configured/unconfigured. The
/// gate in `assemble_meters` reads this directly after the seam change.
#[test]
fn keep_predicate_table() {
    let configured: HashSet<String> = ["kimi", "openai"].iter().map(|s| s.to_string()).collect();
    let unconfigured: HashSet<String> = HashSet::new();

    let cases = vec![
        // (variant, configured_set, id, expected_keep, name)
        (
            UsageOutcome::Reading {
                windows: vec![],
                balance: None,
                meters: vec![],
                detail: None,
            },
            &configured,
            "kimi",
            true,
            "Reading kept regardless of configured_keys",
        ),
        (
            UsageOutcome::NoCredential { hint: "x".into() },
            &configured,
            "kimi",
            false,
            "NoCredential always dropped (even when configured)",
        ),
        (
            UsageOutcome::NoCredential { hint: "x".into() },
            &unconfigured,
            "kimi",
            false,
            "NoCredential always dropped (unconfigured)",
        ),
        (
            UsageOutcome::Rejected { hint: "x".into() },
            &configured,
            "kimi",
            true,
            "Rejected kept when configured (Invalid API key affordance)",
        ),
        (
            UsageOutcome::Rejected { hint: "x".into() },
            &unconfigured,
            "kimi",
            false,
            "Rejected dropped when unconfigured",
        ),
        (
            UsageOutcome::RateLimited { reason: "x".into() },
            &configured,
            "kimi",
            true,
            "RateLimited kept regardless",
        ),
        (
            UsageOutcome::Unavailable { reason: "x".into() },
            &unconfigured,
            "kimi",
            true,
            "Unavailable kept regardless",
        ),
        (
            UsageOutcome::Degraded { detail: "x".into() },
            &configured,
            "kimi",
            true,
            "Degraded kept regardless",
        ),
        (
            UsageOutcome::ManagedExternally {
                platform: "AWS Bedrock".into(),
            },
            &configured,
            "kimi",
            true,
            "ManagedExternally kept regardless",
        ),
        (
            UsageOutcome::Enriched {
                reading: Box::new(UsageOutcome::Reading {
                    windows: vec![],
                    balance: None,
                    meters: vec![],
                    detail: None,
                }),
            },
            &configured,
            "kimi",
            true,
            "Enriched kept regardless",
        ),
        (
            UsageOutcome::Enriched {
                reading: Box::new(UsageOutcome::Rejected { hint: "x".into() }),
            },
            &unconfigured,
            "kimi",
            false,
            "Enriched wrapping a Rejected is dropped when unconfigured",
        ),
    ];

    for (outcome, configured_set, id, expected_keep, name) in cases {
        assert_eq!(outcome.keep(configured_set, id), expected_keep, "{name}");
    }
}

/// An enriched reading renders like the bare reading it wraps.
///
/// This is the guarantee issue #2528 step 1 rests on: making the
/// enrichment a first-class part of the outcome does not change a byte of
/// what the panel renders. The enrichment's contribution is merged into
/// the reading by the shared driver before the outcome is built, so the
/// wrapper is the only difference.
#[test]
fn enriched_reading_projects_identically_to_the_reading_it_wraps() {
    let reading = UsageOutcome::Reading {
        windows: vec![UsageWindow {
            label: "Monthly".into(),
            used_percent: Some(12.0),
            resets_at: None,
        }],
        balance: Some(BillingBalance {
            remaining: 4.25,
            monthly_spend: None,
            currency: "USD".into(),
        }),
        meters: vec![],
        detail: Some("Additional credits: USD 5.00".into()),
    };
    let enriched = UsageOutcome::Enriched {
        reading: Box::new(reading.clone()),
    };
    // Compared as serialized wire bytes: the check is on the panel's bytes,
    // not on the structs looking alike.
    assert_eq!(
        serde_json::to_string(&enriched.into_usage("commandcode")).unwrap(),
        serde_json::to_string(&reading.into_usage("commandcode")).unwrap(),
        "an enriched reading must project byte-identically to its inner reading"
    );
}

/// A reading stays a reading once it has been enriched, so the
/// last-known store records what the provider actually reported.
#[test]
fn is_reading_covers_enriched_readings() {
    let reading = UsageOutcome::Reading {
        windows: vec![],
        balance: None,
        meters: vec![],
        detail: None,
    };
    assert!(reading.is_reading());
    assert!(UsageOutcome::Enriched {
        reading: Box::new(reading),
    }
    .is_reading());
    assert!(!UsageOutcome::RateLimited { reason: "x".into() }.is_reading());
}

/// The transient classification is owned by the taxonomy, not by the
/// command layer's variant list.
///
/// A rate limit and an unavailable response are transient — the remembered
/// reading is preferred over the live error. Everything else is not. The
/// check looks through an enriched wrapper, so a provider that wraps a
/// failure in an enriched reading is classified by the outcome it wraps.
#[test]
fn is_transient_covers_the_failure_variants_only() {
    let reading = UsageOutcome::Reading {
        windows: vec![],
        balance: None,
        meters: vec![],
        detail: None,
    };
    let cases: Vec<(UsageOutcome, bool, &str)> = vec![
        (
            UsageOutcome::RateLimited {
                reason: "Rate limited".into(),
            },
            true,
            "a rate limit is transient",
        ),
        (
            UsageOutcome::Unavailable {
                reason: "Request failed".into(),
            },
            true,
            "an unavailable response is transient",
        ),
        (reading.clone(), false, "a reading is not transient"),
        (
            UsageOutcome::Enriched {
                reading: Box::new(reading),
            },
            false,
            "an enriched reading is not transient",
        ),
        (
            UsageOutcome::NoCredential {
                hint: "No API key configured".into(),
            },
            false,
            "a missing credential is not transient",
        ),
        (
            UsageOutcome::Rejected {
                hint: "Invalid API key".into(),
            },
            false,
            "a rejected credential is not transient",
        ),
        (
            UsageOutcome::Degraded {
                detail: "Billing route unavailable".into(),
            },
            false,
            "a degraded credential is not transient",
        ),
        (
            UsageOutcome::ManagedExternally {
                platform: "AWS Bedrock".into(),
            },
            false,
            "an externally managed platform is not transient",
        ),
    ];

    for (outcome, expected, name) in cases {
        assert_eq!(outcome.is_transient(), expected, "{name}");
    }
}

/// A reading has one projection, so an adapter encodes the same payload one
/// way.
#[test]
fn usage_reading_into_outcome_is_the_only_reading_projection() {
    let reading = UsageReading {
        windows: vec![UsageWindow {
            label: "Weekly".into(),
            used_percent: Some(50.0),
            resets_at: None,
        }],
        balance: None,
        meters: vec![],
        detail: Some("Pro".into()),
    };
    assert_eq!(
        reading.clone().into_outcome(),
        UsageOutcome::Reading {
            windows: reading.windows,
            balance: reading.balance,
            meters: reading.meters,
            detail: reading.detail,
        }
    );
}
