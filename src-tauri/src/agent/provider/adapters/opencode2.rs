//! OpenCode 2.2.x: private servers, assigned identities and API-backed overrides.
//! The full TUI accepts neither --model nor --fork. The private `api` command
//! prepares durable session state before the interactive process receives input.

use std::path::Path;
use std::time::Duration;

use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode, PermissionModeOption};
use crate::agent::launch::SessionIdModeRef;
use crate::agent::provider::{
    shell_for, AgentProvider, LaunchRuntime, Platform, SessionPreparation, SpawnRecipe, UiMeta,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;

mod config;

pub struct OpenCode2Adapter;
pub static OPENCODE2: OpenCode2Adapter = OpenCode2Adapter;

impl AgentProvider for OpenCode2Adapter {
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                attention_capability: {
                    use crate::agent::session_lifecycle::LifecycleKind;
                    AttentionCapability::Hook {
                        events: vec![
                            LifecycleKind::TurnCompleted,
                            LifecycleKind::QuestionRequested,
                            LifecycleKind::PermissionRequested,
                        ],
                        launch_mode: AttentionLaunchMode::PermissionAsk,
                        trust: None,
                        min_version: Some("2.2.3".into()),
                    }
                },
                circuit_observation: {
                    // Native foreground fences and V2 child identity are a separate port.
                    crate::circuit::strategy::ObservationStrategy::UNWIRED
                },
                produces_readable_transcript: true,
                supports_model_override: true,
                // Issue #2716: the V2 full TUI has no `--agent` flag
                // (`opencode2 --help`), so the value is applied through
                // the session API in `prepare_session` instead — the same
                // transport `supports_model_override` uses.
                supports_agent: true,
                supports_extra_args: false,
                supports_prefill: true,
                available_on: { &[Platform::Windows, Platform::Macos, Platform::Linux] },
                permission_modes: {
                    vec![
                    PermissionModeOption::unattended(
                        "--auto",
                        "Use OpenCode's auto-approve mode with managed project permission rules.",
                    ),
                    PermissionModeOption::prompt(
                        "Prompts on (no flag)",
                        "OpenCode asks before permission-gated tools.",
                    ),
                ]
                },
                ..Default::default()
            });
        &DESCRIPTOR
    }

    fn id(&self) -> &'static str {
        "opencode2"
    }
    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "OpenCode 2".into(),
            color: "#f59e0b".into(),
            icon: "O".into(),
        }
    }
    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "opencode",
            base_args: vec!["--standalone".into()],
            trailing_args: vec![],
            windows_shell: shell_for(platform),
        }
    }

    fn confirms_prompt_submission(&self) -> bool {
        true
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == crate::agent::capabilities::PERMISSION_MODE_UNATTENDED {
            vec!["--auto".into()]
        } else {
            vec![]
        }
    }
    fn new_session_id(&self) -> String {
        format!("ses_{}", uuid::Uuid::new_v4().simple())
    }
    fn session_assign_args(&self, id: &str) -> Vec<String> {
        self.resume_args(id)
    }
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
    }
    fn model_args(&self, _model: &str) -> Vec<String> {
        vec![]
    }
    fn model_override_via_session_api(&self) -> bool {
        true
    }
    fn background_model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }
    fn validate_model_override(&self, model: &str) -> Result<(), String> {
        model_ref(model).map(|_| ())
    }
    fn validate_agent_override(&self, agent: &str) -> Result<(), String> {
        super::opencode::validate_agent_name(agent)
    }
    fn agent_args(&self, _agent: &str) -> Vec<String> {
        // The full V2 TUI has no `--agent` flag; the selector is applied
        // through the session API in `prepare_session` (mirroring the
        // model path). Returning empty keeps the interactive argv
        // flag-free so a future V2 release that adds the flag cannot
        // silently double-apply it.
        Vec::new()
    }
    fn agent_override_via_session_api(&self) -> bool {
        true
    }
    fn background_agent_args(&self, agent: &str) -> Vec<String> {
        // Issue #2716: `opencode2 run --agent <name>` is the one-shot
        // spelling; the background recipe never touches the session API.
        if agent.is_empty() {
            Vec::new()
        } else {
            vec!["--agent".into(), agent.into()]
        }
    }
    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![
            "--prompt".into(),
            super::opencode::flatten_cmd_prefill(text),
        ]
    }
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        node_id: i64,
    ) -> Result<(), String> {
        config::provision(Path::new(&resolved.host_path), node_id, None)
    }
    fn launch_environment(&self, resolved: &ResolvedPath, node_id: i64) -> Vec<(String, String)> {
        vec![(
            "OPENCODE_CONFIG".into(),
            config::config_path(Path::new(&resolved.spawn_path), node_id)
                .to_string_lossy()
                .into_owned(),
        )]
    }
    fn prepare_session(&self, input: SessionPreparation<'_>) -> Result<Option<String>, String> {
        if crate::sandbox::sandbox_requested(input.sandbox) {
            return Err("OpenCode 2 cannot prepare its private session API inside the Mesh sandbox yet. Turn off the Mesh sandbox or use another harness.".into());
        }
        if input.resolved.env_type != EnvType::Windows {
            return Err(
                "OpenCode 2 session preparation currently requires a host-native harness.".into(),
            );
        }
        let fallback = crate::agent::detection::resolve_spawn_binary("opencode");
        let executable = input
            .executable
            .or(fallback.as_deref())
            .ok_or("OpenCode 2 executable is missing")?;
        self.validate_executable(executable)?;
        let directory = Path::new(&input.resolved.host_path);
        config::provision(directory, input.node_id, None)?;
        let config_path = config::config_path(directory, input.node_id);
        if let Some(agent) = input.config.agent.as_deref() {
            self.validate_agent_override(agent)?;
        }
        let target = prepare_identity(
            input.session,
            input.config.model.as_deref(),
            input.config.agent.as_deref(),
            &mut |method, path, body| api(executable, directory, &config_path, method, path, body),
        )?;
        let id = match input.session {
            SessionIdModeRef::Assign(id) | SessionIdModeRef::Resume { id, .. } => id,
            SessionIdModeRef::None => unreachable!(),
        };
        config::provision(
            directory,
            input.node_id,
            Some((&target, crate::http_server::current_http_port())),
        )?;
        Ok((target != id).then_some(target))
    }
    fn validate_executable(&self, executable: &Path) -> Result<(), String> {
        super::opencode_version::require_major(executable, 2)
    }
    fn background_recipe(
        &self,
        platform: Platform,
    ) -> Option<crate::agent::background::BackgroundRecipe> {
        use crate::agent::{
            background::BackgroundRecipe,
            capabilities::{BackgroundPromptInput, BackgroundResultOutput},
        };
        let mut spawn = self.spawn_recipe(platform, EnvType::Windows);
        spawn.base_args = ["run", "--standalone", "--format", "json"]
            .map(str::to_owned)
            .to_vec();
        let mut recipe = BackgroundRecipe::new(
            spawn,
            BackgroundPromptInput::Stdin,
            BackgroundResultOutput::OpenCodeJsonLines,
        );
        recipe
            .env_remove
            .extend(["OPENCODE_CONFIG".into(), "OPENCODE_CONFIG_CONTENT".into()]);
        recipe
            .project_files
            .push(("opencode.json".into(), config::BACKGROUND_CONFIG.into()));
        Some(recipe)
    }
}

