//! Spawn Menu composition — derive the `ProviderInfo` rows the desktop and
//! mobile Spawn Option pickers render.
//!
//! This is the deep module for the Spawn Menu derivation. Pre-#1052 the
//! derivation lived in `commands::agent::available_providers` — a 2,400+ line
//! file mixing process-lifecycle, spawn orchestration, and this menu logic.
//! Issue #1052 split the file so this module owns the menu logic next to its
//! unit tests, `agent::process` owns the process-lifecycle Tauri commands,
//! and `commands::agent` is left with thin spawn-orchestration adapters.
//! The Tauri command here is [`list_providers`]; the pure helpers
//! (`compose_provider_menu`, `order_providers`, `order_proxied_children`,
//! `provider_info_for`, `provider_info_for_pairing`) are the unit-test seam.

#[cfg(test)]
use crate::agent::provider::AgentProvider;
use crate::agent::provider::{EffectivePermissionMode, Platform, ProviderInfo};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::command;

/// Compose the `ProviderInfo` (Spawn Option) row for a single harness profile on the
/// current host platform. Pure (no disk / DB / globals) so unit tests can
/// exercise the per-profile derivation — including the `resumable` flag —
/// without touching the preferences module's `APP_DATA_DIR` / `CACHE`
/// shared state (which is global per-process and would otherwise race
/// against other tests).
///
/// Extracted from `available_providers` so the per-profile logic can be
/// pinned without driving `harness_profiles()` (which reads from disk and
/// shares a `OnceLock`). Resolves the executor via `profile.harness` (the
/// stored profile field that names the backing [`Provider`]) rather than
/// `preferences::resolve_harness_provider(&profile.id)` — that helper
/// reads disk via `harness_profiles()` to look up an id, which would
/// defeat the test isolation. For the `available_providers` call site
/// the two paths are equivalent (every profile iterated here comes from
/// `harness_profiles()` and so its id→harness lookup is a no-op).
///
/// The row is a **native Spawn Option**: `id == profile.id` (no `:`), no
/// `provider_id`, `is_proxied = false`. `harness_id == profile.id` is the
/// grouping key the frontend uses to bucket rows under their harness header
/// (issue #575 / ADR-0016).
pub(crate) fn provider_info_for(
    profile: &crate::preferences::HarnessProfile,
    host: Platform,
) -> Option<ProviderInfo> {
    if !profile_runtime_supported(profile, host) {
        return None;
    }
    let adapter = crate::models::Provider::from_db_str(&profile.harness).adapter();
    let target = profile_platform(profile, host);
    if !adapter.available_on().contains(&target) {
        return None;
    }
    Some(profile_row(profile))
}

fn profile_row(profile: &crate::preferences::HarnessProfile) -> ProviderInfo {
    let adapter = crate::models::Provider::from_db_str(&profile.harness).adapter();
    let ui = adapter.ui();
    // Backend-derived answer to "can this provider resume an archived
    // session in place?" — both flags must be true: supports_resume()
    // gates the CLI flag, produces_readable_transcript() gates the
    // coordinator read API that rehydrates the session. The archived-node
    // resume picker consumes this so a custom Claude-compatible profile
    // (e.g. "DeepSeek via Claude") shows up without the old hardcoded id
    // allow-list (#550 follow-up).
    let resumable = adapter.supports_resume() && adapter.produces_readable_transcript();
    ProviderInfo {
        id: profile.id.clone(),
        label: profile.name.clone(),
        color: ui.color,
        icon: ui.icon,
        resumable,
        harness_id: profile.id.clone(),
        provider_id: None,
        is_proxied: false,
        group_key: profile.id.clone(),
        capabilities: crate::agent::capabilities::capabilities_for(adapter),
        runtime: profile.runtime,
        configurations: Vec::new(),
        configuration: None,
        unavailable_reason: None,
        effective_permission: None,
    }
}

