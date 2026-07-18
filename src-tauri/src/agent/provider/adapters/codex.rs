use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;
use std::path::Path;

pub struct CodexAdapter;
pub static CODEX: CodexAdapter = CodexAdapter;

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::PowerShell,
    }
}

fn base_flags() -> Vec<String> {
    vec![
        "--ask-for-approval".into(),
        "never".into(),
        "--sandbox".into(),
        "danger-full-access".into(),
        // Run the project-local `.codex/hooks.json` hooks without Codex's
        // interactive workspace-trust review (issue #884) — a headless spawn
        // must never block on a trust prompt, and Buildmesh never edits the
        // user's global ~/.codex/config.toml to trust the path instead.
        "--dangerously-bypass-hook-trust".into(),
    ]
}

/// The callback command Codex hooks run. Codex pipes the hook's stdin JSON
/// (`{hook_event_name, session_id, transcript_path, …}` — issue #884) into the
/// command; `--data-binary @-` forwards it as the POST body so
/// `http/routes/attention.rs` can classify the event. The port/session env
/// vars are set per-agent by `spawn_environment` and inherited by the hook
/// process; Codex executes the command string itself (no implicit login
/// shell), so each platform wraps in the shell that expands its own env-var
/// syntax.
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

/// Ensure `<project>/.codex/config.toml` enables the hooks feature
/// (`[features] hooks = true` — `codex_hooks` is the legacy alias, issue
/// #884). Text-level merge (no toml dep): a file that already enables the
/// flag no-ops; an existing `[features]` section gets the flag inserted under
/// it; anything else gets the section appended, preserving existing content.
fn ensure_hooks_feature(path: &Path) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    // Substring check covers both `hooks = true` and `codex_hooks = true`.
    if existing.contains("hooks = true") {
        return Ok(());
    }
    let updated = if let Some(pos) = existing.find("[features]") {
        match existing[pos..].find('\n') {
            Some(nl) => {
                let insert_at = pos + nl + 1;
                format!(
                    "{}hooks = true\n{}",
                    &existing[..insert_at],
                    &existing[insert_at..]
                )
            }
            // `[features]` is the last line, without a trailing newline.
            None => format!("{existing}\nhooks = true\n"),
        }
    } else if existing.trim().is_empty() {
        "[features]\nhooks = true\n".to_string()
    } else {
        format!("{}\n\n[features]\nhooks = true\n", existing.trim_end())
    };
    std::fs::write(path, updated).map_err(|e| format!("failed to write config.toml: {e}"))
}

/// Ensure `<project>/.codex/hooks.json` carries the Stop + PermissionRequest
/// attention webhooks. Codex's matcher/event schema nests hook entries one
/// level deeper than Claude Code's (each event maps to matcher groups, each
/// carrying a `hooks` array — issue #884). Idempotent, and preserves any
/// unrelated top-level keys the user added.
fn ensure_hooks_json(path: &Path, command: &str) -> Result<(), String> {
    let mut settings: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !settings.is_object() {
        settings = serde_json::json!({});
    }

    let hook = serde_json::json!({ "type": "command", "command": command });
    // Stop fires the instant a turn ends; PermissionRequest fires when a tool
    // needs approval. Both mean "the user may be needed" — the backend's
    // `decide` sorts genuine yields from background-task waits.
    let expected = serde_json::json!({
        "Stop": [{ "hooks": [hook.clone()] }],
        "PermissionRequest": [{ "hooks": [hook] }],
    });
    if settings.get("hooks") == Some(&expected) {
        return Ok(());
    }
    settings["hooks"] = expected;
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))?;
    std::fs::write(path, content).map_err(|e| format!("failed to write hooks.json: {e}"))?;
    tracing::info!("codex inject_attention_hook: wrote {:?}", path);
    Ok(())
}

