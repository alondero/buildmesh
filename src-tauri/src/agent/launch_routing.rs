//! Prepared provider routing resolved before command construction (issue #1098).

use std::collections::HashMap;
use std::path::PathBuf;

use crate::agent::provider::adapters::codex;
use crate::agent::provider::LaunchRuntime;
use crate::env::ResolvedPath;
use crate::models::Provider;
use crate::preferences;

/// Everything the resolution reads: the spawn option, the harness executor it
/// resolves to, and the runtime it resolved for. Neither `EnvType` nor
/// `Provider` derives `Hash`, so the non-string halves are `&'static str` tags
/// (`Provider` via its adapter id).
type RoutingCacheKey = (String, &'static str, &'static str);

fn routing_env_tag(env_type: crate::models::EnvType) -> &'static str {
    match env_type {
        crate::models::EnvType::Windows => "windows",
        crate::models::EnvType::Wsl => "wsl",
        crate::models::EnvType::WindowsInterop => "windows-interop",
    }
}

fn routing_cache_key(
    spawn_option_id: &str,
    provider: Provider,
    env_type: crate::models::EnvType,
) -> RoutingCacheKey {
    (
        spawn_option_id.to_string(),
        provider.adapter().id(),
        routing_env_tag(env_type),
    )
}

/// Memo for [`prepare`]'s value-only results, invalidated by the preferences
/// generation (issue #1752). Keyed by `(spawn option, harness, runtime)` — the
/// `(provider, mesh env fingerprint)` triple the resolution actually depends on.
///
/// Generation handling is deliberately strict about *direction*. The map-level
/// `generation` only ever moves **forward**: a caller that sampled an older
/// generation must neither evict the newer entries nor be able to publish its
/// result. `get` therefore clears only on a strictly newer generation, and
/// `put` drops any insertion whose generation is not the map's current one.
/// That closes the race where a slow spawn samples generation `G`, a settings
/// write publishes + bumps to `G+1`, and the slow spawn then inserts an entry
/// computed from `G`'s preferences — the entry is dead on arrival rather than
/// becoming serveable state. The per-entry generation check is kept as the
/// read-side twin, so a rotation can never be served from a stale entry.
#[derive(Default)]
struct RoutingCache {
    generation: u64,
    entries: HashMap<RoutingCacheKey, (u64, PreparedLaunchRouting)>,
}

impl RoutingCache {
    fn get(&mut self, key: &RoutingCacheKey, generation: u64) -> Option<PreparedLaunchRouting> {
        if generation > self.generation {
            self.entries.clear();
            self.generation = generation;
        }
        match self.entries.get(key) {
            Some((entry_generation, routing)) if *entry_generation == generation => {
                Some(routing.clone())
            }
            _ => None,
        }
    }

    fn put(&mut self, key: RoutingCacheKey, generation: u64, routing: PreparedLaunchRouting) {
        // A stale generation means the map has already moved past this entry's
        // inputs — drop it instead of inserting dead state.
        if generation != self.generation {
            return;
        }
        self.entries.insert(key, (generation, routing));
    }
}

// Global in production (one preferences source per process); per-test-thread in
// tests, mirroring `preferences::storage` — a shared static would let one
// test's routing leak into another's, since tests use thread-local prefs.
#[cfg(not(test))]
static ROUTING_CACHE: once_cell::sync::Lazy<std::sync::Mutex<RoutingCache>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(RoutingCache::default()));

#[cfg(test)]
thread_local! {
    static ROUTING_CACHE: std::cell::RefCell<RoutingCache> =
        std::cell::RefCell::new(RoutingCache::default());
}

fn with_routing_cache<R>(f: impl FnOnce(&mut RoutingCache) -> R) -> R {
    #[cfg(not(test))]
    let result = {
        let mut guard = ROUTING_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    };
    #[cfg(test)]
    let result = ROUTING_CACHE.with(|cell| f(&mut cell.borrow_mut()));
    result
}