/// Compose the `ProviderInfo` (Spawn Option) row for one **Proxied Provider**
/// pairing — a [`crate::preferences::ProviderPairing`] attaching an account to a
/// harness over a chosen **Compatible API surface** (issue #576, generalises the
/// #575 account-only row). The endpoint URL + model map travel with the pairing
/// (resolved at spawn by [`crate::preferences::resolve_provider_env`]); the brand
/// label comes from the account and the row's colour/icon from the *executor*
/// adapter, so a MiniMax-via-Codex row reads as a Codex-family row while the
/// frontend `ProviderIcon` (keyed off the composite `id`) still renders the
/// MiniMax brand mark.
///
/// The composite `id` is `<harness_id>:<provider_id>` (e.g. `claude:minimax`,
/// `codex:minimax`) and `harness_id`/`group_key` cluster the row under its
/// harness header in the rendered Spawn Menu. The executor is resolved from the
/// pairing's harness *profile* (its `harness` field), falling back to parsing the
/// `harness_id` directly when no matching profile is present (a stored pairing
/// for an undetected harness, or a bare test env) — the same fallback chain the
/// resolver uses. Returns `None` only if that executor isn't available on this
/// host.
pub(super) fn provider_info_for_pairing(
    pairing: &crate::preferences::ProviderPairing,
    account: &crate::preferences::ProviderAccount,
    profiles: &[crate::preferences::HarnessProfile],
    host: Platform,
) -> Option<ProviderInfo> {
    let executor = profiles
        .iter()
        .find(|p| p.id == pairing.harness_id)
        .map(|p| crate::models::Provider::from_db_str(&p.harness))
        .unwrap_or_else(|| crate::models::Provider::from_db_str(&pairing.harness_id));
    let adapter = executor.adapter();
    if profiles
        .iter()
        .any(|p| p.id == pairing.harness_id && !profile_runtime_supported(p, host))
    {
        return None;
    }
    let target = profiles
        .iter()
        .find(|p| p.id == pairing.harness_id)
        .map(|p| profile_platform(p, host))
        .unwrap_or(host);
    if !adapter.available_on().contains(&target) {
        return None;
    }
    let ui = adapter.ui();
    Some(ProviderInfo {
        id: format!("{}:{}", pairing.harness_id, pairing.provider_id),
        label: account.name.clone(),
        color: ui.color,
        icon: ui.icon,
        resumable: adapter.supports_resume() && adapter.produces_readable_transcript(),
        harness_id: pairing.harness_id.clone(),
        provider_id: Some(pairing.provider_id.clone()),
        is_proxied: true,
        group_key: pairing.harness_id.clone(),
        capabilities: crate::agent::capabilities::capabilities_for(adapter),
        runtime: profiles
            .iter()
            .find(|profile| profile.id == pairing.harness_id)
            .and_then(|profile| profile.runtime),
        configurations: Vec::new(),
        configuration: None,
        unavailable_reason: None,
        effective_permission: None,
    })
}

fn profile_runtime_supported(profile: &crate::preferences::HarnessProfile, host: Platform) -> bool {
    match profile.runtime {
        None => true,
        Some(crate::models::EnvType::WindowsInterop) => host == Platform::Linux,
        Some(_) => host == Platform::Windows,
    }
}

