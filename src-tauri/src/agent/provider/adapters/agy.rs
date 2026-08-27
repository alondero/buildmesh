use crate::agent::capabilities::EffortControlKind;
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;
use std::path::Path;

pub struct AgyAdapter;
pub static AGY: AgyAdapter = AgyAdapter;

/// The callback command Antigravity (agy) lifecycle hooks run. AGY pipes
/// the hook's stdin JSON — `{conversationId, transcriptPath, fullyIdle,
/// terminationReason, …}` on `Stop`, `{toolCall, stepIdx, conversationId,
/// transcriptPath, …}` on `PreToolUse` (issue #1285) — into the command;
/// `--data-binary @-` forwards it as the POST body so the attention route
/// can classify the event. The port/session env vars are set per-agent
/// in `spawn_environment` and inherited by the hook process; AGY executes
/// the command string itself (no implicit login shell), so each platform
/// wraps in the shell that expands its own env-var syntax.
fn hook_command(platform: Platform) -> String {
    match platform {
        Platform::Windows => {
            "cmd.exe /c \"curl -sf -X POST --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID%\""
                .to_string()
        }
        _ => {
            "sh -c \"curl -sf -X POST --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true\""
                .to_string()
        }
    }
}

/// Ensure `<project>/.agents/hooks.json` carries the Stop + PreToolUse
/// attention webhooks under the `buildmesh-attention` namespace. AGY's
/// schema mixes two shapes (issue #1285): `Stop` is the simple
/// `[{type, command}]` form, while `PreToolUse` carries the
/// `[{matcher, hooks: [{type, command}]}]` shape so the harness can
/// filter by tool name. Both events mean "the user may be needed" —
/// `Stop` with `fullyIdle: false` and any tool-driven `PreToolUse`
/// prompt get sorted from background-task waits by the backend's
/// `decide`. Idempotent, and preserves any unrelated top-level keys the
/// user added.
fn ensure_hooks_json(path: &Path, command: &str) -> Result<(), String> {
    let mut settings: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !settings.is_object() {
        settings = serde_json::json!({});
    }

    // Stop fires the moment a turn ends. The simple shape — AGY's harness
    // doesn't need a matcher (no tool name to filter on; the whole turn
    // is the event) and the `[{type, command}]` form is the one the
    // spec documents for end-of-turn.
    let stop_hook = serde_json::json!({ "type": "command", "command": command });
    // PreToolUse fires before each tool call — including the run_command
    // / permission cases where AGY pauses for the user. Matcher is `*`
    // so every tool gets forwarded; the backend's `decide` filters by
    // event kind. The nested `[{matcher, hooks}]` form is what AGY uses
    // for PreToolUse per the lifecycle spec.
    let expected = serde_json::json!({
        "Stop": [stop_hook],
        "PreToolUse": [{
            "matcher": "*",
            "hooks": [{ "type": "command", "command": command }],
        }],
    });
    if settings.get("buildmesh-attention") == Some(&expected) {
        return Ok(());
    }
    settings["buildmesh-attention"] = expected;
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))?;
    std::fs::write(path, content).map_err(|e| format!("failed to write hooks.json: {e}"))?;
    tracing::info!("agy inject_attention_hook: wrote {:?}", path);
    Ok(())
}

