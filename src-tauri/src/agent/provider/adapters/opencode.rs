//! OpenCode provider adapter — open-source terminal coding agent installed on
//! PATH as a single `opencode` binary. The binary shape differs by host:
//!
//! - **Windows**: distributed as `opencode.cmd` (resolved via `PATHEXT`);
//!   `CreateProcess` won't run a `.cmd` directly, so the recipe wraps with
//!   `WindowsShell::Cmd` → `cmd.exe /c opencode …` (see
//!   `spawn_environment::wrap`'s `WindowsShell::Cmd` branch).
//! - **macOS / Linux**: a real executable on PATH; no wrapper needed, so
//!   `WindowsShell::Direct` is the right choice (on those platforms
//!   `spawn_environment::wrap` short-circuits to a direct spawn anyway).
//!
//! Prior to issue #827 the adapter hardcoded `WindowsShell::Cmd` for *all*
//! platforms — fine on Windows, but on Linux `cmd.exe /c opencode` doesn't
//! exist, so the provider was advertised as available yet silently failed
//! at spawn time. The adapter also omitted `Platform::Macos` from
//! `available_on()`, so the spawn menu on macOS never showed OpenCode even
//! when the binary was installed — users on macOS were told nothing.
//!
//! **Session identity** (2026-08 research, `docs/learning/opencode-harness-capabilities.md`):
//! OpenCode *self-assigns* IDs of the form `ses_<hex+base62>`. There is no
//! Claude-style `--session-id` assign flag; `opencode --session <uuid>` is
//! rejected as an invalid ID, and `--session ses_unknown` fails with
//! "Session not found" rather than creating that ID. Resume uses
//! `--session <id>` / `-s <id>` (not `--resume`).
//!
//! Capture (issue #1294) is **two-layered**:
//! 1. **Primary** — the OpenCode project plugin installed by
//!    [`Self::provision_attention_hooks`] forwards the `session.created`
//!    event back to Buildmesh's `/api/attention/<node_id>` endpoint
//!    (loopback peer only). The attention route classifies it as
//!    `Ignore` (lifecycle-neutral) and stores the freshly minted `ses_<…>`
//!    id via `set_cli_session_id_if_missing`. This path is unambiguous
//!    for two Root Nodes in the same mesh root — the plugin runs
//!    in-process with the TUI that just started, so it knows which node
//!    it belongs to (the SQLite poller below only sees `directory`, which
//!    is shared).
//! 2. **Fallback** — a post-spawn SQLite read of OpenCode's local
//!    `opencode.db` (`services::opencode_session::start_capture_poller`),
//!    started from [`Self::after_fresh_spawn`]. Used when the plugin is
//!    missing (e.g. `opencode` not on PATH for the user's shell) or
//!    blocked by a future plugin loader regression. Has a 9.3s retry
//!    window that gives up once exhausted — the production repro from
//!    issue #1294's investigation comment shows why this isn't enough on
//!    its own: node `3417` was spawned at `20:10:04` and the poller
//!    gave up at `20:10:13`, but OpenCode only minted the row at
//!    `21:06:29` (an hour later, on first interactive prompt). The
//!    plugin closes that gap because `session.created` fires when
//!    OpenCode itself creates the session — no matter how long the
//!    user waits before typing.
//!
//! The PTY UUID regex (`session_capture`) is *never* used for OpenCode —
//! `ses_<…>` IDs never match the UUID shape.
//!
//! **Model / prefill**: TUI accepts `--model provider/model` and `--prompt`.
//! There is no TUI `--variant` / `--effort` flag (that's `opencode run` only).
//! Windows still wraps with `cmd.exe /c` (`.cmd` shim); `--prompt` therefore
//! flattens CR/LF to spaces so a multi-line handover cannot split the
//! `cmd.exe` command line.
//!
//! **Permission policy** (issue #2151): the unattended mode contributes
//! `--auto` via `permission_args`. Mirrors how AGY's
//! `--dangerously-skip-permissions` is wired — a session-wide policy the
//! harness applies for the whole invocation, not a per-call toggle. The
//! bare `spawn_recipe` carries no approval flags; the orchestrator's outer
//! sandbox (macOS Seatbelt, Windows restricted-token, mesh-level toggle)
//! is the independent OS-level containment layer.

use std::path::Path;