fn profile_platform(profile: &crate::preferences::HarnessProfile, host: Platform) -> Platform {
    match profile.runtime {
        Some(crate::models::EnvType::Wsl) => Platform::Linux,
        Some(crate::models::EnvType::Windows | crate::models::EnvType::WindowsInterop) => {
            Platform::Windows
        }
        None => host,
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    #[test]
    fn muse_is_visible_on_windows_native_or_wsl_runtime() {
        // Native-Windows profile (no runtime annotation → host-native).
        // Pre-fix this returned None because Muse's `available_on()`
        // excluded `Platform::Windows`; with Windows added the filter
        // lets the row through. The WSL fallback row keeps working
        // because `profile_platform` resolves `runtime: Some(Wsl)` to
        // `Platform::Linux`, which was already in `available_on()`.
        let native = crate::preferences::HarnessProfile {
            id: "muse".into(),
            name: "Meta Muse".into(),
            harness: "muse".into(),
            runtime: None,
            wsl_distro: None,
            executable: None,
        };
        let info = provider_info_for(&native, Platform::Windows)
            .expect("muse is available on Windows once Platform::Windows is in available_on()");
        assert_eq!(info.id, "muse");
        assert_eq!(info.harness_id, "muse");
        assert!(info.capabilities.available_on.iter().any(|p| p == "windows"),
                "capabilities descriptor must advertise windows so the frontend renders the row (platform_name normalises to lowercase, see capabilities::platform_name)");

        // WSL fallback profile — preserved from the pre-fix behaviour.
        let wsl = crate::preferences::HarnessProfile {
            id: "muse-wsl".into(),
            name: "Meta Muse (WSL)".into(),
            harness: "muse".into(),
            runtime: Some(crate::models::EnvType::Wsl),
            wsl_distro: None,
            executable: None,
        };
        let wsl_info = provider_info_for(&wsl, Platform::Windows).unwrap();
        assert_eq!(wsl_info.id, "muse-wsl");
        assert_eq!(wsl_info.label, "Meta Muse (WSL)");
    }
}

/// Build the spawn menu from the configuration lists. Pure (no disk/globals) so
/// the derivation is the unit-test seam.
///
/// Harness profiles (Terminal + startup-detected Claude/Codex/Antigravity/OpenCode)
/// come first as native rows, then one **Proxied Provider** row per *stored*
/// pairing for a proxiable account (ADR-0025 / issue #576,
/// [`crate::preferences::effective_pairings`]). Clearing the key or disabling
/// the account drops every stored row that depends on it.
///
/// **Dedup semantics** (issue #575 / ADR-0016): the composite id
/// `<harness>:<provider>` is unique per (harness profile id, account id) pair, so
/// the duplicate-row check by `info.id` only fires when the same (harness,
/// account) pair was produced twice. `effective_pairings` already dedups by
/// `(harness_id, provider_id)`, so this guard is a belt-and-braces on the native
/// rows (a `claude:claude` custom account stays distinct from the native
/// `claude` harness row).
pub(super) fn compose_provider_menu(
    profiles: Vec<crate::preferences::HarnessProfile>,
    accounts: Vec<crate::preferences::ProviderAccount>,
    pairings: Vec<crate::preferences::ProviderPairing>,
    host: Platform,
    distro: Option<&str>,
    order: &[String],
    proxied_order: &[crate::preferences::ProxiedProviderOrder],
) -> Vec<ProviderInfo> {
    let menu_profiles = profiles
        .iter()
        .filter(|profile| crate::agent::detection::visible_in_spawn_menu(profile, host))
        .cloned()
        .collect::<Vec<_>>();
    let visible_profiles =
        crate::agent::detection::preferred_profiles(&menu_profiles, host, distro);
    let mut rows: Vec<ProviderInfo> = visible_profiles
        .iter()
        .filter_map(|profile| provider_info_for(profile, host))
        .collect();
    // The Claude Code harness header the derived default pairings group under
    // (shared rule — see `preferences::claude_harness_id_from`).
    let _claude_harness_id = crate::preferences::claude_harness_id_from(&visible_profiles);
    let effective = crate::preferences::effective_pairings(&accounts, &pairings);
    for pairing in &effective {
        // A route attached to a runtime-suffixed mirror of a natively-installed
        // harness is the same redundant choice as the mirror profile itself
        // (issue #1864), so drop it alongside the profile.
        let hidden_mirror = if let Some(profile) = profiles
            .iter()
            .find(|profile| profile.id == pairing.harness_id)
        {
            !crate::agent::detection::visible_in_spawn_menu(profile, host)
        } else {
            host == Platform::Windows
                && crate::agent::detection::canonical_wsl_harness(&pairing.harness_id).is_some_and(
                    |harness| {
                        crate::models::Provider::from_db_str(harness)
                            .adapter()
                            .available_on()
                            .contains(&host)
                    },
                )
        };
        if hidden_mirror {
            continue;
        }
        let Some(account) = accounts.iter().find(|a| a.id == pairing.provider_id) else {
            continue;
        };
        if let Some(info) = provider_info_for_pairing(pairing, account, &visible_profiles, host) {
            if !rows.iter().any(|r| r.id == info.id) {
                rows.push(info);
            }
        }
    }
    order_proxied_children(order_providers(rows, order), proxied_order)
}

/// Discover the native and foreign-runtime Codex installs concurrently.
///
/// The two are independent chains - separate `CODEX_INSTALL_CACHE` slots,
/// separate process spawns - combined only further down, so serializing them
/// made the tab pay their sum rather than their max (issue #1934). The caller's
/// `foreign_runtime` is never `Windows` (it is `WindowsInterop` or `Wsl`), so
/// the two never contend for one cache entry.
///
/// Both results are `Option`s because `available_providers` drops a failed
/// discovery rather than reporting it, so there is no error ordering to
/// preserve here and no need to distinguish the two slots by type - unlike the
/// probes inside one runtime's chain, where the order decides which message a
/// broken install surfaces.
fn discover_codex_runtimes_concurrently(
    native: impl FnOnce() -> Option<crate::agent::provider::adapters::codex::CodexInstall> + Send,
    foreign: impl FnOnce() -> Option<crate::agent::provider::adapters::codex::CodexInstall> + Send,
) -> (
    Option<crate::agent::provider::adapters::codex::CodexInstall>,
    Option<crate::agent::provider::adapters::codex::CodexInstall>,
) {
    std::thread::scope(|scope| {
        let native = scope.spawn(native);
        let foreign = scope.spawn(foreign);
        (
            crate::agent::provider::adapters::codex::join_probe(native),
            crate::agent::provider::adapters::codex::join_probe(foreign),
        )
    })
}

/// Wall-clock bound on one memoized menu.
///
/// The preferences generation is what makes the memo correct: it bumps on every
/// durable preference write, and a change reaches the rest of the app only once
/// one of those lands, so a memoized menu is never older than the preferences
/// cache every other reader sees. What the generation cannot see is a change
/// made outside Buildmesh — a `npm i -g @openai/codex` (or a WSL-side install)
/// while the window is open. 45s matches `codex::CODEX_INSTALL_CACHE_TTL`, the
/// staleness bound the Codex install probe already accepts, so the menu never
/// serves install metadata older than the probe behind it.
const DERIVED_MENU_TTL: Duration = Duration::from_secs(45);

/// Single-slot memo for the Spawn Menu derivation (issue #2349), keyed on the
/// preferences generation exactly as `agent::launch_routing` keys its routing
/// cache. The generation only moves **forward**, so a caller that sampled an
/// older one must neither evict the newer entry nor publish its own: `get`
/// clears only on a strictly newer generation, and `put` drops a derivation
/// whose inputs have already moved past it. `now` is a parameter rather than
/// an internal clock read so the TTL is testable without sleeping.
#[derive(Default)]
struct DerivedMenuCache {
    generation: u64,
    derived_at: Option<Instant>,
    menu: Option<Arc<Vec<ProviderInfo>>>,
}

impl DerivedMenuCache {
    fn get(&mut self, generation: u64, now: Instant) -> Option<Arc<Vec<ProviderInfo>>> {
        if generation < self.generation {
            // A caller holding an older generation must not evict the newer entry.
            return None;
        }
        if generation > self.generation {
            self.generation = generation;
            self.derived_at = None;
            self.menu = None;
        }
        let derived_at = self.derived_at?;
        let menu = self.menu.clone()?;
        (now.saturating_duration_since(derived_at) < DERIVED_MENU_TTL).then_some(menu)
    }

    fn put(&mut self, generation: u64, now: Instant, menu: Arc<Vec<ProviderInfo>>) {
        // A stale generation means the slot already moved past this
        // derivation's inputs — dropping it beats storing dead state.
        if generation != self.generation {
            return;
        }
        self.derived_at = Some(now);
        self.menu = Some(menu);
    }
}

// Global in production (one preferences source per process); per-test-thread
// in tests, mirroring `agent::launch_routing` — under `cfg(test)` the
// preferences app-data dir is thread-local (issue #1386), so a shared static
// would serve one test's preferences to another thread.
#[cfg(not(test))]
static DERIVED_MENU_CACHE: once_cell::sync::Lazy<std::sync::Mutex<DerivedMenuCache>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(DerivedMenuCache::default()));

