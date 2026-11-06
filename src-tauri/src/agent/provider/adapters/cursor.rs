//! Cursor CLI provider adapter — Cursor's interactive coding agent installed
//! on PATH as `cursor-agent`.
//!
//! Cursor's default interactive mode runs inside Buildmesh's PTY. The same
//! recipe works across Windows ConPTY and native macOS/Linux PTYs; Windows
//! uses `cmd.exe` because Cursor's installer exposes a `.cmd` shim.
//!
//! Cursor assigns session UUIDs itself and persists workspace-scoped JSONL
//! transcripts. Fresh spawns therefore omit a session-id argument, while
//! archived sessions resume with `--resume <id>`.
//!
//! **Attention hooks** (issue #1368). Cursor's `--force` flag ships the
//! adapter in `SkipPermissions` mode — every tool call auto-runs unless
//! explicitly denied by a hook matcher — so the only signal Cursor emits
//! at a useful cadence is `Stop` (a turn finished, the agent is at its
//! prompt). Buildmesh provisions `<project>/.cursor/hooks.json` with a
//! single `stop` handler that forwards the hook stdin JSON (Cursor's
//! `conversation_id` + `hook_event_name: "stop"` envelope) to the local
//! attention endpoint. Cursor documents `conversation_id` (snake_case) as
//! the canonical session id field, distinct from Claude's `session_id`
//! and AGY's `conversationId` (camelCase). The route parser accepts all
//! three casings via stacked `#[serde(alias)]`.
//!
//! **Cursor's hooks.json schema** (round-2 review, issue #1368): the
//! documented shape is
//! `{ "version": 1, "hooks": { "<event>": [{ "command": "..." }, …] } }`,
//! not AGY's per-namespace convention. Top-level must be an object, must
//! carry `"version": 1`, the `hooks` object is keyed by lowercase event
//! names (`stop`, `notification`, `pretooluse`, …), and each event's
//! handler array stores entries as `{ "command": "..." }` with no `type`
//! field. The merge below walks `settings["hooks"]["stop"]` as an array
//! and detects a stale Buildmesh entry by URL substring so re-provisioning
//! is idempotent (issue #886).