impl AgentProvider for AgyAdapter {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Antigravity CLI".into(),
            color: "#10b981".into(),
            icon: "G".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "agy",
            base_args: vec!["--dangerously-skip-permissions".into()],
            windows_shell: WindowsShell::Direct,
        }
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn produces_readable_transcript(&self) -> bool {
        // Issue #1283: AGY writes per-conversation JSONL under
        // `~/.gemini/antigravity-cli/brain/<conversation-id>/.system_generated/
        // logs/transcript.jsonl`. `services::transcript_reader` knows the
        // shape (`TranscriptFormat::Agy`), so the Node Digest rich layer,
        // the `read_last_assistant_message` cheap digest, and the
        // archived-node resume picker all hydrate AGY sessions.
        true
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn requires_attention_hook(&self) -> bool {
        true
    }

    /// AGY lifecycle hooks live in the project-local `.agents/` dir as
    /// `hooks.json` (issue #1285). The namespace key is
    /// `buildmesh-attention` so user-added sibling namespaces (other
    /// tools, custom automation) round-trip through a re-run untouched.
    fn inject_attention_hook(&self, project_path: &Path) -> Result<(), String> {
        let agents_dir = project_path.join(".agents");
        std::fs::create_dir_all(&agents_dir)
            .map_err(|e| format!("failed to create .agents dir: {e}"))?;
        ensure_hooks_json(&agents_dir.join("hooks.json"), &hook_command(Platform::current()))
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--conversation".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec!["--prompt-interactive".into(), text.into()]
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    /// Antigravity CLI exposes a closed-vocab reasoning-effort knob via
    /// `--effort <low|medium|high>` (`agy --help` verified). The trait
    /// default `effort_args` already emits `["--effort", effort]`, which
    /// matches AGY's flag exactly; advertising `Closed` here lets the
    /// capability mask forward resolved effort values from the resolver
    /// (issue #1286).
    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::Closed {
            allowed: vec!["low".into(), "medium".into(), "high".into()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::capabilities::{EffortControlKind, ResolvedAgentConfig};
    use crate::agent::launch::{
        assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef,
    };
    use tempfile::TempDir;

    fn read_hooks_json(project: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(project.join(".agents").join("hooks.json"))
            .expect("hooks.json not written");
        serde_json::from_str(&content).expect("hooks.json is not valid JSON")
    }

    // -------------------------------------------------------------------
    // Capability contract — issue #1286.
    // -------------------------------------------------------------------

    /// Issue #1286: end-to-end descriptor pin. The Spawn Menu,
    /// resolver, and autopilot compatibility gate all consume this
    /// descriptor — drift here means the menu misroutes Antigravity.
    /// Mirrors the equivalent pin in `grok::tests`.
    #[test]
    fn capabilities_descriptor_advertises_effort_override() {
        let caps = AGY.capabilities();
        assert_eq!(caps.harness_id, "agy");
        assert!(caps.supports_resume);
        assert!(caps.supports_model_override);
        // Issue #1286: `--effort <low|medium|high>` is now advertised.
        assert!(caps.supports_effort_override);
        assert!(caps.supports_prefill);
        assert!(caps.requires_attention_hook);
        // Issue #1283: AGY writes per-conversation JSONL, so the
        // transcript reader can hydrate the Node Digest / archive picker.
        assert!(caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            EffortControlKind::Closed {
                allowed: vec!["low".into(), "medium".into(), "high".into()],
            }
        );
    }

    /// Recipe pin: when the resolver forwards an effort value for agy,
    /// `default_prepare` must append `--effort <level>` to the recipe
    /// (issue #1286 acceptance criteria 5). The table-driven
    /// `capability_recipe_coherence` test covers this for every
    /// adapter; this focused pin makes the agy shape explicit.
    #[test]
    fn agy_recipe_appends_effort_arg_when_resolved() {
        let config = ResolvedAgentConfig {
            model: None,
            effort: Some("high".to_string()),
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
        };
        let prepared = default_prepare(&AGY, input);
        assert_flag_followed_by_value(&prepared.recipe.base_args, "--effort", "high");
    }

    // -------------------------------------------------------------------
    // Attention hook injection — issue #1285.
    // -------------------------------------------------------------------

    /// AGY's Stop payload carries the turn-end event in the simple
    /// `[{type, command}]` shape (no tool matcher — the whole turn is
    /// the event), while PreToolUse uses the nested
    /// `[{matcher, hooks}]` shape so the harness can filter by tool name.
    /// Both must POST to the attention endpoint and forward the hook's
    /// stdin as the body (`--data-binary @-`).
    #[test]
    fn inject_writes_stop_and_pre_tool_use_webhooks() {
        let temp = TempDir::new().unwrap();
        AGY.inject_attention_hook(temp.path()).unwrap();

        let hooks = read_hooks_json(temp.path());
        let attention = hooks
            .get("buildmesh-attention")
            .expect("hooks.json must own the buildmesh-attention namespace");

        // Stop uses the simple shape — index straight into the first
        // array element.
        let stop_command = attention["Stop"][0]["command"]
            .as_str()
            .expect("Stop command missing");
        assert!(
            stop_command.contains("/api/attention/"),
            "Stop must POST to the attention endpoint: {stop_command}"
        );
        assert!(
            stop_command.contains("--data-binary @-"),
            "Stop must forward the hook stdin as the POST body: {stop_command}"
        );

        // PreToolUse nests under matcher → hooks → command.
        let pretool_command = attention["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .expect("PreToolUse command missing");
        assert!(
            pretool_command.contains("/api/attention/"),
            "PreToolUse must POST to the attention endpoint: {pretool_command}"
        );
        assert!(
            pretool_command.contains("--data-binary @-"),
            "PreToolUse must forward the hook stdin as the POST body: {pretool_command}"
        );

        // The PreToolUse matcher is `*` (every tool forwarded; backend
        // filters). Belt-and-braces: assert the literal so a future
        // refactor that narrows it accidentally is caught.
        assert_eq!(attention["PreToolUse"][0]["matcher"], "*");
    }

    /// Re-running injection over an already-correct project is a no-op —
    /// re-spawns (resume / handover / re-spawn on a closed node) must
    /// not rewrite the file and risk churn on unrelated siblings the
    /// user has added.
    #[test]
    fn inject_is_idempotent() {
        let temp = TempDir::new().unwrap();
        AGY.inject_attention_hook(temp.path()).unwrap();
        let hooks_first = read_hooks_json(temp.path());

        AGY.inject_attention_hook(temp.path()).unwrap();
        let hooks_second = read_hooks_json(temp.path());
        assert_eq!(hooks_first, hooks_second);
    }

    /// Injection only owns the `buildmesh-attention` key — unrelated
    /// top-level keys the user added (other tools, custom automation)
    /// round-trip through a re-injection untouched.
    #[test]
    fn inject_preserves_unrelated_top_level_keys() {
        let temp = TempDir::new().unwrap();
        let agents_dir = temp.path().join(".agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("hooks.json"),
            r#"{"custom_namespace":{"user":"kept"},"sibling":"keep-me"}"#,
        )
        .unwrap();

        AGY.inject_attention_hook(temp.path()).unwrap();

        let hooks = read_hooks_json(temp.path());
        assert_eq!(hooks["custom_namespace"]["user"], "kept");
        assert_eq!(hooks["sibling"], "keep-me");
        // And the new namespace is in place.
        assert!(hooks["buildmesh-attention"]["Stop"].is_array());
    }

    /// A user's existing `hooks.json` that already owns a
    /// `buildmesh-attention` key survives intact — same namespace, just
    /// re-asserted with the current command shape (which is byte-for-
    /// byte identical, so no actual rewrite fires).
    #[test]
    fn inject_preserves_sibling_keys_alongside_our_namespace() {
        let temp = TempDir::new().unwrap();
        let agents_dir = temp.path().join(".agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("hooks.json"),
            r#"{"buildmesh-attention":{"stale":"old"},"other":"kept"}"#,
        )
        .unwrap();

        AGY.inject_attention_hook(temp.path()).unwrap();

        let hooks = read_hooks_json(temp.path());
        assert!(hooks["buildmesh-attention"]["Stop"].is_array());
        assert_eq!(hooks["other"], "kept");
    }

    /// The Windows hook command must expand env vars with cmd syntax
    /// (`%VAR%`) under an explicit `cmd.exe /c`, and the Unix one with
    /// sh syntax — AGY executes the command string without a login shell
    /// of its own.
    #[test]
    fn hook_command_uses_platform_env_syntax() {
        let win = hook_command(Platform::Windows);
        assert!(win.starts_with("cmd.exe /c"), "win: {win}");
        assert!(win.contains("%BUILDMESH_PORT%"), "win: {win}");
        assert!(win.contains("%BUILDMESH_SESSION_ID%"), "win: {win}");

        for platform in [Platform::Macos, Platform::Linux] {
            let unix = hook_command(platform);
            assert!(unix.starts_with("sh -c"), "unix: {unix}");
            assert!(unix.contains("$BUILDMESH_PORT"), "unix: {unix}");
            assert!(unix.contains("$BUILDMESH_SESSION_ID"), "unix: {unix}");
        }
    }

    /// AGY's adapter contract: the harness still requires an attention
    /// hook (it's how Buildmesh learns a turn ended), and the harness
    /// has a transcript reader (issue #1283) so the Node Digest's rich
    /// layer populates alongside the spine.
    #[test]
    fn agy_declares_attention_hook_with_readable_transcript() {
        assert!(AGY.requires_attention_hook());
        // Issue #1283: AGY writes per-conversation JSONL.
        assert!(AGY.produces_readable_transcript());
    }
}
