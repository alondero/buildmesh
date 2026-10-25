//! Single-flight Codex install cache, freshness, and runtime identity.

use super::*;
// Bound stale executable/version metadata after an out-of-process CLI update.
pub(super) const CODEX_INSTALL_CACHE_TTL: Duration = Duration::from_secs(45);

pub(super) struct CachedCodexInstall {
    pub(super) resolved_at: Instant,
    pub(super) result: Result<CodexInstall, String>,
}

pub(super) struct CodexInstallCacheSlot {
    pub(super) cell: Arc<OnceLock<CachedCodexInstall>>,
    pub(super) refreshing: bool,
}

#[derive(Default)]
pub(super) struct CodexInstallCache {
    pub(super) entries: Mutex<HashMap<&'static str, CodexInstallCacheSlot>>,
}

impl CodexInstallCache {
    pub(super) fn discover(
        &self,
        env_type: EnvType,
        probe: impl FnOnce() -> Result<CodexInstall, String>,
    ) -> Result<CodexInstall, String> {
        self.discover_at(env_type, Instant::now(), probe)
    }

    pub(super) fn discover_at(
        &self,
        env_type: EnvType,
        now: Instant,
        probe: impl FnOnce() -> Result<CodexInstall, String>,
    ) -> Result<CodexInstall, String> {
        let runtime = runtime_identity(env_type);
        let cell = {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            match entries.get(runtime) {
                Some(slot)
                    if slot
                        .cell
                        .get()
                        .is_none_or(|cached| is_entry_fresh(cached, now)) =>
                {
                    Arc::clone(&slot.cell)
                }
                _ => {
                    let cell = Arc::new(OnceLock::new());
                    entries.insert(
                        runtime,
                        CodexInstallCacheSlot {
                            cell: Arc::clone(&cell),
                            refreshing: false,
                        },
                    );
                    cell
                }
            }
        };

        let result = resolve_codex_install_cell(&cell, probe);
        if result.is_err() {
            self.discard_failed_entry(runtime, &cell);
        }
        result
    }

    pub(super) fn discover_fresh(
        &self,
        env_type: EnvType,
        probe: impl FnOnce() -> Result<CodexInstall, String>,
    ) -> Result<CodexInstall, String> {
        let runtime = runtime_identity(env_type);
        let cell = {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            match entries.get_mut(runtime) {
                Some(slot) if slot.refreshing => Arc::clone(&slot.cell),
                _ => {
                    let cell = Arc::new(OnceLock::new());
                    entries.insert(
                        runtime,
                        CodexInstallCacheSlot {
                            cell: Arc::clone(&cell),
                            refreshing: true,
                        },
                    );
                    cell
                }
            }
        };

        let result = resolve_codex_install_cell(&cell, probe);
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if entries
            .get(runtime)
            .is_some_and(|slot| Arc::ptr_eq(&slot.cell, &cell))
        {
            if result.is_err() {
                entries.remove(runtime);
            } else if let Some(slot) = entries.get_mut(runtime) {
                slot.refreshing = false;
            }
        }
        result
    }

    /// Whether the next [`CodexInstallCache::discover`] for this runtime
    /// would reuse a cached install instead of probing (issue #1948).
    /// Read-only: never inserts, never probes. An in-flight probe started
    /// by another thread reports `false` — joining it still waits on
    /// process work, so for timing attribution it is cold, not cached.
    /// Same freshness predicate as `discover_at`, so the two never disagree
    /// on what counts as cached.
    pub(super) fn is_fresh(&self, env_type: EnvType) -> bool {
        self.is_fresh_at(env_type, Instant::now())
    }

    pub(super) fn is_fresh_at(&self, env_type: EnvType, now: Instant) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.get(runtime_identity(env_type)).is_some_and(|slot| {
            slot.cell
                .get()
                .is_some_and(|cached| is_entry_fresh(cached, now))
        })
    }

    pub(super) fn discard_failed_entry(
        &self,
        runtime: &'static str,
        cell: &Arc<OnceLock<CachedCodexInstall>>,
    ) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if entries
            .get(runtime)
            .is_some_and(|slot| Arc::ptr_eq(&slot.cell, cell))
        {
            entries.remove(runtime);
        }
    }
}