use crate::agent::attention_hooks::{
    provision_attention_hooks, EventMerge, EventRule, HandlerShape, HookFormat,
    HookProvisioningSpec, JsonHookShape, Ownership, RequiredRoot,
};
use crate::agent::capabilities::{
    AttentionCapability, AttentionLaunchMode, PermissionModeOption, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::provider::{
    shell_for, AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta,
};
// The recipe tests assert the shell each platform produces; nothing outside
// `cfg(test)` names the type now that `shell_for` is shared.
#[cfg(test)]
use crate::agent::provider::WindowsShell;
use crate::agent::session_lifecycle::LifecycleKind;
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::path::Path;

pub struct CursorAdapter;
pub static CURSOR: CursorAdapter = CursorAdapter;

/// Minimum Cursor Agent release the Buildmesh hook integration has been
/// validated against (issue #1368). Cursor's `--force` flag and the
/// `<project>/.cursor/hooks.json` shape were available in the release
/// that introduced the CLI hook runner; the constant pins the descriptor
/// to that release so a future Cursor that drops `--force` or moves the
/// hook path surfaces a visible capability/health change. The pin is
/// descriptor-shape only — same `_MIN_` semantics as the Grok
/// (`GROK_MIN_HOOK_VERSION`) precedent (issue #1366): we do **not**
/// enforce a runtime version gate because Cursor doesn't expose
/// semver through the hook surface.
pub const CURSOR_MIN_HOOK_VERSION: &str = "1.0.0";

/// Marker substring written into the Buildmesh-owned `stop` handler.
/// Used by `ensure_hooks_json` to detect a stale Buildmesh entry on
/// re-provision (issue #886 idempotency invariant): a re-run that finds
/// an existing handler with this substring leaves it alone, a re-run
/// that finds a different Buildmesh entry updates it in place, and a
/// re-run that finds no Buildmesh entry appends a fresh one.
const BUILDMESH_HOOK_MARKER: &str = "/api/attention/";

/// Every anchor that must appear in a `stop` handler for it to be ours.
/// `BUILDMESH_HOOK_MARKER` plus the two per-agent env var names are the
/// triple `window_attention_command` writes into every handler this adapter
/// installs; a user handler that merely posts somewhere is not ours to
/// replace. These markers identify already-provisioned configs, so they must
/// not drift.
const BUILDMESH_HOOK_ANCHORS: &[&str] = &[
    BUILDMESH_HOOK_MARKER,
    "BUILDMESH_PORT",
    "BUILDMESH_SESSION_ID",
];

/// The callback command Cursor's hook runner invokes on `stop`. The runner
/// forwards stdin JSON (Cursor's `conversation_id` +
/// `hook_event_name: "stop"` envelope) and we POST it verbatim to the
/// attention endpoint via curl. `--data-binary @-` forwards the hook
/// stdin as the POST body so the attention route's classifier can read
/// `conversation_id` and `hook_event_name`. Cursor's hook runner
/// inherits the agent process's environment (no `env_clear` like
/// Codex's), so `$BUILDMESH_PORT` / `$BUILDMESH_SESSION_ID` set
/// per-agent by `spawn_environment::wrap` expand at run time.
///
/// The trailing `|| exit 0` (cmd) / `|| true` (POSIX) ensures a curl
/// failure (Buildmesh restarting, port dropped, EHOSTUNREACH, …) never
/// surfaces as a non-zero hook exit. Cursor propagates hook exit codes
/// into its TUI as visible errors, and the attention webhook is
/// best-effort telemetry — it must never fail-block the agent. Same
/// shape as `agent::spawn::process::hook_command` (issue #878, line
/// 101: `... || true`).
///
/// Round-3 review (PR #1511, issue #1368): Windows cmd syntax is
/// ONLY correct when the *target* runtime is Windows — and
/// `EnvType::Windows` is the default for native macOS and Linux
/// projects too (`host_path.rs:295-303` defaults non-WSL native
/// paths to `EnvType::Windows`). A naive `match env_type` would
/// write `curl.exe`/`%VAR%`/`>nul` into `.cursor/hooks.json` on
/// macOS and Linux, where `curl.exe` doesn't exist, POSIX shells
/// don't expand `%VAR%`, and `>nul` creates literal files named
/// `nul` and `2>nul` in the user's project. Gate cmd syntax on
/// `cfg!(target_os = "windows") && env_type == EnvType::Windows`
/// (cross-compile safe — the `cfg!` is the *target* triple) so a
/// macOS or Linux build never emits cmd syntax.
fn hook_command(env_type: EnvType) -> String {
    if env_type == EnvType::WindowsInterop {
        if let Some(command) = crate::env::windows_attention_command(None) {
            return command;
        }
    }
    if cfg!(target_os = "windows") && env_type == EnvType::Windows {
        "curl.exe -sf --connect-timeout 1 --max-time 2 -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID% >nul 2>nul || exit 0"
            .to_string()
    } else {
        "curl -sf --connect-timeout 1 --max-time 2 -X POST -H 'Content-Type: application/json' --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID >/dev/null 2>/dev/null || true"
            .to_string()
    }
}

/// Cursor's documented shape — `{ "version": 1, "hooks": { "stop": […] } }`
/// — declared once as provisioning data. The shared module performs the
/// read/compare/upsert/atomic-write sequence; this adapter owns only the
/// shape, the callback, and the anchors that identify its own handler.
const CURSOR_HOOKS: HookProvisioningSpec<'static> = HookProvisioningSpec {
    harness: "cursor",
    file: "hooks.json",
    label: "cursor hooks.json",
    command: "",
    event_commands: &[],
    format: HookFormat::Json(JsonHookShape {
        root_key: Some("hooks"),
        replace_root: false,
        required_root: &[("version", RequiredRoot::Integer(1))],
        handler: HandlerShape::Command,
        ownership: Ownership {
            fields: &["command"],
            anchors: BUILDMESH_HOOK_ANCHORS,
        },
        events: &[EventRule {
            name: "stop",
            optional: false,
            merge: EventMerge::Flat,
        }],
    }),
};

