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
    fn supports_resume(&self) -> bool {
        true
    }
    fn auto_resume_on_startup(&self) -> bool {
        true
    }
    fn requires_attention_hook(&self) -> bool {
        true
    }
    fn produces_readable_transcript(&self) -> bool {
        true
    }
    fn supports_model_override(&self) -> bool {
        true
    }
    fn supports_extra_args(&self) -> bool {
        false
    }
    fn supports_prefill(&self) -> bool {
        true
    }
    fn confirms_prompt_submission(&self) -> bool {
        true
    }
    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Macos, Platform::Linux]
    }
    fn circuit_observation(&self) -> crate::circuit::strategy::ObservationStrategy {
        // Native foreground fences and V2 child identity are a separate port.
        crate::circuit::strategy::ObservationStrategy::UNWIRED
    }
    fn attention_capability(&self) -> AttentionCapability {
        use crate::agent::session_lifecycle::LifecycleKind;
        AttentionCapability::Hook {
            events: vec![
                LifecycleKind::TurnCompleted,
                LifecycleKind::PermissionRequested,
            ],
            launch_mode: AttentionLaunchMode::PermissionAsk,
            trust: None,
            min_version: Some("2.2.3".into()),
        }
    }
    fn permission_modes(&self) -> Vec<PermissionModeOption> {
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
        let target = prepare_identity(
            input.session,
            input.config.model.as_deref(),
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
        let response = request("POST", "/api/session", body)?;
        if response["data"]["id"].as_str() != Some(&target) {
            return Err("OpenCode 2 created a different session identity".into());
        }
    } else if let Some(model) = model {
        request(
            "POST",
            &format!("/api/session/{target}/model"),
            serde_json::json!({"model":model_ref(model)?}),
        )?;
    }
    Ok(target)
}

fn model_ref(model: &str) -> Result<serde_json::Value, String> {
    let (provider, id) = model
        .split_once('/')
        .filter(|(p, m)| !p.trim().is_empty() && !m.trim().is_empty() && !m.contains('#'))
        .ok_or("OpenCode 2 model must be provider/model without a #variant")?;
    Ok(serde_json::json!({"providerID": provider, "id": id}))
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
        recipe.base_args.extend(["--data".into(), body.to_string()]);
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
