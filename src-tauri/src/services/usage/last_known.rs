//! Durable "last known" Usage Meter readings.
//!
//! The in-process cache in [`super::cache`] only keeps a reading fresh for five
//! minutes, and dies with the process. Several providers (Antigravity, Grok,
//! Muse Code) only report usage while their harness credential is fresh, so the
//! common case is: Buildmesh starts for the day, none of those harnesses have
//! been logged into yet, every fetch fails, and their meters vanish.
//!
//! This module keeps the last reading each provider *did* report on disk for
//! [`LAST_KNOWN_TTL`], so a fetch that cannot produce a reading can fall back to
//! the last one instead of hiding the row. Nothing here ever gates or discards a
//! live reading (issue #1073): a missing or corrupt cache file just means "no
//! fallback", never an error.
//!
//! **Keyed by provider id, not by [`super::adapter::UsageIdentityFingerprint`].**
//! The fingerprint is salted with a per-process random value, so it cannot key an
//! on-disk cache; and `MuseCodeAdapter::cache_identity` hashes the access token
//! itself, so a token rotation — the very situation we are falling back *from* —
//! would miss. Provider id also matches the row granularity the Usage tab
//! renders. See ADR-0037.

use super::types::ProviderUsage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a remembered reading stays usable as a fallback. Measured from the
/// last successful fetch, so a provider polled daily never expires.
pub(crate) const LAST_KNOWN_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Cache file name, alongside `preferences.json` in the app data dir.
const CACHE_FILE: &str = "usage_last_known.json";

/// Bumped if the on-disk shape ever changes incompatibly. Unknown fields and
/// unknown versions are tolerated on read (the cache is disposable).
const CACHE_VERSION: u32 = 1;

/// One remembered reading, plus when the provider actually reported it.
#[derive(Debug, Clone)]
pub(crate) struct LastKnown {
    pub(crate) usage: ProviderUsage,
    /// Epoch seconds the reading was fetched from the provider.
    pub(crate) cached_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    #[serde(rename = "cachedAt")]
    cached_at: i64,
    usage: ProviderUsage,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileShape {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: HashMap<String, Entry>,
}

#[derive(Default)]
struct State {
    loaded: bool,
    /// The directory `entries` was read from. Compared against the currently
    /// resolved directory so a late `preferences::init` still triggers a load.
    loaded_dir: Option<PathBuf>,
    entries: HashMap<String, Entry>,
}

struct Inner {
    /// Resolved per call rather than at construction: `tauri::Builder::manage`
    /// runs before `setup()` wires `APP_DATA_DIR`, so the directory has to be
    /// resolved lazily. Tests inject a fixed directory.
    dir_fn: Box<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    now_fn: Box<dyn Fn() -> i64 + Send + Sync>,
    state: Mutex<State>,
}

/// Durable store of the last reading each provider reported. Injectable (and
/// `Clone` so it can be moved into `run_blocking` / `spawn_blocking`) per the
/// repo's injectable-cache convention — no process-global static.
#[derive(Clone)]
pub(crate) struct UsageLastKnownCache(Arc<Inner>);

impl UsageLastKnownCache {
    fn new_inner(
        dir_fn: Box<dyn Fn() -> Option<PathBuf> + Send + Sync>,
        now_fn: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self(Arc::new(Inner {
            dir_fn,
            now_fn,
            state: Mutex::new(State::default()),
        }))
    }

    /// Production constructor: the app data dir and the real clock.
    pub(crate) fn new() -> Self {
        Self::new_inner(
            Box::new(crate::preferences::app_data_dir),
            Box::new(now_epoch_secs),
        )
    }

    /// Test fixture pinned to `dir` with an injected clock (epoch seconds). Two
    /// instances over the same `dir` model two app runs.
    #[cfg(test)]
    pub(crate) fn for_test(
        dir: PathBuf,
        now_fn: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self::new_inner(Box::new(move || Some(dir.clone())), Box::new(now_fn))
    }

    /// Test fixture with no directory, i.e. the in-memory-only path used before
    /// `preferences::init` runs.
    #[cfg(test)]
    pub(crate) fn for_test_in_memory() -> Self {
        Self::new_inner(Box::new(|| None), Box::new(now_epoch_secs))
    }