#[cfg(test)]
thread_local! {
    static DERIVED_MENU_CACHE: std::cell::RefCell<DerivedMenuCache> =
        std::cell::RefCell::new(DerivedMenuCache::default());
}

fn with_derived_menu<R>(f: impl FnOnce(&mut DerivedMenuCache) -> R) -> R {
    #[cfg(not(test))]
    let result = {
        let mut guard = DERIVED_MENU_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    };
    #[cfg(test)]
    let result = DERIVED_MENU_CACHE.with(|cell| f(&mut cell.borrow_mut()));
    result
}

/// Derive the Spawn Menu, or serve the memoized menu (issue #2349).
///
/// The Android client re-reads `GET /api/providers` on a fixed 5s timer
/// while it is foregrounded, so a paired phone idling on the node list
/// re-derived the whole menu — per-runtime Codex probe included, and a
/// failing WSL-side probe is discarded rather than cached warm
/// (`codex::CodexInstallCache::discard_failed_entry`) — twelve times a minute.
/// Both production entry points come through here ([`available_providers`] for
/// the mobile route, `list_providers_blocking` for the Tauri command), so the
/// memo covers every client instead of one caller, and the derivation log line
/// stops repeating for an idle one.
///
/// `derive` runs on a miss only, and a hit returns the rows a derivation
/// would have produced — the same derivation, less the probes.
fn derived_menu(derive: impl FnOnce() -> Vec<ProviderInfo>) -> Vec<ProviderInfo> {
    derived_menu_at(crate::preferences::generation(), derive)
}

/// [`derived_menu`] with the cache key supplied, so a unit test can pin a
/// generation. Sampling the real one is inherently racy under a parallel test
/// binary: `preferences::generation` is process-wide (one preferences source
/// per process) while the preferences themselves are per-test-thread, so any
/// other thread's `save`/`update` moves the key mid-test.
fn derived_menu_at(
    generation: u64,
    derive: impl FnOnce() -> Vec<ProviderInfo>,
) -> Vec<ProviderInfo> {
    if let Some(menu) = with_derived_menu(|cache| cache.get(generation, Instant::now())) {
        return menu.as_ref().clone();
    }
    let menu = Arc::new(derive());
    with_derived_menu(|cache| cache.put(generation, Instant::now(), Arc::clone(&menu)));
    menu.as_ref().clone()
}

/// Returns the list of agent providers available on this host platform.
/// Each provider declares which platforms it runs on via `AgentProvider::available_on()`.
///
/// `pub(crate)` so the mobile HTTP route (`http/routes/providers.rs`) can
/// keep using it as its menu source — the route wraps this call in
/// `commands::run_blocking` already, so the menu derivation continues to
/// stay off the async worker pool (issue #634).
pub(crate) fn available_providers() -> Vec<ProviderInfo> {
    derived_menu(|| available_providers_with_preferences(crate::preferences::load().ok()))
}