#[derive(Debug)]
enum SessionApiError {
    MissingSession,
    Failed(String),
}

impl From<SessionApiError> for String {
    fn from(error: SessionApiError) -> Self {
        match error {
            SessionApiError::MissingSession => "OpenCode 2 session was not found".into(),
            SessionApiError::Failed(message) => message,
        }
    }
}

fn prepare_identity(
    session: SessionIdModeRef<'_>,
    model: Option<&str>,
    agent: Option<&str>,
    request: &mut dyn FnMut(
        &str,
        &str,
        serde_json::Value,
    ) -> Result<serde_json::Value, SessionApiError>,
) -> Result<String, String> {
    let (id, fresh, fork) = match session {
        SessionIdModeRef::Assign(id) => (id, true, false),
        SessionIdModeRef::Resume { id, cause } => (
            id,
            false,
            cause == crate::agent::spawn::ResumeCause::Regenerate,
        ),
        SessionIdModeRef::None => {
            return Err("OpenCode 2 requires an assigned session identity".into())
        }
    };
    if crate::http::request::parse_opencode_session_id(id).as_deref() != Some(id) {
        return Err("Invalid OpenCode 2 session identity".into());
    }
    if let Some(agent) = agent {
        super::opencode::validate_agent_name(agent)?;
    }
    let target = if fork {
        let response = request(
            "POST",
            &format!("/api/session/{id}/fork"),
            serde_json::json!({}),
        )?;
        response["data"]["id"]
            .as_str()
            .and_then(crate::http::request::parse_opencode_session_id)
            .filter(|target| target != id)
            .ok_or("OpenCode 2 fork returned no distinct session identity")?
    } else {
        id.to_string()
    };
    let create = fresh
        || (!fork
            && match request(
                "GET",
                &format!("/api/session/{target}"),
                serde_json::Value::Null,
            ) {
                Ok(response) if response["data"]["id"].as_str() == Some(&target) => false,
                Ok(_) => {
                    return Err("OpenCode 2 lookup returned a different session identity".into())
                }
                // A failed initial create can leave only Buildmesh's persisted id.
                // Recover this explicit server result; do not mask transient failures.
                Err(SessionApiError::MissingSession) => true,
                Err(error) => return Err(error.into()),
            });
    if create {
        let mut body = serde_json::json!({"id": target});
        if let Some(model) = model {
            body["model"] = model_ref(model)?;
        }
        if let Some(agent) = agent {
            body["agent"] = serde_json::json!(agent);
        }
        let response = request("POST", "/api/session", body)?;
        if response["data"]["id"].as_str() != Some(&target) {
            return Err("OpenCode 2 created a different session identity".into());
        }
    } else {
        if let Some(model) = model {
            request(
                "POST",
                &format!("/api/session/{target}/model"),
                serde_json::json!({"model":model_ref(model)?}),
            )?;
        }
        if let Some(agent) = agent {
            request(
                "POST",
                &format!("/api/session/{target}/agent"),
                serde_json::json!({"agent": agent}),
            )?;
        }
    }
    Ok(target)
}