use crate::agent::capabilities::{PermissionModeOption, PERMISSION_MODE_UNATTENDED};
use crate::agent::managed_config;
use crate::agent::provider::{
    shell_for, AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta,
};
// The recipe tests assert the shell each platform produces; nothing outside
// `cfg(test)` names the type now that `shell_for` is shared.
#[cfg(test)]
use crate::agent::provider::WindowsShell;
use crate::env::ResolvedPath;
use crate::models::EnvType;

pub struct OpenCodeAdapter;
pub static OPENCODE: OpenCodeAdapter = OpenCodeAdapter;

/// The OpenCode attention plugin template, embedded at compile time via
/// `include_str!`. Placed alongside the adapter (rather than in
/// `agent::spawn::process`) because the file is harness-specific
/// scaffolding: every other adapter owns its own hook JSON / config
/// (`agy::ensure_hooks_json`, `codex::ensure_codex_project_files`,
/// `grok::ensure_hooks_json`), and OpenCode's plugin file is the same
/// shape — see `mod.rs:367-374` ("each adapter owns its harness's config
/// format"). The orphan `process::inject_attention_hook` predates the
/// modularisation and is the only legacy exception.
const OPENCODE_ATTENTION_PLUGIN: &str = include_str!("opencode_attention_plugin.js");

/// Install (or refresh) the OpenCode attention plugin. Idempotent — a
/// re-run with the same template is a no-op, so the orchestrator's
/// before-spawn call has zero cost once the file is in place. The
/// plugin file is fully owned by Buildmesh (the user has no reason to
/// hand-edit it; the contents are a pure template). A re-run with a
/// user-modified file overwrites back to the template — same shape as
/// `agent::spawn::inject_attention_hook`'s ownership of
/// `.claude/settings.local.json`.
///
/// The write is `managed_config::atomic_write` (temp file + fsync +
/// rename): the OpenCode ESM loader, a file watcher, or anything else
/// with the directory open sees either the old content or the new
/// content, never a half-written file.
///
/// Returns `Ok(())` even when the disk write is a no-op; an `Err` only
/// surfaces a real filesystem failure (missing directory perms, full
/// disk, etc.). The spawn caller treats this as best-effort: a failure
/// is logged, the spawn proceeds, and the attention callback is the
/// only casualty.
fn inject_opencode_attention_plugin(project_path: &Path) -> Result<(), String> {
    let plugins_dir = project_path.join(".opencode").join("plugins");
    std::fs::create_dir_all(&plugins_dir)
        .map_err(|e| format!("failed to create .opencode/plugins dir: {e}"))?;

    let plugin_path = plugins_dir.join("buildmesh-attention.js");
    match std::fs::read(&plugin_path) {
        Ok(existing) if existing.as_slice() == OPENCODE_ATTENTION_PLUGIN.as_bytes() => {
            // Already up to date — keep the existing mtime so repeated
            // spawns don't churn the project working tree.
            return Ok(());
        }
        _ => {}
    }

    managed_config::atomic_write(&plugin_path, OPENCODE_ATTENTION_PLUGIN)
        .map_err(|e| format!("failed to write OpenCode attention plugin: {e}"))?;
    tracing::info!(
        "inject_opencode_attention_plugin: wrote plugin at {:?}",
        plugin_path
    );
    Ok(())
}

