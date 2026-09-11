//! Muse Code subscription Usage Meter — local counting against Meta's published
//! static tier table. There is no account-level quota API; remaining allowance
//! is `tier_limit − local_count` (or unavailable when the user has not chosen
//! a tier). Never scrapes dashboards, reads credential files, or treats MSP
//! token events as quota.
//!
//! `record_turn` is the counting seam for a future MSP `turn/start`. Muse
//! currently has no attention hook, transcript, or MSP transport, so production
//! does not increment automatically.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{UsageAdapter, UsageIdentityFingerprint};
use crate::services::usage::types::{
    unavailable, MuseCodeTier, ProviderUsage, UsageAmount, UsageError, UsageMeter,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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
        let tier = selected_tier()
            .map(|tier| tier.label())
            .unwrap_or("unconfigured");
        UsageIdentityFingerprint::new("muse-code-tier", tier.as_bytes())
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        muse_code_usage()
    }
}

pub(crate) fn selected_tier() -> Option<MuseCodeTier> {
    with_store(|store| Ok((false, store.tier))).ok().flatten()
}

pub(crate) fn set_tier(tier: Option<MuseCodeTier>) -> Result<(), String> {
    with_store(|store| {
        store.tier = tier;
        Ok((true, ()))
    })
    .map_err(|e| store_error_message(&e))?;
    crate::services::usage::invalidate_provider_cache("muse-code");
    Ok(())
}

/// Count one Muse Code request against the current 5-hour window.
///
/// Automatic counting is blocked until MSP `turn/start` exists. Muse installs
/// no attention hook and produces no transcript, so nothing in production
/// currently calls this. Tests and a future MSP transport should.
pub(crate) fn record_turn() {
    if with_store(|store| {
        if store.tier.is_none() {
            return Ok((false, false));
        }
        let now = now_ms();
        reset_window_if_elapsed(store, now);
        if store.window_start_ms.is_none() {
            store.window_start_ms = Some(now);
        }
        store.count = store.count.saturating_add(1);
        Ok((true, true))
    })
    .ok()
    == Some(true)
    {
        crate::services::usage::invalidate_provider_cache("muse-code");
    }
}

fn muse_code_usage() -> ProviderUsage {
    match with_store(|store| {
        let now = now_ms();
        let dirty = reset_window_if_elapsed(store, now);
        Ok((dirty, usage_from_store(store)))
    }) {
        Ok(usage) => usage,
        Err(e) => unavailable("muse-code", store_error_message(&e)),
    }
}

/// Holds [`STORE_LOCK`] for the duration of `f`. Return `true` from `f` to
/// persist the mutated store. Nested store access must not call this again.
fn with_store<T>(f: impl FnOnce(&mut MuseCodeStore) -> Result<(bool, T), UsageError>) -> Result<T, UsageError> {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = read_store()?;
    let (dirty, value) = f(&mut store)?;
    if dirty {
        write_store(&store)?;
    }
    Ok(value)
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
    let used = store.count;
    let remaining = limit.saturating_sub(used.min(limit));
    let used_percent = if limit == 0 {
        None
    } else {
        Some((used as f64) * 100.0 / (limit as f64))
    };
    let resets_at = store.window_start_ms.and_then(|start| {
        chrono::DateTime::from_timestamp_millis(start.saturating_add(WINDOW_MS))
            .map(|dt| dt.to_rfc3339())
    });
    let exhausted = used >= limit;
    let amount = UsageAmount {
        used: used as f64,
        limit: Some(limit as f64),
        remaining: Some(remaining as f64),
        unit: UNIT.to_string(),
        used_percent,
        resets_at: resets_at.clone(),
    };
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
        // UsagePanel renders both `windows` and `meters`. Discrete request
        // counts live on UsageMeter::Metered only so the card is not stacked.
        windows: vec![],
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

fn store_error_message(error: &UsageError) -> String {
    match error {
        UsageError::Shape(msg) => msg.clone(),
        UsageError::NoCredential(msg) => format!("No credential found at {msg}"),
    }
}

fn store_io_error(op: &str, path: &Path, error: impl std::fmt::Display) -> UsageError {
    UsageError::Shape(format!("{op} {}: {error}", path.display()))
}

fn read_store() -> Result<MuseCodeStore, UsageError> {
    #[cfg(test)]
    if STORE_PATH.with(|cell| cell.borrow().is_none()) {
        return Ok(MEMORY_STORE.with(|cell| cell.borrow().clone()));
    }
    let path = store_path();
    if !path.exists() {
        return Ok(MuseCodeStore::default());
    }
    let content = fs::read_to_string(&path).map_err(|e| store_io_error("failed to read", &path, e))?;
    serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))
}

fn write_store(store: &MuseCodeStore) -> Result<(), UsageError> {
    #[cfg(test)]
    if STORE_PATH.with(|cell| cell.borrow().is_none()) {
        MEMORY_STORE.with(|cell| *cell.borrow_mut() = store.clone());
        return Ok(());
    }
    let path = store_path();
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).ok_or_else(|| {
        UsageError::Shape(format!("{} has no parent directory", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|e| store_io_error("failed to create", parent, e))?;
    let json = serde_json::to_string(store).map_err(|e| UsageError::Shape(e.to_string()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| store_io_error("failed to create temporary", parent, e))?;
    temporary
        .write_all(json.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|e| store_io_error("failed to write temporary", parent, e))?;
    temporary
        .persist(&path)
        .map_err(|e| store_io_error("failed to replace", &path, e.error))?;
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
                assert!(
                    usage.windows.is_empty(),
                    "metered request counts must not also emit a percentage window: {:?}",
                    usage.windows
                );
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
                assert_eq!(amount.used, 51.0);
                assert_eq!(amount.remaining, Some(0.0));
                assert_eq!(amount.limit, Some(50.0));
                assert!(usage.windows.is_empty());
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
    fn store_io_failure_stays_logged_in_and_is_not_a_missing_credential() {
        reset_for_tests();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("muse-code-usage.json");
        fs::create_dir_all(&path).unwrap();
        STORE_PATH.with(|cell| *cell.borrow_mut() = Some(path));
        NOW_MS.with(|cell| *cell.borrow_mut() = Some(T0));
        let _guard = EnvGuard;
        let usage = fetch();
        assert!(usage.logged_in, "disk failure must not look like logout: {usage:?}");
        let error = usage.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("failed to read"),
            "expected a store I/O error, got {error:?}"
        );
        assert!(
            !error.to_ascii_lowercase().contains("no credential"),
            "I/O must not masquerade as missing credentials, got {error:?}"
        );
        assert_eq!(selected_tier(), None);
    }

    #[test]
    fn selected_tier_round_trips_through_the_locked_disk_store() {
        reset_for_tests();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("muse-code-usage.json");
        STORE_PATH.with(|cell| *cell.borrow_mut() = Some(path.clone()));
        NOW_MS.with(|cell| *cell.borrow_mut() = Some(T0));
        let _guard = EnvGuard;
        set_tier(Some(MuseCodeTier::High)).unwrap();
        assert_eq!(selected_tier(), Some(MuseCodeTier::High));
        let usage = fetch();
        assert!(usage.windows.is_empty());
        assert_eq!(metered(&usage).limit, Some(150.0));
        let persisted = fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("high"), "got {persisted}");
    }
}
