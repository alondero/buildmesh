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
//! single `Stop` handler that forwards the hook stdin JSON (Cursor's
//! `conversation_id` + `hook_event_name: "stop"` envelope) to the local
//! attention endpoint. Cursor documents `conversation_id` (snake_case) as
//! the canonical session id field, distinct from Claude's `session_id`
//! and AGY's `conversationId` (camelCase). The route parser accepts all
//! three casings via stacked `#[serde(alias)]`.

use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
use crate::agent::provider::{AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::agent::session_lifecycle::LifecycleKind;
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// File-name-relative-to-`buildmesh-attention`-namespace key we own
/// under `<project>/.cursor/hooks.json`. Mirrors the AGY
/// `buildmesh-attention` namespace convention (`agy.rs:166`).
const HOOK_NAMESPACE: &str = "buildmesh-attention";

/// Counter backing the PID+counter `.tmp` suffix for atomic writes
/// (`agy.rs:14-39`). Same pattern as AGY: two concurrent writes in the
/// same process would otherwise collide on a fixed tmp name.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

/// The callback command Cursor's hook runner invokes on `Stop`. The
/// runner forwards stdin JSON (Cursor's `conversation_id` +
/// `hook_event_name: "stop"` envelope) and we POST it verbatim to the
/// attention endpoint via curl. `--data-binary @-` forwards the hook
/// stdin as the POST body so the attention route's classifier can read
/// `conversation_id` and `hook_event_name`. Cursor's hook runner
/// inherits the agent process's environment (no `env_clear` like
/// Codex's), so `$BUILDMESH_PORT` / `$BUILDMESH_SESSION_ID` set
/// per-agent by `spawn_environment::wrap` expand at run time — same
/// shape as AGY's `hook_command` (`agy.rs:50-61`). The command is
/// bare (no `cmd.exe /c` / `sh -c` wrapper) because Cursor invokes
/// hook commands through its own shell. Windows uses `%VAR%` syntax
/// (cmd); Unix uses `$VAR`.
fn hook_command(platform: Platform) -> String {
    match platform {
        Platform::Windows => {
            "curl.exe -sf --connect-timeout 1 --max-time 2 -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID% >nul 2>nul".to_string()
        }
        _ => {
            "curl -sf --connect-timeout 1 --max-time 2 -X POST -H 'Content-Type: application/json' --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID >/dev/null 2>/dev/null".to_string()
        }
    }
}

/// Atomically persist `content` to `path` via a PID+counter `.tmp`
/// file + rename. Mirrors AGY's `atomic_write` (`agy.rs:17-40`) — the
/// `NamedTempFile::new_in` + `persist` pattern from Codex/Grok is
/// equivalent but the AGY pattern is the more directly comparable
/// precedent for a project-local file (no WSL read/write split needed).
fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("hooks.json");
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
                "cursor atomic_write: failed to clean up temp file {:?}: {}",
                tmp,
                rm_err
            );
        }
        return Err(e);
    }
    Ok(())
}