/// Resolve the effective launch permission mode for one menu row (issue
/// #2151): the stored per-harness default when it names one of the
/// harness's own modes, else the harness's unattended default. `None`
/// for harnesses with no modes.
///
/// Reads the mode list off the row's own `capabilities` (populated at
/// composition time) instead of re-resolving the adapter — which would
/// reload and deep-clone the whole preferences tree per row on a path
/// this file instruments for wall-clock (issue #1937). Pure over the
/// already-loaded prefs snapshot (no `preferences::load()`) so unit
/// tests pin it without touching global state. Native and proxied rows
/// of one harness share the harness-defaults entry.
fn effective_permission_for(
    row: &ProviderInfo,
    prefs: &crate::preferences::AppPreferences,
) -> Option<EffectivePermissionMode> {
    let stored = prefs
        .harness_defaults
        .get(&row.harness_id)
        .and_then(|v| v.permission_mode.as_deref());
    effective_permission_for_modes(
        &row.capabilities.permission_modes,
        row.capabilities.default_permission_mode.as_deref(),
        stored,
    )
}

/// Pure modes-level core of [`effective_permission_for`]: no prefs, no
/// disk, no adapter resolution — the unit-test seam. A stored value that
/// names no known mode (stale after an adapter change) falls back to the
/// default. Same fallback rule as the spawn path; both delegate to
/// `agent::capabilities::effective_permission_mode`.
fn effective_permission_for_modes(
    modes: &[crate::agent::capabilities::PermissionModeOption],
    default_mode: Option<&str>,
    stored: Option<&str>,
) -> Option<EffectivePermissionMode> {
    // Validated here (as well as inside the shared fallback) so a stale
    // value counts as "no stored choice" for `is_default`, matching the
    // pre-review behavior the adapter tests pin.
    let stored = stored
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| modes.iter().any(|m| m.id == *s));
    let option =
        crate::agent::capabilities::effective_permission_mode(modes, default_mode, stored)?;
    Some(EffectivePermissionMode {
        mode_id: option.id,
        label: option.label,
        description: option.description,
        is_default: stored.is_none(),
    })
}

/// Pure adapter-level core of [`effective_permission_for`]: no prefs, no
/// disk — the unit-test seam. Test-only since the menu stamp reads the
/// row's own capabilities (finding 4); kept because the adapter tests pin
/// the contract without building menu rows.
#[cfg(test)]
fn effective_permission_for_adapter(
    adapter: &dyn AgentProvider,
    stored: Option<&str>,
) -> Option<EffectivePermissionMode> {
    effective_permission_for_modes(
        &adapter.permission_modes(),
        adapter.default_permission_mode().as_deref(),
        stored,
    )
}

/// Stamp the resolved launch permission mode onto every menu row (issue
/// #2151) from the already-loaded prefs snapshot. Runs after
/// `configuration_menu` so saved-configuration rows carry it too; the
/// per-profile constructors stay pure (see `profile_row`).
fn stamp_effective_permission(
    menu: Vec<ProviderInfo>,
    prefs: &crate::preferences::AppPreferences,
) -> Vec<ProviderInfo> {
    menu.into_iter()
        .map(|mut row| {
            row.effective_permission = effective_permission_for(&row, prefs);
            row
        })
        .collect()
}

