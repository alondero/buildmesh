//! Muse Code subscription Usage Meter — local counting against Meta's published
//! static tier table. There is no account-level quota API; remaining allowance
//! is `tier_limit − local_count` (or unavailable when the user has not chosen
//! a tier). Never scrapes dashboards, reads credential files, or treats MSP
//! token events as quota.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{UsageAdapter, UsageIdentityFingerprint};
use crate::services::usage::types::{
    unavailable, MuseCodeTier, ProviderUsage, UsageAmount, UsageError, UsageMeter, UsageWindow,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

/// Drop-in [`UsageAdapter`] for `muse-code`.
pub(crate) struct MuseCodeAdapter;

/// Published 5-hour request window from developer.meta.com/ai/products/muse-code.
const WINDOW_MS: i64 = 5 * 60 * 60 * 1000;
const STORE_FILE: &str = "muse-code-usage.json";
const UNIT: &str = "requests";

static STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct MuseCodeStore {
    #[serde(default)]
    tier: Option<MuseCodeTier>,
    #[serde(default)]
    window_start_ms: Option<i64>,
    #[serde(default)]
    count: u32,
}

impl UsageAdapter for MuseCodeAdapter {
    fn id(&self) -> &'static str {
        "muse-code"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("muse")
    }

    fn cache_identity(&self, _accounts: &[ProviderAccount]) -> UsageIdentityFingerprint {
        let tier = load_store()
            .ok()
            .and_then(|store| store.tier)
            .map(|tier| tier.label())
            .unwrap_or("unconfigured");
        UsageIdentityFingerprint::new("muse-code-tier", tier.as_bytes())
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        muse_code_usage()
    }
}

pub(crate) fn selected_tier() -> Option<MuseCodeTier> {
    load_store().ok().and_then(|store| store.tier)
}

pub(crate) fn set_tier(tier: Option<MuseCodeTier>) -> Result<(), String> {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load_store().unwrap_or_default();
    store.tier = tier;
    save_store(&store).map_err(|e| e.to_string())?;
    crate::services::usage::invalidate_provider_cache("muse-code");
    Ok(())
}

/// Count one Muse Code request against the current 5-hour window.
pub(crate) fn record_turn() {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(mut store) = load_store() else {
        return;
    };
    if store.tier.is_none() {
        return;
    }
    let now = now_ms();
    reset_window_if_elapsed(&mut store, now);
    if store.window_start_ms.is_none() {
        store.window_start_ms = Some(now);
    }
    store.count = store.count.saturating_add(1);
    let _ = save_store(&store);
    crate::services::usage::invalidate_provider_cache("muse-code");
}

/// `TurnCompleted` is the observable request boundary until MSP `turn/start`
/// is wired. Provider ids are harness/spawn-option strings, including WSL
/// profiles such as `muse-wsl-…`.
pub(crate) fn record_turn_for_provider(provider: Option<&str>) {
    if is_muse_provider(provider) {
        record_turn();
    }
}

fn is_muse_provider(provider: Option<&str>) -> bool {
    provider.is_some_and(|id| {
        id == "muse" || id.starts_with("muse-") || id.ends_with(":muse") || id.contains(":muse-")
    })
}

fn muse_code_usage() -> ProviderUsage {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = match load_store() {
        Ok(store) => store,
        Err(e) => return unavailable("muse-code", e.to_string()),
    };
    let now = now_ms();
    if reset_window_if_elapsed(&mut store, now) {
        let _ = save_store(&store);
    }
    usage_from_store(&store)
}

fn usage_from_store(store: &MuseCodeStore) -> ProviderUsage {
    let Some(tier) = store.tier else {
        return ProviderUsage {
            provider: "muse-code".to_string(),
            logged_in: true,
            windows: vec![],
            balance: None,
            meters: vec![UsageMeter::Unavailable],
            detail: Some(
                "Select a Muse Code subscription plan in Settings to track remaining requests."
                    .to_string(),
            ),
            error: None,
        };
    };

    let limit = tier.request_limit();
    let used = store.count.min(limit);
    let remaining = limit.saturating_sub(used);
    let used_percent = if limit == 0 {
        None
    } else {
        Some((used as f64) * 100.0 / (limit as f64))
    };
    let resets_at = store.window_start_ms.and_then(|start| {
        chrono::DateTime::from_timestamp_millis(start.saturating_add(WINDOW_MS))
            .map(|dt| dt.to_rfc3339())
    });
    let exhausted = store.count >= limit;
    let amount = UsageAmount {
        used: used as f64,
        limit: Some(limit as f64),
        remaining: Some(remaining as f64),
        unit: UNIT.to_string(),
        used_percent,
        resets_at: resets_at.clone(),
    };
    let window_label = format!("{} · 5-hour", tier.label());
    let detail = if exhausted {
        match &resets_at {
            Some(when) => Some(format!(
                "{} allowance exhausted. Further Muse Code turns wait until the 5-hour window resets at {when}.",
                tier.label()
            )),
            None => Some(format!(
                "{} allowance exhausted. Further Muse Code turns wait until the 5-hour window resets.",
                tier.label()
            )),
        }
    } else {
        None
    };

    ProviderUsage {
        provider: "muse-code".to_string(),
        logged_in: true,
        windows: vec![UsageWindow {
            label: window_label,
            used_percent,
            resets_at,
        }],
        balance: None,
        meters: vec![UsageMeter::Metered { amount }],
        detail,
        error: None,
    }
}