/// Parse the plugin's native-subagent events (issue #2636) into owned-work
/// receipts. The plugin synthesizes `subagent_start` / `subagent_stop` from a
/// child session's `session.created` (which carries `info.parentID`), its
/// `session.idle` and its `session.deleted`; OpenCode has no such event of its
/// own. `sessionID` is the *parent* (root) session and `agent_id` the child's,
/// both OpenCode `ses_` ids, so the receipt is fenced to the node's own session
/// and the child identity is stable across both events.
///
/// This parser reports child work only. It never yields a foreground turn
/// (`Stop`, `UserPromptSubmit`), a turn token or a registry snapshot, which is
/// why the strategy declares `foreground: false`. Event names are read the way
/// the attention route reads them (lowercased, `_` dropped) so the route can
/// never track a child for a spelling this parser would refuse.
fn parse_circuit_hook(value: &serde_json::Value) -> Option<crate::circuit::strategy::NativeHook> {
    use crate::circuit::strategy::NativeHook;
    use crate::http::request::parse_opencode_session_id;
    let named = ["hook_event_name", "hookEventName", "hookName"]
        .iter()
        .find_map(|key| value.get(*key)?.as_str())?;
    let event = match named.to_ascii_lowercase().replace('_', "").as_str() {
        "subagentstart" => "SubagentStart",
        "subagentstop" => "SubagentStop",
        _ => return None,
    };
    let id = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key)?.as_str())
            .and_then(parse_opencode_session_id)
    };
    let parent = id(&["sessionID", "sessionId", "session_id"])?;
    let child = id(&["agent_id"])?;
    // A session cannot own itself; that would let the root's own events
    // masquerade as owned work.
    if parent == child {
        return None;
    }
    Some(NativeHook {
        session_id: Some(parent),
        turn_id: None,
        event: event.into(),
        child_id: Some(child),
        active_work: None,
        final_report: None,
        background_busy: false,
        execution_num: None,
        termination_reason: None,
        human_fact: None,
        provider: Some("opencode".into()),
        prompt_digest: None,
    })
}

