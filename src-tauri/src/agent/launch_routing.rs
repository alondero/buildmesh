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
    ///
    /// `harness_home` is the Harness Login's home (`CODEX_HOME`) when the
    /// frozen plan selected an additional login; `None` is the runtime's
    /// ordinary home.
    Native {
        executable: Option<PathBuf>,
        harness_home: Option<String>,
    },
    /// Proxied Claude-compatible launch: backend env plus the same resolved
    /// absolute `executable` `Native` carries. Without it a paired
    /// Claude/Cline launch would fall back to a bare stem the GUI process
    /// `PATH` cannot resolve, re-creating the picker-visible-but-spawn-fails
    /// gap for the most common harness.
    Environment {
        values: Vec<(String, String)>,
        executable: Option<PathBuf>,
    },
    CodexProxy {
        harness_id: String,
        provider_id: String,
        /// The provider account's display name — the label the Spawn Menu
        /// shows for this route, and the name the launch warning uses.
        account_name: String,
        profile_name: String,
        descriptor: crate::agent::provider::compatibility::EndpointModelDescriptor,
        /// The check that covers this route, when one exists on this runtime.
        verification: Option<preferences::PairingVerification>,
        /// Set when the route is not covered by a current successful check.
        ///
        /// The pairing verification is advisory: the route launches either
        /// way, and this is what the spawn warns the user about while it
        /// re-checks the pairing in the background.
        advisory: Option<crate::services::provider_verification::VerificationAdvisory>,
        runtime: crate::models::EnvType,
        install: codex::CodexInstall,
        credential_reference: String,
        credential: String,
    },
}

impl std::fmt::Debug for PreparedLaunchRouting {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native {
                executable,
                harness_home,
            } => formatter
                .debug_struct("Native")
                .field("executable", executable)
                .field("harness_home", harness_home)
                .finish(),
            Self::Environment { values, executable } => formatter
                .debug_struct("Environment")
                .field(
                    "values",
                    &format_args!("{} values (redacted)", values.len()),
                )
                .field("executable", executable)
                .finish(),
            Self::CodexProxy {
                harness_id,
                provider_id,
                account_name,
                profile_name,
                descriptor,
                verification,
                advisory,
                runtime,
                install,
                credential_reference,
                credential: _,
            } => formatter
                .debug_struct("CodexProxy")
                .field("harness_id", harness_id)
                .field("provider_id", provider_id)
                .field("account_name", account_name)
                .field("profile_name", profile_name)
                .field("descriptor", descriptor)
                .field("verification", verification)
                .field("advisory", advisory)
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
            Self::Native {
                executable: None,
                harness_home: None,
            }
        } else {
            Self::Environment {
                values: values.to_vec(),
                executable: None,
            }
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
            Self::Native { harness_home, .. } => LaunchRuntime {
                harness_home: harness_home.clone(),
                wsl_distro: None,
            },
            Self::Environment { .. } => LaunchRuntime::default(),
        }
    }

    /// The resolved absolute path to spawn, when the harness's detection
    /// yielded something the GUI process `PATH` lookup wouldn't find (or the
    /// spawn-time re-resolution in [`prepare`] found it after startup).
    /// Returns `None` when neither source resolved a path, or for the Codex
    /// proxy, which carries its own `install.executable` plumbing.
    ///
    /// Unified across variants so [`super::command::build_spawn_command_prepared`]
    /// has one place to read it. Codex's `install.executable` wins over the
    /// profile's `executable` because it's the verified npm-shim path,
    /// whereas the profile's `executable` is the bare-detection result.
    pub fn executable_override(&self) -> Option<&std::path::Path> {
        match self {
            Self::CodexProxy { install, .. } => Some(std::path::Path::new(&install.executable)),
            Self::Native { executable, .. } => executable.as_deref(),
            Self::Environment { executable, .. } => executable.as_deref(),
        }
    }

    /// The runtime this routing will spawn *into*, for the variants that pin
    /// one themselves.
    ///
    /// `CodexProxy` selected its install during prepare (its npm shim lives
    /// under a specific `CODEX_HOME`, and the launch may target a different
    /// distro than the mesh path implies), so the spawn-path preflight must
    /// check the binary on *that* runtime. `Native` / `Environment` inherit
    /// the mesh path's runtime, which is why they return `None` rather than
    /// guessing — a guessed runtime would silently exempt them.
    pub fn pinned_runtime(&self) -> Option<crate::models::EnvType> {
        match self {
            Self::CodexProxy { runtime, .. } => Some(*runtime),
            Self::Native { .. } | Self::Environment { .. } => None,
        }
    }

    /// Why this routing is not covered by a current successful pairing check,
    /// when that is the case.
    ///
    /// The check is advisory: the route launches either way. The spawn reads
    /// this to warn the user and to re-check the pairing in the background
    /// while the session runs, instead of refusing to start.
    pub fn verification_advisory(
        &self,
    ) -> Option<&crate::services::provider_verification::VerificationAdvisory> {
        match self {
            Self::CodexProxy { advisory, .. } => advisory.as_ref(),
            Self::Native { .. } | Self::Environment { .. } => None,
        }
    }

    /// The identity of the Codex route this routing points at, together with
    /// the labels a launch warning needs.
    ///
    /// `None` for a routing that is not a Codex proxy route — only a Codex
    /// route carries a pairing verification.
    pub fn codex_route_identity(&self) -> Option<CodexRouteIdentity<'_>> {
        match self {
            Self::CodexProxy {
                harness_id,
                provider_id,
                account_name,
                runtime,
                ..
            } => Some(CodexRouteIdentity {
                harness_id,
                provider_id,
                account_name,
                runtime: *runtime,
                runtime_label: match runtime {
                    crate::models::EnvType::Wsl => "WSL",
                    crate::models::EnvType::Windows | crate::models::EnvType::WindowsInterop => {
                        "Windows"
                    }
                }
                .to_string(),
            }),
            Self::Native { .. } | Self::Environment { .. } => None,
        }
    }
}

