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

/// Marker substring written into the Buildmesh-owned `stop` handler.
/// Used by `ensure_hooks_json` to detect a stale Buildmesh entry on
/// re-provision (issue #886 idempotency invariant): a re-run that finds
/// an existing handler with this substring leaves it alone, a re-run
/// that finds a different Buildmesh entry updates it in place, and a
/// re-run that finds no Buildmesh entry appends a fresh one.
const BUILDMESH_HOOK_MARKER: &str = "/api/attention/";

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
fn hook_command(env_type: EnvType) -> String {
    match env_type {
        EnvType::Windows => {
            "curl.exe -sf --connect-timeout 1 --max-time 2 -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID% >nul 2>nul || exit 0"
                .to_string()
        }
        EnvType::Wsl => {
            "curl -sf --connect-timeout 1 --max-time 2 -X POST -H 'Content-Type: application/json' --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID >/dev/null 2>/dev/null || true"
                .to_string()
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

/// True when `handler` looks like the Buildmesh-owned `stop` entry —
/// the URL contains our `/api/attention/` marker AND the command carries
/// the per-agent env var names. Used by `ensure_hooks_json` to upsert
/// the Buildmesh entry into an event's handler array.
fn is_buildmesh_handler(handler: &serde_json::Value) -> bool {
    handler
        .get("command")
        .and_then(|v| v.as_str())
        .is_some_and(|command| {
            command.contains(BUILDMESH_HOOK_MARKER)
                && command.contains("BUILDMESH_PORT")
                && command.contains("BUILDMESH_SESSION_ID")
        })
}

/// Add or refresh the Buildmesh-owned `stop` handler in
/// `<project>/.cursor/hooks.json`. The merge walks the documented
/// Cursor shape — `{ "version": 1, "hooks": { "stop": [...] } }` —
/// and upserts the Buildmesh entry into the `stop` array, preserving
/// any sibling handlers (other tools, user-authored automation) the
/// user has registered. Returns `Ok(())` when the file already carries
/// the expected handler (no rewrite fires; the issue #886 idempotency
/// invariant).
///
/// A malformed existing file (trailing comma, partial edit, syntax
/// error) is treated as an explicit `Err` rather than silently clobbered
/// with `{}` — the Grok precedent (`grok.rs:222-243`) pins the round-2
/// review fix: a missing file is `{}`, but a present-but-unparseable
/// file is the user's data and must surface to the spawn path as a
/// provision failure so the agent user can repair it.
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
    // Cursor's documented shape requires `"version": 1` at the root.
    // We don't write anything other than `1` — a user with a future
    // major-version file would surface that here as an explicit
    // overwrite rather than silently coercing it.
    match settings.get("version") {
        None => {
            settings["version"] = serde_json::json!(1);
        }
        Some(v) if v == &serde_json::json!(1) => {}
        Some(other) => {
            return Err(format!(
                "cursor hooks.json `version` must be 1; got {other}"
            ));
        }
    }
    if settings.get("hooks").is_none() {
        settings["hooks"] = serde_json::json!({});
    }
    let hooks = settings
        .get_mut("hooks")
        .expect("hooks key inserted above");
    if !hooks.is_object() {
        return Err(format!(
            "cursor hooks.json `hooks` must be a JSON object; got {}",
            settings_kind(hooks)
        ));
    }
    if hooks.get("stop").is_none() {
        hooks["stop"] = serde_json::json!([]);
    }
    let stop = hooks.get_mut("stop").expect("stop key inserted above");
    if !stop.is_array() {
        return Err(format!(
            "cursor hooks.json `hooks.stop` must be an array; got {}",
            settings_kind(stop)
        ));
    }
    let stop_array = stop
        .as_array_mut()
        .expect("verified is_array above");
    let new_handler = serde_json::json!({ "command": command });
    let mut changed = false;
    if let Some(existing) = stop_array
        .iter_mut()
        .find(|h| is_buildmesh_handler(h))
    {
        if *existing != new_handler {
            *existing = new_handler;
            changed = true;
        }
    } else {
        stop_array.push(new_handler);
        changed = true;
    }
    if !changed {
        return Ok(());
    }
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
        // `<project>/.cursor/hooks.json` (`stop` event only — under
        // `--force` Cursor doesn't raise permission prompts by
        // construction). The descriptor at `attention_capability()`
        // below carries the structured contract.
        true
    }

    fn attention_capability(&self) -> AttentionCapability {
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
            events: vec![LifecycleKind::TurnCompleted, LifecycleKind::BackgroundRunning],
            launch_mode: AttentionLaunchMode::SkipPermissions,
            trust: None,
            // Issue #1368: pin the validated Cursor release (1.0.0) so
            // a refactor that flips `min_version` to None trips this
            // pin. Mirrors the Grok (`GROK_MIN_HOOK_VERSION`, issue
            // #1366) and AGY (`"1.0.0"`) precedents.
            min_version: Some(CURSOR_MIN_HOOK_VERSION.into()),
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
    // Attention hook injection — issue #1368 round-2.
    //
    // Cursor under `--force` behaves like AGY under
    // `--dangerously-skip-permissions`: every tool auto-runs unless a
    // hook matcher denies it, so the only signal Cursor emits at a
    // useful cadence is `stop`. Buildmesh provisions
    // `<project>/.cursor/hooks.json` with a single `stop` handler that
    // POSTs the hook stdin to the attention endpoint; the route's
    // classifier reads Cursor's documented `conversation_id` (snake_case)
    // + `hook_event_name: "stop"` envelope to land the node in `Ready`
    // (clean turn completion, issue #1364) or `AwaitingInput` (rare,
    // if Cursor adds permission prompts in a future release).
    //
    // Round-2 review fix: the schema asserted here is the documented
    // Cursor shape (`version: 1`, `hooks: { stop: [{command: ...}] }`),
    // NOT the AGY `buildmesh-attention` namespace. The earlier tests
    // were tautological (asserted that `ensure_hooks_json` wrote what
    // `ensure_hooks_json` told it to write); these tests assert against
    // the public schema Cursor actually loads.
    // -------------------------------------------------------------------

    use crate::agent::capabilities::AttentionCapability;
    use tempfile::TempDir;

    fn read_hooks_json(project: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(project.join(".cursor").join("hooks.json"))
            .expect("hooks.json not written");
        serde_json::from_str(&content).expect("hooks.json is not valid JSON")
    }

    fn provision_cursor(project: &Path, env_type: EnvType) {
        let path = project.to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type,
        };
        CURSOR
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
            .unwrap();
    }

    /// Cursor's documented `.cursor/hooks.json` shape is:
    ///   { "version": 1, "hooks": { "<event>": [{ "command": "..." }, …] } }
    /// Asserted against the public schema, not against the implementation's
    /// own bookkeeping. The `stop` entry stores the bare `{command}` object
    /// (no `type` field — Cursor's runner doesn't read one).
    #[test]
    fn inject_writes_cursor_shaped_hooks_json() {
        let temp = TempDir::new().unwrap();
        provision_cursor(temp.path(), EnvType::Windows);

        let hooks = read_hooks_json(temp.path());

        // Top-level shape: object, version: 1, hooks: object.
        assert!(hooks.is_object(), "top-level must be an object: {hooks}");
        assert_eq!(hooks["version"], serde_json::json!(1));
        let hooks_obj = hooks.get("hooks").expect("hooks key missing");
        assert!(hooks_obj.is_object(), "`hooks` must be an object: {hooks_obj}");

        // stop is an array; the Buildmesh handler lives at index 0.
        let stop_array = hooks_obj["stop"]
            .as_array()
            .expect("`hooks.stop` must be an array");
        assert!(
            !stop_array.is_empty(),
            "`hooks.stop` must carry at least one handler: {stop_array:?}"
        );
        let handler = &stop_array[0];
        assert!(handler.is_object(), "handler must be an object: {handler}");
        // Cursor's runner ignores a `type` field — the documented entry is
        // bare `{ "command": "..." }`. Asserting the absence locks the wire.
        assert!(
            handler.get("type").is_none(),
            "Cursor handler must NOT carry a `type` field: {handler}"
        );
        let cmd = handler["command"]
            .as_str()
            .expect("`command` missing on stop handler");
        assert!(
            cmd.contains("/api/attention/"),
            "Stop must POST to the attention endpoint: {cmd}"
        );
        assert!(
            cmd.contains("--data-binary @-"),
            "Stop must forward the hook stdin as the POST body: {cmd}"
        );
        assert!(
            cmd.contains("BUILDMESH_PORT") && cmd.contains("BUILDMESH_SESSION_ID"),
            "Stop must expand the per-agent env vars at runner time: {cmd}"
        );
    }

    /// Re-running provision over an already-correct project is a no-op.
    /// Re-spawns (resume / handover / re-spawn on a closed node) must
    /// not rewrite the file and risk churn on unrelated siblings the
    /// user has added. Mirrors the AGY `inject_is_idempotent` pattern.
    #[test]
    fn inject_is_idempotent() {
        let temp = TempDir::new().unwrap();
        provision_cursor(temp.path(), EnvType::Windows);
        let hooks_first = read_hooks_json(temp.path());

        provision_cursor(temp.path(), EnvType::Windows);
        let hooks_second = read_hooks_json(temp.path());
        assert_eq!(hooks_first, hooks_second);
    }

    /// Idempotency at the byte level — when the Buildmesh handler
    /// already matches, the file is NOT rewritten. Asserted by mtime.
    /// Same pattern as Grok's
    /// `idempotent_rerun_does_not_rewrite_when_already_wired`
    /// (`grok.rs:1280-1296`).
    #[test]
    fn idempotent_rerun_does_not_rewrite_when_already_wired() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".cursor").join("hooks.json");
        provision_cursor(temp.path(), EnvType::Windows);
        let first_bytes = std::fs::read(&path).unwrap();
        let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        provision_cursor(temp.path(), EnvType::Windows);
        let second_bytes = std::fs::read(&path).unwrap();
        let second_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        assert_eq!(first_bytes, second_bytes, "byte-identical re-run");
        assert_eq!(
            first_mtime, second_mtime,
            "idempotent re-run must NOT rewrite the file (no mtime change)"
        );
    }

    /// Cursor's documented schema is `hooks[event]: [...]`, NOT the AGY
    /// `buildmesh-attention` root namespace. Provisioning must NOT
    /// introduce a `buildmesh-attention` key — that's a leftover from
    /// the AGY copy that round-2 review caught. The Cursor runner
    /// ignores any top-level keys other than `version` + `hooks`, so
    /// the AGY namespace would silently no-op (the hook never fires).
    #[test]
    fn inject_does_not_use_agy_buildmesh_attention_namespace() {
        let temp = TempDir::new().unwrap();
        provision_cursor(temp.path(), EnvType::Windows);

        let hooks = read_hooks_json(temp.path());
        assert!(
            hooks.get("buildmesh-attention").is_none(),
            "Cursor's documented schema is `hooks[event]`; a top-level \
             `buildmesh-attention` namespace is the AGY shape and would \
             be silently ignored: {hooks}"
        );
    }

    /// Re-provision must preserve sibling handlers (other tools, user
    /// automation) the user has registered in `hooks.stop`. We append
    /// our own handler without touching theirs. Mirrors
    /// `inject_preserves_sibling_handlers` in `grok.rs`.
    #[test]
    fn inject_preserves_sibling_handlers_in_stop_array() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        // Pre-existing user-authored entry — an audit log shipping
        // every Cursor stop event to a separate collector. Provisioning
        // must keep it.
        let existing = r#"{
            "version": 1,
            "hooks": {
                "stop": [
                    { "command": "echo user-audit-log >> /tmp/audit.log" }
                ]
            }
        }"#;
        std::fs::write(cursor_dir.join("hooks.json"), existing).unwrap();

        provision_cursor(temp.path(), EnvType::Windows);

        let hooks = read_hooks_json(temp.path());
        let stop = hooks["hooks"]["stop"].as_array().expect("stop array");
        assert_eq!(
            stop.len(),
            2,
            "sibling handler must be preserved alongside Buildmesh: {stop:?}"
        );
        assert!(
            stop.iter()
                .any(|h| h["command"]
                    .as_str()
                    .unwrap_or("")
                    .contains("/api/attention/")),
            "Buildmesh handler must be present: {stop:?}"
        );
        assert!(
            stop.iter()
                .any(|h| h["command"].as_str() == Some("echo user-audit-log >> /tmp/audit.log")),
            "user-authored handler must round-trip: {stop:?}"
        );
    }

    /// A pre-existing Buildmesh handler (carrying the URL marker but a
    /// stale command body) must be detected and replaced in place —
    /// appending would create duplicates. This is the issue #886
    /// idempotency invariant on the array-of-handlers shape: an
    /// existing entry with the marker substring is updated rather
    /// than duplicated.
    #[test]
    fn inject_dedupes_existing_buildmesh_stop_entry() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        // Pre-existing Buildmesh entry (same marker substring so the
        // upsert path runs, stale body so we can detect the
        // replace-in-place rather than a duplicate append).
        let existing = r#"{
            "version": 1,
            "hooks": {
                "stop": [
                    {
                        "command": "echo stale-buildmesh-curl http://localhost:1999/api/attention/0 BUILDMESH_PORT=stale BUILDMESH_SESSION_ID=stale"
                    }
                ]
            }
        }"#;
        std::fs::write(cursor_dir.join("hooks.json"), existing).unwrap();

        provision_cursor(temp.path(), EnvType::Windows);

        let hooks = read_hooks_json(temp.path());
        let stop = hooks["hooks"]["stop"].as_array().expect("stop array");
        assert_eq!(
            stop.len(),
            1,
            "stale Buildmesh entry must be replaced, not appended: {stop:?}"
        );
        let cmd = stop[0]["command"].as_str().expect("command");
        assert!(
            cmd.contains("/api/attention/"),
            "replaced entry must carry the Buildmesh URL: {cmd}"
        );
        assert!(
            !cmd.contains("stale-buildmesh-curl"),
            "stale body must be replaced, not preserved: {cmd}"
        );
        assert!(
            cmd.contains("%BUILDMESH_PORT%"),
            "replaced body must use the live Windows env-var syntax: {cmd}"
        );
    }

    /// Injection must preserve user-authored top-level keys the user
    /// has added (custom tooling, schema metadata). The Cursor runner
    /// only reads `version` + `hooks`, but the round-trip is cheap and
    /// matches the project's "additive merge" invariant.
    #[test]
    fn inject_preserves_unrelated_top_level_keys() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        std::fs::write(
            cursor_dir.join("hooks.json"),
            r#"{"custom_key":"keep-me","version":1,"hooks":{}}"#,
        )
        .unwrap();

        provision_cursor(temp.path(), EnvType::Windows);

        let hooks = read_hooks_json(temp.path());
        assert_eq!(hooks["custom_key"], "keep-me");
        assert_eq!(hooks["version"], serde_json::json!(1));
        assert!(hooks["hooks"]["stop"].is_array());
    }

    /// Atomic write leaves no `.tmp` residue in the hooks dir. Mirrors
    /// the AGY precedent (`agy.rs:567-582`).
    #[test]
    fn inject_atomic_write_leaves_no_tmp_residue() {
        let temp = TempDir::new().unwrap();
        provision_cursor(temp.path(), EnvType::Windows);

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
        let malformed = "{ \"hooks\": [],, }";
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

    /// The `hooks.stop` value that exists as a non-array (e.g. a
    /// string) is a misconfiguration — refuse rather than clobbering.
    /// The AGY round-2 review caught the analogous `trustedWorkspaces`
    /// case at `workspace_trust.rs:144-156`.
    #[test]
    fn inject_refuses_non_array_stop() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        let path = cursor_dir.join("hooks.json");
        // `hooks.stop` is a string, not an array — Cursor's runner
        // would refuse the whole file.
        std::fs::write(
            &path,
            r#"{ "version": 1, "hooks": { "stop": "not-an-array" } }"#,
        )
        .unwrap();

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
            "non-array `hooks.stop` must be rejected; got {result:?}"
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("`hooks.stop`"),
            "error must name the offending field: {error}"
        );
    }

    /// A `version` field other than `1` (e.g. a user with a future
    /// Cursor major-version file) must be refused — silently coercing
    /// it to `1` would mask a real schema mismatch and the runner
    /// would silently drop the handler. Pin the gate so a future
    /// Cursor that documents `version: 2` trips here before silently
    /// rewriting.
    #[test]
    fn inject_refuses_unsupported_version() {
        let temp = TempDir::new().unwrap();
        let cursor_dir = temp.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        let path = cursor_dir.join("hooks.json");
        std::fs::write(
            &path,
            r#"{ "version": 2, "hooks": { "stop": [] } }"#,
        )
        .unwrap();

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
            "unsupported `version` must be rejected; got {result:?}"
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("`version`"),
            "error must name the offending field: {error}"
        );
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

        provision_cursor(temp.path(), EnvType::Windows);
        let written = std::fs::read_to_string(cursor_dir.join("hooks.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(value["version"], serde_json::json!(1));
        assert!(value["hooks"]["stop"].is_array());
        assert!(!value["hooks"]["stop"].as_array().unwrap().is_empty());
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
    /// wrapper. The Windows command must end in `|| exit 0`; the
    /// Unix command must end in `|| true` (round-2 review, point 5).
    #[test]
    fn hook_command_uses_env_type_specific_syntax_with_fail_safe() {
        let win = hook_command(EnvType::Windows);
        assert!(win.starts_with("curl.exe "), "win: {win}");
        assert!(!win.contains("cmd.exe /c"), "win: {win}");
        assert!(win.contains("%BUILDMESH_PORT%"), "win: {win}");
        assert!(win.contains("%BUILDMESH_SESSION_ID%"), "win: {win}");
        // Fail-safe: a curl failure (port dropped, Buildmesh restarting)
        // must not surface as a hook exit error. cmd's `|| exit 0` is
        // the analogue of POSIX `|| true`.
        assert!(
            win.trim_end().ends_with("|| exit 0"),
            "win command must end with `|| exit 0`: {win}"
        );

        let unix = hook_command(EnvType::Wsl);
        assert!(unix.starts_with("curl "), "unix: {unix}");
        assert!(!unix.contains("sh -c"), "unix: {unix}");
        assert!(unix.contains("$BUILDMESH_PORT"), "unix: {unix}");
        assert!(unix.contains("$BUILDMESH_SESSION_ID"), "unix: {unix}");
        assert!(
            unix.trim_end().ends_with("|| true"),
            "unix command must end with `|| true`: {unix}"
        );
    }

    /// Round-2 review point 3: a Windows host_path with a WSL Cursor
    /// underneath must write the bash-flavored command (`$VAR`,
    /// `>/dev/null`), not the cmd-flavored one. The earlier PR
    /// hard-coded `Platform::current()` for the shell syntax, which
    /// would silently write a `%VAR%` command into a Linux WSL
    /// hooks.json — Linux shells do not expand `%VAR%`, so the curl
    /// POST never fires. Provision must derive the syntax from
    /// `resolved.env_type`.
    #[test]
    fn provision_uses_resolved_env_type_not_host_platform() {
        let temp = TempDir::new().unwrap();
        // host_path is a Windows-style UNC path, but env_type is WSL —
        // the agent runs inside a Linux distro even though Buildmesh
        // is on Windows. We must write the bash syntax.
        let resolved = ResolvedPath {
            host_path: temp.path().to_string_lossy().into_owned(),
            spawn_path: temp.path().to_string_lossy().into_owned(),
            raw_path: temp.path().to_string_lossy().into_owned(),
            env_type: EnvType::Wsl,
        };
        CURSOR
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
            .unwrap();

        let hooks = read_hooks_json(temp.path());
        let cmd = hooks["hooks"]["stop"][0]["command"]
            .as_str()
            .expect("command")
            .to_string();
        assert!(
            cmd.contains("$BUILDMESH_PORT"),
            "WSL target must use POSIX `$VAR` syntax: {cmd}"
        );
        assert!(
            !cmd.contains("%BUILDMESH_PORT%"),
            "WSL target must NOT write cmd `%VAR%` syntax: {cmd}"
        );
        assert!(
            cmd.contains(">/dev/null 2>/dev/null"),
            "WSL target must redirect to /dev/null: {cmd}"
        );
        assert!(
            !cmd.contains(">nul"),
            "WSL target must NOT write cmd `>nul` syntax: {cmd}"
        );
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

    /// Issue #1368 round-2: the descriptor advertises `trust = None`,
    /// not `"workspace trust"`. Cursor under `--force` does not
    /// require a workspace-trust side-effect; advertising a non-empty
    /// `trust` would promise a step the codebase never takes
    /// (Cursor's `ensure_workspace_trusted` falls through to the
    /// trait no-op default). Pin so a refactor that flips the value
    /// back to `Some("workspace trust".into())` trips here before
    /// the wire shape drifts.
    #[test]
    fn cursor_attention_capability_advertises_no_trust() {
        let caps = CURSOR.capabilities();
        assert!(caps.requires_attention_hook);
        let cap = caps.attention_capability;
        match &cap {
            AttentionCapability::Hook { trust, .. } => {
                assert!(
                    trust.is_none(),
                    "Cursor advertises no workspace-trust side-effect under `--force`; \
                     a non-None `trust` would promise a step we never take: {trust:?}"
                );
            }
            _ => panic!("expected Hook, got {cap:?}"),
        }
    }

    /// Issue #1368: pin the attention capability descriptor — the
    /// `--force` launch mode, the stop-only events list, and the
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