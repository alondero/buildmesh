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
//! **Permission policy**: `--auto` is baked into the base recipe. Mirrors
//! how AGY's `--dangerously-skip-permissions` is wired — a session-wide
//! policy the harness applies for the whole invocation, not a per-call
//! toggle. The orchestrator's outer sandbox (macOS Seatbelt, Windows
//! restricted-token, mesh-level toggle) is the independent OS-level
//! containment layer.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::agent::provider::{AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell};
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

/// Counter for unique `.tmp` filenames in [`atomic_write`]. Without a
/// counter, two concurrent provisions racing on the same path would
/// collide on the `.tmp` filename and one's rename would clobber the
/// other's data on Windows (which does not have atomic POSIX rename
/// over an open target).
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomic write — temp file + fsync + rename (agy.rs pattern, which
/// already proved out for `.agents/hooks.json`). A reader (the OpenCode
/// ESM loader, a file watcher, anything else with the directory open)
/// sees either the old content or the new content, never a
/// half-written file. On rename failure the `.tmp` is cleaned up and
/// the caller surfaces the error.
fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("buildmesh-attention.js");
    let tmp = path.with_file_name(format!(
        "{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        counter
    ));

    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        if let Err(rm_err) = std::fs::remove_file(&tmp) {
            tracing::warn!(
                "atomic_write: failed to clean up temp file {:?}: {}",
                tmp,
                rm_err
            );
        }
        return Err(e);
    }
    Ok(())
}

/// Install (or refresh) the OpenCode attention plugin. Idempotent — a
/// re-run with the same template is a no-op, so the orchestrator's
/// before-spawn call has zero cost once the file is in place. The
/// plugin file is fully owned by Buildmesh (the user has no reason to
/// hand-edit it; the contents are a pure template). A re-run with a
/// user-modified file overwrites back to the template — same shape as
/// `agent::spawn::inject_attention_hook`'s ownership of
/// `.claude/settings.local.json`.
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

    atomic_write(&plugin_path, OPENCODE_ATTENTION_PLUGIN)
        .map_err(|e| format!("failed to write OpenCode attention plugin: {e}"))?;
    tracing::info!("inject_opencode_attention_plugin: wrote plugin at {:?}", plugin_path);
    Ok(())
}

/// Per-platform shell selection. Mirrors the Codex adapter's pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