/// What a launch owes the user about a route whose pairing check is not
/// current.
///
/// The decision is separated from its side effects (the warning event and the
/// background re-check) so a test can pin it without a Tauri app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationFollowUp {
    /// Warn the user, and re-check the pairing in the background so the next
    /// launch finds a current record.
    WarnAndRecheck {
        provider_id: String,
        message: String,
    },
    /// Warn the user. The last check needs a credential or endpoint fix
    /// first, so the pairing is not re-probed on this launch.
    WarnOnly {
        provider_id: String,
        message: String,
    },
}

impl PreparedLaunchRouting {
    /// The follow-up this launch owes about the pairing check, if any.
    ///
    /// `None` when the route is covered by a current successful check — or
    /// when it is not a Codex proxy route, which carries no pairing
    /// verification of its own.
    pub fn verification_follow_up(&self) -> Option<VerificationFollowUp> {
        let advisory = self.verification_advisory()?;
        let identity = self.codex_route_identity()?;
        let message = advisory.warning(identity.account_name, &identity.runtime_label);
        let provider_id = identity.provider_id.to_string();
        Some(if advisory.wants_recheck() {
            VerificationFollowUp::WarnAndRecheck {
                provider_id,
                message,
            }
        } else {
            VerificationFollowUp::WarnOnly {
                provider_id,
                message,
            }
        })
    }
}

/// The identity and labels of the Codex route a launch is about to use.
#[derive(Debug, Clone)]
pub struct CodexRouteIdentity<'a> {
    pub harness_id: &'a str,
    pub provider_id: &'a str,
    pub account_name: &'a str,
    pub runtime: crate::models::EnvType,
    /// The runtime as the user knows it (`Windows`, `WSL`) — the name the
    /// warning uses for the machine the route was checked on.
    pub runtime_label: String,
}