/// Add or refresh the Buildmesh-owned `stop` handler in
/// `<project>/.cursor/hooks.json`. Sibling handlers (other tools, user-authored
/// automation) survive; a re-provision over the expected handler does not
/// rewrite the file (issue #886 idempotency invariant).
fn ensure_hooks_json(path: &Path, command: &str) -> Result<(), String> {
    let target_dir = path
        .parent()
        .ok_or_else(|| "cursor hooks.json has no parent directory".to_string())?;
    let spec = HookProvisioningSpec {
        command,
        ..CURSOR_HOOKS
    };
    provision_attention_hooks(&spec, target_dir)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

impl AgentProvider for CursorAdapter {
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                attention_capability: {
                    // Issue #1368 + #1364 §3 — Cursor's `--force` flag launches
                    // with permission prompts suppressed (every tool call
                    // auto-runs unless a hook matcher denies it). The only signal
                    // Cursor emits at a useful cadence is `stop` (turn finished,
                    // agent at its prompt). Background-running distinguishes a
                    // false-yield stop from a clean completion the same way AGY's
                    // `fullyIdle: false` does — Cursor's documented `transcript_path`
                    // carries the JSONL the route can scan for pending tasks.
                    //
                    // Round-2 review: `trust` is intentionally `None` here.
                    // Cursor under `--force` does not require an explicit workspace
                    // trust entry; the project's `.cursor/hooks.json` is loaded
                    // unconditionally because `--force` itself is the trust
                    // grant. Advertising `"workspace trust"` would lie about a
                    // step we never take and trip a future caller that assumes
                    // the descriptor's promise (issue #1368 review point 2).
                    AttentionCapability::Hook {
                        events: vec![
                            LifecycleKind::TurnCompleted,
                            LifecycleKind::BackgroundRunning,
                        ],
                        launch_mode: AttentionLaunchMode::SkipPermissions,
                        trust: None,
                        // Issue #1368: pin the validated Cursor release (1.0.0) so
                        // a refactor that flips `min_version` to None trips this
                        // pin. Mirrors the Grok (`GROK_MIN_HOOK_VERSION`, issue
                        // #1366) and AGY (`"1.0.0"`) precedents.
                        min_version: Some(CURSOR_MIN_HOOK_VERSION.into()),
                    }
                },
                produces_readable_transcript: {
                    // Cursor writes Anthropic-shaped JSONL under its workspace-scoped
                    // ~/.cursor/projects/agent-transcripts layout.
                    true
                },
                supports_model_override: true,
                supports_extra_args: true,
                supports_prefill: true,
                available_on: { &[Platform::Windows, Platform::Linux, Platform::Macos] },
                permission_modes: {
                    vec![
            PermissionModeOption::unattended(
                "--force",
                "Commands allowed unless denied (today's behavior; required for unattended runs).",
            ),
            PermissionModeOption::prompt(
                "Prompts on (no flag)",
                "Cursor asks for approval like a human-launched session.",
            ),
        ]
                },
                ..Default::default()
            });
        &DESCRIPTOR
    }

    fn id(&self) -> &'static str {
        "cursor"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Cursor Agent".into(),
            // Cursor's current brand palette uses a warm near-black surface;
            // the frontend renders the official cube mark in its light
            // variant through the brand registry.
            color: "#1B1913".into(),
            icon: "C".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        // Issue #2151: bare — no approval flags. The effective permission
        // mode contributes `--force` via `permission_args` in
        // `default_prepare`. Cursor documents `--force` as allowing
        // commands unless denied; `--trust` is a print-mode option, not
        // an interactive-TUI flag.
        SpawnRecipe {
            binary: "cursor-agent",
            base_args: Vec::new(),
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == PERMISSION_MODE_UNATTENDED {
            vec!["--force".into()]
        } else {
            Vec::new()
        }
    }

    /// Provision Cursor's project-local attention hook into
    /// `<project>/.cursor/hooks.json` (issue #1368). The file lives in
    /// the worktree because Cursor loads project-local hooks directly
    /// under `--force`. The hook URL template expands
    /// `$BUILDMESH_PORT` and `$BUILDMESH_SESSION_ID` at runner time
    /// (set per-agent by `spawn_environment::wrap`) and forwards the
    /// hook stdin JSON verbatim to the attention route, which reads
    /// Cursor's documented `conversation_id` + `hook_event_name: "stop"`
    /// envelope. Idempotent and atomic (PID+counter `.tmp` + rename,
    /// AGY precedent `agy.rs:17-40`).
    ///
    /// The hook command's shell syntax is derived from
    /// `resolved.env_type`, not from the host `Platform::current()`:
    /// a WSL-guest Cursor invocation reads `%VAR%` as literal text and
    /// `>nul` as a literal redirect target, so a host-side Windows
    /// host_path with a Linux Cursor under it would write an unrunnable
    /// command. `%VAR%` for `EnvType::Windows` (cmd.exe), `$VAR` for
    /// `EnvType::Wsl` (bash).
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        let cursor_dir = Path::new(&resolved.host_path).join(".cursor");
        std::fs::create_dir_all(&cursor_dir)
            .map_err(|e| format!("failed to create .cursor dir: {e}"))?;
        let hooks_path = cursor_dir.join("hooks.json");
        ensure_hooks_json(&hooks_path, &hook_command(resolved.env_type))
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--resume".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // Cursor accepts the initial prompt as a positional argument in
        // interactive mode; `--prefill` is not a Cursor CLI flag.
        vec![text.into()]
    }
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
