use super::reader::SessionIdMode;
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::provider::{Platform, CLAUDE_BACKEND_ENV_VARS};
use crate::agent::spawn_environment;
use crate::env;
use crate::models::{EnvType, Provider};
use portable_pty::CommandBuilder;

/// Build the spawn command by composing the provider's recipe with the runtime environment.
///
/// `backend_env` is the per-profile backend selection resolved by the caller
/// (`preferences::resolve_provider_env(&node.provider)`): the `ANTHROPIC_*`
/// variables a custom Claude-compatible profile (MiniMax/DeepSeek) needs to
/// target its endpoint. Empty for the built-in Anthropic subscription and for
/// the native-binary providers (Codex, Grok, Kimi Code, Antigravity, OpenCode).
/// Passed in (rather than resolved here) so this
/// function stays a pure composition of its inputs — no disk / preferences-cache
/// access — and the env injection can be unit-tested with an explicit list.
///
/// `config` carries the **already-resolved, capability-masked** model and
/// effort values (issue #1149). The caller runs
/// [`crate::agent::capabilities::resolve_agent_config`] with the harness's
/// capability descriptor and the per-field cascade inputs; this function
/// forwards the resolved values verbatim and never re-consults capability
/// flags. Empty / whitespace inputs and unsupported values are masked before
/// they reach here.
#[allow(clippy::too_many_arguments)]
pub fn build_spawn_command(
    resolved: &env::ResolvedPath,
    provider_enum: Provider,
    backend_env: &[(String, String)],
    session_id_mode: &SessionIdMode,
    session_id: i64,
    config: &crate::agent::capabilities::ResolvedAgentConfig,
    prefill: Option<&str>,
    sandbox: bool,
) -> CommandBuilder {
    build_spawn_command_prepared(
        resolved,
        provider_enum,
        &crate::agent::launch_routing::PreparedLaunchRouting::environment(backend_env),
        session_id_mode,
        session_id,
        config,
        prefill,
        sandbox,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_spawn_command_prepared(
    resolved: &env::ResolvedPath,
    provider_enum: Provider,
    routing: &crate::agent::launch_routing::PreparedLaunchRouting,
    session_id_mode: &SessionIdMode,
    session_id: i64,
    config: &crate::agent::capabilities::ResolvedAgentConfig,
    prefill: Option<&str>,
    sandbox: bool,
) -> CommandBuilder {
    let adapter = provider_enum.adapter();
    let platform = if resolved.env_type == EnvType::Wsl {
        Platform::Linux
    } else {
        Platform::current()
    };

    // Compose the harness's launch contribution: recipe + capability
    // descriptor + env policy, all from the same adapter. The
    // capability-mask guarantee still holds — the resolver ran before
    // we got here, and the helper re-asserts the descriptor on the
    // forward as defence in depth (issue #1179).
    let session_ref = match session_id_mode {
        SessionIdMode::Assign(id) => SessionIdModeRef::Assign(id.as_str()),
        SessionIdMode::Resume(id) => SessionIdModeRef::Resume(id.as_str()),
        SessionIdMode::None => SessionIdModeRef::None,
    };
    let input = HarnessLaunchInput {
        platform,
        runtime: resolved.env_type,
        session: session_ref,
        config,
        prefill,
        sandbox,
    };
    let prepared = crate::agent::launch::default_prepare(adapter, input);

    // CodexProxy contributes --profile / --model to the recipe. This
    // belongs at the orchestrator layer (not the harness): the
    // pairing's verified profile is the orchestrator's knowledge, and
    // the per-pairing model id is a routing fact, not a harness fact.
    let mut recipe = prepared.recipe;
    if let crate::agent::launch_routing::PreparedLaunchRouting::CodexProxy {
        profile_name,
        descriptor,
        ..
    } = routing
    {
        recipe.base_args.extend([
            "--profile".into(),
            profile_name.clone(),
            "--model".into(),
            descriptor.model_id.clone(),
        ]);
    }

    let (wsl_distro, executable_override) = match routing {
        crate::agent::launch_routing::PreparedLaunchRouting::CodexProxy { install, .. } => (
            install.wsl_distro.as_deref(),
            Some(install.executable.as_str()),
        ),
        _ => (None, None),
    };
    let mut cmd = spawn_environment::wrap(
        recipe,
        resolved.env_type,
        wsl_distro,
        executable_override,
        &resolved.spawn_path,
        session_id,
        sandbox,
    );

    // Apply the harness's environment policy (CLAUDE_BACKEND_ENV_VARS
    // reset + per-harness env_remove). The adapter owns this — the
    // Claude-backed anthropic adapter sets the reset, Codex sets
    // OPENAI_* strip; every other adapter uses HarnessEnvironmentPolicy::NONE.
    if prepared.environment.resets_backend_env {
        for k in CLAUDE_BACKEND_ENV_VARS {
            cmd.env_remove(k);
        }
    }
    for k in prepared.environment.env_remove {
        cmd.env_remove(k);
    }

    // Inject the per-profile backend env + Codex Proxy credential. WSLENV is
    // assembled once after all command-defined variables are known, avoiding
    // one routing branch overwriting another branch's entries.
    let mut command_wsl_env = apply_routing_env(&mut cmd, routing);
    if let Some(key) = apply_codex_proxy_credential(&mut cmd, routing, provider_enum) {
        command_wsl_env.push(key);
    }
    spawn_environment::apply_wsl_env(
        &mut cmd,
        resolved.env_type,
        &command_wsl_env,
        adapter.wsl_passthrough_env(),
    );
    cmd
}

/// Apply the per-profile backend env (`PreparedLaunchRouting::Environment`)
/// to the child command and return the names that need WSL propagation.
pub(super) fn apply_routing_env<'a>(
    cmd: &mut CommandBuilder,
    routing: &'a crate::agent::launch_routing::PreparedLaunchRouting,
) -> Vec<&'a str> {
    let backend_env: &[(String, String)] = match routing {
        crate::agent::launch_routing::PreparedLaunchRouting::Environment(values) => {
            values.as_slice()
        }
        _ => &[],
    };
    for (k, v) in backend_env {
        cmd.env(k, v);
    }
    backend_env.iter().map(|(key, _)| key.as_str()).collect()
}

