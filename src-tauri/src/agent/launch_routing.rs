//! Prepared provider routing resolved before command construction (issue #1098).

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
    Native,
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
            Self::Native => formatter.write_str("Native"),
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
            Self::Native
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
                node_id: None,
            },
            Self::Native | Self::Environment(_) => LaunchRuntime::default(),
        }
    }
}

pub fn prepare(
    spawn_option_id: &str,
    provider: Provider,
    resolved: &ResolvedPath,
) -> Result<PreparedLaunchRouting, String> {
    let Some((pairing, account)) =
        preferences::resolve_stored_pairing_and_account(spawn_option_id)?
    else {
        if spawn_option_id.contains(':') {
            return Err(format!(
                "selected proxied pairing '{spawn_option_id}' no longer exists"
            ));
        }
        return Ok(PreparedLaunchRouting::Native);
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