impl AgentProvider for OpenCodeAdapter {
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                attention_capability: {
                    use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
                    use crate::agent::session_lifecycle::LifecycleKind;
                    // Issue #1295: OpenCode's project plugin forwards two lifecycle
                    // events back to the attention endpoint:
                    //   - `session.idle` → `TurnCompleted` (ready for another prompt)
                    //   - `question.asked` → `QuestionRequested` (an answer is needed)
                    //   - `permission.asked` → `PermissionRequested` (tool approval)
                    // `PermissionAsk` is the honest launch mode: OpenCode's `--auto`
                    // auto-approves most permissions, but the rare non-auto case still
                    // raises a `permission.asked` signal that the plugin forwards.
                    AttentionCapability::Hook {
                        events: vec![
                            LifecycleKind::TurnCompleted,
                            LifecycleKind::QuestionRequested,
                            LifecycleKind::PermissionRequested,
                        ],
                        launch_mode: AttentionLaunchMode::PermissionAsk,
                        trust: None,
                        min_version: None,
                    }
                },
                circuit_observation: {
                    use crate::circuit::strategy::{
                        HookPush, ObservationStrategy, OwnedWorkCoverage, PushSource, StrategyNotes,
                    };
                    ObservationStrategy {
            push: PushSource::Hooks(HookPush {
                parse: parse_circuit_hook,
                source: "opencode_native_hook",
                request_source: "opencode_request_hook",
                // Child events name owned work, so a turn without a registry
                // is unknown rather than a validated gap.
                ownership_gap: None,
                foreground: false,
            }),
            owned_work: OwnedWorkCoverage::HookRegistry,
            notes: StrategyNotes {
                foreground: "OpenCode 1.18.3 (Windows .cmd via cmd.exe /c; Linux/macOS direct) has no validated Circuit lifecycle adapter; the session.idle plugin hook carries no turn/input fence and cannot establish foreground termination",
                owned_work: "OpenCode 1.18.3 native subagents (the task tool) run as child sessions, and the plugin reports each child session's start and end with its session id under the parent; with no turn token these receipts are reduced confidence and never settle a step. There is no registry snapshot, so a settled turn leaves other ownership unknown. Subagents that run in the background (the v2 subagent tool) and resumed child sessions are not validated",
                final_report: "Transcript (opencode.db SQLite) or PTY text may inform interpretation; complete native report unavailable",
                ..ObservationStrategy::UNWIRED.notes
            },
            ..ObservationStrategy::UNWIRED
        }
                },
                produces_readable_transcript: true,
                supports_model_override: true,
                supports_extra_args: true,
                supports_prefill: true,
                available_on: { &[Platform::Windows, Platform::Linux, Platform::Macos] },
                permission_modes: {
                    vec![
                        PermissionModeOption::unattended(
                            "--auto",
                            "Auto-approve (today's behavior; required for unattended runs).",
                        ),
                        PermissionModeOption::prompt(
                            "Prompts on (no flag)",
                            "OpenCode asks for approval like a human-launched session.",
                        ),
                    ]
                },
                ..Default::default()
            });
        &DESCRIPTOR
    }

    fn prepare_session(
        &self,
        input: crate::agent::provider::SessionPreparation<'_>,
    ) -> Result<Option<String>, String> {
        if input.resolved.env_type == EnvType::Windows {
            let fallback = crate::agent::detection::resolve_spawn_binary("opencode");
            let executable = input
                .executable
                .or(fallback.as_deref())
                .ok_or("OpenCode 1 executable is missing")?;
            self.validate_executable(executable)?;
        }
        Ok(None)
    }
    fn validate_executable(&self, executable: &Path) -> Result<(), String> {
        super::opencode_version::require_major(executable, 1)
    }

    fn id(&self) -> &'static str {
        "opencode"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "OpenCode".into(),
            color: "#f59e0b".into(),
            icon: "O".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        // Issue #2151: bare — no approval flags. The effective permission
        // mode contributes `--auto` via `permission_args` in
        // `default_prepare`.
        SpawnRecipe {
            binary: "opencode",
            base_args: Vec::new(),
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == PERMISSION_MODE_UNATTENDED {
            vec!["--auto".into()]
        } else {
            Vec::new()
        }
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
        spawn.base_args = ["run", "--format", "json"].map(str::to_owned).to_vec();
        let mut recipe = BackgroundRecipe::new(
            spawn,
            BackgroundPromptInput::Stdin,
            BackgroundResultOutput::OpenCodeJsonLines,
        );
        recipe.env.push((
            "OPENCODE_CONFIG_CONTENT".into(),
            r#"{"permission":{"*":"deny"},"share":"disabled"}"#.into(),
        ));
        Some(recipe)
    }

    /// Install the OpenCode attention plugin (issue #1295). The plugin is
    /// loaded from `.opencode/plugins/` and forwards `session.idle` and
    /// `permission.asked` to the local attention endpoint via env vars set
    /// per-agent by `spawn_environment` (no node_id baking needed — unlike
    /// Codex, the OpenCode TUI does NOT env_clear `BUILDMESH_*`).
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        inject_opencode_attention_plugin(std::path::Path::new(&resolved.host_path))
    }

    /// The attention plugin forwards `session.busy` when a prompt starts work.
    fn confirms_prompt_submission(&self) -> bool {
        true
    }

    /// Issue #2344: the TUI's renderer attaches its stdin listener during boot
    /// (`setupInput` → `setRawMode` → `on("data")` → `resume()`, and the
    /// resume path drains whatever stdin already holds). Keystrokes Buildmesh
    /// writes before that point are consumed without reaching the composer, so
    /// the first prompt typed into a fresh node vanishes: no `session.busy`, an
    /// empty input box, and no turn. OpenCode discards early input rather than
    /// queueing it, which is what makes a re-send safe; a harness that queued
    /// it would run the user's prompt twice.
    fn replays_unconfirmed_submit(&self) -> bool {
        true
    }

    /// OpenCode mints `ses_…` IDs itself. There is no `--session-id` assign
    /// flag; a fresh spawn must capture the ID after the TUI starts.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    /// The PTY UUID regex (`session id: 01a0-…`) cannot see `ses_…` IDs, and
    /// the TUI is not documented to print them. Capture runs from
    /// [`Self::after_fresh_spawn`].
    fn captures_session_id_from_pty(&self) -> bool {
        false
    }

    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        env_type: EnvType,
        _app: &tauri::AppHandle,
    ) {
        crate::services::opencode_session::start_capture_poller(
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
        crate::services::opencode_session::find_historic_id_for_directory(
            env_type,
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
    }

    /// Regenerate-on-the-same-harness branches instead of continuing in
    /// place (issue #1550). `--fork` immediately after `--session <id>`:
    /// the new session is a sibling that leaves the parent transcript
    /// untouched, so regenerating on a provider swap does not overwrite the
    /// history it branched from. Verified against OpenCode 1.18.3.
    fn resume_args_fork(&self, id: &str) -> Vec<String> {
        let mut args = self.resume_args(id);
        args.push("--fork".into());
        args
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // `cmd.exe /c` treats a newline as end-of-command. Flatten so a
        // multi-line handover/issue prefill cannot split the argv.
        vec!["--prompt".into(), flatten_cmd_prefill(text)]
    }
}

/// Collapse CR/LF so `--prompt` stays a single `cmd.exe /c` argument.
pub(super) fn flatten_cmd_prefill(text: &str) -> String {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "opencode_contract_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod child_session_tests;
