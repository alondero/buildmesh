//! Prepared provider routing resolved before command construction (issue #1098).

use std::path::PathBuf;

use crate::agent::provider::adapters::codex;
use crate::agent::provider::LaunchRuntime;
use crate::env::ResolvedPath;
use crate::models::Provider;
use crate::preferences;

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
    if resolved.env_type == crate::models::EnvType::WindowsInterop && (!crate::env::is_wsl_host() || crate::env::windows_home().is_none()) {
        return Err("Windows harnesses require an interoperable WSL host with powershell.exe on PATH.".into());
    }
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

    match provider {
        Provider::Codex => {
            let verified = crate::services::provider_verification::verified_codex_pairing(
                &pairing,
                &account,
                resolved.env_type,
            )?;
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
            let _ = executable_override;
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
        Provider::Anthropic => {
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
            preferences::preflight_resolve_provider_env(spawn_option_id)?;
            Ok(PreparedLaunchRouting::Environment(
                preferences::resolve_provider_env(spawn_option_id),
            ))
        }
        _ => Err("the selected harness does not support proxied providers".into()),
    }
}