/// Freshness predicate shared by [`CodexInstallCache::discover_at`] (cache
/// reuse) and [`CodexInstallCache::is_fresh_at`] (cold-vs-warm reporting) so
/// the two can never disagree on what counts as cached (issue #1948).
/// `now` is read before the cache lock, so a concurrent resolution can be newer
/// than it; that entry's age is zero, not unknown.
pub(super) fn is_entry_fresh(cached: &CachedCodexInstall, now: Instant) -> bool {
    now.saturating_duration_since(cached.resolved_at) < CODEX_INSTALL_CACHE_TTL
}

pub(super) fn resolve_codex_install_cell(
    cell: &OnceLock<CachedCodexInstall>,
    probe: impl FnOnce() -> Result<CodexInstall, String>,
) -> Result<CodexInstall, String> {
    cell.get_or_init(|| {
        let result = probe();
        CachedCodexInstall {
            resolved_at: Instant::now(),
            result,
        }
    })
    .result
    .clone()
}

/// Keep Settings reads fast without hiding a Codex CLI update for long: Codex
/// can update independently while Buildmesh stays open. Per-runtime OnceLocks
/// share one in-flight probe, while this map mutex never spans process work.
pub(super) static CODEX_INSTALL_CACHE: Lazy<CodexInstallCache> =
    Lazy::new(CodexInstallCache::default);

pub fn runtime_identity(env_type: EnvType) -> &'static str {
    match env_type {
        EnvType::Wsl => "wsl",
        EnvType::WindowsInterop => "windows-interop",
        EnvType::Windows if cfg!(target_os = "windows") => "native-windows",
        EnvType::Windows if cfg!(target_os = "macos") => "native-macos",
        EnvType::Windows => "native-linux",
    }
}

pub(super) fn wsl_runtime_identity(distro: &str, codex_home: &str) -> String {
    format!("wsl:{distro}:{codex_home}")
}

/// Log- and UI-safe runtime identity for diagnostics (issue #1937): the WSL
/// identity is `wsl:{distro}:{codex_home}`, and the home path can carry the
/// guest username — report `wsl:{distro}` so support logs and error surfaces
/// never include filesystem paths. Native identities pass through unchanged.
/// Built from the `wsl_distro` field (never by parsing the identity string),
/// so the home path is structurally unreachable. The stored
/// `runtime_identity` (DB keys, signatures) is untouched.
pub fn log_safe_runtime_identity(install: &CodexInstall) -> String {
    if let Some(distro) = install.wsl_distro.as_deref() {
        format!("wsl:{distro}")
    } else {
        install.runtime_identity.clone()
    }
}

pub fn discover_supported_install(env_type: EnvType) -> Result<CodexInstall, String> {
    CODEX_INSTALL_CACHE.discover(env_type, || discover_supported_install_uncached(env_type))
}

/// Re-resolve Codex for an explicit pairing verification. Verifying still
/// checks the live endpoint, and it must also observe a CLI update immediately
/// instead of trusting the short-lived Settings cache.
pub fn discover_supported_install_fresh(env_type: EnvType) -> Result<CodexInstall, String> {
    CODEX_INSTALL_CACHE.discover_fresh(env_type, || discover_supported_install_uncached(env_type))
}

/// Whether [`discover_supported_install`] for this runtime currently serves
/// from the short-lived install cache instead of spawning probes (issue
/// #1948). Secret-free by construction (a bool per runtime) — the
/// cold-vs-warm bit in the provider-menu timing log. Read-only: never
/// inserts, never probes.
///
/// This mirrors the TTL reuse of [`discover_supported_install`] only:
/// [`discover_supported_install_fresh`] keys off the in-flight `refreshing`
/// flag and re-probes every completed entry, so reading this bit next to a
/// `_fresh` call would misreport cold discovery as warm.
pub fn codex_install_cached(env_type: EnvType) -> bool {
    CODEX_INSTALL_CACHE.is_fresh(env_type)
}
