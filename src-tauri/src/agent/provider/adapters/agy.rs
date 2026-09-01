use crate::agent::capabilities::EffortControlKind;
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::path::Path;

use std::sync::atomic::{AtomicU64, Ordering};

pub struct AgyAdapter;
pub static AGY: AgyAdapter = AgyAdapter;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write content to path atomically using a unique PID+counter .tmp file,
/// fsync for durability, and atomic rename.
fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("hooks.json");
    let tmp = path.with_file_name(format!("{}.{}.{}.tmp", file_name, std::process::id(), counter));

    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        if let Err(rm_err) = std::fs::remove_file(&tmp) {
            tracing::warn!("atomic_write: failed to clean up temp file {:?}: {}", tmp, rm_err);
        }
        return Err(e);
    }
    Ok(())
}

/// The callback command Antigravity (agy) lifecycle hooks run. AGY pipes
/// the hook's stdin JSON — `{conversationId, transcriptPath, fullyIdle,
/// terminationReason, …}` on `Stop` (issue #1285) — into the command;
/// `--data-binary @-` forwards it as the POST body so the attention route
/// can classify the event. The port/session env vars are set per-agent
/// in `spawn_environment` and inherited by the hook process. AGY supplies
/// the platform shell, so this function returns a bare shell command
/// rather than adding a second `cmd.exe /c` or `sh -c` wrapper.
fn hook_command(platform: Platform) -> String {
    match platform {
        Platform::Windows => {
            "curl.exe -sf --connect-timeout 1 --max-time 2 -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID% >nul 2>nul & echo {\"decision\":\"allow\"}"
                .to_string()
        }
        _ => {
            "curl -sf --connect-timeout 1 --max-time 2 -X POST -H 'Content-Type: application/json' --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID >/dev/null 2>/dev/null; printf '%s\\n' '{\"decision\":\"allow\"}'"
                .to_string()
        }
    }
}