/// Add or refresh the Buildmesh-owned `Stop` handler under the
/// `buildmesh-attention` namespace in `<project>/.cursor/hooks.json`.
/// Preserves user-authored sibling namespaces (other tools, custom
/// automation) round-trip through a re-injection untouched — same
/// convention as AGY (`agy.rs:70-94`).
///
/// The `Stop` event uses the simple `[{type, command}]` shape — the
/// whole turn is the event (no tool-name matcher needed) and `Stop` is
/// the only signal Cursor emits under `--force`. Returns `Ok(())`
/// when the file already matches the expected shape (no rewrite
/// fires; the idempotency invariant from issue #886).
///
/// A malformed existing file (trailing comma, partial edit, syntax
/// error) is treated as an explicit `Err` rather than silently
/// clobbered with `{}` — the Grok precedent (`grok.rs:222-243`)
/// pins the round-2 review fix: a missing file is `{}`, but a
/// present-but-unparseable file is the user's data and must surface
/// to the spawn path as a provision failure so the agent user can
/// repair it. The AGY pattern (`.ok().and_then(...).ok().unwrap_or`)
/// would silently overwrite a malformed file, which is exactly the
/// regression class the test `inject_refuses_to_overwrite_malformed_user_file`
/// guards against.
fn ensure_hooks_json(path: &Path, command: &str) -> Result<(), String> {
    let mut settings: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).map_err(|e| {
            format!(
                "refusing to overwrite malformed {path:?}: {e}. \
                 Repair or remove the file and retry"
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("failed to read {path:?}: {e}")),
    };
    if !settings.is_object() {
        // A valid JSON top-level that isn't an object (e.g. an array,
        // a string, a number, `null`) is a misconfiguration — refuse
        // to clobber the user's payload with `{}`. Mirrors the
        // `codex.rs:941` "must be an object" rejection.
        return Err(format!(
            "cursor hooks.json top-level must be a JSON object; got {}",
            settings_kind(&settings)
        ));
    }
    let stop_handler = serde_json::json!({ "type": "command", "command": command });
    let expected = serde_json::json!({ "Stop": [stop_handler] });
    if settings.get(HOOK_NAMESPACE) == Some(&expected) {
        return Ok(());
    }
    settings[HOOK_NAMESPACE] = expected;
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))?;
    atomic_write(path, &content).map_err(|e| format!("failed to write hooks.json: {e}"))?;
    tracing::info!("cursor provision_attention_hooks: wrote {:?}", path);
    Ok(())
}

/// Human-readable JSON kind tag for error messages. Mirrors
/// `grok.rs:146-155` — distinguishes `Object`, `Array`, `String`,
/// `Number`, `Boolean`, `Null` so the malformed-file rejection names
/// the actual shape rather than `serde_json::Value`.
fn settings_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Object(_) => "Object",
        serde_json::Value::Array(_) => "Array",
        serde_json::Value::String(_) => "String",
        serde_json::Value::Number(_) => "Number",
        serde_json::Value::Bool(_) => "Boolean",
        serde_json::Value::Null => "Null",
    }
}

impl AgentProvider for CursorAdapter {
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
        SpawnRecipe {
            binary: "cursor-agent",
            // Cursor documents `--force` as allowing commands unless denied.
            // `--trust` is a print-mode option, not an interactive-TUI flag.
            base_args: vec!["--force".into()],
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
        // Issue #1368 — Cursor now ships an attention hook via
        // `<project>/.cursor/hooks.json` (`Stop` event only — under
        // `--force` Cursor doesn't raise permission prompts by
        // construction). The descriptor at `attention_capability()`
        // below carries the structured contract.
        true
    }

    fn attention_capability(&self) -> AttentionCapability {
        // Issue #1368 + #1364 §3 — Cursor's `--force` flag launches
        // with permission prompts suppressed (every tool call
        // auto-runs unless a hook matcher denies it). The only signal
        // Cursor emits at a useful cadence is `Stop` (turn finished,
        // agent at its prompt). Background-running distinguishes a
        // false-yield Stop from a clean completion the same way AGY's
        // `fullyIdle: false` does — Cursor's documented `transcript_path`
        // carries the JSONL the route can scan for pending tasks.
        AttentionCapability::Hook {
            events: vec![LifecycleKind::TurnCompleted, LifecycleKind::BackgroundRunning],
            launch_mode: AttentionLaunchMode::SkipPermissions,
            trust: Some("workspace trust".into()),
            // Issue #1368: pin the validated Cursor release (1.0.0) so
            // a refactor that flips `min_version` to None trips this
            // pin. Mirrors the Grok (`GROK_MIN_HOOK_VERSION`, issue
            // #1366) and AGY (`"1.0.0"`) precedents.
            min_version: Some(CURSOR_MIN_HOOK_VERSION.into()),
        }
    }