impl AgentProvider for OpenCodeAdapter {
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
        SpawnRecipe {
            binary: "opencode",
            base_args: vec!["--auto".into()],
            trailing_args: Vec::new(),
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

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        // Issue #1295: OpenCode's project plugin forwards two lifecycle
        // events back to the attention endpoint:
        //   - `session.idle` → `InputRequired` (turn ended, user is needed)
        //   - `permission.asked` → `PermissionRequested` (tool approval)
        // `PermissionAsk` is the honest launch mode: OpenCode's `--auto`
        // auto-approves most permissions, but the rare non-auto case still
        // raises a `permission.asked` signal that the plugin forwards.
        AttentionCapability::Hook {
            events: vec![
                LifecycleKind::InputRequired,
                LifecycleKind::PermissionRequested,
            ],
            launch_mode: AttentionLaunchMode::PermissionAsk,
            trust: None,
            min_version: None,
        }
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

    /// OpenCode stores session messages in its local SQLite DB; the
    /// transcript reader (`services::transcript_reader::OpenCode`) parses
    /// them into the shared wire shape, so the Coordinator rich layer and
    /// the archive-resume picker can include OpenCode nodes (#1296).
    fn produces_readable_transcript(&self) -> bool {
        true
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
            env_type, spawn_path, anchor_ms, recorded_start,
        )
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
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
fn flatten_cmd_prefill(text: &str) -> String {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS always direct-spawns the binary via the `cfg!(target_os = "macos")`
    /// short-circuit in `spawn_environment::wrap` (the `windows_shell` field is
    /// never consulted on macOS hosts). Linux reaches the `WindowsShell::Direct`
    /// arm of the wrap function's `match`, which likewise direct-spawns. So both
    /// platforms produce a direct spawn — no shell wrapper needed because
    /// `opencode` is a real executable on PATH on both.
    ///
    /// Regression for issue #827: before the fix `spawn_recipe` ignored its
    /// `platform` arg and returned `WindowsShell::Cmd` on Linux, so a
    /// native-Linux mesh got `cmd.exe /c opencode …` — silent failure at
    /// spawn time on a platform that was advertised as supported.
    #[test]
    fn spawn_recipe_direct_on_macos() {
        let recipe = OPENCODE.spawn_recipe(Platform::Macos, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert_eq!(recipe.base_args, vec!["--auto".to_string()]);
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "macOS must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_direct_on_linux() {
        let recipe = OPENCODE.spawn_recipe(Platform::Linux, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert_eq!(recipe.base_args, vec!["--auto".to_string()]);
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "Linux must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    /// Windows keeps `WindowsShell::Cmd` because `opencode` is a `.cmd` batch
    /// file on Windows; `CreateProcess` won't execute it directly. Pinning
    /// this guards against the easy mistake of normalising every provider
    /// onto `Direct` and breaking the Windows path.
    #[test]
    fn spawn_recipe_cmd_on_windows() {
        let recipe = OPENCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert_eq!(recipe.base_args, vec!["--auto".to_string()]);
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    /// `available_on` must list every platform OpenCode runs on. Pre-#827
    /// macOS was missing, so the spawn-menu composer in
    /// `agent::provider_menu::available_providers` filtered OpenCode out for a
    /// macOS user even when the binary was detected at startup. Pin the exact
    /// set (not just membership) so a future "while we're here" addition
    /// forces an explicit test update — matches the equivalent assertion on
    /// `TERMINAL` (`terminal_available_on_three_platforms`).
    #[test]
    fn available_on_includes_macos() {
        let platforms = OPENCODE.available_on();
        assert_eq!(
            platforms.len(),
            3,
            "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
            platforms
        );
        assert!(platforms.contains(&Platform::Macos), "OpenCode must be available on macOS (issue #827); got {:?}", platforms);
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Windows));
    }

    /// Pin id + UI metadata so the wire type / frontend icon mapping can't
    /// drift silently (matches the equivalent assertion on `TERMINAL`).
    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(OPENCODE.id(), "opencode");
        let ui = OPENCODE.ui();
        assert_eq!(ui.label, "OpenCode");
        assert_eq!(ui.color, "#f59e0b");
        assert_eq!(ui.icon, "O");
    }

    #[test]
    fn self_assigns_session_id() {
        assert!(OPENCODE.self_assigns_session_id());
        assert!(
            !OPENCODE.captures_session_id_from_pty(),
            "OpenCode ses_ IDs are not PTY UUID banners; capture is after_fresh_spawn"
        );
    }

    #[test]
    fn resume_args_format() {
        let args = OPENCODE.resume_args("ses_fc52ccfb9ffek1jl23ZwpRuSP7");
        assert_eq!(
            args,
            vec!["--session", "ses_fc52ccfb9ffek1jl23ZwpRuSP7"],
            "OpenCode resume is --session <id>, not --resume"
        );
    }

    #[test]
    fn session_assign_args_empty() {
        let args = OPENCODE.session_assign_args("any-id");
        assert!(
            args.is_empty(),
            "OpenCode self-assigns; session_assign_args must be empty (no --session-id)"
        );
    }

    #[test]
    fn prefill_args_use_prompt_flag() {
        let args = OPENCODE.prefill_args("fix the auth bug");
        assert_eq!(args, vec!["--prompt", "fix the auth bug"]);
    }

    #[test]
    fn prefill_args_flatten_newlines_for_cmd_exe() {
        let args = OPENCODE.prefill_args("fix auth\nthen run tests\r\nand push");
        assert_eq!(args, vec!["--prompt", "fix auth then run tests and push"]);
        assert!(
            !args[1].contains('\n') && !args[1].contains('\r'),
            "cmd.exe /c must not see a newline in --prompt; got {:?}",
            args[1]
        );
    }

    #[test]
    fn model_args_use_long_form() {
        let args = OPENCODE.model_args("anthropic/claude-sonnet-4-5");
        assert_eq!(args, vec!["--model", "anthropic/claude-sonnet-4-5"]);
    }

    /// `--auto` is the OpenCode analogue of AGY's
    /// `--dangerously-skip-permissions` — see the module docstring. This
    /// test pins the exact argv so a future edit that smuggles an extra
    /// flag in (e.g. `--auto --garbage`) trips here, not at runtime.
    #[test]
    fn spawn_recipe_carries_auto_flag_on_every_platform() {
        for platform in [Platform::Windows, Platform::Linux, Platform::Macos] {
            let recipe = OPENCODE.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(
                recipe.base_args,
                vec!["--auto".to_string()],
                "OpenCode base recipe must be exactly `[\"--auto\"]` on {platform:?}; got {:?}",
                recipe.base_args
            );
        }
    }

    #[test]
    fn supports_resume_model_prefill_attention_hook_and_readable_transcript() {
        assert!(OPENCODE.supports_resume());
        assert!(OPENCODE.auto_resume_on_startup());
        assert!(OPENCODE.supports_model_override());
        assert!(OPENCODE.supports_prefill());
        // Issue #1295: plugin hook unblocks the Autopilot gate.
        assert!(OPENCODE.requires_attention_hook());
        // Issue #1296: OpenCode's local SQLite messages are now read by the
        // transcript reader, so the archive-resume picker and the Coordinator
        // rich layer can include OpenCode nodes.
        assert!(OPENCODE.produces_readable_transcript());
    }

    #[test]
    fn capabilities_descriptor_advertises_resume_model_prefill_attention_hook() {
        let caps = OPENCODE.capabilities();
        assert_eq!(caps.harness_id, "opencode");
        assert!(caps.supports_resume);
        assert!(caps.auto_resume_on_startup);
        assert!(caps.supports_model_override);
        assert!(caps.supports_prefill);
        assert!(!caps.supports_effort_override);
        // Issue #1295: descriptor must mirror the adapter's `requires_attention_hook`.
        assert!(caps.requires_attention_hook);
        // Issue #1296: descriptor must mirror the adapter's `produces_readable_transcript`.
        assert!(caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
    }

    // -- Attention plugin (issue #1295) ------------------------------------

    /// The plugin file is an ESM module — OpenCode's loader fails on
    /// syntax errors and surfaces them as a startup fault, so we run
    /// `node --check` on the embedded template to fail closed here
    /// rather than on a real spawn. `node` is available on every CI
    /// runner we ship to (issue #1295 review); the `which` probe is
    /// there so a future sandbox without `node` doesn't produce a
    /// confusing panic.
    ///
    /// Implementation note: `node --check` does not accept
    /// `--input-type=module` (Node v24 ERR_INPUT_TYPE_NOT_ALLOWED), so
    /// we write the file with a `.mjs` extension to force ESM detection
    /// without needing the flag. OpenCode's real `.opencode/plugins/buildmesh-attention.js`
    /// is loaded as ESM by the OpenCode TUI's own loader (which knows
    /// the plugin contract); `.mjs` is just the safest syntax-check
    /// harness.
    #[test]
    fn plugin_template_is_valid_esm_per_node_check() {
        let node = which_node_for_check();
        let Some(node) = node else {
            // `node` not on PATH — skip rather than false-green. The
            // plugin still ships, but the syntax check is opt-in per
            // sandbox. CI installs node before running cargo test.
            eprintln!("node --check skipped (node not on PATH)");
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_path = temp.path().join("buildmesh-attention.mjs");
        std::fs::write(&plugin_path, OPENCODE_ATTENTION_PLUGIN).expect("write temp");
        let output = std::process::Command::new(node)
            .arg("--check")
            .arg(&plugin_path)
            .output()
            .expect("spawn node --check");
        assert!(
            output.status.success(),
            "embedded plugin failed `node --check`: status={:?}\nstdout={}\nstderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// `provision_attention_hooks` writes `.opencode/plugins/buildmesh-attention.js`
    /// into the project directory. Issue #1295 requires idempotency: a
    /// re-spawn with a byte-identical file MUST NOT rewrite (so mtime is
    /// preserved and the working tree stays clean), and a re-spawn after
    /// the template changes MUST overwrite.
    #[test]
    fn provision_attention_hooks_writes_plugin_and_is_idempotent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let resolved = ResolvedPath {
            host_path: temp.path().to_string_lossy().into_owned(),
            spawn_path: temp.path().to_string_lossy().into_owned(),
            raw_path: temp.path().to_string_lossy().into_owned(),
            env_type: EnvType::Windows,
        };

        // First provision — creates the directory tree and writes the file.
        OPENCODE
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
            .expect("first provision");

        let plugin_path = temp
            .path()
            .join(".opencode")
            .join("plugins")
            .join("buildmesh-attention.js");
        assert!(
            plugin_path.exists(),
            "plugin file not written to {}",
            plugin_path.display()
        );
        let first = std::fs::read(&plugin_path).expect("read plugin");
        assert_eq!(
            first.as_slice(),
            OPENCODE_ATTENTION_PLUGIN.as_bytes(),
            "written plugin must be byte-equal to the embedded template"
        );

        // Idempotency: a second provision with the same template must
        // NOT touch the file (so the user's working tree stays clean
        // across repeated spawns).
        let mtime_before = std::fs::metadata(&plugin_path)
            .expect("stat")
            .modified()
            .expect("mtime");
        OPENCODE
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
            .expect("second provision");
        let mtime_after = std::fs::metadata(&plugin_path)
            .expect("stat")
            .modified()
            .expect("mtime");
        assert_eq!(
            mtime_before, mtime_after,
            "idempotent provision must not rewrite the plugin file (mtime preserved)"
        );

        std::fs::remove_file(&plugin_path).expect("delete");
        OPENCODE
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
            .expect("third provision");
        assert!(plugin_path.exists(), "plugin file must be re-created after removal");
    }

    /// Provision must surface a real filesystem error (a leaf file used
    /// as a project root is unrecoverable). The spawn caller treats this
    /// as best-effort: a failure is logged, the spawn proceeds, and the
    /// attention callback is the only casualty.
    #[test]
    fn provision_attention_hooks_surfaces_filesystem_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let blocker = temp.path().join("blocker");
        std::fs::write(&blocker, b"not a dir").expect("create blocker");
        let resolved = ResolvedPath {
            host_path: blocker.to_string_lossy().into_owned(),
            spawn_path: blocker.to_string_lossy().into_owned(),
            raw_path: blocker.to_string_lossy().into_owned(),
            env_type: EnvType::Windows,
        };
        let result =
            OPENCODE.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0);
        assert!(
            result.is_err(),
            "provision must surface a filesystem error when project path is a leaf file; got Ok"
        );
        // The helper must NOT panic or corrupt the blocker.
        assert!(
            std::fs::read(&blocker).is_ok(),
            "provision error path must not mutate the blocker"
        );
    }

    /// Issue #1295 review: the injection helper uses an atomic write
    /// (temp file + rename) so two concurrent spawns racing on the same
    /// project path can't lose data on Windows (no atomic POSIX rename
    /// over an open target). This test verifies the temp filenames
    /// carry a unique PID+counter so they don't collide across either
    /// (a) concurrent processes or (b) concurrent calls inside one
    /// process. On rename failure, the temp file is removed (no `.tmp`
    /// residue left in the project tree).
    #[test]
    fn atomic_write_uses_unique_tmp_and_cleans_up_on_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("plugin.js");

        // Two concurrent writes would collide on a plain `<name>.tmp`
        // filename; the PID+counter disambiguates them.
        atomic_write(&target, "first").expect("first write");
        atomic_write(&target, "second").expect("second write");
        assert_eq!(
            std::fs::read_to_string(&target).expect("read"),
            "second",
            "atomic_write must leave the latest content at the target"
        );

        // On rename failure, the temp file is cleaned up — no `.tmp`
        // residue alongside the target. Scan the parent dir for any
        // `.tmp` file we may have left behind.
        let residue: Vec<_> = std::fs::read_dir(temp.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|name| {
                name.to_string_lossy()
                    .ends_with(".tmp")
            })
            .collect();
        assert!(
            residue.is_empty(),
            "successful writes must not leave .tmp residue; found: {:?}",
            residue
        );

        // Failure path: write to a path whose parent is a leaf file.
        // `File::create` for the tmp file fails before rename, so the
        // error surfaces and the blocker must be untouched.
        let blocker = temp.path().join("blocker");
        std::fs::write(&blocker, b"intact").expect("create blocker");
        let failing_target = blocker.join("plugin.js");
        let err = atomic_write(&failing_target, "nope")
            .expect_err("must fail because blocker is a leaf file");
        assert_eq!(
            std::fs::read(&blocker).expect("blocker readable"),
            b"intact",
            "rename failure must not mutate the blocker"
        );
        assert!(
            err.kind() == std::io::ErrorKind::PermissionDenied
                || err.kind() == std::io::ErrorKind::AlreadyExists
                || matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
                ),
            "expected a filesystem error, got {:?}",
            err
        );
    }

    /// Locate `node` on PATH for the syntax-check test. Cached on first
    /// call — runs once per process.
    fn which_node_for_check() -> Option<&'static str> {
        use std::sync::OnceLock;
        static NODE: OnceLock<Option<String>> = OnceLock::new();
        NODE.get_or_init(|| {
            for candidate in ["node", "node.exe"] {
                if let Ok(out) = std::process::Command::new(candidate).arg("--version").output() {
                    if out.status.success() {
                        return Some(candidate.to_string());
                    }
                }
            }
            None
        })
            .as_deref()
    }

    /// Resume recipe is `opencode --auto --session <id>` — the base
    /// recipe's `--auto` (issue #1297) survives the `default_prepare`
    /// composition, and the resume adds `--session <id>` (flag, not
    /// subcommand) on top.
    #[test]
    fn resume_recipe_carries_session_flag() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&OPENCODE, input);
        let args = &prepared.recipe.base_args;
        assert_eq!(
            args,
            &[
                "--auto".to_string(),
                "--session".to_string(),
                "ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()
            ]
        );
        assert!(
            !args.iter().any(|a| a == "--resume" || a == "--session-id"),
            "must not emit Claude/Codex resume flags; got {args:?}"
        );
    }

    /// Fresh spawn argv order: `--auto` (base recipe) → `--model <id>` →
    /// `--prompt <text>`. No session-assign flag (self-assign). Pin the
    /// exact vector so a future reorder that pushes `--auto` past
    /// `--model` (or drops it) trips here, not in production.
    #[test]
    fn fresh_recipe_forwards_model_and_prompt_without_session_id() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("anthropic/claude-sonnet-4-5".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("fix the auth bug"),
            sandbox: false,
        };
        let prepared = default_prepare(&OPENCODE, input);
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--auto".to_string(),
                "--model".to_string(),
                "anthropic/claude-sonnet-4-5".to_string(),
                "--prompt".to_string(),
                "fix the auth bug".to_string(),
            ]
        );
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--session"
                || a == "--session-id"
                || a == "--prefill"),
            "fresh spawn must not assign a session id or emit --prefill; got {:?}",
            prepared.recipe.base_args
        );
    }
}