    /// Remember the readings this round actually fetched. Best-effort: a write
    /// failure is logged and dropped — the live result is unaffected.
    pub(crate) fn record_many(&self, readings: &[(String, ProviderUsage)]) {
        let usable: Vec<(&str, &ProviderUsage)> = readings
            .iter()
            .filter(|(_, usage)| is_worth_remembering(usage))
            .map(|(provider, usage)| (provider.as_str(), usage))
            .collect();
        if usable.is_empty() {
            return;
        }
        let now = (self.0.now_fn)();
        // Resolved before locking so this never nests inside the preferences
        // mutex `app_data_dir()` takes.
        let dir = (self.0.dir_fn)();
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        Self::ensure_loaded(&mut state, dir.as_deref(), now);
        prune(&mut state.entries, now);
        for (provider, usage) in usable {
            state.entries.insert(
                provider.to_string(),
                Entry {
                    cached_at: now,
                    usage: usage.clone(),
                },
            );
        }
        // Held across the write: two concurrent commands must not race a
        // read-modify-write and clobber each other's entry.
        if let Some(dir) = dir.as_deref() {
            if let Err(e) = write_to_disk(dir, &state.entries) {
                tracing::warn!("usage last-known cache write failed: {e}");
            }
        }
    }

    /// Every unexpired remembered reading, keyed by provider id. Loads from disk
    /// on first use, so a cold start sees the previous run's readings before any
    /// fetch has happened this session. Expired entries are dropped.
    pub(crate) fn snapshot(&self) -> HashMap<String, LastKnown> {
        let now = (self.0.now_fn)();
        let dir = (self.0.dir_fn)();
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        Self::ensure_loaded(&mut state, dir.as_deref(), now);
        prune(&mut state.entries, now);
        state
            .entries
            .iter()
            .map(|(provider, entry)| {
                (
                    provider.clone(),
                    LastKnown {
                        usage: entry.usage.clone(),
                        cached_at: entry.cached_at,
                    },
                )
            })
            .collect()
    }

    fn ensure_loaded(state: &mut State, dir: Option<&Path>, now: i64) {
        if state.loaded && state.loaded_dir.as_deref() == dir {
            return;
        }
        state.entries = dir.map(read_from_disk).unwrap_or_default();
        state.loaded = true;
        state.loaded_dir = dir.map(Path::to_path_buf);
        prune(&mut state.entries, now);
    }
}

impl Default for UsageLastKnownCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Only a real reading with something to render is worth restoring. A
/// `logged_in: false` (or error-carrying) envelope is a failure projection, and a
/// reading with no windows, no balance, and no meters would render "Unavailable"
/// where the row is hidden today.
fn is_worth_remembering(usage: &ProviderUsage) -> bool {
    usage.logged_in
        && usage.error.is_none()
        && (!usage.windows.is_empty() || usage.balance.is_some() || !usage.meters.is_empty())
}

fn prune(entries: &mut HashMap<String, Entry>, now: i64) {
    let cutoff = now - LAST_KNOWN_TTL.as_secs() as i64;
    entries.retain(|_, entry| entry.cached_at >= cutoff);
}

fn read_from_disk(dir: &Path) -> HashMap<String, Entry> {
    let path = dir.join(CACHE_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        // Absent is the normal first-run case, not a problem worth logging.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(e) => {
            tracing::warn!("usage last-known cache unreadable, ignoring: {e}");
            return HashMap::new();
        }
    };
    match serde_json::from_str::<FileShape>(&raw) {
        Ok(file) => file.entries,
        Err(e) => {
            tracing::warn!("usage last-known cache is not valid JSON, ignoring: {e}");
            HashMap::new()
        }
    }
}