fn model_ref(model: &str) -> Result<serde_json::Value, String> {
    let invalid =
        || "OpenCode 2 model must be provider/model or provider/model#variant".to_string();
    let (provider, rest) = model
        .split_once('/')
        .filter(|(provider, id)| !provider.trim().is_empty() && !id.trim().is_empty())
        .ok_or_else(invalid)?;
    if provider.contains('#') {
        return Err(invalid());
    }
    let (id, variant) = match rest.split_once('#') {
        Some((id, variant)) if !id.is_empty() && !variant.is_empty() && !variant.contains('#') => {
            (id, Some(variant))
        }
        Some(_) => return Err(invalid()),
        None => (rest, None),
    };
    let mut model = serde_json::json!({"providerID": provider, "id": id});
    if let Some(variant) = variant {
        model["variant"] = serde_json::Value::String(variant.into());
    }
    Ok(model)
}

/// Windows PowerShell's legacy native-argument marshaller consumes JSON quotes.
/// Protect them (including preceding backslashes) for the native argv parser.
/// Encode cmd metacharacters as JSON Unicode escapes so an npm .cmd shim cannot
/// expand environment references or interpret data as command syntax.
fn api_body_argument(body: serde_json::Value) -> String {
    let json = body.to_string();
    if !cfg!(windows) {
        return json;
    }
    let mut escaped = String::with_capacity(json.len());
    let mut backslashes = 0;
    for ch in json.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        let count = if ch == '"' {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        escaped.extend(std::iter::repeat_n('\\', count));
        backslashes = 0;
        if "&|<>^!%()".contains(ch) {
            escaped.push_str(&format!("\\u{:04x}", ch as u32));
        } else {
            escaped.push(ch);
        }
    }
    escaped.extend(std::iter::repeat_n('\\', backslashes));
    escaped
}

fn api(
    executable: &Path,
    directory: &Path,
    config_path: &Path,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, SessionApiError> {
    let mut recipe = SpawnRecipe {
        binary: "opencode",
        base_args: vec![
            "api".into(),
            "--standalone".into(),
            method.into(),
            path.into(),
        ],
        trailing_args: vec![],
        windows_shell: crate::agent::provider::WindowsShell::PowerShell,
    };
    if method == "POST" {
        recipe
            .base_args
            .extend(["--data".into(), api_body_argument(body)]);
    }
    let mut command =
        crate::agent::spawn_environment::background_command(&recipe, Some(executable))
            .map_err(SessionApiError::Failed)?;
    command.current_dir(directory);
    command.env("OPENCODE_CONFIG", config_path);
    let output = super::opencode_version::run_private_command(
        command,
        "OpenCode 2 session API",
        Duration::from_secs(30),
    )
    .map_err(SessionApiError::Failed)?;
    decode_api_output(output)
}

fn decode_api_output(output: std::process::Output) -> Result<serde_json::Value, SessionApiError> {
    if !output.status.success() {
        if serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .is_ok_and(|body| body["_tag"] == "SessionNotFoundError")
        {
            return Err(SessionApiError::MissingSession);
        }
        return Err(SessionApiError::Failed(format!(
            "OpenCode 2 session API failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    if output.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&output.stdout).map_err(|e| {
        SessionApiError::Failed(format!(
            "OpenCode 2 session API returned malformed JSON: {e}"
        ))
    })
}

#[cfg(test)]
#[path = "opencode2_tests.rs"]
mod tests;
