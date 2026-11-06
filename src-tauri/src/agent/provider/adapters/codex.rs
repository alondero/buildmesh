mod hooks;
use hooks::*;
mod install_cache;
pub use install_cache::*;
mod install_probe;
pub(crate) use install_probe::*;
mod profiles;
pub use profiles::*;
mod runtime_files;
use runtime_files::*;
mod trust;
use trust::*;

use super::hook_contract;
use crate::agent::capabilities::{EffortControlKind, CODEX_EFFORT_ALLOWED, CODEX_EFFORT_KEY};
use crate::agent::managed_config;
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::circuit::strategy::{
    FinalReportSource, HookPush, NativePull, ObservationStrategy, OwnedWorkCoverage, PullSource,
    PushSource, ReconciliationPolicy, StrategyNotes, TurnIdentity,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use base64::Engine;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use toml_edit::{value, DocumentMut, Item, Table};

pub struct CodexAdapter;
pub static CODEX: CodexAdapter = CodexAdapter;

const CODEX_OWNERSHIP_LIMIT: &str = "Codex recorded a completed foreground turn, but its rollout cannot verify all owned child and background work. Inspect the agent and Recheck evidence; this turn does not establish assigned-work completion.";

/// Native `request_user_input` hooks were verified against Codex 0.154.0.
/// Older binaries may accept the config but omit the callbacks Buildmesh uses
/// to track an outstanding human question.
const CODEX_MIN_HOOK_VERSION: &str = "0.154.0";

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::PowerShell,
    }
}

fn base_flags() -> Vec<String> {
    let sandbox = if std::env::var_os(CODEX_HOOK_RELAY_ENV).is_some() {
        // Controlled hook-delivery runs may exercise real callbacks, but
        // must not give the model a path to alter the fixture or workspace.
        "read-only"
    } else {
        "danger-full-access"
    };
    base_flags_with_sandbox(sandbox)
}

fn base_flags_with_sandbox(sandbox: &str) -> Vec<String> {
    // Issue #2151: bare of approval flags — `--ask-for-approval never`
    // moved to `permission_args` so Settings owns the choice. `--sandbox`
    // stays here (it is the sandbox control, separate per #542/#828/#1053/
    // #2034), as does `--dangerously-bypass-hook-trust` (hook trust, not
    // tool approval).
    vec![
        "--sandbox".into(),
        sandbox.into(),
        // Do not force `--no-alt-screen`: Codex's fullscreen transcript avoids
        // rebuilding terminal scrollback after width changes.
        // Run the project-local `.codex/hooks.json` hooks without Codex's
        // interactive hook-review prompt (issue #884) — a headless spawn
        // must never block on a trust prompt. The adapter also provisions the
        // runtime's project trust entry before launch.
        "--dangerously-bypass-hook-trust".into(),
    ]
}

