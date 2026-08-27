use crate::agent::capabilities::EffortControlKind;
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct AgyAdapter;
pub static AGY: AgyAdapter = AgyAdapter;

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
        // Issue #1283: AGY now ships a transcript the reader parses
        // (`TranscriptFormat::Agy`); flip from the pre-#1283 negative.
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
}