/// Ensure `<project>/.agents/hooks.json` carries the Stop attention webhook
/// under the `buildmesh-attention` namespace. `Stop` uses the simple
/// `[{type, command}]` form. `PreToolUse` fires before every tool and its
/// required decision response makes it a blocking gate. Buildmesh launches
/// AGY with permissions skipped, so Stop is the unambiguous Node Turn
/// signal here. Idempotent, and preserves any unrelated top-level keys the
/// user added. Writes atomically to prevent corruption.
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
    let expected = serde_json::json!({ "Stop": [stop_hook] });
    if settings.get("buildmesh-attention") == Some(&expected) {
        return Ok(());
    }
    settings["buildmesh-attention"] = expected;
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))?;
    atomic_write(path, &content).map_err(|e| format!("failed to write hooks.json: {e}"))?;
    tracing::info!("agy provision_attention_hooks: wrote {:?}", path);
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
            trailing_args: Vec::new(),
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

    /// Delivers completion / background turn signals via `Stop` hook (`fullyIdle: true`
    /// vs `fullyIdle: false`). Tool approvals are unavailable under the current
    /// `--dangerously-skip-permissions` launch policy (issue #1367).
    fn requires_attention_hook(&self) -> bool {
        true
    }

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        AttentionCapability::Hook {
            events: vec![LifecycleKind::TurnCompleted, LifecycleKind::BackgroundRunning],
            launch_mode: AttentionLaunchMode::SkipPermissions,
            trust: Some("workspace trust".into()),
            min_version: Some("1.0.0".into()),
        }
    }

    fn ensure_workspace_trusted(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
    ) -> Result<(), String> {
        crate::agent::workspace_trust::ensure_trusted(resolved);
        Ok(())
    }

    /// AGY lifecycle hooks live in the project-local `.agents/` dir as
    /// `hooks.json` (issue #1285, #1367). The namespace key is
    /// `buildmesh-attention` so user-added sibling namespaces (other
    /// tools, custom automation) round-trip through a re-run untouched.
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        let agents_dir = Path::new(&resolved.host_path).join(".agents");
        std::fs::create_dir_all(&agents_dir)
            .map_err(|e| format!("failed to create .agents dir: {e}"))?;
        let hooks_path = agents_dir.join("hooks.json");
        ensure_hooks_json(&hooks_path, &hook_command(Platform::current()))
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_extra_args(&self) -> bool {
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

    /// Issue #1287 — Antigravity's CLI accepts a native `--sandbox`
    /// flag ("Run in a sandbox with terminal restrictions enabled").
    /// Forwarded when the parent mesh has its `sandbox` toggle on, in
    /// addition to the orchestrator's outer platform-level containment
    /// (macOS Seatbelt, Windows restricted-token). The two layers are
    /// independent — the outer wrapper confines the filesystem; this
    /// flag confines the agent's own terminal-side operations.
    fn sandbox_args(&self) -> Vec<String> {
        vec!["--sandbox".into()]
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

    fn provision_agy(project: &Path) {
        let path = project.to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: EnvType::Windows,
        };
        AGY
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
            .unwrap();
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
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&AGY, input);
        assert_flag_followed_by_value(&prepared.recipe.base_args, "--effort", "high");
    }

    /// Issue #1287 — when the mesh has its `sandbox` toggle ON, the
    /// prepared recipe must carry `--sandbox` so the agy process
    /// itself runs in its own terminal-restricted sandbox (in addition
    /// to the orchestrator's outer wrapper).
    #[test]
    fn default_prepare_appends_sandbox_flag_when_mesh_sandbox_is_true() {
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: true,
        };
        let prepared = default_prepare(&AGY, input);
        let args = &prepared.recipe.base_args;
        assert!(
            args.contains(&"--sandbox".to_string()),
            "agy recipe must carry --sandbox when mesh.sandbox=true; got args = {args:?}"
        );
        // The flag must appear AFTER the base-recipe flags (`--dangerously-skip-permissions`)
        // so it's grouped with the harness's own switches, not the binary.
        let skip_perm = args
            .iter()
            .position(|a| a == "--dangerously-skip-permissions")
            .expect("base recipe carries --dangerously-skip-permissions");
        let sandbox_pos = args
            .iter()
            .position(|a| a == "--sandbox")
            .expect("--sandbox appended");
        assert!(
            sandbox_pos > skip_perm,
            "--sandbox must come AFTER the base recipe flags; got args = {args:?}"
        );
    }

    /// Issue #1287 — when the mesh has its `sandbox` toggle OFF (the
    /// default), `--sandbox` must NOT be added to the recipe. The
    /// orchestrator's outer wrapper also stays disabled, so the agent
    /// runs in its normal mode.
    #[test]
    fn default_prepare_omits_sandbox_flag_when_mesh_sandbox_is_false() {
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&AGY, input);
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--sandbox"),
            "agy recipe must NOT carry --sandbox when mesh.sandbox=false; got {:?}",
            prepared.recipe.base_args
        );
    }

    /// Adapter contract — `sandbox_args()` is the canonical source of
    /// truth for agy's sandbox flag shape. The default-prepare wiring
    /// should call it verbatim, so any future refactor that changes the
    /// flag (e.g. swapping `--sandbox` for `--no-sandbox=false`) trips
    /// this pin before the wire shape shifts.
    #[test]
    fn sandbox_args_returns_expected_flag() {
        assert_eq!(AGY.sandbox_args(), vec!["--sandbox".to_string()]);
    }

    // -------------------------------------------------------------------
    // Attention hook injection — issue #1285.
    // -------------------------------------------------------------------

    /// AGY's Stop payload carries the turn-end event in the simple
    /// `[{type, command}]` shape (no tool matcher — the whole turn is
    /// the event). It must POST to the attention endpoint and forward the
    /// hook's stdin as the body (`--data-binary @-`).
    #[test]
    fn inject_writes_stop_webhook() {
        let temp = TempDir::new().unwrap();
        provision_agy(temp.path());

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

        assert!(attention.get("PreToolUse").is_none());
    }

    /// Re-running injection over an already-correct project is a no-op —
    /// re-spawns (resume / handover / re-spawn on a closed node) must
    /// not rewrite the file and risk churn on unrelated siblings the
    /// user has added.
    #[test]
    fn inject_is_idempotent() {
        let temp = TempDir::new().unwrap();
        provision_agy(temp.path());
        let hooks_first = read_hooks_json(temp.path());

        provision_agy(temp.path());
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

        provision_agy(temp.path());

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

        provision_agy(temp.path());

        let hooks = read_hooks_json(temp.path());
        assert!(hooks["buildmesh-attention"]["Stop"].is_array());
        assert_eq!(hooks["other"], "kept");
    }

    /// The Windows hook command uses cmd environment syntax (`%VAR%`) and
    /// is a bare command because AGY supplies the shell. Unix uses `$VAR`
    /// syntax and likewise does not add a nested shell wrapper.
    #[test]
    fn hook_command_uses_platform_env_syntax() {
        let win = hook_command(Platform::Windows);
        assert!(win.starts_with("curl.exe "), "win: {win}");
        assert!(!win.contains("cmd.exe /c"), "win: {win}");
        assert!(win.contains("%BUILDMESH_PORT%"), "win: {win}");
        assert!(win.contains("%BUILDMESH_SESSION_ID%"), "win: {win}");

        for platform in [Platform::Macos, Platform::Linux] {
            let unix = hook_command(platform);
            assert!(unix.starts_with("curl "), "unix: {unix}");
            assert!(!unix.contains("sh -c"), "unix: {unix}");
            assert!(unix.contains("$BUILDMESH_PORT"), "unix: {unix}");
            assert!(unix.contains("$BUILDMESH_SESSION_ID"), "unix: {unix}");
        }
    }

    /// AGY invokes hook commands through its own Windows shell wrapper. The
    /// command stored in hooks.json must therefore be a bare command rather
    /// than a second `cmd.exe /c "..."` wrapper. AGY also requires JSON on
    /// stdout for hook decisions, so the callback must fail open with allow
    /// even when Buildmesh is not reachable.
    #[test]
    fn hook_command_is_bare_and_fail_open() {
        let win = hook_command(Platform::Windows);
        assert!(!win.starts_with("cmd.exe /c"), "win: {win}");
        assert!(!win.starts_with('"') && !win.ends_with('"'), "win: {win}");
        assert!(win.contains("curl.exe"), "win: {win}");
        assert!(win.contains("%BUILDMESH_PORT%"), "win: {win}");
        assert!(win.contains("echo {\"decision\":\"allow\"}"), "win: {win}");

        let unix = hook_command(Platform::Linux);
        assert!(!unix.starts_with("sh -c"), "unix: {unix}");
        assert!(
            unix.contains("printf '%s\\n' '{\"decision\":\"allow\"}'"),
            "unix: {unix}"
        );
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

    /// Issue #1367: Verify that atomic write leaves no temporary `.tmp` residue.
    #[test]
    fn inject_leaves_no_tmp_residue() {
        let temp = TempDir::new().unwrap();
        provision_agy(temp.path());

        let agents_dir = temp.path().join(".agents");
        assert!(agents_dir.join("hooks.json").exists());
        // Verify no leftover .tmp files matching the pattern
        let entries = std::fs::read_dir(&agents_dir).unwrap();
        let tmp_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(tmp_files.is_empty(), "found leftover tmp files: {tmp_files:?}");
    }

    /// Issue #1367: Verify that user-defined sibling namespaces in
    /// .agents/hooks.json are preserved across an inject.
    #[test]
    fn inject_preserves_user_defined_sibling_namespaces() {
        let temp = TempDir::new().unwrap();
        let agents = temp.path().join(".agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("hooks.json"),
            r#"{"user-tool":{"Stop":[{"type":"command","command":"echo"}]}}"#,
        )
        .unwrap();

        provision_agy(temp.path());

        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(agents.join("hooks.json")).unwrap())
                .unwrap();
        assert!(parsed["user-tool"]["Stop"].is_array());
        assert!(parsed["buildmesh-attention"]["Stop"].is_array());
    }

    /// Issue #1367: Under `--dangerously-skip-permissions`, AGY automatically
    /// executes tools without prompting for user permission. PreToolUse is
    /// a blocking execution gate (requiring synchronous decision responses),
    /// not an async notification hook. Thus, only Stop is injected.
    #[test]
    fn skip_permissions_mode_omits_pre_tool_use_gate() {
        let temp = TempDir::new().unwrap();
        provision_agy(temp.path());

        let hooks = read_hooks_json(temp.path());
        let attention = &hooks["buildmesh-attention"];
        assert!(
            attention.get("Stop").is_some(),
            "Stop hook must be present for turn completion and background detection"
        );
        assert!(
            attention.get("PreToolUse").is_none(),
            "PreToolUse must NOT be injected under skip-permissions mode"
        );
        assert!(
            attention.get("Notification").is_none(),
            "Notification is a Claude/Grok concept; AGY uses Stop"
        );
    }
}