fn reset_window_if_elapsed(store: &mut MuseCodeStore, now: i64) -> bool {
    let Some(start) = store.window_start_ms else {
        return false;
    };
    if now.saturating_sub(start) < WINDOW_MS {
        return false;
    }
    store.count = 0;
    store.window_start_ms = None;
    true
}

fn now_ms() -> i64 {
    #[cfg(test)]
    if let Some(ms) = NOW_MS.with(|cell| *cell.borrow()) {
        return ms;
    }
    chrono::Utc::now().timestamp_millis()
}

fn store_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = STORE_PATH.with(|cell| cell.borrow().clone()) {
        return path;
    }
    crate::preferences::app_data_dir()
        .unwrap_or_default()
        .join(STORE_FILE)
}

fn load_store() -> Result<MuseCodeStore, UsageError> {
    #[cfg(test)]
    if STORE_PATH.with(|cell| cell.borrow().is_none()) {
        return Ok(MEMORY_STORE.with(|cell| cell.borrow().clone()));
    }
    let path = store_path();
    if !path.exists() {
        return Ok(MuseCodeStore::default());
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| UsageError::NoCredential(format!("{} ({e})", path.display())))?;
    serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))
}

fn save_store(store: &MuseCodeStore) -> Result<(), UsageError> {
    #[cfg(test)]
    if STORE_PATH.with(|cell| cell.borrow().is_none()) {
        MEMORY_STORE.with(|cell| *cell.borrow_mut() = store.clone());
        return Ok(());
    }
    let path = store_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| UsageError::NoCredential(format!("{} ({e})", parent.display())))?;
    }
    let json = serde_json::to_string(store).map_err(|e| UsageError::Shape(e.to_string()))?;
    fs::write(&path, json)
        .map_err(|e| UsageError::NoCredential(format!("{} ({e})", path.display())))?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static MEMORY_STORE: std::cell::RefCell<MuseCodeStore> =
        const { std::cell::RefCell::new(MuseCodeStore { tier: None, window_start_ms: None, count: 0 }) };
    static NOW_MS: std::cell::RefCell<Option<i64>> = const { std::cell::RefCell::new(None) };
    static STORE_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct EnvGuard;

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        reset_for_tests();
    }
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    MEMORY_STORE.with(|cell| *cell.borrow_mut() = MuseCodeStore::default());
    NOW_MS.with(|cell| *cell.borrow_mut() = None);
    STORE_PATH.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(test)]