impl AgentProvider for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "OpenAI Codex".into(),
            color: "#10a37f".into(),
            icon: "X".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "codex",
            base_args: base_flags(),
            windows_shell: shell_for(platform),
        }
    }

    fn spawn_recipe_for_resume(
        &self,
        platform: Platform,
        session_id: &str,
    ) -> Option<SpawnRecipe> {
        let mut args = vec!["resume".into(), session_id.into()];
        args.extend(base_flags());
        Some(SpawnRecipe {
            binary: "codex",
            base_args: args,
            windows_shell: shell_for(platform),
        })
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

    /// Codex hooks live in the project-local `.codex/` dir: `config.toml`
    /// enables the hooks feature, `hooks.json` declares the webhooks
    /// (issue #886). The spawn flags carry the trust bypass that lets them
    /// run headlessly.
    fn inject_attention_hook(&self, project_path: &Path) -> Result<(), String> {
        let codex_dir = project_path.join(".codex");
        std::fs::create_dir_all(&codex_dir)
            .map_err(|e| format!("failed to create .codex dir: {e}"))?;
        ensure_hooks_feature(&codex_dir.join("config.toml"))?;
        ensure_hooks_json(&codex_dir.join("hooks.json"), &hook_command(Platform::current()))
    }

    /// Codex writes rollout transcripts under `~/.codex/sessions/` that
    /// `services::transcript_reader` parses via `TranscriptFormat::Codex`
    /// (issue #887).
    fn produces_readable_transcript(&self) -> bool {
        true
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Macos, Platform::Windows, Platform::Linux]
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn resume_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Local hooks only run headlessly with the trust bypass (issue #884) —
    /// both the fresh and resume spawn paths must carry the flag, or Codex
    /// blocks on an interactive workspace-review prompt.
    #[test]
    fn spawn_recipes_carry_the_hook_trust_bypass() {
        let bypass = "--dangerously-bypass-hook-trust".to_string();
        let fresh = CODEX.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert!(fresh.base_args.contains(&bypass), "fresh: {:?}", fresh.base_args);
        let resume = CODEX
            .spawn_recipe_for_resume(Platform::Windows, "sid-123")
            .expect("codex has a resume recipe");
        assert!(resume.base_args.contains(&bypass), "resume: {:?}", resume.base_args);
    }

    #[test]
    fn codex_declares_attention_hook_and_readable_transcript() {
        assert!(CODEX.requires_attention_hook());
        assert!(CODEX.produces_readable_transcript());
    }

    fn read_hooks_json(project: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(project.join(".codex").join("hooks.json"))
            .expect("hooks.json not written");
        serde_json::from_str(&content).expect("hooks.json is not valid JSON")
    }

    /// Injection writes both files: the feature flag and the two webhooks in
    /// Codex's nested matcher/event schema, POSTing the hook's stdin to the
    /// attention endpoint.
    #[test]
    fn inject_writes_config_and_hooks() {
        let temp = TempDir::new().unwrap();
        CODEX.inject_attention_hook(temp.path()).unwrap();

        let config = std::fs::read_to_string(temp.path().join(".codex").join("config.toml"))
            .expect("config.toml not written");
        assert!(config.contains("[features]"), "config: {config}");
        assert!(config.contains("hooks = true"), "config: {config}");

        let hooks = read_hooks_json(temp.path());
        for event in ["Stop", "PermissionRequest"] {
            let command = hooks["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("{event} hook missing: {hooks:#}"));
            assert!(
                command.contains("/api/attention/"),
                "{event} must POST to the attention endpoint: {command}"
            );
            assert!(
                command.contains("--data-binary @-"),
                "{event} must forward the hook stdin as the POST body: {command}"
            );
        }
    }

    /// Re-running injection over an already-correct project is a no-op.
    #[test]
    fn inject_is_idempotent() {
        let temp = TempDir::new().unwrap();
        CODEX.inject_attention_hook(temp.path()).unwrap();
        let config_first =
            std::fs::read_to_string(temp.path().join(".codex").join("config.toml")).unwrap();
        let hooks_first = read_hooks_json(temp.path());

        CODEX.inject_attention_hook(temp.path()).unwrap();
        let config_second =
            std::fs::read_to_string(temp.path().join(".codex").join("config.toml")).unwrap();
        assert_eq!(config_first, config_second);
        assert_eq!(hooks_first, read_hooks_json(temp.path()));
    }

    /// A user's existing config.toml keys survive; the flag lands under an
    /// existing `[features]` section instead of duplicating it (a duplicate
    /// table is a TOML parse error that would break Codex's whole config).
    #[test]
    fn config_merge_preserves_content_and_existing_features_section() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            "model = \"gpt-5.2-codex\"\n\n[features]\nweb_search = true\n",
        )
        .unwrap();

        CODEX.inject_attention_hook(temp.path()).unwrap();

        let config = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(config.contains("model = \"gpt-5.2-codex\""), "config: {config}");
        assert!(config.contains("web_search = true"), "config: {config}");
        assert!(config.contains("hooks = true"), "config: {config}");
        assert_eq!(
            config.matches("[features]").count(),
            1,
            "must not duplicate the [features] table: {config}"
        );
    }

    /// A config without a `[features]` section gets one appended, keeping the
    /// user's content intact.
    #[test]
    fn config_merge_appends_features_section_when_missing() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.2-codex\"\n").unwrap();

        CODEX.inject_attention_hook(temp.path()).unwrap();

        let config = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(config.contains("model = \"gpt-5.2-codex\""));
        assert!(config.contains("[features]\nhooks = true"), "config: {config}");
    }

    /// Injection only owns the `hooks` key of hooks.json — unrelated keys the
    /// user added survive.
    #[test]
    fn hooks_json_merge_preserves_unrelated_keys() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("hooks.json"), r#"{"custom":"kept"}"#).unwrap();

        CODEX.inject_attention_hook(temp.path()).unwrap();

        let hooks = read_hooks_json(temp.path());
        assert_eq!(hooks["custom"], "kept");
        assert!(hooks["hooks"]["Stop"].is_array());
    }

    /// The Windows hook command must expand env vars with cmd syntax (`%VAR%`)
    /// under an explicit `cmd.exe /c`, and the Unix one with sh syntax —
    /// Codex executes the command string without a login shell of its own.
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
}
