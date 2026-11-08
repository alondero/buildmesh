use super::intent::ResumeCause;
use super::reader::SessionIdMode;
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::provider::Platform;
use crate::agent::spawn_environment;
use crate::env;
use crate::models::{EnvType, Provider};
use portable_pty::CommandBuilder;

/// Everything [`build_spawn_command_prepared`] composes the launch from.
///
/// The inputs arrive as one named value rather than nine positional ones, so
/// the routing choice (`routing`) is an explicit field the caller fills and
/// every spawn site writes the same set.
pub struct SpawnCommandInput<'a> {
    pub resolved: &'a env::ResolvedPath,
    pub provider: Provider,
    /// The per-profile backend env, a Codex Proxy pairing, or a native
    /// executable override — one value for all three, so this function stays a
    /// pure composition of its inputs (no disk / preferences-cache access) and
    /// the env injection can be unit-tested with an explicit list.
    ///
    /// `PreparedLaunchRouting::Environment`'s values are the per-profile backend
    /// selection resolved by the caller
    /// (`preferences::resolve_provider_env`): the
    /// `ANTHROPIC_*` variables a custom Claude-compatible profile
    /// (MiniMax/DeepSeek) needs to target its endpoint. Empty for the built-in
    /// Anthropic subscription and for the native-binary providers (Codex, Grok,
    /// Kimi Code, Antigravity, OpenCode).
    pub routing: &'a crate::agent::launch_routing::PreparedLaunchRouting,
    pub session_id_mode: &'a SessionIdMode,
    pub session_id: i64,
    /// The **already-resolved, capability-masked** model and effort values
    /// (issue #1149). The caller runs
    /// [`crate::agent::capabilities::resolve_agent_config`] with the harness's
    /// capability descriptor and the per-field cascade inputs; this function
    /// validates adapter-owned model syntax and forwards accepted values
    /// verbatim, without re-consulting capability flags. Empty / whitespace
    /// inputs and unsupported values are masked before they reach here.
    pub config: &'a crate::agent::capabilities::ResolvedAgentConfig,
    pub prefill: Option<&'a str>,
    pub sandbox: bool,
    pub resume_cause: ResumeCause,
}

/// Build the spawn command by composing the provider's recipe with the runtime environment.
///
/// `Err` means the spawn cannot be assembled safely, including an invalid model
/// reference or a macOS Seatbelt profile-write failure, which must not degrade
/// into an unsandboxed launch (#2034). The caller surfaces it as a spawn error.
pub fn build_spawn_command_prepared(
    input: SpawnCommandInput<'_>,
) -> Result<CommandBuilder, String> {
    build_spawn_command_with_adapter(input.provider.adapter(), input)
}

pub(super) fn build_spawn_command_with_adapter(
    adapter: &dyn crate::agent::provider::AgentProvider,
    input: SpawnCommandInput<'_>,
) -> Result<CommandBuilder, String> {
    let SpawnCommandInput {
        resolved,
        routing,
        session_id_mode,
        session_id,
        config,
        prefill,
        sandbox,
        resume_cause,
        ..
    } = input;
    let platform = if resolved.env_type == EnvType::Wsl {
        Platform::Linux
    } else if resolved.env_type == EnvType::WindowsInterop {
        Platform::Windows
    } else {
        Platform::current()
    };

    if let Some(model) = config.model.as_deref() {
        adapter.validate_model_override(model)?;
    }
    if let Some(agent) = config.agent.as_deref() {
        adapter.validate_agent_override(agent)?;
    }

    // Compose the harness's launch contribution: recipe + capability
    // descriptor + env policy, all from the same adapter. The
    // capability-mask guarantee still holds — the resolver ran before
    // we got here, and the helper re-asserts the descriptor on the
    // forward as defence in depth (issue #1179).
    let session_ref = match session_id_mode {
        SessionIdMode::Assign(id) => SessionIdModeRef::Assign(id.as_str()),
        SessionIdMode::Resume(id) => SessionIdModeRef::Resume {
            id: id.as_str(),
            cause: resume_cause,
        },
        SessionIdMode::None => SessionIdModeRef::None,
    };
    let input = HarnessLaunchInput {
        platform,
        runtime: resolved.env_type,
        session: session_ref,
        config,
        prefill,
        sandbox,
        routing: Some(routing),
        node_id: Some(session_id),
    };
    let prepared = adapter.prepare_launch(input);

    let runtime = routing.launch_runtime();
    let executable_override = routing.executable_override().and_then(|path| path.to_str());
    let mut cmd = spawn_environment::wrap(
        prepared.recipe,
        resolved.env_type,
        runtime.wsl_distro.as_deref(),
        executable_override,
        &resolved.spawn_path,
        session_id,
        sandbox,
    )?;

    // Apply the concrete environment contribution from the selected adapter.
    for k in &prepared.environment.env_remove {
        cmd.env_remove(k);
    }
    let mut command_wsl_env: Vec<&str> = Vec::new();
    let launch_environment = adapter.launch_environment(resolved, session_id);
    for (key, value) in &launch_environment {
        cmd.env(key, value);
        command_wsl_env.push(key);
    }
    for (k, v) in &prepared.environment.env_set {
        cmd.env(k, v);
        command_wsl_env.push(k);
    }

    command_wsl_env.extend(["TERM", "COLORTERM", "FORCE_COLOR"]);
    spawn_environment::apply_wsl_env(
        &mut cmd,
        resolved.env_type,
        &command_wsl_env,
        adapter.wsl_passthrough_env(),
    );
    Ok(cmd)
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
    explicit_agent: Option<&'a str>,
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
            mesh: mesh_model,
            application: app_default.and_then(|v| v.model.as_deref()),
        },
        effort: crate::agent::capabilities::FieldInputs {
            explicit: explicit_effort.and_then(non_empty_trim),
            mesh: mesh_effort,
            application: app_default.and_then(|v| v.effort.as_deref()),
        },
        // Issue #2151: only the application layer carries a permission
        // mode today (the per-harness Settings default) — there is no
        // per-launch or per-mesh permission slot yet.
        permission_mode: crate::agent::capabilities::FieldInputs {
            explicit: None,
            mesh: None,
            application: app_default.and_then(|v| v.permission_mode.as_deref()),
        },
        agent: crate::agent::capabilities::FieldInputs {
            explicit: explicit_agent.and_then(non_empty_trim),
            mesh: None,
            application: app_default.and_then(|v| v.agent.as_deref()),
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
/// `app_default` is the ALREADY-LOOKED-UP application layer for this Spawn
/// Option (`preferences::harness_default_for` reads the harness half off the
/// composite id and drops the model/effort for a Proxied row). The launch
/// phase resolves it at its seam so this helper stays free of
/// `preferences::load()` (which would force the test to populate the
/// in-process preferences cache).
pub(crate) fn resolve_spawn_config(
    provider: Provider,
    explicit_model: Option<&str>,
    explicit_effort: Option<&str>,
    explicit_extra_args: Option<&str>,
    app_default: Option<&crate::preferences::HarnessConfigValue>,
    explicit_agent: Option<&str>,
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
            explicit_agent,
        ),
        explicit_extra_args,
    )
}