fn with_env(now_ms: i64, store: MuseCodeStore, f: impl FnOnce()) {
    reset_for_tests();
    NOW_MS.with(|cell| *cell.borrow_mut() = Some(now_ms));
    MEMORY_STORE.with(|cell| *cell.borrow_mut() = store);
    let _guard = EnvGuard;
    f();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::usage::catalog;

    const T0: i64 = 1_700_000_000_000;

    fn fetch() -> ProviderUsage {
        catalog::dispatch("muse-code")
            .expect("muse-code adapter registered")
            .fetch(&[])
    }

    fn metered(usage: &ProviderUsage) -> &UsageAmount {
        match usage.meters.as_slice() {
            [UsageMeter::Metered { amount }] => amount,
            other => panic!("expected a single metered reading, got {other:?}"),
        }
    }

    #[test]
    fn unconfigured_tier_is_unavailable_not_zero_or_unlimited() {
        with_env(T0, MuseCodeStore::default(), || {
            let usage = fetch();
            assert_eq!(usage.provider, "muse-code");
            assert!(usage.logged_in);
            assert!(usage.error.is_none());
            assert!(usage.windows.is_empty());
            assert_eq!(usage.meters, vec![UsageMeter::Unavailable]);
            assert!(
                usage
                    .detail
                    .as_deref()
                    .is_some_and(|d| d.contains("Select a Muse Code subscription plan")),
                "unconfigured detail, got {:?}",
                usage.detail
            );
            assert!(
                !usage.meters.iter().any(|m| matches!(
                    m,
                    UsageMeter::Unlimited | UsageMeter::Metered { .. }
                )),
                "unconfigured must not invent zero or unlimited quota: {:?}",
                usage.meters
            );
        });
    }

    #[test]
    fn everyday_tier_reports_full_allowance_before_any_turn() {
        with_env(
            T0,
            MuseCodeStore {
                tier: Some(MuseCodeTier::Everyday),
                ..MuseCodeStore::default()
            },
            || {
                let usage = fetch();
                let amount = metered(&usage);
                assert_eq!(amount.used, 0.0);
                assert_eq!(amount.limit, Some(50.0));
                assert_eq!(amount.remaining, Some(50.0));
                assert_eq!(amount.unit, "requests");
                assert_eq!(amount.used_percent, Some(0.0));
                assert!(amount.resets_at.is_none());
                assert_eq!(usage.windows[0].label, "Everyday · 5-hour");
                assert!(usage.detail.is_none());
            },
        );
    }

    #[test]
    fn high_and_power_limits_are_three_and_ten_times_everyday() {
        with_env(
            T0,
            MuseCodeStore {
                tier: Some(MuseCodeTier::High),
                ..MuseCodeStore::default()
            },
            || {
                let usage = fetch();
                assert_eq!(metered(&usage).limit, Some(150.0));
            },
        );
        with_env(
            T0,
            MuseCodeStore {
                tier: Some(MuseCodeTier::Power),
                ..MuseCodeStore::default()
            },
            || {
                let usage = fetch();
                assert_eq!(metered(&usage).limit, Some(500.0));
            },
        );
    }

    #[test]
    fn each_turn_decrements_remaining_requests() {
        with_env(
            T0,
            MuseCodeStore {
                tier: Some(MuseCodeTier::Everyday),
                ..MuseCodeStore::default()
            },
            || {
                record_turn();
                record_turn();
                record_turn();
                let usage = fetch();
                let amount = metered(&usage);
                assert_eq!(amount.used, 3.0);
                assert_eq!(amount.remaining, Some(47.0));
                assert_eq!(amount.used_percent, Some(6.0));
                assert!(amount.resets_at.is_some());
            },
        );
    }

    #[test]
    fn window_elapse_restores_full_allowance() {
        with_env(
            T0,
            MuseCodeStore {
                tier: Some(MuseCodeTier::Everyday),
                window_start_ms: Some(T0),
                count: 12,
            },
            || {
                NOW_MS.with(|cell| *cell.borrow_mut() = Some(T0 + WINDOW_MS));
                let usage = fetch();
                let amount = metered(&usage);
                assert_eq!(amount.used, 0.0);
                assert_eq!(amount.remaining, Some(50.0));
                assert!(amount.resets_at.is_none());
            },
        );
    }

    #[test]
    fn exceeding_allowance_clamps_remaining_and_sets_exhausted_notice() {
        with_env(
            T0,
            MuseCodeStore {
                tier: Some(MuseCodeTier::Everyday),
                window_start_ms: Some(T0),
                count: 50,
            },
            || {
                record_turn();
                let usage = fetch();
                let amount = metered(&usage);
                assert_eq!(amount.used, 50.0);
                assert_eq!(amount.remaining, Some(0.0));
                assert_eq!(amount.limit, Some(50.0));
                let detail = usage.detail.as_deref().expect("exhausted notice");
                assert!(
                    detail.contains("Everyday allowance exhausted"),
                    "got {detail}"
                );
                assert!(detail.contains("5-hour"), "got {detail}");
                assert!(amount.resets_at.is_some());
            },
        );
    }

    #[test]
    fn turns_are_ignored_until_a_tier_is_selected() {
        with_env(T0, MuseCodeStore::default(), || {
            record_turn();
            record_turn();
            assert_eq!(fetch().meters, vec![UsageMeter::Unavailable]);
            set_tier(Some(MuseCodeTier::Everyday)).unwrap();
            let usage = fetch();
            let amount = metered(&usage);
            assert_eq!(amount.used, 0.0);
            assert_eq!(amount.remaining, Some(50.0));
        });
    }

    #[test]
    fn muse_code_identity_is_not_a_payg_key_meter() {
        let adapter = catalog::dispatch("muse-code").expect("registered");
        assert_eq!(adapter.id(), "muse-code");
        assert_eq!(adapter.native_harness(), Some("muse"));
        assert!(catalog::dispatch("meta-model-api").is_none());
    }

    #[test]
    fn record_turn_for_provider_counts_muse_harness_ids_only() {
        with_env(
            T0,
            MuseCodeStore {
                tier: Some(MuseCodeTier::Everyday),
                ..MuseCodeStore::default()
            },
            || {
                record_turn_for_provider(Some("codex"));
                record_turn_for_provider(Some("muse"));
                record_turn_for_provider(Some("muse-wsl-ubuntu"));
                record_turn_for_provider(None);
                let usage = fetch();
                assert_eq!(metered(&usage).used, 2.0);
            },
        );
    }
}