#[derive(Clone, PartialEq, Eq)]
// `CodexProxy` is intentionally the largest variant (~600 bytes — carries the
// resolved install + verification + credential strings) while `Native` and
// `Environment` are tiny. Boxing the big variant would force an allocation
// on every `Native`/`Environment` resolution for the common Claude-Code path,
// which dominates this enum in practice. The size difference is structural —
// the enum exists *because* Codex's prepare phase carries more state — so we
// accept the lint rather than pay the indirection cost.
#[allow(clippy::large_enum_variant)]
pub enum PreparedLaunchRouting {
    /// Native harness with no proxy and no env injection. `executable`
    /// carries the resolved absolute path from the profile when the harness
    /// was detected off-`PATH` (e.g. Cline's `CLINE_BIN_PATH` or
    /// `@cline/cli-windows-{x64,arm64}\bin\cline.exe` walk — issue #1773
    /// review); `None` keeps `spawn_environment::wrap` falling back to its
    /// normal `recipe.binary` lookup.
    Native { executable: Option<PathBuf> },
    Environment(Vec<(String, String)>),
    CodexProxy {
        harness_id: String,
        provider_id: String,
        profile_name: String,
        descriptor: crate::agent::provider::compatibility::EndpointModelDescriptor,
        verification: preferences::PairingVerification,
        runtime: crate::models::EnvType,
        install: codex::CodexInstall,
        credential_reference: String,
        credential: String,
    },
}

impl std::fmt::Debug for PreparedLaunchRouting {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native { executable } => formatter
                .debug_struct("Native")
                .field("executable", executable)
                .finish(),
            Self::Environment(values) => formatter
                .debug_tuple("Environment")
                .field(&format_args!("{} values (redacted)", values.len()))
                .finish(),
            Self::CodexProxy {
                harness_id,
                provider_id,
                profile_name,
                descriptor,
                verification,
                runtime,
                install,
                credential_reference,
                credential: _,
            } => formatter
                .debug_struct("CodexProxy")
                .field("harness_id", harness_id)
                .field("provider_id", provider_id)
                .field("profile_name", profile_name)
                .field("descriptor", descriptor)
                .field("verification", verification)
                .field("runtime", runtime)
                .field("install", install)
                .field("credential_reference", credential_reference)
                .field("credential", &"<redacted>")
                .finish(),
        }
    }
}

impl PreparedLaunchRouting {
    pub fn environment(values: &[(String, String)]) -> Self {
        if values.is_empty() {
            Self::Native { executable: None }
        } else {
            Self::Environment(values.to_vec())
        }
    }

    /// Carry the runtime identity already established during provider
    /// preflight into later provisioning. This prevents Codex proxy launches
    /// from independently selecting a second WSL distro or home.
    pub fn launch_runtime(&self) -> LaunchRuntime {
        match self {
            Self::CodexProxy { install, .. } => LaunchRuntime {
                harness_home: Some(install.codex_home.clone()),
                wsl_distro: install.wsl_distro.clone(),
            },
            Self::Native { .. } | Self::Environment(_) => LaunchRuntime::default(),
        }
    }

    /// The resolved absolute path to spawn, when the harness's detection
    /// yielded something the `PATH` lookup wouldn't find. Returns `None`
    /// for the common case (PATH-resolvable binary or Codex proxy, which
    /// carries its own `install.executable` plumbing).
    ///
    /// Unified across variants so [`super::command::build_spawn_command`]
    /// has one place to read it. Codex's `install.executable` wins over the
    /// profile's `executable` because it's the verified npm-shim path,
    /// whereas the profile's `executable` is the bare-detection result.
    pub fn executable_override(&self) -> Option<&std::path::Path> {
        match self {
            Self::CodexProxy { install, .. } => Some(std::path::Path::new(&install.executable)),
            Self::Native { executable } => executable.as_deref(),
            Self::Environment(_) => None,
        }
    }
}