fn available_providers_with_preferences(
    prefs: Option<crate::preferences::AppPreferences>,
) -> Vec<ProviderInfo> {
    // Issue #1937: this derivation dominates the Settings -> Providers load,
    // so each derivation emits one line (lands in `logs\buildmesh.log`
    // on a default install) with the total wall-clock and the Codex probe cost.
    // Only the runtime identity and CLI version travel in the fields - never
    // credentials, keys, or endpoint URLs.
    // Issue #1948: the same line now separates Codex discovery from the rest
    // of the derivation (`menu_compose_duration_ms`) and reports per-runtime
    // cache reuse (`*_codex_cached`), so a slow save-refresh can be
    // attributed to cold discovery, warm discovery, or menu composition.
    // Issue #1934: the two runtimes now discover concurrently, so the probe
    // cost is one overlapped `codex_probe_duration_ms` window rather than two
    // per-runtime durations that summed to the same wall clock. The per-runtime
    // `*_codex_cached` bits still discriminate cold from warm, which is what
    // the attribution needed them for.
    // Issue #2349: `debug`, not `info`. The default filter already admits this
    // crate at `debug`, so the level no longer separates this line from routine
    // detail - the memo in `derived_menu` is what keeps an idle client from
    // deriving at all. At `info` a phone on its 5s poll wrote this line twelve
    // times a minute over the attention and lifecycle lines it exists to help
    // read.
    let derivation_started = Instant::now();
    let accounts = crate::preferences::provider_accounts();
    let configured_pairings = crate::preferences::provider_pairings();
    let needs_codex = configured_pairings
        .iter()
        .any(|pairing| pairing.surface == crate::preferences::ApiSurface::OpenAI);
    let foreign_runtime = if crate::env::is_wsl_host() {
        crate::models::EnvType::WindowsInterop
    } else {
        crate::models::EnvType::Wsl
    };
    // Snapshot cache state before probing (issue #1948): `false` covers both
    // a cold cache and "no probe ran" when `needs_codex` is false (the probe
    // duration is 0 then, so the line still reads unambiguously).
    let native_codex_cached = needs_codex
        && crate::agent::provider::adapters::codex::codex_install_cached(
            crate::models::EnvType::Windows,
        );
    let foreign_codex_cached = needs_codex
        && crate::agent::provider::adapters::codex::codex_install_cached(foreign_runtime);
    let probe_started = Instant::now();
    let (native_codex, wsl_codex) = if needs_codex {
        discover_codex_runtimes_concurrently(
            || {
                crate::agent::provider::adapters::codex::discover_supported_install(
                    crate::models::EnvType::Windows,
                )
                .ok()
            },
            || {
                crate::agent::provider::adapters::codex::discover_supported_install(foreign_runtime)
                    .ok()
            },
        )
    } else {
        (None, None)
    };
    let codex_probe_duration_ms = probe_started.elapsed().as_millis();
    // Everything after the probes is menu composition (issue #1948): pairing
    // filters, harness detection, row ordering, and Launch Configuration
    // attachment. Timed separately so cold Codex discovery is never blamed on
    // composition (or vice versa).
    let compose_started = Instant::now();
    let pairings = configured_pairings
        .into_iter()
        .filter(|pairing| {
            accounts
                .iter()
                .find(|account| account.id == pairing.provider_id)
                .is_some_and(|account| {
                    crate::services::provider_verification::launchable_on_runtime(
                        pairing,
                        account,
                        crate::models::EnvType::Windows,
                        native_codex.as_ref(),
                    ) || crate::services::provider_verification::launchable_on_runtime(
                        pairing,
                        account,
                        foreign_runtime,
                        wsl_codex.as_ref(),
                    )
                })
        })
        .collect();
    let profiles = crate::agent::detection::currently_installed_profiles(
        crate::preferences::harness_profiles(),
    );
    let distro = if cfg!(windows) {
        crate::env::get_default_wsl_distro()
    } else {
        None
    };
    let menu = compose_provider_menu(
        profiles,
        accounts,
        pairings,
        Platform::current(),
        distro.as_deref(),
        &crate::preferences::harness_order(),
        &crate::preferences::proxied_provider_order(),
    );
    let menu = match prefs {
        Some(mut prefs) => {
            crate::preferences::launch_configurations::reconcile(&mut prefs);
            let menu = configuration_menu(menu, &prefs, Platform::current());
            stamp_effective_permission(menu, &prefs)
        }
        None => menu,
    };
    tracing::debug!(
        needs_codex,
        codex_probe_duration_ms,
        native_codex_cached,
        native_runtime_identity = native_codex
            .as_ref()
            .map(crate::agent::provider::adapters::codex::log_safe_runtime_identity)
            .unwrap_or_else(|| "none".to_string()),
        native_codex_version = native_codex
            .as_ref()
            .map(|install| install.version.as_str())
            .unwrap_or("none"),
        foreign_env = %foreign_runtime,
        foreign_codex_cached,
        foreign_runtime_identity = wsl_codex
            .as_ref()
            .map(crate::agent::provider::adapters::codex::log_safe_runtime_identity)
            .unwrap_or_else(|| "none".to_string()),
        foreign_codex_version = wsl_codex
            .as_ref()
            .map(|install| install.version.as_str())
            .unwrap_or("none"),
        menu_rows = menu.len(),
        menu_compose_duration_ms = compose_started.elapsed().as_millis(),
        total_duration_ms = derivation_started.elapsed().as_millis(),
        "provider menu derivation completed"
    );
    menu
}