pub fn prepare(
    spawn_option_id: &str,
    provider: Provider,
    resolved: &ResolvedPath,
) -> Result<PreparedLaunchRouting, String> {
    // This guard reads only the runtime + process-static host probes, so it
    // stays ahead of the cache (no preferences involved).
    if resolved.env_type == crate::models::EnvType::WindowsInterop
        && (!crate::env::is_wsl_host() || crate::env::windows_home().is_none())
    {
        return Err(
            "Windows harnesses require an interoperable WSL host with powershell.exe on PATH."
                .into(),
        );
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
    if let Some(distro) = profile_for_distro
        .as_ref()
        .and_then(|profile| profile.wsl_distro.clone())
    {
        if resolved.env_type == crate::models::EnvType::Wsl
            && crate::env::get_default_wsl_distro().as_deref() != Some(distro.as_str())
        {
            return Err(format!("This harness belongs to WSL distribution '{distro}'. Set it as the default distribution and restart Buildmesh, or select a harness from the current default distribution."));
        }
    }
    // Carry the resolved absolute path (issue #1773 review): detection records
    // it for every binary-found harness so a GUI-launched app spawns the same
    // path it discovered. When the profile carries none (config-dir-only
    // install, custom profile, or a binary that appeared after startup),
    // re-resolve the recipe stem through the same enriched search path at
    // spawn time — changing picker detection alone does not fix the launch.
    let executable_override = spawn_time_executable(
        profile_for_distro
            .as_ref()
            .and_then(|profile| profile.executable.clone()),
        provider,
        resolved.env_type,
    );

    let routing = resolve_routing(spawn_option_id, provider, resolved, executable_override)?;
    // Only the value-only variants are memoised. `CodexProxy` carries a live
    // credential + verification and rewrites a profile file on every call; an
    // `Err` must be re-derived so its guard sees the current settings.
    if matches!(
        routing,
        PreparedLaunchRouting::Native { .. } | PreparedLaunchRouting::Environment { .. }
    ) {
        with_routing_cache(|cache| cache.put(cache_key, generation, routing.clone()));
    }
    Ok(routing)
}

/// The recipe stem the spawn would invoke for `provider` on `env_type` — the
/// same binary [`super::command::build_spawn_command_prepared`] hands to
/// `spawn_environment::wrap` when no override is present. Used for the
/// spawn-time re-resolution, so discovery and spawning consult the same name.
pub(crate) fn recipe_binary_for(
    provider: Provider,
    env_type: crate::models::EnvType,
) -> &'static str {
    let platform = if env_type == crate::models::EnvType::Wsl {
        crate::agent::provider::Platform::Linux
    } else if env_type == crate::models::EnvType::WindowsInterop {
        crate::agent::provider::Platform::Windows
    } else {
        crate::agent::provider::Platform::current()
    };
    provider.adapter().spawn_recipe(platform, env_type).binary
}

/// Spawn-time executable for a routing: the profile's detection-resolved path,
/// falling back to a live re-resolution of the recipe stem through the same
/// enriched search path detection uses.
///
/// Guest-side (`Wsl`) spawns resolve the stem inside the guest login shell,
/// and Windows-side (`WindowsInterop`) spawns resolve Windows-side: a
/// host-resolved absolute path (e.g. `/mnt/c/...` from translated Windows
/// `PATH` mounts) is not a valid Windows path and must never reach
/// PowerShell, so interop drops the override entirely — including a
/// profile-carried path, which was resolved on the Linux side (review
/// round 1).
fn spawn_time_executable(
    profile_executable: Option<PathBuf>,
    provider: Provider,
    env_type: crate::models::EnvType,
) -> Option<PathBuf> {
    use crate::models::EnvType;
    if env_type == EnvType::WindowsInterop {
        return None;
    }
    profile_executable.or_else(|| {
        if env_type == EnvType::Wsl {
            return None;
        }
        crate::agent::detection::resolve_spawn_binary(recipe_binary_for(provider, env_type))
    })
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
        return Ok(PreparedLaunchRouting::Environment {
            values: env,
            executable: executable_override,
        });
    }
    let Some((pairing, account)) =
        preferences::resolve_stored_pairing_and_account(spawn_option_id)?
    else {
        if spawn_option_id.contains(':') {
            return Err(format!(
                "selected proxied pairing '{spawn_option_id}' no longer exists"
            ));
        }
        return Ok(PreparedLaunchRouting::Native {
            executable: executable_override,
            harness_home: None,
        });
    };

    preferences::preflight_resolve_provider_env(spawn_option_id)?;
    if provider == Provider::Anthropic {
        return Ok(PreparedLaunchRouting::Environment {
            values: preferences::resolve_provider_env(spawn_option_id),
            executable: executable_override,
        });
    }
    prepare_route(
        pairing,
        account,
        provider,
        resolved,
        None,
        executable_override,
    )
}