pub fn prepare(
    spawn_option_id: &str,
    provider: Provider,
    resolved: &ResolvedPath,
) -> Result<PreparedLaunchRouting, String> {
    // This guard reads only the runtime + process-static host probes, so it
    // stays ahead of the cache (no preferences involved).
    if resolved.env_type == crate::models::EnvType::WindowsInterop && (!crate::env::is_wsl_host() || crate::env::windows_home().is_none()) {
        return Err("Windows harnesses require an interoperable WSL host with powershell.exe on PATH.".into());
    }

    // Cache lookup FIRST (issue #1752). `resolved_harness_profile` below walks
    // the detected profile set on every spawn; resolving it before consulting
    // the cache would defeat the memo entirely.
    let generation = preferences::generation();
    let cache_key = routing_cache_key(spawn_option_id, provider, resolved.env_type);
    if let Some(cached) = with_routing_cache(|cache| cache.get(&cache_key, generation)) {
        return Ok(cached);
    }

    // Miss. The WSL-distro guard reads only preferences (covered by the
    // generation) plus the process-static default distro, so skipping it on a
    // hit cannot mask a settings change — any change already bumped the
    // generation and forced this path.
    let profile_for_distro = preferences::resolved_harness_profile(spawn_option_id);
    if let Some(distro) = profile_for_distro.as_ref().and_then(|profile| profile.wsl_distro.clone()) {
        if resolved.env_type == crate::models::EnvType::Wsl
            && crate::env::get_default_wsl_distro().as_deref() != Some(distro.as_str()) {
            return Err(format!("This harness belongs to WSL distribution '{distro}'. Set it as the default distribution and restart Buildmesh, or select a harness from the current default distribution."));
        }
    }
    // Carry the resolved absolute path (issue #1773 review). The profile's
    // `executable` is `None` for every harness whose binary is on `PATH`
    // (Claude Code, native Codex, Antigravity, OpenCode on PATH, etc.) —
    // the spawn path skips the absolute-path override in that case.
    let executable_override = profile_for_distro
        .as_ref()
        .and_then(|profile| profile.executable.clone());

    let routing = resolve_routing(spawn_option_id, provider, resolved, executable_override)?;
    // Only the value-only variants are memoised. `CodexProxy` carries a live
    // credential + verification and rewrites a profile file on every call; an
    // `Err` must be re-derived so its guard sees the current settings.
    if matches!(
        routing,
        PreparedLaunchRouting::Native { .. } | PreparedLaunchRouting::Environment(_)
    ) {
        with_routing_cache(|cache| cache.put(cache_key, generation, routing.clone()));
    }
    Ok(routing)
}

/// The uncached body of [`prepare`] — resolved pairing → routing. Split out so
/// [`prepare`] owns the memo while this keeps the resolution exact.
fn resolve_routing(
    spawn_option_id: &str,
    provider: Provider,
    resolved: &ResolvedPath,
    executable_override: Option<PathBuf>,
) -> Result<PreparedLaunchRouting, String> {
    // Cline can consume a stored pairing attached to Claude Code or Codex
    // when no Cline-specific pairing exists. Resolve that surface fallback
    // through the consumer-aware compatibility emitter before attempting the
    // exact pairing lookup below; otherwise a legacy `cline:<account>` launch
    // (and its snapshot resume) is rejected as "pairing no longer exists".
    if provider == Provider::Cline && spawn_option_id.contains(':') {
        preferences::preflight_resolve_provider_env(spawn_option_id)?;
        let env = preferences::resolve_provider_env(spawn_option_id);
        if env.is_empty() {
            return Err(format!(
                "selected proxied pairing '{spawn_option_id}' no longer exists"
            ));
        }
        return Ok(PreparedLaunchRouting::Environment(env));
    }
    let Some((pairing, account)) =
        preferences::resolve_stored_pairing_and_account(spawn_option_id)?
    else {
        if spawn_option_id.contains(':') {
            return Err(format!(
                "selected proxied pairing '{spawn_option_id}' no longer exists"
            ));
        }
        return Ok(PreparedLaunchRouting::Native { executable: executable_override });
    };

    preferences::preflight_resolve_provider_env(spawn_option_id)?;
    if provider == Provider::Anthropic {
        return Ok(PreparedLaunchRouting::Environment(preferences::resolve_provider_env(spawn_option_id)));
    }
    prepare_route(pairing, account, provider, resolved, None)
}