// Keep the native harness rows as submenu parents. Route rows still serve
// selectors that choose a provider directly; configuration rows carry launch
// availability, including the reason a saved recipe cannot start.
fn configuration_menu(
    mut menu: Vec<ProviderInfo>,
    prefs: &crate::preferences::AppPreferences,
    host: Platform,
) -> Vec<ProviderInfo> {
    let available = menu
        .iter()
        .map(|row| row.id.clone())
        .collect::<std::collections::HashSet<_>>();
    for configuration in &prefs.spawn_configurations {
        let id =
            crate::agent::provider::SpawnOptionId::from(configuration.spawn_option_id.as_str());
        let wsl_harness = crate::agent::detection::canonical_wsl_harness(id.harness_id());
        let fallback = crate::preferences::HarnessProfile {
            id: id.harness_id.clone(),
            name: id.harness_id.clone(),
            harness: wsl_harness.map(str::to_owned).unwrap_or_else(|| {
                if id.harness_id == "claude" {
                    "anthropic".into()
                } else {
                    id.harness_id.clone()
                }
            }),
            runtime: wsl_harness.map(|_| crate::models::EnvType::Wsl),
            wsl_distro: None,
            executable: None,
        };
        let profile = prefs
            .harness_profiles
            .iter()
            .find(|p| p.id == id.harness_id())
            .unwrap_or(&fallback);
        if !crate::agent::detection::visible_in_spawn_menu(profile, host) {
            continue;
        }
        if !menu.iter().any(|row| row.id == id.harness_id()) {
            let mut header = profile_row(profile);
            header.unavailable_reason =
                Some("Harness is unavailable; install and enable it".into());
            menu.push(header);
        }
        let mut row = menu
            .iter()
            .find(|r| r.id == configuration.spawn_option_id)
            .cloned()
            .unwrap_or_else(|| profile_row(profile));
        row.id = configuration.id.clone();
        row.label = configuration.name.clone();
        row.provider_id = id.provider_id.clone();
        row.is_proxied = id.is_proxied();
        row.configuration = Some(configuration.clone());
        row.configurations.clear();
        row.unavailable_reason = crate::preferences::launch_configurations::resolve(
            prefs,
            &configuration.id,
            &Default::default(),
        )
        .err();
        if row.unavailable_reason.is_none() && !available.contains(&configuration.spawn_option_id) {
            row.unavailable_reason = Some(
                if id.is_proxied() && available.contains(id.harness_id()) {
                    "Provider Route needs verification for this harness/runtime; open advanced routes".into()
                } else {
                    "Harness is unavailable; install and enable it".into()
                },
            );
        }
        menu.push(row);
    }
    order_proxied_children(
        order_providers(menu, &prefs.harness_order),
        &prefs.proxied_provider_order,
    )
}

/// Within each harness bucket, sort **Proxied Provider** children by the
/// user's stored per-harness order (issue #577). The harness-level rank is
/// untouched — this runs after [`order_providers`] and only re-orders the
/// within-bucket child sequence. A child present in the bucket but not in
/// the stored list appends at the end in its natural input order (stable
/// sort). Native harness headers are never reordered — they're not proxied
/// children; a stored entry that names a native id is silently ignored.
///
/// Pure (no disk / globals) so the ordering is the unit-test seam. The
/// bucketing is by `group_key == harness_id`, the same wire-shape field
/// the frontend `groupBy` uses (ADR-0016 §6); every Proxied row carries
/// its harness id via that field.
pub(super) fn order_proxied_children(
    mut rows: Vec<ProviderInfo>,
    proxied_order: &[crate::preferences::ProxiedProviderOrder],
) -> Vec<ProviderInfo> {
    if proxied_order.is_empty() {
        return rows;
    }
    let index_by_harness: std::collections::HashMap<&str, &[String]> = proxied_order
        .iter()
        .map(|o| (o.harness_id.as_str(), o.provider_ids.as_slice()))
        .collect();
    rows.sort_by(|a, b| {
        // Only proxied rows within the same harness bucket compete on the
        // stored order. Native rows and rows in different buckets fall
        // through to the stable sort, preserving the harness-level rank
        // established by `order_providers`.
        if !a.is_proxied || !b.is_proxied || a.harness_id != b.harness_id {
            return std::cmp::Ordering::Equal;
        }
        let Some(provider_ids) = index_by_harness.get(a.harness_id.as_str()) else {
            return std::cmp::Ordering::Equal;
        };
        let rank_a = provider_ids
            .iter()
            .position(|id| id == a.provider_id.as_deref().unwrap_or(""));
        let rank_b = provider_ids
            .iter()
            .position(|id| id == b.provider_id.as_deref().unwrap_or(""));
        match (rank_a, rank_b) {
            (Some(ra), Some(rb)) => ra.cmp(&rb),
            (Some(_), None) => std::cmp::Ordering::Less, // listed before unlisted
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal, // both unlisted → stable
        }
    });
    rows
}