fn write_to_disk(dir: &Path, entries: &HashMap<String, Entry>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create app data dir: {e}"))?;
    let file = FileShape {
        version: CACHE_VERSION,
        entries: entries.clone(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("failed to serialize usage last-known cache: {e}"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| format!("failed to create temporary usage last-known file: {e}"))?;
    temporary
        .write_all(json.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|e| format!("failed to write temporary usage last-known file: {e}"))?;
    let target = dir.join(CACHE_FILE);
    temporary
        .persist(&target)
        .map_err(|e| format!("failed to atomically replace {CACHE_FILE}: {}", e.error))?;
    Ok(())
}

pub(crate) fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::usage::types::UsageWindow;

    const DAY: i64 = 24 * 60 * 60;

    fn reading(provider: &str, used_percent: f64) -> ProviderUsage {
        ProviderUsage {
            provider: provider.to_string(),
            logged_in: true,
            windows: vec![UsageWindow {
                label: "5-hour".to_string(),
                used_percent: Some(used_percent),
                resets_at: None,
            }],
            balance: None,
            meters: vec![],
            detail: None,
            error: None,
        }
    }

    fn temp_dir() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("temp dir")
    }

    #[test]
    fn records_and_serves_a_reading_with_its_timestamp() {
        let dir = temp_dir();
        let cache = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 1_000_000);
        cache.record_many(&[("agy".to_string(), reading("agy", 41.0))]);

        let snapshot = cache.snapshot();
        let last = snapshot.get("agy").expect("agy reading remembered");
        assert_eq!(last.cached_at, 1_000_000);
        assert_eq!(last.usage.windows[0].used_percent, Some(41.0));
        assert!(last.usage.logged_in);
    }

    #[test]
    fn a_reading_older_than_the_ttl_is_not_served() {
        let dir = temp_dir();
        let ttl_days = (LAST_KNOWN_TTL.as_secs() / DAY as u64) as i64;
        assert_eq!(ttl_days, 7, "TTL is the documented seven days");

        let cache = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 0);
        cache.record_many(&[("muse-code".to_string(), reading("muse-code", 12.0))]);

        let just_inside = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 7 * DAY - 1);
        assert!(just_inside.snapshot().contains_key("muse-code"));

        let just_outside = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 7 * DAY + 1);
        assert!(
            !just_outside.snapshot().contains_key("muse-code"),
            "an entry past the 7-day TTL must not be served"
        );
    }

    #[test]
    fn a_fresh_fetch_refreshes_the_reading_and_its_timestamp() {
        let dir = temp_dir();
        let first = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 0);
        first.record_many(&[("grok".to_string(), reading("grok", 10.0))]);

        let second = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 3 * DAY);
        second.record_many(&[("grok".to_string(), reading("grok", 55.0))]);

        let last = second.snapshot();
        let grok = last.get("grok").expect("grok reading remembered");
        assert_eq!(grok.cached_at, 3 * DAY);
        assert_eq!(grok.usage.windows[0].used_percent, Some(55.0));
    }

    /// The load-bearing case: Buildmesh restarts, the harness has not been logged
    /// into yet today, so nothing is fetched — the previous run's reading must
    /// still be available as a fallback.
    #[test]
    fn a_new_instance_serves_the_previous_runs_reading_without_fetching() {
        let dir = temp_dir();
        let day_one = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 0);
        day_one.record_many(&[("agy".to_string(), reading("agy", 88.0))]);

        // Second "app run", one day later, performs no successful fetch at all.
        let day_two = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || DAY);
        let snapshot = day_two.snapshot();
        let agy = snapshot.get("agy").expect("cold start serves the cached reading");
        assert_eq!(agy.cached_at, 0, "timestamp is when it was fetched, not now");
        assert_eq!(agy.usage.windows[0].used_percent, Some(88.0));
    }

    #[test]
    fn skips_failure_envelopes() {
        let dir = temp_dir();
        let cache = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 0);

        let mut logged_out = reading("agy", 5.0);
        logged_out.logged_in = false;
        let mut errored = reading("grok", 5.0);
        errored.error = Some("Request failed".to_string());

        cache.record_many(&[
            ("agy".to_string(), logged_out),
            ("grok".to_string(), errored),
        ]);
        assert!(cache.snapshot().is_empty());
    }

    #[test]
    fn skips_readings_with_nothing_to_render() {
        let dir = temp_dir();
        let cache = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 0);

        let mut empty = reading("cursor", 0.0);
        empty.windows.clear();
        cache.record_many(&[("cursor".to_string(), empty)]);

        assert!(
            cache.snapshot().is_empty(),
            "a valueless reading would render 'Unavailable'; don't restore it"
        );
    }

    #[test]
    fn separate_providers_are_stored_independently() {
        let dir = temp_dir();
        let cache = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 0);
        cache.record_many(&[
            ("agy".to_string(), reading("agy", 41.0)),
            ("grok".to_string(), reading("grok", 7.0)),
        ]);

        let snapshot = cache.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot["agy"].usage.windows[0].used_percent, Some(41.0));
        assert_eq!(snapshot["grok"].usage.windows[0].used_percent, Some(7.0));
    }

    #[test]
    fn a_corrupt_file_reads_as_empty_and_is_then_overwritten() {
        let dir = temp_dir();
        std::fs::write(dir.path().join(CACHE_FILE), "{not json").unwrap();

        let cache = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 0);
        assert!(cache.snapshot().is_empty(), "corrupt file must not panic");

        cache.record_many(&[("agy".to_string(), reading("agy", 33.0))]);
        let reloaded = UsageLastKnownCache::for_test(dir.path().to_path_buf(), || 0);
        assert_eq!(
            reloaded.snapshot()["agy"].usage.windows[0].used_percent,
            Some(33.0)
        );
    }

    #[test]
    fn without_a_directory_it_stays_in_memory() {
        let cache = UsageLastKnownCache::for_test_in_memory();
        cache.record_many(&[("agy".to_string(), reading("agy", 41.0))]);
        assert!(cache.snapshot().contains_key("agy"));
    }
}