pub fn prepare_snapshot(
    plan: &preferences::launch_configurations::ResolvedLaunchPlan,
    resolved: &ResolvedPath,
) -> Result<PreparedLaunchRouting, String> {
    if resolved.env_type == crate::models::EnvType::WindowsInterop
        && (!crate::env::is_wsl_host() || crate::env::windows_home().is_none())
    {
        return Err("Windows harnesses require an interoperable WSL host with powershell.exe on PATH.".into());
    }
    if let Some(distro) = &plan.harness.wsl_distro {
        if resolved.env_type == crate::models::EnvType::Wsl
            && crate::env::get_default_wsl_distro().as_deref() != Some(distro.as_str())
        {
            return Err(format!("This saved launch belongs to WSL distribution '{distro}'. Set it as the default distribution and restart Buildmesh."));
        }
    }
    let Some(route) = plan.route.clone() else {
        return Ok(PreparedLaunchRouting::Native { executable: plan.harness.executable.clone() });
    };
    let account = preferences::provider_accounts().into_iter().find(|a| a.id == route.provider_id)
        .ok_or_else(|| format!("Provider account '{}' is missing; restore its credential to resume", route.provider_id))?;
    prepare_route(route, account, Provider::from_db_str(&plan.harness.harness), resolved, plan.verification.as_ref())
}

fn prepare_route(
    pairing: preferences::ProviderPairing,
    account: preferences::ProviderAccount,
    provider: Provider,
    resolved: &ResolvedPath,
    verification: Option<&preferences::PairingVerification>,
) -> Result<PreparedLaunchRouting, String> {
    preferences::compatibility::preflight_pairing_env(Some(&pairing), &account.id)?;
    match provider {
        Provider::Codex => {
            let verified = if verification.is_some() {
                crate::services::provider_verification::verified_codex_snapshot(&pairing, &account, resolved.env_type, verification)?
            } else {
                crate::services::provider_verification::verified_codex_pairing(&pairing, &account, resolved.env_type)?
            };
            let profile_name = codex::stable_profile_name(&pairing.harness_id, &pairing.provider_id);
            codex::materialize_proxy_profile(
                resolved.env_type,
                &verified.install,
                &profile_name,
                &account.name,
                &verified.descriptor.endpoint,
            )?;
            // Codex's verified install path wins over the profile's bare
            // `executable` — it carries the npm-shim location after
            // verification, which is what Codex's actual binary is.
            Ok(PreparedLaunchRouting::CodexProxy {
                harness_id: pairing.harness_id,
                provider_id: pairing.provider_id,
                profile_name,
                descriptor: verified.descriptor,
                verification: verified.verification,
                runtime: resolved.env_type,
                install: verified.install,
                credential_reference: codex::PROXY_CREDENTIAL_ENV.into(),
                credential: verified.credential,
            })
        }
        Provider::Anthropic | Provider::Cline => {
            if !account.enabled {
                return Err(format!("provider '{}' is disabled", account.name));
            }
            if account
                .api_key
                .as_deref()
                .is_none_or(|credential| credential.trim().is_empty())
            {
                return Err(format!("provider '{}' has no credential", account.name));
            }
            let decision = preferences::pairing_compatibility(&pairing);
            if !decision.compatible {
                return Err(decision
                    .reason
                    .unwrap_or_else(|| "incompatible capability contract".into()));
            }
            let env = if provider == Provider::Cline {
                preferences::compatibility::cline_consumer_env(
                    pairing.surface,
                    pairing.base_url.as_deref(),
                    account.api_key.as_deref(),
                    &pairing.model_tiers,
                )
            } else {
                preferences::compatibility::surface_env(
                    pairing.surface,
                    pairing.base_url.as_deref(),
                    account.api_key.as_deref(),
                    &pairing.model_tiers,
                )
            };
            Ok(PreparedLaunchRouting::Environment(env))
        }
        _ => Err("the selected harness does not support proxied providers".into()),
    }
}

#[cfg(test)]
mod routing_cache_tests {
    use super::*;