    /// Provision Cursor's project-local attention hook into
    /// `<project>/.cursor/hooks.json` (issue #1368). The file lives in
    /// the worktree because Cursor requires workspace trust for any
    /// `.cursor/hooks.json` it loads — same trust pattern as Codex
    /// (`codex.rs:1134-1141`). The hook URL template expands
    /// `$BUILDMESH_PORT` and `$BUILDMESH_SESSION_ID` at runner time
    /// (set per-agent by `spawn_environment::wrap`) and forwards the
    /// hook stdin JSON verbatim to the attention route, which reads
    /// Cursor's documented `conversation_id` + `hook_event_name: "stop"`
    /// envelope. Idempotent and atomic (PID+counter `.tmp` + rename,
    /// AGY precedent `agy.rs:17-40`).
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
        ensure_hooks_json(&hooks_path, &hook_command(Platform::current()))
    }

    fn produces_readable_transcript(&self) -> bool {
        // Cursor writes Anthropic-shaped JSONL under its workspace-scoped
        // ~/.cursor/projects/agent-transcripts layout.
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
mod tests {
    use super::*;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(CURSOR.id(), "cursor");
        let ui = CURSOR.ui();
        assert_eq!(ui.label, "Cursor Agent");
        assert_eq!(ui.color, "#1B1913");
        assert_eq!(ui.icon, "C");
    }

    #[test]
    fn spawn_recipe_uses_cmd_only_on_windows() {
        let windows = CURSOR.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(windows.binary, "cursor-agent");
        assert_eq!(windows.base_args, vec!["--force"]);
        assert!(matches!(windows.windows_shell, WindowsShell::Cmd));

        for platform in [Platform::Linux, Platform::Macos] {
            let recipe = CURSOR.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "cursor-agent");
            assert_eq!(recipe.base_args, vec!["--force"]);
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "{platform:?} must use WindowsShell::Direct"
            );
        }
    }

    #[test]
    fn available_on_all_three_platforms() {
        let platforms = CURSOR.available_on();
        assert_eq!(platforms.len(), 3);
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    #[test]
    fn self_assigns_session_id_and_resumes_by_id() {
        assert!(CURSOR.self_assigns_session_id());
        assert!(CURSOR.session_assign_args("any-id").is_empty());
        assert_eq!(CURSOR.resume_args("abc-123"), vec!["--resume", "abc-123"]);
    }

    #[test]
    fn supports_model_override_and_positional_prefill() {
        assert!(CURSOR.supports_model_override());
        assert_eq!(
            CURSOR.model_args("claude-3-7-sonnet"),
            vec!["--model", "claude-3-7-sonnet"]
        );
        assert!(CURSOR.supports_prefill());
        assert_eq!(
            CURSOR.prefill_args("inspect the repo"),
            vec!["inspect the repo"]
        );
    }

    #[test]
    fn advertises_transcript_support_after_reader_wiring() {
        assert!(CURSOR.produces_readable_transcript());
        assert!(CURSOR.requires_attention_hook());
    }

    // -------------------------------------------------------------------
    // Attention hook injection — issue #1368.
    //
    // Cursor under `--force` behaves like AGY under
    // `--dangerously-skip-permissions`: every tool auto-runs unless a
    // hook matcher denies it, so the only signal Cursor emits at a
    // useful cadence is `Stop`. Buildmesh provisions
    // `<project>/.cursor/hooks.json` with a single `Stop` handler that
    // POSTs the hook stdin to the attention endpoint; the route's
    // classifier reads Cursor's documented `conversation_id` (snake_case)
    // + `hook_event_name: "stop"` envelope to land the node in `Ready`
    // (clean turn completion, issue #1364) or `AwaitingInput` (rare,
    // if Cursor adds permission prompts in a future release).
    // -------------------------------------------------------------------

    use crate::agent::capabilities::AttentionCapability;
    use tempfile::TempDir;

    fn read_hooks_json(project: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(project.join(".cursor").join("hooks.json"))
            .expect("hooks.json not written");
        serde_json::from_str(&content).expect("hooks.json is not valid JSON")
    }

    fn provision_cursor(project: &Path) {
        let path = project.to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: EnvType::Windows,
        };
        CURSOR
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
            .unwrap();
    }

    /// Cursor's Stop hook POSTs the hook stdin (Cursor's documented
    /// `conversation_id` + `hook_event_name: "stop"` envelope) to the
    /// attention endpoint via curl. The command is bare (no `cmd.exe /c`
    /// wrapper) because Cursor invokes hook commands through its own
    /// shell — same shape as AGY (`agy.rs:50-61`).
    #[test]
    fn inject_writes_stop_webhook() {
        let temp = TempDir::new().unwrap();
        provision_cursor(temp.path());

        let hooks = read_hooks_json(temp.path());
        let attention = hooks
            .get(HOOK_NAMESPACE)
            .expect("hooks.json must own the buildmesh-attention namespace");

        // Stop uses the simple shape — index straight into the first
        // array element. No matcher, no tool-name filter — the whole
        // turn is the event.
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
        assert!(
            stop_command.contains("BUILDMESH_PORT") && stop_command.contains("BUILDMESH_SESSION_ID"),
            "Stop must expand the per-agent env vars at runner time: {stop_command}"
        );

        // No other event handlers — Cursor under `--force` only emits Stop.
        assert!(attention.get("PermissionRequest").is_none());
        assert!(attention.get("PreToolUse").is_none());
    }

    /// Re-running injection over an already-correct project is a no-op.
    /// Re-spawns (resume / handover / re-spawn on a closed node) must
    /// not rewrite the file and risk churn on unrelated siblings the
    /// user has added. Mirrors AGY's `inject_is_idempotent`
    /// (`agy.rs:454-467`).
    #[test]
    fn inject_is_idempotent() {
        let temp = TempDir::new().unwrap();
        provision_cursor(temp.path());
        let hooks_first = read_hooks_json(temp.path());

        provision_cursor(temp.path());
        let hooks_second = read_hooks_json(temp.path());
        assert_eq!(hooks_first, hooks_second);
    }

    /// Idempotency at the byte level — when the namespace already
    /// matches the expected shape, the file is NOT rewritten. Asserted
    /// by checking mtime is unchanged between two injects that find the
    /// integration already wired. Same pattern as Grok's
    /// `idempotent_rerun_does_not_rewrite_when_already_wired`
    /// (`grok.rs:1280-1296`).
    #[test]
    fn idempotent_rerun_does_not_rewrite_when_already_wired() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".cursor").join("hooks.json");
        provision_cursor(temp.path());
        let first_bytes = std::fs::read(&path).unwrap();
        let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        provision_cursor(temp.path());
        let second_bytes = std::fs::read(&path).unwrap();
        let second_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        assert_eq!(first_bytes, second_bytes, "byte-identical re-run");
        assert_eq!(
            first_mtime, second_mtime,
            "idempotent re-run must NOT rewrite the file (no mtime change)"
        );
    }

    /// Injection only owns the `buildmesh-attention` key — unrelated
    /// top-level keys the user added (other tools, custom automation)
    /// round-trip through a re-injection untouched.
    #[test]
    fn inject_preserves_unrelated_top_level_keys() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        std::fs::write(
            cursor_dir.join("hooks.json"),
            r#"{"custom_namespace":{"user":"kept"},"sibling":"keep-me"}"#,
        )
        .unwrap();

        provision_cursor(temp.path());

        let hooks = read_hooks_json(temp.path());
        assert_eq!(hooks["custom_namespace"]["user"], "kept");
        assert_eq!(hooks["sibling"], "keep-me");
        // And the new namespace is in place.
        assert!(hooks[HOOK_NAMESPACE]["Stop"].is_array());
    }

    /// A user's existing `hooks.json` that already owns a
    /// `buildmesh-attention` key survives intact — same namespace, just
    /// re-asserted with the current command shape (which is byte-for-
    /// byte identical, so no actual rewrite fires).
    #[test]
    fn inject_preserves_sibling_namespaces_alongside_our_namespace() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        std::fs::write(
            cursor_dir.join("hooks.json"),
            r#"{"buildmesh-attention":{"stale":"old"},"other":"kept"}"#,
        )
        .unwrap();

        provision_cursor(temp.path());

        let hooks = read_hooks_json(temp.path());
        assert!(hooks[HOOK_NAMESPACE]["Stop"].is_array());
        assert_eq!(hooks["other"], "kept");
    }

    /// Atomic write leaves no `.tmp` residue in the hooks dir. Mirrors
    /// AGY precedent (`agy.rs:567-582`).
    #[test]
    fn inject_atomic_write_leaves_no_tmp_residue() {
        let temp = TempDir::new().unwrap();
        provision_cursor(temp.path());

        let dir = temp.path().join(".cursor");
        let entries = std::fs::read_dir(&dir).unwrap();
        let tmp_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            tmp_files.is_empty(),
            "atomic write must not leave .tmp residue; found {tmp_files:?}"
        );
    }

    /// A malformed user-authored JSON file (trailing comma, partial
    /// edit, syntax error) must NOT cause `ensure_hooks_json` to
    /// silently overwrite with `{}`. The function returns an `Err`,
    /// the spawn path surfaces it as a provision failure, and the
    /// user's content survives intact. Mirrors Grok's
    /// `inject_refuses_to_overwrite_malformed_user_file`
    /// (`grok.rs:1305-1329`).
    #[test]
    fn inject_refuses_to_overwrite_malformed_user_file() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        let path = cursor_dir.join("hooks.json");
        // Deliberately malformed: trailing comma + unmatched brace.
        let malformed = "{ \"buildmesh-attention\": [],, }";
        std::fs::write(&path, malformed).unwrap();

        let path_str = project_to_string(temp.path());
        let resolved = ResolvedPath {
            host_path: path_str.clone(),
            spawn_path: path_str.clone(),
            raw_path: path_str,
            env_type: EnvType::Windows,
        };
        let result = CURSOR.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0);
        assert!(
            result.is_err(),
            "provision must refuse a malformed existing file; got {result:?}"
        );

        // The user's malformed content must survive intact.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, malformed,
            "malformed file content must NOT be overwritten"
        );
    }

    /// A valid JSON top-level that isn't an object (e.g. an array) is a
    /// misconfiguration — return Err rather than clobbering the user's
    /// payload with `{}`. Mirrors Grok's round-2 fix (review point 5,
    /// `grok.rs:237-243`).
    #[test]
    fn inject_refuses_top_level_array() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        let path = cursor_dir.join("hooks.json");
        // A valid JSON top-level that isn't an object.
        std::fs::write(&path, "[1, 2, 3]").unwrap();

        let path_str = project_to_string(temp.path());
        let resolved = ResolvedPath {
            host_path: path_str.clone(),
            spawn_path: path_str.clone(),
            raw_path: path_str,
            env_type: EnvType::Windows,
        };
        let result = CURSOR.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0);
        assert!(
            result.is_err(),
            "non-object top-level must be rejected; got {result:?}"
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("must be a JSON object"),
            "error must name the shape: {error}"
        );
        // The user's array must survive intact.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1, 2, 3]");
    }

    /// The matching positive case for the malformed-file pin: a missing
    /// file should be treated as `{}` (fresh install) and written
    /// normally. Lock the happy path so a future refactor that treats
    /// both missing AND malformed the same way trips both regressions
    /// in one place.
    #[test]
    fn inject_treats_missing_file_as_empty_settings() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        // Note: no file at .cursor/hooks.json — fresh install.
        assert!(!cursor_dir.join("hooks.json").exists());

        provision_cursor(temp.path());
        let written = std::fs::read_to_string(cursor_dir.join("hooks.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert!(value[HOOK_NAMESPACE]["Stop"].is_array());
    }

    /// Helper: convert a project path into a string for `ResolvedPath`
    /// (the `ResolvedPath` struct carries owned strings, not the
    /// `TempDir` borrow). Mirrors AGY's `provision_agy` shim
    /// (`agy.rs:282-293`).
    fn project_to_string(project: &Path) -> String {
        project.to_string_lossy().into_owned()
    }

    /// The Windows hook command uses cmd environment syntax (`%VAR%`)
    /// and is a bare command because Cursor supplies the shell. Unix
    /// uses `$VAR` syntax and likewise does not add a nested shell
    /// wrapper. Mirrors AGY's `hook_command_uses_platform_env_syntax`
    /// (`agy.rs:514-532`).
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

    /// Cursor invokes hook commands through its own Windows shell
    /// wrapper. The command stored in hooks.json must therefore be a
    /// bare command rather than a second `cmd.exe /c "..."` wrapper
    /// (the second `cmd.exe` would env-clear the parent's exports, so
    /// `$BUILDMESH_PORT` / `$BUILDMESH_SESSION_ID` never expand). Same
    /// mirror as AGY (`agy.rs:533-554`).
    #[test]
    fn hook_command_is_bare_no_double_shell_wrap() {
        let win = hook_command(Platform::Windows);
        assert!(!win.starts_with("cmd.exe /c"), "win: {win}");
        assert!(!win.starts_with('"') && !win.ends_with('"'), "win: {win}");
        assert!(win.contains("curl.exe"), "win: {win}");
        assert!(win.contains("%BUILDMESH_PORT%"), "win: {win}");
        assert!(win.contains("%BUILDMESH_SESSION_ID%"), "win: {win}");

        let unix = hook_command(Platform::Linux);
        assert!(!unix.starts_with("sh -c"), "unix: {unix}");
        assert!(unix.contains("$BUILDMESH_PORT"), "unix: {unix}");
    }

    /// Adapter contract: the harness now requires an attention hook
    /// (issue #1368) and continues to advertise the transcript reader
    /// (Cursor writes Anthropic-shaped JSONL under its workspace-
    /// scoped `~/.cursor/projects/agent-transcripts` layout).
    #[test]
    fn cursor_declares_attention_hook_with_readable_transcript() {
        assert!(CURSOR.requires_attention_hook());
        assert!(CURSOR.produces_readable_transcript());
    }

    /// Issue #1368: pin the attention capability descriptor — the
    /// `--force` launch mode, the Stop-only events list, and the
    /// `min_version` pin to `CURSOR_MIN_HOOK_VERSION`. Drift in any of
    /// these flips the Inspector's capability table for Cursor and is
    /// the load-bearing contract for the Spawn Menu's hook-aware
    /// affordances. Mirrors the Grok `capabilities_descriptor_*`
    /// precedent (`grok.rs:688-735`).
    #[test]
    fn cursor_attention_capability_advertises_skip_permissions_with_min_version() {
        let caps = CURSOR.capabilities();
        assert_eq!(caps.harness_id, "cursor");
        assert!(caps.requires_attention_hook);
        let cap = caps.attention_capability;
        assert!(matches!(
            cap,
            AttentionCapability::Hook {
                launch_mode: AttentionLaunchMode::SkipPermissions,
                min_version: Some(ref v),
                ..
            } if v == CURSOR_MIN_HOOK_VERSION
        ));
        // Stop-only signal under --force; no permission / pre-tool event.
        let events = match &cap {
            AttentionCapability::Hook { events, .. } => events.clone(),
            _ => panic!("expected Hook, got {cap:?}"),
        };
        assert!(events.contains(&LifecycleKind::TurnCompleted));
        assert!(events.contains(&LifecycleKind::BackgroundRunning));
        assert!(!events.contains(&LifecycleKind::PermissionRequested));
        assert!(!events.contains(&LifecycleKind::QuestionRequested));
    }

    /// Issue #1368: pin the minimum-version constant so a refactor that
    /// silently bumps or drops the pin trips here before the wire shape
    /// drifts. The Grok precedent (`GROK_MIN_HOOK_VERSION`, issue
    /// #1366) uses the same `_MIN_` descriptor-only convention.
    #[test]
    fn cursor_min_hook_version_constant_is_pinned() {
        assert_eq!(CURSOR_MIN_HOOK_VERSION, "1.0.0");
        assert!(
            !CURSOR_MIN_HOOK_VERSION.contains(".."),
            "version must not contain '..' (would silently match anything): {CURSOR_MIN_HOOK_VERSION}"
        );
    }
}
