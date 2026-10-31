//! Per-login Codex meter rows: which rows exist, their labels, their order,
//! and the same last-known rules the default row follows.

use super::*;
use crate::services::usage::types::UsageWindow;

fn meter_login(id: &str, label: &str) -> MeterLogin {
    MeterLogin {
        id: id.into(),
        label: label.into(),
        email: Some(format!("{id}@example.com")),
    }
}

fn usage(percent: f64) -> ProviderUsage {
    UsageOutcome::Reading {
        windows: vec![UsageWindow {
            label: "5-hour".into(),
            used_percent: Some(percent),
            resets_at: None,
        }],
        balance: None,
        meters: Vec::new(),
        detail: None,
    }
    .into_usage(LOGIN_PROVIDER)
}

fn reading(id: &str, outcome: UsageOutcome) -> LoginReading {
    let usage = outcome.clone().into_usage(LOGIN_PROVIDER);
    LoginReading {
        login: meter_login(id, id),
        outcome,
        usage,
    }
}

fn live(percent: f64) -> UsageOutcome {
    UsageOutcome::Reading {
        windows: vec![UsageWindow {
            label: "5-hour".into(),
            used_percent: Some(percent),
            resets_at: None,
        }],
        balance: None,
        meters: Vec::new(),
        detail: None,
    }
}

fn row(provider: &str, login: Option<MeterLogin>) -> ProviderMeters {
    ProviderMeters {
        provider: provider.into(),
        usage_page: None,
        usage_tracked: true,
        usage: Some(usage(1.0)),
        cached_at: None,
        login,
    }
}

fn window_percent(meter: &ProviderMeters) -> Option<f64> {
    meter.usage.as_ref()?.windows.first()?.used_percent
}

#[test]
fn a_signed_in_login_shows_its_own_live_reading_labelled_with_the_login() {
    let meter = login_row(reading("work", live(42.0)), &HashMap::new()).unwrap();
    assert_eq!(meter.provider, "codex");
    assert_eq!(meter.login, Some(meter_login("work", "work")));
    assert_eq!(window_percent(&meter), Some(42.0));
    assert_eq!(meter.cached_at, None);
    assert!(
        meter.usage_page.is_some(),
        "the Codex usage page link still applies"
    );
}

#[test]
fn a_login_that_never_signed_in_has_no_row_unless_one_was_remembered() {
    let missing = UsageOutcome::NoCredential {
        hint: "no auth.json".into(),
    };
    assert!(login_row(reading("work", missing.clone()), &HashMap::new()).is_none());

    let previous = HashMap::from([(
        last_known_key("work"),
        LastKnown {
            usage: usage(7.0),
            cached_at: 1_700_000_000,
        },
    )]);
    let meter = login_row(reading("work", missing), &previous).unwrap();
    assert_eq!(window_percent(&meter), Some(7.0));
    assert_eq!(meter.cached_at, Some(1_700_000_000));
}

#[test]
fn a_transient_failure_prefers_this_logins_remembered_reading_not_the_default() {
    let unavailable = UsageOutcome::Unavailable {
        reason: "timeout".into(),
    };
    let previous = HashMap::from([
        (
            "codex".to_string(),
            LastKnown {
                usage: usage(99.0),
                cached_at: 1,
            },
        ),
        (
            last_known_key("work"),
            LastKnown {
                usage: usage(11.0),
                cached_at: 2,
            },
        ),
    ]);
    let meter = login_row(reading("work", unavailable.clone()), &previous).unwrap();
    assert_eq!(window_percent(&meter), Some(11.0));
    assert_eq!(meter.cached_at, Some(2));

    // With nothing remembered for this login the live error shows.
    let meter = login_row(reading("home", unavailable), &previous).unwrap();
    assert_eq!(meter.cached_at, None);
}

#[test]
fn last_known_keys_keep_the_default_login_on_the_bare_provider_id() {
    assert_eq!(last_known_key("work"), "codex@work");
    assert_ne!(last_known_key("work"), LOGIN_PROVIDER);
}

#[test]
fn with_one_login_the_list_is_unchanged() {
    let rows = vec![row("anthropic", None), row("codex", None)];
    let merged = merge_login_rows(rows.clone(), meter_login("default", "Default"), Vec::new());
    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(|meter| meter.login.is_none()));
}

#[test]
fn additional_logins_follow_the_labelled_default_row() {
    let rows = vec![
        row("anthropic", None),
        row("codex", None),
        row("minimax", None),
    ];
    let merged = merge_login_rows(
        rows,
        meter_login("default", "Default"),
        vec![
            row("codex", Some(meter_login("work", "Work"))),
            row("codex", Some(meter_login("home", "Home"))),
        ],
    );
    let order: Vec<(String, Option<String>)> = merged
        .iter()
        .map(|meter| {
            (
                meter.provider.clone(),
                meter.login.as_ref().map(|l| l.label.clone()),
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            ("anthropic".into(), None),
            ("codex".into(), Some("Default".into())),
            ("codex".into(), Some("Work".into())),
            ("codex".into(), Some("Home".into())),
            ("minimax".into(), None),
        ]
    );
}

#[test]
fn additional_logins_still_show_when_the_default_row_was_dropped() {
    let merged = merge_login_rows(
        vec![row("anthropic", None)],
        meter_login("default", "Default"),
        vec![row("codex", Some(meter_login("work", "Work")))],
    );
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[1].login.as_ref().unwrap().id, "work");
}
