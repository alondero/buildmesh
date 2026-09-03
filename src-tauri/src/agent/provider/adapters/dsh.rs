//! DeepSeek Harness CLI provider adapter (`dsh`).
//!
//! `dsh` is a launcher, not a single TUI binary: per the upstream
//! `apps/cli/README.md`, it parses only its own flags (`--profile`,
//! `--patch`, `--dump-default-config`, `--dump-config`, `--help`)
//! and forwards everything else to the booted profile
//! (`web`/`headless`/`sdk`/`sdk-minimal`/`acp`/custom).
//!
//! Issue #1365: no profile is validated, so we gate the capability
//! flags that depend on profile acceptance (resume, model override,
//! attention, transcript, prefill). The orchestrator's
//! `SessionIdMode` plumbing (prepare.rs:249) routes `None` when
//! `supports_resume = false`, so `session_assign_args` and
//! `resume_args` are never called. The default wire formatters
//! apply if a maintainer flips a capability back to true.
//!
//! **Shell wrapping**: `dsh.cmd` (Windows) is a `.cmd` shim that
//! `CreateProcess` won't run directly — `WindowsShell::Cmd` →
//! `cmd.exe /c dsh …`. macOS / Linux use `WindowsShell::Direct`.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct DshAdapter;
pub static DSH: DshAdapter = DshAdapter;

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

impl AgentProvider for DshAdapter {
    fn id(&self) -> &'static str {
        "dsh"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "DeepSeek Harness".into(),
            color: "#1E88E5".into(),
            icon: "D".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "dsh",
            base_args: Vec::new(),
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    // Gated to false pending profile validation (issue #1365).
    // When a maintainer flips a profile in, every false here needs
    // re-evaluation and the inventory pins in
    // `agent::capabilities::tests::inventory_matches_research_matrix`
    // + `models::tests::provider_capabilities_split_correctly` need
    // to flip too.
    fn supports_resume(&self) -> bool {
        false
    }
    fn auto_resume_on_startup(&self) -> bool {
        false
    }
    fn requires_attention_hook(&self) -> bool {
        false
    }
    fn produces_readable_transcript(&self) -> bool {
        false
    }
    fn supports_model_override(&self) -> bool {
        false
    }
    fn supports_prefill(&self) -> bool {
        false
    }
    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }
    fn supports_extra_args(&self) -> bool {
        // Escape hatch for users to manually inject `--profile <name>`
        // (or anything else the launcher accepts) until the
        // capability layer knows which profile to advertise. The
        // user's string is tokenised by `extra_args_args` and
        // appended verbatim — whether bare `dsh` accepts the
        // resulting argv is unverified.
        true
    }
    fn self_assigns_session_id(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(DSH.id(), "dsh");
        let ui = DSH.ui();
        assert_eq!(ui.label, "DeepSeek Harness");
        assert_eq!(ui.color, "#1E88E5");
        assert_eq!(ui.icon, "D");
    }

    #[test]
    fn spawn_recipe_is_bare_dsh_with_per_platform_shell() {
        for platform in [Platform::Linux, Platform::Macos] {
            let recipe = DSH.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "dsh");
            assert!(
                recipe.base_args.is_empty(),
                "no profile / no app args while gated; got {:?}",
                recipe.base_args
            );
            assert!(recipe.trailing_args.is_empty());
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "{:?} must use WindowsShell::Direct — got {:?}",
                platform,
                recipe.windows_shell
            );
        }
        let win_recipe = DSH.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(win_recipe.binary, "dsh");
        assert!(win_recipe.base_args.is_empty());
        assert!(win_recipe.trailing_args.is_empty());
        assert!(matches!(win_recipe.windows_shell, WindowsShell::Cmd));
    }

    #[test]
    fn available_on_all_three_platforms() {
        let platforms = DSH.available_on();
        assert_eq!(
            platforms.len(),
            3,
            "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
            platforms
        );
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    /// End-to-end: when `supports_resume = false`, the orchestrator
    /// routes `SessionIdModeRef::None` (prepare.rs:249) and the
    /// resolver drops `--model` — so the prepared recipe carries
    /// neither flag regardless of whether the user supplied values.
    /// This is the production path; gating the test to `Assign` would
    /// exercise a code path the orchestrator never takes.
    #[test]
    fn default_prepare_emits_no_session_id_or_model() {
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &crate::agent::capabilities::ResolvedAgentConfig {
                model: Some("deepseek-chat".into()),
                effort: None,
                extra_args: None,
            },
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&DSH, input);
        assert_eq!(prepared.recipe.binary, "dsh");
        let args = &prepared.recipe.base_args;
        assert!(
            !args.iter().any(|a| a == "--session-id"),
            "--session-id must not be in the recipe when supports_resume = false; \
             orchestrator routes SessionIdModeRef::None; got {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--model"),
            "--model must be dropped by the resolver while supports_model_override = false; \
             got {args:?}"
        );
    }

    /// Capabilities descriptor end-to-end (issue #1149 + #1365).
    /// Pins the gated state: no resume, no model, no attention hook,
    /// no transcript, no prefill. A maintainer flipping a profile in
    /// must flip these back too.
    #[test]
    fn capabilities_descriptor_advertises_gated_state() {
        let caps = DSH.capabilities();
        assert_eq!(caps.harness_id, "dsh");
        assert!(!caps.supports_resume);
        assert!(!caps.auto_resume_on_startup);
        assert!(!caps.requires_attention_hook);
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.supports_model_override);
        assert!(!caps.supports_effort_override);
        assert!(caps.supports_extra_args);
        assert!(!caps.supports_prefill);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
        assert_eq!(
            caps.available_on,
            vec![
                "windows".to_string(),
                "linux".to_string(),
                "macos".to_string()
            ]
        );
    }
}