pub const PROXY_CREDENTIAL_ENV: &str = "BUILDMESH_CODEX_PROVIDER_KEY";
pub const MIN_PROXY_CODEX_VERSION: (u32, u32, u32) = (0, 144, 0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexInstall {
    pub executable: String,
    pub version: String,
    pub runtime_identity: String,
    pub codex_home: String,
    pub wsl_distro: Option<String>,
}
static CLI_CAPABILITY_CACHE: Lazy<Mutex<HashSet<String>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Longest prompt passed as a launch argument.
///
/// A Windows launch crosses two limits, and the smaller one is the binding
/// one. The whole launch is re-encoded into one PowerShell `-EncodedCommand`
/// script, which caps at CreateProcess' 32,767 characters (~2.7 characters of
/// base64 per character of script); PowerShell then runs the npm `codex.CMD`
/// shim by handing it to `cmd.exe`, whose own command line caps at 8,191
/// characters. Codex's attention hooks already spend about 3,200 of those 8,191
/// characters, and a prompt is encoded into the script the same way (a
/// quote-heavy one doubles), so 2,000 characters keeps the worst case inside
/// both limits (#2184).
const MAX_POSITIONAL_PREFILL_CHARS: usize = 2_000;

impl AgentProvider for CodexAdapter {
    fn prepare_launch(
        &self,
        input: crate::agent::launch::HarnessLaunchInput<'_>,
    ) -> crate::agent::launch::PreparedHarnessLaunch {
        use crate::agent::launch_routing::PreparedLaunchRouting;
        let routing = input.routing;
        let runtime = input.runtime;
        let mut config = input.config.clone();
        if let Some(PreparedLaunchRouting::CodexProxy { descriptor, .. }) = routing {
            if config.model.is_none() {
                config.model = Some(descriptor.model_id.clone());
            }
        }
        let mut prepared = crate::agent::launch::default_prepare(
            self,
            crate::agent::launch::HarnessLaunchInput {
                config: &config,
                ..input
            },
        );
        prepared
            .environment
            .env_remove
            .extend(["OPENAI_API_KEY".into(), "OPENAI_BASE_URL".into()]);
        if routing.is_some() {
            prepared
                .environment
                .env_remove
                .push(PROXY_CREDENTIAL_ENV.into());
        }
        // A pairing-scoped key inherited through an environment route is never
        // an alternative credential for a native Codex process.
        prepared
            .environment
            .env_set
            .retain(|(key, _)| key != PROXY_CREDENTIAL_ENV);
        if let Some(PreparedLaunchRouting::CodexProxy {
            profile_name,
            descriptor,
            credential_reference,
            credential,
            ..
        }) = routing
        {
            prepared
                .recipe
                .base_args
                .extend(["--profile".into(), profile_name.clone()]);
            if descriptor.reasoning_effort == Some(true) {
                prepared.recipe.base_args.extend([
                    "-c".into(),
                    "model_supports_reasoning_summaries=true".into(),
                    "-c".into(),
                    "model_reasoning_summary=\"none\"".into(),
                ]);
            }
            debug_assert_eq!(credential_reference, PROXY_CREDENTIAL_ENV);
            prepared
                .environment
                .env_set
                .push((credential_reference.clone(), credential.clone()));
        }
        if let Some(routing) = routing {
            let home = if runtime == EnvType::WindowsInterop {
                routing
                    .launch_runtime()
                    .harness_home
                    .map(|home| crate::env::windows_path_from_wsl(&home))
            } else if runtime == EnvType::Windows {
                match routing {
                    PreparedLaunchRouting::Native { harness_home, .. } => harness_home.clone(),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(home) = home {
                prepared
                    .environment
                    .env_set
                    .push(("CODEX_HOME".into(), home));
            }
        }
        prepared
    }

    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                attention_capability: {
                    use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
                    use crate::agent::session_lifecycle::LifecycleKind;
                    AttentionCapability::Hook {
                        events: vec![
                            LifecycleKind::TurnCompleted,
                            LifecycleKind::InputRequired,
                            LifecycleKind::QuestionRequested,
                            LifecycleKind::PermissionRequested,
                            LifecycleKind::BackgroundRunning,
                        ],
                        launch_mode: AttentionLaunchMode::PermissionAsk,
                        trust: Some("codex project trust (#1379)".into()),
                        min_version: Some(CODEX_MIN_HOOK_VERSION.into()),
                    }
                },
                circuit_observation: {
                    ObservationStrategy {
            push: PushSource::Hooks(HookPush {
                parse: |value| hook_contract::parse("codex", false, value),
                source: "codex_native_hook",
                request_source: "codex_request_hook",
                ownership_gap: None,
                foreground: true,
            }),
            pull: PullSource::NativeTurnCompletion(NativePull {
                source: "codex_rollout_task_complete",
                recheck_source: "codex_rollout_foreground_recheck",
                label: "Codex",
                ownership_limit: CODEX_OWNERSHIP_LIMIT,
            }),
            turn_identity: TurnIdentity::NativeToken,
            owned_work: OwnedWorkCoverage::Unavailable {
                reason: "rollouts do not expose a complete child/background registry",
            },
            final_report: FinalReportSource::PullMessage,
            reconciliation: ReconciliationPolicy {
                yielded_budget_ms: 60_000,
            },
            passive_watcher: None,
            notes: StrategyNotes {
                foreground: "Identity-bound rollout task_complete pull",
                owned_work: "rollouts do not expose a complete child/background registry",
                final_report: "Scrubbed task_complete response; oversized or missing records remain unavailable",
                reconciliation: "Bounded native rollout pull with session, turn, input and timestamp fences",
            },
        }
                },
                produces_readable_transcript: true,
                supports_model_override: true,
                supports_extra_args: true,
                supports_prefill: true,
                available_on: { &[Platform::Macos, Platform::Windows, Platform::Linux] },
                effort_control: {
                    EffortControlKind::InlineConfig {
                        key: CODEX_EFFORT_KEY.to_string(),
                        allowed: CODEX_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
                    }
                },
                permission_modes: {
                    use crate::agent::capabilities::PermissionModeOption;
                    vec![
                        PermissionModeOption::unattended(
                            "--ask-for-approval never",
                            "Approvals off (today's behavior; required for unattended runs).",
                        ),
                        PermissionModeOption::prompt(
                            "Prompts on (no flag)",
                            "Codex asks for approval like a human-launched session.",
                        ),
                    ]
                },
                ..Default::default()
            });
        &DESCRIPTOR
    }

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
        // Issue #2151: bare of approval flags (`base_flags` no longer
        // carries `--ask-for-approval never`). The effective permission
        // mode contributes it via `permission_args` in `default_prepare`.
        SpawnRecipe {
            binary: "codex",
            base_args: base_flags(),
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == crate::agent::capabilities::PERMISSION_MODE_UNATTENDED {
            vec!["--ask-for-approval".into(), "never".into()]
        } else {
            Vec::new()
        }
    }

    /// Codex draws mid-size pastes inline and can collapse larger Windows
    /// paste bursts individually. Readiness accounts for every burst count
    /// and the expected inline segments between markers.
    fn paste_gate_policy(&self) -> crate::agent::provider::PasteGatePolicy {
        crate::agent::provider::PasteGatePolicy::RenderedWithSplitMarker
    }

    /// Codex's composer needs CR line endings inside a bracketed paste.
    ///
    /// A ConPTY probe against Codex 0.162.1 (140x35, flags `0x2 | 0x4`,
    /// bundled 1.24.260710001 runtime) staged a 12-line synthetic paste inside
    /// `ESC[200~ … ESC[201~` and read the rendered composer: LF dropped every
    /// newline and glued the lines together, while both CRLF and CR kept the
    /// multiline layout. Nothing was submitted in any run.
    ///
    /// This is safe for the split-marker gate because Codex's `apply_paste`
    /// normalizes CRLF and bare CR to LF *before* counting, so the
    /// `[Pasted Content N chars]` proof is identical for all three endings.
    /// Runtime evidence: `docs/learning/codex-paste-confirmation.md` and
    /// `docs/learning/grok-terminal-paste.md`; the defect is issue #2199.
    fn paste_requires_cr_newlines(&self) -> bool {
        true
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
        spawn.base_args = [
            "--ask-for-approval",
            "never",
            "exec",
            "--ignore-user-config",
            "--ephemeral",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "--color",
            "never",
            "-c",
            "features.shell_tool=false",
            "-c",
            "features.multi_agent=false",
        ]
        .map(str::to_owned)
        .to_vec();
        spawn.trailing_args = vec!["-".into()];
        let mut recipe = BackgroundRecipe::new(
            spawn,
            BackgroundPromptInput::Stdin,
            BackgroundResultOutput::LastMessageFile,
        );
        recipe.env_remove = vec!["OPENAI_API_KEY".into(), "OPENAI_BASE_URL".into()];
        Some(recipe)
    }

    fn spawn_recipe_for_resume(&self, platform: Platform, session_id: &str) -> Option<SpawnRecipe> {
        // `codex resume [OPTIONS] [SESSION_ID] [PROMPT]`. Options after the
        // UUID are the prompt, so a restart would "resume" into a garbage turn.
        // Keep the id in trailing_args; default_prepare and Codex proxy
        // `--profile`/`--model` append options to base_args only.
        let mut args = vec!["resume".into()];
        args.extend(base_flags());
        Some(SpawnRecipe {
            binary: "codex",
            base_args: args,
            trailing_args: vec![session_id.into()],
            windows_shell: shell_for(platform),
        })
    }

    /// Codex's global project trust is a launch prerequisite, not an attention
    /// hook side effect. Proxy preflight supplies the exact runtime home/distro
    /// so this edits the same trust store the child will read.
    fn ensure_workspace_trusted(
        &self,
        resolved: &ResolvedPath,
        runtime: &LaunchRuntime,
    ) -> Result<(), String> {
        ensure_codex_project_trusted(resolved, runtime)
    }

    /// Hooks travel as launch arguments ([`Self::launch_hook_args`]); what is
    /// left to do here is retiring the handlers earlier versions wrote into
    /// project files.
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        retire_project_file_hooks(resolved, runtime)
    }

    fn launch_hook_args(&self, node_id: i64, env_type: EnvType) -> Vec<String> {
        attention_hook_launch_args(node_id, env_type)
    }

    fn wsl_passthrough_env(&self) -> &'static [&'static str] {
        // The guest resolves its own CODEX_HOME. Forwarding the host process
        // value through WSLENV would make Windows state override guest state.
        &[]
    }

    /// Codex fires `UserPromptSubmit` when a prompt starts a turn.
    fn confirms_prompt_submission(&self) -> bool {
        true
    }

    fn prefill_requires_pty(&self, text: &str) -> bool {
        // Codex accepts a short positional prompt, but multiline review text
        // has been observed to reach its clap parser as separate arguments
        // (notably diff lines beginning with `+`). A single-line prompt that
        // begins with a CLI flag is also unsafe as a positional argument. The
        // PTY path preserves the prompt as one pasted turn and is the safe
        // automated-launch mode.
        //
        // A long prompt is also unsafe as an argument: on Windows the launch is
        // one re-encoded PowerShell script that already carries the attention
        // hooks, and the whole of it has to fit a 32,767-character command line.
        let trimmed = text.trim_start();
        text.contains('\n')
            || text.contains('\r')
            || trimmed.starts_with('-')
            || trimmed.starts_with('+')
            || text.chars().count() > MAX_POSITIONAL_PREFILL_CHARS
    }

    fn ready_for_initial_prompt(&self, tail: &str) -> bool {
        // Codex paints its composer (the `›` chevron plus a one-line
        // placeholder) in the TUI's first frame, and that painted input box is
        // the only startup evidence that stdin is being read — the state a
        // pasted prompt needs. Its earlier readiness banner
        // (`model: <name> /model to change`) is gone: Codex 0.158 renders the
        // model only as a bare `loading` line on boot and moves the resolved
        // model into the bottom status footer, so keying on that banner (or on
        // "not loading") never matched and the initial prompt was dropped after
        // the 300s wait. Launch still passes `--dangerously-bypass-hook-trust`,
        // so no review dialog can hold the composer back.
        const COMPOSER_PLACEHOLDERS: [&str; 2] =
            ["Ask Codex to do anything", "Ask a follow-up question"];
        COMPOSER_PLACEHOLDERS
            .iter()
            .any(|placeholder| tail.contains(placeholder))
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    /// Recent Codex TUIs no longer reliably print the session UUID on the
    /// PTY. Its rollout's `session_meta` record is the durable fallback, so
    /// capture it shortly after every fresh spawn rather than leaving a node
    /// impossible to resume after a Buildmesh restart.
    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        env_type: EnvType,
        _app: &tauri::AppHandle,
    ) {
        crate::services::codex_session::start_capture_poller(
            node_id,
            spawn_path.to_string(),
            env_type,
        );
    }

    fn recover_suspended_session_id(
        &self,
        spawn_path: &str,
        env_type: EnvType,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        crate::services::codex_session::find_historic_id_for_directory(
            env_type,
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn resume_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn effort_args(&self, effort: &str) -> Vec<String> {
        // Codex has no dedicated --effort flag, but exposes the same setting
        // as a stable per-invocation config override. Rust's debug string
        // representation supplies the quoted/escaped TOML string value.
        vec!["-c".into(), format!("model_reasoning_effort={effort:?}")]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