pub fn prepare_snapshot(
    plan: &preferences::launch_configurations::ResolvedLaunchPlan,
    resolved: &ResolvedPath,
) -> Result<PreparedLaunchRouting, String> {
    if resolved.env_type == crate::models::EnvType::WindowsInterop
        && (!crate::env::is_wsl_host() || crate::env::windows_home().is_none())
    {
        return Err(
            "Windows harnesses require an interoperable WSL host with powershell.exe on PATH."
                .into(),
        );
    }
    if let Some(distro) = &plan.harness.wsl_distro {
        if resolved.env_type == crate::models::EnvType::Wsl
            && crate::env::get_default_wsl_distro().as_deref() != Some(distro.as_str())
        {
            return Err(format!("This saved launch belongs to WSL distribution '{distro}'. Set it as the default distribution and restart Buildmesh."));
        }
    }
    let provider = Provider::from_db_str(&plan.harness.harness);
    let executable_override =
        spawn_time_executable(plan.harness.executable.clone(), provider, resolved.env_type);
    let Some(route) = plan.route.clone() else {
        let harness_home = match &plan.harness_login {
            Some(login) => {
                if let Some(refusal) =
                    preferences::harness_logins::runtime_refusal(login, resolved.env_type)
                {
                    return Err(refusal);
                }
                Some(login.home.clone())
            }
            None => None,
        };
        return Ok(PreparedLaunchRouting::Native {
            executable: executable_override,
            harness_home,
        });
    };
    let account = preferences::provider_accounts()
        .into_iter()
        .find(|a| a.id == route.provider_id)
        .ok_or_else(|| {
            format!(
                "Provider account '{}' is missing; restore its credential to resume",
                route.provider_id
            )
        })?;
    prepare_route(
        route,
        account,
        provider,
        resolved,
        plan.verification.as_ref(),
        executable_override,
    )
}

fn prepare_route(
    pairing: preferences::ProviderPairing,
    account: preferences::ProviderAccount,
    provider: Provider,
    resolved: &ResolvedPath,
    verification: Option<&preferences::PairingVerification>,
    executable_override: Option<PathBuf>,
) -> Result<PreparedLaunchRouting, String> {
    preferences::compatibility::preflight_pairing_env(Some(&pairing), &account.id)?;
    match provider {
        Provider::Codex => {
            let route = if verification.is_some() {
                crate::services::provider_verification::codex_proxy_route_snapshot(
                    &pairing,
                    &account,
                    resolved.env_type,
                    verification,
                )?
            } else {
                crate::services::provider_verification::codex_proxy_route(
                    &pairing,
                    &account,
                    resolved.env_type,
                )?
            };
            let profile_name =
                codex::stable_profile_name(&pairing.harness_id, &pairing.provider_id);
            codex::materialize_proxy_profile(
                resolved.env_type,
                &route.install,
                &profile_name,
                &account.name,
                &route.descriptor.endpoint,
            )?;
            // Codex's resolved install path wins over the profile's bare
            // `executable` — it carries the npm-shim location after
            // resolution, which is what Codex's actual binary is.
            Ok(PreparedLaunchRouting::CodexProxy {
                harness_id: pairing.harness_id,
                provider_id: pairing.provider_id,
                account_name: account.name.clone(),
                profile_name,
                descriptor: route.descriptor,
                verification: route.verification,
                advisory: route.advisory,
                runtime: resolved.env_type,
                install: route.install,
                credential_reference: codex::PROXY_CREDENTIAL_ENV.into(),
                credential: route.credential,
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
            Ok(PreparedLaunchRouting::Environment {
                values: env,
                executable: executable_override,
            })
        }
        _ => Err("the selected harness does not support proxied providers".into()),
    }
}

#[cfg(test)]
#[path = "launch_routing_tests.rs"]
mod launch_routing_tests;

#[cfg(test)]
#[path = "launch_routing_cache_tests.rs"]
mod routing_cache_tests;