/// Order the Spawn Menu by the user's stored harness order, with the plain
/// `terminal` row always pinned to the bottom (issue #534 / #573).
///
/// `order` is the persisted list of **harness profile ids** (Terminal excluded
/// — it's forced last regardless of where it appears). Each row's rank is
/// derived from its `harness_id` (the grouping key) — not its composite `id`
/// — so a Proxied Provider row like `claude:minimax` clusters under its
/// `claude` harness header instead of ranking at the bottom. A row whose
/// `harness_id` isn't in the list (a newly-detected harness) ranks just below
/// `usize::MAX` so it appends at the end *above* Terminal. An uninstalled
/// harness simply isn't among `providers`, so its id sits dormant in `order`
/// and its slot is restored verbatim when it reappears.
///
/// Pure and stable (no disk / globals) so the ordering is the unit-test seam:
/// the `(is_terminal, rank, harness_id)` tuple key sorts Terminal last via the
/// bool, ranks the rest by stored harness order, and the `harness_id`
/// tiebreak pins multiple *newcomers* — all sharing `rank = usize::MAX - 1` —
/// into a deterministic alphabetical order rather than relying on the input
/// order from `harness_profiles()` (issue #581). Listed harnesses always have
/// distinct ranks so the tiebreak is moot for them; Proxied rows share their
/// parent's `harness_id` and the stable sort keeps the native header ahead
/// of its children (the native row is built first in `compose_provider_menu`).
pub(super) fn order_providers(
    mut providers: Vec<ProviderInfo>,
    order: &[String],
) -> Vec<ProviderInfo> {
    providers.sort_by(|a, b| {
        let key_a = (
            a.harness_id == "terminal",
            order
                .iter()
                .position(|id| *id == a.harness_id)
                .unwrap_or(usize::MAX - 1),
            a.harness_id.as_str(),
        );
        let key_b = (
            b.harness_id == "terminal",
            order
                .iter()
                .position(|id| *id == b.harness_id)
                .unwrap_or(usize::MAX - 1),
            b.harness_id.as_str(),
        );
        key_a.cmp(&key_b)
    });
    providers
}

/// Routing preferences need labels and capabilities, not subprocess discovery.
/// OpenAI routes remain visibly unavailable until the live menu verifies them.
fn routing_options(
    prefs: &crate::preferences::AppPreferences,
    profiles: Vec<crate::preferences::HarnessProfile>,
    accounts: Vec<crate::preferences::ProviderAccount>,
    host: Platform,
    distro: Option<&str>,
) -> Vec<ProviderInfo> {
    let pairings = prefs
        .provider_pairings
        .iter()
        .filter(|pairing| {
            pairing.surface == crate::preferences::ApiSurface::OpenAI
                || accounts
                    .iter()
                    .find(|account| account.id == pairing.provider_id)
                    .is_some_and(|account| {
                        crate::services::provider_verification::launchable_on_runtime(
                            pairing,
                            account,
                            crate::models::EnvType::Windows,
                            None,
                        )
                    })
        })
        .cloned()
        .collect();
    let menu = compose_provider_menu(
        profiles,
        accounts,
        pairings,
        host,
        distro,
        &prefs.harness_order,
        &prefs.proxied_provider_order,
    );
    let pending_routes = prefs
        .provider_pairings
        .iter()
        .filter(|pairing| pairing.surface == crate::preferences::ApiSurface::OpenAI)
        .map(|pairing| format!("{}:{}", pairing.harness_id, pairing.provider_id))
        .collect::<std::collections::HashSet<_>>();
    let mut menu = configuration_menu(menu, prefs, host);
    for row in &mut menu {
        let selection = row
            .configuration
            .as_ref()
            .map(|configuration| configuration.spawn_option_id.as_str())
            .unwrap_or(&row.id);
        if pending_routes.contains(selection) && row.unavailable_reason.is_none() {
            row.unavailable_reason =
                Some("Runtime verification pending; retry provider checks if needed".into());
        }
    }
    menu
}

#[command]
pub async fn list_routing_options() -> Result<Vec<ProviderInfo>, String> {
    crate::commands::run_blocking("list_routing_options", || {
        let mut prefs = crate::preferences::load()?;
        crate::preferences::launch_configurations::reconcile(&mut prefs);
        let distro = if cfg!(windows) {
            crate::env::cached_default_wsl_distro()
        } else {
            None
        };
        Ok(routing_options(
            &prefs,
            crate::agent::detection::currently_installed_profiles(
                crate::preferences::harness_profiles(),
            ),
            crate::preferences::provider_accounts(),
            Platform::current(),
            distro.as_deref(),
        ))
    })
    .await
}

/// Tauri command — returns the derived Spawn Menu to the desktop / mobile
/// frontend (issue #575 / ADR-0016). Wraps `available_providers` in
/// `commands::run_blocking` so the menu derivation stays off the async
/// worker pool (issue #634). The result is what the desktop Spawn Modal
/// and the mobile provider picker render — the single source of truth
/// for "what can I spawn?".
#[command]
pub async fn list_providers() -> Result<Vec<ProviderInfo>, String> {
    crate::commands::run_blocking("list_providers", list_providers_blocking).await
}

fn list_providers_blocking() -> Result<Vec<ProviderInfo>, String> {
    // Load first so an unreadable `preferences.json` still surfaces as the
    // IPC error this command documents; the memo then serves the rows a
    // derivation over that same snapshot would have produced.
    let prefs = crate::preferences::load()?;
    Ok(derived_menu(move || {
        available_providers_with_preferences(Some(prefs))
    }))
}

#[cfg(test)]
#[path = "provider_menu_tests.rs"]
mod tests;