/// Apply the Codex Proxy pairing-scoped credential. A verified profile
/// authenticates exclusively through its pairing-scoped reference
/// (`PROXY_CREDENTIAL_ENV`); generic `OPENAI_API_KEY` / `OPENAI_BASE_URL`
/// inherited by Buildmesh are stripped so they cannot become an alternate
/// credential/endpoint. The generated credential key is returned for the
/// shared WSL environment pass.
pub(super) fn apply_codex_proxy_credential(
    cmd: &mut CommandBuilder,
    routing: &crate::agent::launch_routing::PreparedLaunchRouting,
    provider_enum: Provider,
) -> Option<&'static str> {
    if !matches!(provider_enum, Provider::Codex) {
        return None;
    }
    let key = crate::agent::provider::adapters::codex::PROXY_CREDENTIAL_ENV;
    cmd.env_remove(key);
    let crate::agent::launch_routing::PreparedLaunchRouting::CodexProxy {
        credential_reference,
        credential,
        ..
    } = routing
    else {
        return None;
    };
    debug_assert_eq!(credential_reference, key);
    cmd.env_remove("OPENAI_API_KEY");
    cmd.env_remove("OPENAI_BASE_URL");
    cmd.env(credential_reference, credential);
    Some(key)
}

/// Build the per-field cascade inputs the spawn pipeline hands to
/// [`crate::agent::capabilities::resolve_agent_config`] (issue #1155).
///
/// Pure helper so the wiring is testable independent of the resolver —
/// the resolver already has unit tests for its cascade order
/// (`resolver_cascade_prefers_explicit_over_mesh_over_application`), but
/// that test never proves the spawn pipeline *populates* the explicit
/// slot from `SpawnOptions`. This helper is the seam every future spawn
/// site writes to if it wants layer-1 precedence, and the unit tests pin
/// both the field-by-field wiring AND the cascade precedence when fed
/// through the resolver.
///
/// Whitespace-only / empty strings on the explicit slot collapse to
/// `None` here (closer to the transport — mobile HTTP / autopilot / UI
/// — than the resolver) so the cascade falls through to the next layer
/// regardless of whether the caller or the resolver did the trimming.
/// Mirrors `resolve_field`'s `normalize_non_empty` (issue #1148 AC #32,
/// #1155 AC #3).
///
/// `mesh_*` and the `application` slot borrow from the mesh row /
/// preferences cache and pass straight through; the resolver normalises
/// those layers at its seam.
pub(crate) fn cascade_inputs_for<'a>(
    explicit_model: Option<&'a str>,
    explicit_effort: Option<&'a str>,
    mesh_model: Option<&'a str>,
    mesh_effort: Option<&'a str>,
    app_default: Option<&'a crate::preferences::HarnessConfigValue>,
    mesh_override: Option<&'a crate::preferences::HarnessConfigValue>,
) -> crate::agent::capabilities::AgentConfigInputs<'a> {
    /// Trim; collapse empty / whitespace-only to `None`. Mirrors
    /// `capabilities::normalize_non_empty` at the spawn seam (issue
    /// #1148 AC #32 + #1155 AC #3). Inline closure hoisted here so the
    /// model + effort legs share the same shape.
    fn non_empty_trim(s: &str) -> Option<&str> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
    crate::agent::capabilities::AgentConfigInputs {
        model: crate::agent::capabilities::FieldInputs {
            explicit: explicit_model.and_then(non_empty_trim),
            mesh_override: mesh_override.and_then(|v| v.model.as_deref()),
            mesh: mesh_model,
            application: app_default.and_then(|v| v.model.as_deref()),
        },
        effort: crate::agent::capabilities::FieldInputs {
            explicit: explicit_effort.and_then(non_empty_trim),
            mesh_override: mesh_override.and_then(|v| v.effort.as_deref()),
            mesh: mesh_effort,
            application: app_default.and_then(|v| v.effort.as_deref()),
        },
    }
}