    #[test]
    fn cline_legacy_pairing_uses_cline_auth_environment() {
        let temp = tempfile::tempdir().unwrap();
        preferences::init_for_tests(temp.path().to_path_buf());
        preferences::save(preferences::AppPreferences {
            provider_accounts: vec![preferences::ProviderAccount {
                id: "minimax".into(),
                name: "MiniMax".into(),
                enabled: true,
                billing_mode: preferences::BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("test-key".into()),
            }],
            provider_pairings: vec![preferences::ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "minimax".into(),
                surface: preferences::ApiSurface::Anthropic,
                base_url: Some("https://example.invalid/anthropic".into()),
                model_tiers: preferences::ModelTiers {
                    default: Some("MiniMax-M3".into()),
                    ..Default::default()
                },
            }],
            ..Default::default()
        })
        .unwrap();

        let resolved = crate::env::ResolvedPath {
            host_path: "C:/work".into(),
            spawn_path: "C:/work".into(),
            raw_path: "C:/work".into(),
            env_type: crate::models::EnvType::Windows,
        };
        let routing = resolve_routing("cline:minimax", Provider::Cline, &resolved, None).unwrap();
        let PreparedLaunchRouting::Environment(env) = routing else {
            panic!("expected Cline environment routing");
        };
        assert_eq!(env.iter().find(|(key, _)| key == "ANTHROPIC_API_KEY").map(|(_, value)| value.as_str()), Some("test-key"));
        assert!(!env.iter().any(|(key, value)| key == "ANTHROPIC_API_KEY" && value.is_empty()));
        preferences::reset_for_tests();
    }

    #[test]
    fn cline_launch_configuration_resolves_fallback_pairings_for_both_surfaces() {
        let temp = tempfile::tempdir().unwrap();
        preferences::init_for_tests(temp.path().to_path_buf());
        let resolved = crate::env::ResolvedPath {
            host_path: "C:/work".into(),
            spawn_path: "C:/work".into(),
            raw_path: "C:/work".into(),
            env_type: crate::models::EnvType::Windows,
        };
        for (surface, source_harness, base_url) in [
            (preferences::ApiSurface::Anthropic, "claude", "https://example.invalid/anthropic"),
            (preferences::ApiSurface::OpenAI, "codex", "https://example.invalid/v1"),
        ] {
            let prefs = preferences::AppPreferences {
                harness_profiles: vec![preferences::HarnessProfile {
                    id: "cline".into(), name: "Cline".into(), harness: "cline".into(),
                    runtime: None, wsl_distro: None, executable: None,
                }],
                provider_accounts: vec![preferences::ProviderAccount {
                    id: "custom".into(), name: "Custom".into(), enabled: true,
                    billing_mode: preferences::BillingMode::PayAsYouGo,
                    claude_compatible: true, api_key: Some("test-key".into()),
                }],
                provider_pairings: vec![preferences::ProviderPairing {
                    harness_id: source_harness.into(), provider_id: "custom".into(), surface,
                    base_url: Some(base_url.into()),
                    model_tiers: preferences::ModelTiers { default: Some("test-model".into()), ..Default::default() },
                }],
                spawn_configurations: vec![preferences::spawn_configurations::SpawnConfiguration {
                    id: "launch/cline:custom".into(), name: "Cline custom".into(),
                    spawn_option_id: "cline:custom".into(), ..Default::default()
                }],
                ..Default::default()
            };
            preferences::save(prefs.clone()).unwrap();
            let plan = preferences::launch_configurations::capture_legacy(
                &prefs, "launch/cline:custom", &Default::default(),
            ).unwrap();
            assert_eq!(plan.route.as_ref().map(|route| route.surface), Some(surface));
            let routing = prepare_snapshot(&plan, &resolved).unwrap();
            let PreparedLaunchRouting::Environment(env) = routing else {
                panic!("expected Cline environment routing");
            };
            let key_name = match surface {
                preferences::ApiSurface::Anthropic => "ANTHROPIC_API_KEY",
                preferences::ApiSurface::OpenAI => "OPENAI_API_KEY",
            };
            assert_eq!(env.iter().find(|(key, _)| key == key_name).map(|(_, value)| value.as_str()), Some("test-key"));
        }
        preferences::reset_for_tests();
    }

    fn key(id: &str) -> RoutingCacheKey {
        (id.to_string(), "anthropic", "windows")
    }

    fn native(path: &str) -> PreparedLaunchRouting {
        PreparedLaunchRouting::Native {
            executable: Some(PathBuf::from(path)),
        }
    }

    /// Store `routing` at `generation`, going through `get` first exactly as
    /// `prepare` does — `put` only accepts the map's current generation, so a
    /// bare `put` would be dropped.
    fn seed(cache: &mut RoutingCache, key: RoutingCacheKey, generation: u64, routing: PreparedLaunchRouting) {
        assert!(cache.get(&key, generation).is_none(), "seed expects a cold key");
        cache.put(key, generation, routing);
    }

    /// A hit returns the stored value; a repeated read at the same generation
    /// keeps hitting (the memo's whole point).
    #[test]
    fn get_returns_the_value_stored_at_the_current_generation() {
        let mut cache = RoutingCache::default();
        assert!(cache.get(&key("claude"), 7).is_none(), "cold cache misses");
        cache.put(key("claude"), 7, native("/usr/bin/claude"));
        match cache.get(&key("claude"), 7) {
            Some(PreparedLaunchRouting::Native { executable }) => {
                assert_eq!(executable.as_deref(), Some(std::path::Path::new("/usr/bin/claude")));
            }
            other => panic!("expected the memoised Native routing, got {other:?}"),
        }
    }

    /// A generation bump invalidates the map — the settings-change path.
    #[test]
    fn a_generation_bump_invalidates_every_entry() {
        let mut cache = RoutingCache::default();
        seed(&mut cache, key("claude"), 7, native("/usr/bin/claude"));
        assert!(
            cache.get(&key("claude"), 8).is_none(),
            "a bumped generation must drop entries computed before it"
        );
    }

    /// The write-side race guard: a slow spawn that sampled generation 7 must
    /// not be able to publish its result once the map has moved to 8. Without
    /// this `put` a rotated endpoint/key could become serveable state.
    #[test]
    fn put_drops_an_entry_computed_before_a_bump() {
        let mut cache = RoutingCache::default();
        // The map was already advanced to generation 8 by another caller...
        assert!(cache.get(&key("claude"), 8).is_none());
        // ...then a slow spawn tries to insert its generation-7 result.
        cache.put(key("claude"), 7, native("/usr/bin/claude"));
        assert!(
            cache.get(&key("claude"), 8).is_none(),
            "a put stamped with a stale generation must be dropped, not stored"
        );
    }

    /// A caller holding an older generation must not evict the newer entries.
    #[test]
    fn a_stale_reader_does_not_evict_newer_entries() {
        let mut cache = RoutingCache::default();
        seed(&mut cache, key("claude"), 8, native("/usr/bin/claude"));
        // A slow reader that sampled generation 7 must miss, not clear the map.
        assert!(cache.get(&key("claude"), 7).is_none());
        assert!(
            cache.get(&key("claude"), 8).is_some(),
            "the generation-8 entry must survive a generation-7 reader"
        );
    }

    /// Different runtimes are different fingerprints — a WSL resolution must
    /// not satisfy a Windows lookup of the same spawn option.
    #[test]
    fn the_runtime_is_part_of_the_fingerprint() {
        let mut cache = RoutingCache::default();
        seed(&mut cache, ("claude".to_string(), "anthropic", "wsl"), 1, native("/home/u/bin/claude"));
        assert!(
            cache.get(&("claude".to_string(), "anthropic", "windows"), 1).is_none(),
            "a Windows lookup must not reuse the WSL entry"
        );
    }

    /// The harness executor is part of the fingerprint — the same spawn option
    /// string resolved under a different executor must not reuse the entry.
    #[test]
    fn the_provider_is_part_of_the_fingerprint() {
        let mut cache = RoutingCache::default();
        seed(&mut cache, ("claude".to_string(), "anthropic", "windows"), 1, native("/usr/bin/claude"));
        assert!(
            cache.get(&("claude".to_string(), "codex", "windows"), 1).is_none(),
            "a different harness must not reuse the entry"
        );
    }
}