/// Pure seam for the spawn orchestrator's resolver call (issue #1157).
/// Composes `capabilities_for(provider.adapter())` +
/// `cascade_inputs_for` + `resolve_agent_config` into a single pure
/// function so the integration test for issue #1155 AC #4 ("Regression
/// tests must verify layer-1 behavior at a real spawn site, not just
/// resolver unit tests") can drive the full `SpawnRequest → resolver`
/// path through the same call shape `spawn_agent_inner` uses — without
/// standing up a Tauri runtime, a preferences cache, or a DB.
///
/// `app_default` is the ALREADY-LOOKED-UP value for the harness
/// profile. The orchestrator parses the composite `node.provider` id
/// (`"<harness>:<provider>"` for Proxied rows) and resolves the harness
/// default at its seam so this helper stays free of
/// `preferences::load()` (which would force the test to populate the
/// in-process preferences cache).
pub(crate) fn resolve_spawn_config(
    provider: Provider,
    explicit_model: Option<&str>,
    explicit_effort: Option<&str>,
    explicit_extra_args: Option<&str>,
    app_default: Option<&crate::preferences::HarnessConfigValue>,
    mesh_override: Option<&crate::preferences::HarnessConfigValue>,
) -> crate::agent::capabilities::ResolvedAgentConfig {
    let capabilities = crate::agent::capabilities::capabilities_for(provider.adapter());
    crate::agent::capabilities::resolve_agent_config(
        &capabilities,
        cascade_inputs_for(
            explicit_model,
            explicit_effort,
            None,
            None,
            app_default,
            mesh_override,
        ),
        explicit_extra_args,
    )
}
