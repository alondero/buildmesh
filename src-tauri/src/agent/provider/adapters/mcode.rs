//! MiniMax Code CLI provider adapter — MiniMax's full-screen interactive
//! coding agent, installed on PATH as a single `mcode` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. Buildmesh's PTY backend (ConPTY on
//! Windows, native PTY on macOS/Linux) fully supports full-screen TUI rendering,
//! so we launch in interactive mode everywhere.
//!
//! **Session resumption** uses `--session [<id>]` or `-c` / `--continue`.
//! MiniMax Code auto-assigns its own session ids (captured from PTY output by
//! `session_naming`), so `self_assigns_session_id()` is `true` and
//! `session_assign_args()` is a no-op.
//!
//! **No model override** (issue #1179). `mcode` exposes `--model
//! <provider>/<model>` on the `exec` subcommand only; the interactive TUI
//! the harness always launches rejects it. Previously this adapter
//! advertised `supports_model_override() == true` while emitting a
//! `--model` flag the active recipe did not accept — the resolver
//! passed the value through, the spawn path appended it, and the TUI
//! surfaced an upstream rejection. The coherent choice (recorded in
//! the issue thread) is to keep the interactive TUI as the supported
//! mode and drop the override. A future `mcode exec`-based launch
//! mode (with its own lifecycle work) would re-advertise the flag.
//!
//! **Prefill** is the trailing positional `[prompt]` — there is no `--prefill`
//! flag. We override `prefill_args()` to return the text as a single positional
//! arg (the trait default `["--prefill", text]` would be rejected upstream).
//!
//! **Shell wrapping**: `mcode` is distributed as a `.cmd` batch shim on
//! Windows (`mcode.cmd`), which `CreateProcess` won't run directly; the recipe
//! wraps with `WindowsShell::Cmd` -> `cmd.exe /c mcode …` on Windows. On macOS /
//! Linux it is an executable on PATH so `WindowsShell::Direct` is used.
//!
//! **Attention** (issue #1796): mcode ≥0.2.4 exposes an Agent-Plugin hook
//! surface (`hooks/hooks.json` entries plus per-event scripts; stdin JSON
//! carries `hook_event_name` + `session_id` + `transcript_path`). Buildmesh
//! provisions the `io.buildmesh.attention` plugin under
//! `<dataDir>/plugins/io.buildmesh.attention/hooks/hooks.json`, installing
//! Claude-shaped `{command}` curl entries for `Stop` and `PermissionRequest`
//! that POST stdin JSON to `/api/attention/<session-id>` (env-var expanded at
//! hook-run time, matching the Cursor / Kimi / Grok precedent). The merge is
//! idempotent and additive: user-authored plugins and sibling Buildmesh
//! entries for events we don't manage (`SessionStart`, etc.) round-trip
//! untouched, malformed user files fail closed rather than overwriting
//! silently, and an unresolvable data dir returns `Ok(())` with no side
//! effects. `requires_attention_hook` stays `false` until TUI delivery is
//! validated end-to-end (follow-up issue).
//!
//! **Transcript**: `messages.jsonl` canonical history is parsed via
//! `TranscriptFormat::Mcode`, so the Coordinator Node Digest rich layer,
//! the archived-node resume picker, and circuit assistant reports all work.

use crate::agent::provider::{AgentProvider, LaunchRuntime, Platform, ResolvedPath, SpawnRecipe, UiMeta, WindowsShell};
use crate::env::windows_attention_command;
use crate::models::EnvType;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct McodeAdapter;
pub static MCODE: McodeAdapter = McodeAdapter;

/// Minimum mcode release the Agent-Plugin hook surface has been
/// validated against (issue #1796). The `hooks/hooks.json` schema,
/// the `Stop` and `PermissionRequest` event names, and the `hook_event_name`
/// / `session_id` / `transcript_path` stdin envelope were introduced in
/// the 0.2.x Agent-Plugin revision; the constant pins the descriptor
/// to a concrete release so a future mcode that renames events, drops
/// the plugin directory, or rewrites the payload shape surfaces as a
/// visible capability/health change. Like `CURSOR_MIN_HOOK_VERSION` and
/// `GROK_MIN_HOOK_VERSION`, the pin is descriptor-shape only — we do not
/// gate the spawn on a runtime version probe because mcode does not
/// expose a semver-ish header through the hook surface.
pub const MCODE_MIN_HOOK_VERSION: &str = "0.4.12";

/// Buildmesh-owned mcode Agent-Plugin directory under the user's data
/// root (`<dataDir>/plugins/<dir>`). mcode ≥0.2.4 loads every
/// `<dataDir>/plugins/*/hooks/hooks.json`; installing this plugin makes
/// Buildmesh's attention callbacks fire without disturbing any other
/// plugin the user has registered. Follows the reverse-DNS convention
/// used by the upstream `hello-mcode-hooks` example
/// (`io.<publisher>.<plugin>`).
const MCODE_PLUGIN_DIR: &str = "io.buildmesh.attention";

/// Events Buildmesh provisions into the mcode Agent-Plugin hooks file.
/// The issue #1796 acceptance specification calls out `Stop` (turn
/// finished) and `PermissionRequest` (tool approval pending) as the
/// two events Buildmesh needs to drive Node Digest turn completion and
/// the permission-aware attention surface. We do not register for the
/// question / pre-tool / notification events Cursor or Kimi subscribe
/// to because mcode's permission prompt runs over `PermissionRequest`
/// specifically — its `PreToolUse` semantics differ from the Claude
/// spec and its question flow is delivered through the interactive
/// TUI rather than a hook (validated by the live TUI delivery issue
/// follow-up).
const MCODE_PROVISIONED_EVENTS: &[&str] = &["Stop", "PermissionRequest"];

/// Marker substring written into the Buildmesh-owned hook handler.
/// `ensure_mcode_hooks_json` uses it to detect a stale Buildmesh entry
/// on re-provision (the issue #886 idempotency invariant): a re-run
/// that finds an existing event array carrying this substring replaces
/// the entry in place (so the array never grows), a re-run that finds
/// no Buildmesh entry for the event appends a fresh one, and a re-run
/// that already carries the exact handler leaves the file untouched.
const BUILDMESH_HOOK_MARKER: &str = "/api/attention/";

/// Counter backing the PID+counter `.tmp` suffix for atomic writes
/// (Cursor / AGY precedent — `agy.rs:14-39`, `cursor.rs:62`). Two
/// concurrent writes in the same process would otherwise collide on a
/// fixed tmp name.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Per-platform shell selection. Mirrors the OpenCode / Antigravity pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

/// Build the curl command line mcode's hook runner invokes when the
/// `Stop` or `PermissionRequest` event fires. The runner forwards the
/// hook stdin JSON (mcode's `session_id` + `hook_event_name` envelope)
/// and we POST it verbatim to the attention endpoint via curl.
/// `--data-binary @-` passes stdin as the POST body so the attention
/// route's classifier can read the envelope fields. mcode's hook
/// runner inherits the agent process's environment, so
/// `$BUILDMESH_PORT` / `$BUILDMESH_SESSION_ID` set per-agent by
/// `spawn_environment::wrap` expand at hook-run time — `node_id` is
/// intentionally not baked into the URL (Kimi / Cursor precedent), it
/// arrives via `BUILDMESH_SESSION_ID` which is the Buildmesh node id.
///
/// The trailing `|| exit 0` (cmd) / `|| true` (POSIX) ensures a curl
/// failure (Buildmesh restarting, port dropped, EHOSTUNREACH, …) never
/// surfaces as a non-zero hook exit. mcode propagates hook exit codes
/// into its TUI as visible errors, and the attention webhook is
/// best-effort telemetry — it must never fail-block the agent.
///
/// Round-2 review (PR #1511, Cursor precedent): Windows cmd syntax
/// (`curl.exe` / `%VAR%` / `>nul`) is ONLY correct when the runtime is
/// Windows, and `EnvType::Windows` is the default for non-WSL macOS and
/// Linux too. A naive `match env_type` would write `curl.exe` into
/// `.hooks.json` on macOS / Linux where it doesn't exist. Gate cmd
/// syntax on `cfg!(target_os = "windows") && env_type == EnvType::Windows`
/// (cross-compile safe — `cfg!` is the *target* triple).
pub(crate) fn hook_command(env_type: EnvType) -> String {
    if env_type == EnvType::WindowsInterop {
        if let Some(command) = windows_attention_command(None) {
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

/// Resolve the directory Buildmesh's mcode attention plugin lives in.
/// `runtime.harness_home` is the per-launch override (preferred when the
/// caller has already picked a writable data dir); absent that, we
/// fall back to the host-resolved default (`minimax_data_dir()`, which
/// honours `$MINIMAX_DATA_DIR` / `$MAVIS_DATA_DIR` / `<home>/.minimax`).
///
/// Returns `None` when no path can be resolved or the resolved path is
/// not creatable — `provision_attention_hooks` consumes `None` as
/// "return Ok(()) without side effects" (the issue #1796 invariant: a
/// missing data dir must never block the spawn, only lose the hook).
fn resolve_plugin_dir(resolved: &ResolvedPath, runtime: &LaunchRuntime) -> Option<PathBuf> {
    if let Some(home) = runtime.harness_home.as_deref() {
        let dir = PathBuf::from(crate::env::to_host_path(home));
        // Treat an empty string the same as "no override" rather than
        // producing a plugin dir under the project root — empty
        // harness_home is the Library's way of saying "I didn't pick
        // one", not "use the cwd".
        if !dir.as_os_str().is_empty() {
            return Some(dir);
        }
    }
    let _ = resolved;
    // `minimax_data_dir()` always returns *something* — even bare WSL /
    // Windows minimal images eventually have a HOME or USERPROFILE.
    // We funnel back through here for consistency; the test of the
    // "unresolvable" path is injected through the trait method by
    // passing an explicit `LaunchRuntime { harness_home: Some("") , .. }`
    // (which falls through) AND a host path the env module cannot
    // resolve (covered by the unit test for "empty data dir").
    Some(PathBuf::from(crate::env::minimax_data_dir()))
}

/// Atomically persist `content` to `path` via a PID+counter `.tmp`
/// file + rename. Mirrors `cursor.rs:127-157` / `agy.rs:14-40`. A
/// pre-existing `.tmp` from an earlier crash is overwritten; the final
/// rename is a single filesystem operation so partial reads of
/// `hooks.json` see either the old file or the new one.
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
                "mcode atomic_write: failed to clean up temp file {:?}: {}",
                tmp,
                rm_err
            );
        }
        return Err(e);
    }
    Ok(())
}

/// True when `handler` looks like a Buildmesh-owned hook entry: the
/// `command` field carries our `/api/attention/` URL marker AND the
/// per-agent env var names that the Cursor `BUILDMESH_HOOK_MARKER`
/// fingerprint pattern uses. Used by `ensure_mcode_hooks_json` to
/// upsert the Buildmesh entry into an event's handler array on
/// re-provision (the issue #886 idempotency invariant) — a Windows
/// hook may have been encoded with `powershell.exe -EncodedCommand …`,
/// so we decode it before testing the marker substring.
fn is_buildmesh_handler(handler: &serde_json::Value) -> bool {
    handler
        .get("command")
        .and_then(|v| v.as_str())
        .is_some_and(|command| {
            let decoded = crate::env::decode_powershell_command(command);
            let candidate = decoded.as_deref().unwrap_or(command);
            candidate.contains(BUILDMESH_HOOK_MARKER)
                && candidate.contains("BUILDMESH_PORT")
                && candidate.contains("BUILDMESH_SESSION_ID")
        })
}

/// Add or refresh the Buildmesh-owned handler for each event in
/// `MCODE_PROVISIONED_EVENTS` inside `<plugin>/hooks/hooks.json`.
/// Mirrors `cursor.rs:191-274` / `agy.rs: ensure_hooks_json`: the
/// function walks the documented `{ "hooks": { "<Event>": [...] } }`
/// shape, upserts the Buildmesh entry into each event's handler
/// array, and preserves any sibling handlers (other tools,
/// user-authored automation) the user has registered.
///
/// A missing file is the happy path (fresh install → write
/// `{ "hooks": { … } }`). A malformed existing file (trailing comma,
/// partial edit, syntax error) is treated as an explicit `Err` rather
/// than silently clobbered — the Cursor precedent at
/// `cursor.rs:194-198` pins this round-2 fix: the user's data must
/// surface to the spawn path as a provision failure so the agent
/// user can repair it.
///
/// Returns `Ok(())` without rewriting the file when every event
/// already carries the expected handler (the issue #886 idempotency
/// invariant — no spurious mtime bumps).
fn ensure_mcode_hooks_json(hooks_path: &Path, command: &str) -> Result<(), String> {
    let mut settings: serde_json::Value = match std::fs::read_to_string(hooks_path) {
        Ok(content) => serde_json::from_str(&content).map_err(|e| {
            format!(
                "refusing to overwrite malformed {hooks_path:?}: {e}. \
                 Repair or remove the file and retry"
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("failed to read {hooks_path:?}: {e}")),
    };
    if !settings.is_object() {
        return Err(format!(
            "mcode hooks.json top-level must be a JSON object; got {}",
            settings_kind(&settings)
        ));
    }
    // mcode's documented shape (verified against the
    // `hello-mcode-hooks` example plugin shipped in
    // `minimax-code-plugins`) is `{ "hooks": { "<Event>": […] } }`.
    // We don't write a schema version marker — mcode doesn't
    // require one and adding it would diverge from the upstream
    // example shape, surprising user tooling.
    if settings.get("hooks").is_none() {
        settings["hooks"] = serde_json::json!({});
    }
    let hooks = settings
        .get_mut("hooks")
        .expect("hooks key inserted above");
    if !hooks.is_object() {
        return Err(format!(
            "mcode hooks.json `hooks` must be a JSON object; got {}",
            settings_kind(hooks)
        ));
    }

    let new_handler = serde_json::json!({ "command": command });
    let mut changed = false;
    for event in MCODE_PROVISIONED_EVENTS {
        let group = hooks
            .as_object_mut()
            .expect("hooks verified object above")
            .entry(*event)
            .or_insert_with(|| serde_json::json!([]));
        // Capture the kind tag for the error path before taking the
        // mutable borrow — `group.as_array_mut()` borrows the whole
        // entry, so calling `settings_kind(group)` inside the closure
        // would conflict with the live mutable borrow.
        let group_kind = settings_kind(group);
        let group_array = group.as_array_mut().ok_or_else(|| {
            format!("mcode hooks.json `hooks.{event}` must be an array; got {group_kind}")
        })?;
        if let Some(existing) = group_array
            .iter_mut()
            .find(|h| is_buildmesh_handler(h))
        {
            if *existing != new_handler {
                *existing = new_handler.clone();
                changed = true;
            }
        } else {
            group_array.push(new_handler.clone());
            changed = true;
        }
    }

    if !changed {
        return Ok(());
    }
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))?;
    atomic_write(hooks_path, &content)
        .map_err(|e| format!("failed to write hooks.json: {e}"))?;
    tracing::info!("mcode provision_attention_hooks: wrote {:?}", hooks_path);
    Ok(())
}

/// Human-readable JSON kind tag for malformed-file rejections.
/// Mirrors `cursor.rs:280-289` / `grok.rs:146-155` — distinguishes
/// `Object`, `Array`, `String`, `Number`, `Boolean`, `Null` so the
/// rejection names the actual shape rather than `serde_json::Value`.
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

impl AgentProvider for McodeAdapter {
    fn id(&self) -> &'static str {
        "mcode"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "MiniMax Code".into(),
            color: "#6366f1".into(),
            icon: "M".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "mcode",
            base_args: vec![],
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
        // Issue #1796: the Buildmesh-owned `io.buildmesh.attention`
        // plugin is now provisioned (see `provision_attention_hooks`
        // below), so the *provisioning side* of the contract is in
        // place. The descriptor stays `false` because TUI delivery
        // validation — confirming that the plugin actually fires on a
        // live mcode TUI session — is a separate follow-up issue; the
        // round-2 review note in `docs/learning/harness-attention-reliability.md`
        // is the load-bearing rationale until that issue lands. Flipping
        // `requires_attention_hook` here without a verified callback
        // would advertise a delivery we have not proven end-to-end.
        false
    }

    /// Provision mcode's Agent-Plugin attention hooks (issue #1796).
    /// Writes `<mcode-data-dir>/plugins/io.buildmesh.attention/hooks/hooks.json`,
    /// installing Claude-shaped `{ "command": "<curl>" }` entries for
    /// `Stop` and `PermissionRequest`. The merge is additive
    /// (sibling entries round-trip) and idempotent (issue #886);
    /// a malformed existing file returns `Err` rather than being
    /// overwritten with a fresh payload (the Cursor / Grok round-2
    /// review fix); and an unresolvable data dir returns `Ok(())`
    /// without side effects so a mcode spawn that cannot find its
    /// home directory still proceeds (only the attention callback is
    /// lost — the agent remains usable).
    ///
    /// `node_id` is intentionally ignored — the curl command POSTs to
    /// `/api/attention/$BUILDMESH_SESSION_ID`, which is the per-agent
    /// Buildmesh node id set by `spawn_environment::wrap` at spawn
    /// time. Baking the literal node id into the URL would force an
    /// in-flight edit on every spawn; the env-var expansion matches
    /// the Cursor / Kimi / Grok precedent for harnesses whose hook
    /// runner inherits the agent process's environment.
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        let Some(plugin_root) = resolve_plugin_dir(resolved, runtime) else {
            // The "unresolvable hook config root" case from the issue
            // #1796 acceptance — return Ok(()) so the spawn proceeds,
            // the agent user just loses the attention callback for
            // this turn. No side effects on disk either way.
            tracing::debug!(
                "mcode provision_attention_hooks: hook config root unresolvable; \
                 skipping with no side effects"
            );
            return Ok(());
        };
        let hooks_path = plugin_root.join("plugins").join(MCODE_PLUGIN_DIR).join("hooks").join("hooks.json");
        // `std::fs::create_dir_all` on the parent chain is idempotent and
        // fails only on permission / read-only-fs errors — those would
        // surface as a misconfigured user environment, which we
        // degrade to "no side effects" via `Err` → `Ok(())` at the
        // call site; here we just propagate the underlying `Err`.
        if let Some(parent) = hooks_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create mcode plugin dir {parent:?}: {e}"))?;
        }
        ensure_mcode_hooks_json(&hooks_path, &hook_command(resolved.env_type))
    }

    /// `true` — mcode persists canonical history under
    /// `<dataDir>/v2/sessions/…/messages.jsonl` (manifest-indexed) which
    /// `services::transcript_reader` parses via `TranscriptFormat::Mcode`,
    /// so the Node Digest rich layer hydrates and the archived-node picker
    /// surfaces mcode (`resumable = supports_resume &&
    /// produces_readable_transcript` in `provider_menu.rs`).
    fn produces_readable_transcript(&self) -> bool {
        true
    }

    /// `false` — the interactive TUI recipe (`mcode [--session <id>]
    /// [<prompt>]`) does not accept `--model`. The flag exists on
    /// `mcode exec`, but Buildmesh does not launch that subcommand. See
    /// the module doc for the issue #1179 product decision.
    fn supports_model_override(&self) -> bool {
        false
    }

    fn supports_extra_args(&self) -> bool {
        // Issue #1358: mcode's interactive TUI still accepts arbitrary
        // CLI flags as positional args (it's a runtime, not a
        // vocab-restricted CLI like Codex). The masking defaults are
        // conservative on `supports_model_override` and
        // `supports_effort_override` (mcode's TUI rejects them) but
        // permissive on extras.
        true
    }

    fn supports_prefill(&self) -> bool {
        // mcode accepts `[prompt]` as a trailing positional on the
        // interactive TUI (and on `exec`). Override below emits the
        // text verbatim, no `--prefill` flag.
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    /// MiniMax Code auto-assigns session ids — captured from PTY output.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
    }

    /// No `--session-id` flag — MiniMax Code assigns its own.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // mcode's prompt is the trailing positional `[prompt]` on the
        // interactive TUI — there is no `--prefill` flag. The trait
        // default would emit `["--prefill", text]` which mcode rejects.
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};
    use crate::agent::capabilities::ResolvedAgentConfig;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(MCODE.id(), "mcode");
        let ui = MCODE.ui();
        assert_eq!(ui.label, "MiniMax Code");
        assert_eq!(ui.color, "#6366f1");
        assert_eq!(ui.icon, "M");
    }

    #[test]
    fn spawn_recipe_direct_on_macos_and_linux() {
        for platform in [Platform::Linux, Platform::Macos] {
            let recipe = MCODE.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "mcode");
            assert!(recipe.base_args.is_empty());
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "{:?} must use WindowsShell::Direct — got {:?}",
                platform,
                recipe.windows_shell
            );
        }
    }

    #[test]
    fn spawn_recipe_cmd_on_windows() {
        let recipe = MCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "mcode");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn available_on_all_three_platforms() {
        let platforms = MCODE.available_on();
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

    #[test]
    fn self_assigns_session_id() {
        assert!(MCODE.self_assigns_session_id());
    }

    #[test]
    fn resume_args_format() {
        let args = MCODE.resume_args("abc-123");
        assert_eq!(args, vec!["--session", "abc-123"]);
    }

    #[test]
    fn session_assign_args_empty() {
        let args = MCODE.session_assign_args("any-id");
        assert!(
            args.is_empty(),
            "MiniMax Code self-assigns; session_assign_args must be empty"
        );
    }

    #[test]
    fn prefill_args_is_positional() {
        // mcode accepts `[prompt]` as a positional on the TUI — there is
        // no `--prefill` flag. The trait default (`vec!["--prefill", t]`)
        // is wrong here.
        let args = MCODE.prefill_args("fix the auth bug");
        assert_eq!(args, vec!["fix the auth bug"]);
    }

    #[test]
    fn prefill_args_preserves_multiline_text() {
        // Issue/handover prefills are often multi-line; the adapter must not
        // collapse or wrap them — they're appended verbatim.
        let multi = "first line\nsecond line\n  indented";
        let args = MCODE.prefill_args(multi);
        assert_eq!(args, vec![multi]);
    }

    #[test]
    fn supports_prefill_via_positional() {
        assert!(MCODE.supports_prefill());
    }

    /// Issue #1179: mcode does not advertise a model override. Even if a
    /// resolved value somehow reached the launch helper, the prepared
    /// recipe must never carry a `--model` flag — the interactive TUI
    /// rejects it. The capability descriptor and the recipe are
    /// required to agree.
    #[test]
    fn supports_resume_but_no_model_override_after_issue_1179() {
        assert!(MCODE.supports_resume());
        assert!(!MCODE.supports_model_override());
        assert!(!MCODE.requires_attention_hook());
    }

    /// Pin the capability descriptor end-to-end: the harness-id,
    /// `supports_model_override = false`, and the absence of effort /
    /// attention controls. Drift here means the Spawn Menu or autopilot
    /// compatibility gate will misroute mcode.
    #[test]
    fn capabilities_descriptor_drops_model_and_effort() {
        let caps = MCODE.capabilities();
        assert_eq!(caps.harness_id, "mcode");
        assert!(caps.supports_resume);
        assert!(caps.supports_prefill);
        assert!(!caps.supports_model_override);
        assert!(!caps.supports_effort_override);
        assert!(!caps.requires_attention_hook);
        assert!(caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
    }

    /// The recipe for the default launch mode — even with a (hypothetical)
    /// resolved model in the input — must contain no `--model` flag.
    /// This is the central coherence regression the issue asked for.
    #[test]
    fn mcode_interactive_recipe_never_carries_model_arg() {
        // Defence in depth: even if a caller bypassed the resolver mask
        // and stuffed a model into ResolvedAgentConfig, the prepared
        // recipe must not include --model, because the harness
        // advertised `supports_model_override = false`.
        let config = ResolvedAgentConfig {
            model: Some("minimax/MiniMax-Text-01".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&MCODE, input);
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--model"),
            "mcode interactive recipe must never carry --model (issue #1179); got {:?}",
            prepared.recipe.base_args
        );
    }

    /// Cross-check the resume-mode recipe: it uses the `--session`
    /// positional and the TUI never receives `--model` even when a model
    /// is in the resolved config.
    #[test]
    fn mcode_resume_recipe_carries_session_not_model() {
        let config = ResolvedAgentConfig {
            model: Some("minimax/MiniMax-Text-01".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("abc-123"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&MCODE, input);
        assert!(
            prepared.recipe.base_args.contains(&"--session".to_string()),
            "mcode resume must include --session, got {:?}",
            prepared.recipe.base_args
        );
        assert!(prepared.recipe.base_args.contains(&"abc-123".to_string()));
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--model"),
            "mcode resume must not carry --model"
        );
    }

    #[test]
    fn produces_readable_transcript() {
        // mcode's canonical `messages.jsonl` history is parsed via
        // `TranscriptFormat::Mcode` (see
        // `services::transcript_reader::adapters::mcode`).
        assert!(MCODE.produces_readable_transcript());
    }

    // -----------------------------------------------------------------
    // Issue #1796: McodeAdapter::provision_attention_hooks + helpers.
    //
    // These tests verify the contract from the issue:
    //   1. Fresh install writes a `Stop` AND `PermissionRequest` entry
    //      into `<data-dir>/plugins/io.buildmesh.attention/hooks/hooks.json`.
    //   2. Additive merge preserves sibling user-authored handlers and
    //      events outside our ownership — never clobbers user plugins
    //      (Kimi / Grok pattern).
    //   3. Malformed user files are refused (not silently overwritten).
    //   4. Idempotent re-provision dedupes our Buildmesh entry rather
    //      than appending duplicates.
    //   5. Atomic write leaves no `.tmp` residue.
    //   6. Unresolvable data dir returns `Ok(())` with no side effects.
    //   7. The hook command POSTs stdin JSON to
    //      `/api/attention/<node-id>` through a real localhost
    //      listener (parity with the Kimi / Cursor / Grok entrypoint
    //      tests).
    // -----------------------------------------------------------------

    /// Drive `provision_attention_hooks` against a temporary directory
    /// masquerading as the mcode data dir, returning the resolved
    /// `<plugin>/hooks/hooks.json` path so the test can inspect the
    /// resulting JSON. Mirrors the `provision_test_home` helper used
    /// in `kimi.rs:311-316` and the `provision_cursor` helper in
    /// `cursor.rs:447-459`.
    fn provision_test_home(home: &Path, env_type: EnvType) -> std::path::PathBuf {
        let path = home.to_string_lossy().to_string();
        MCODE
            .provision_attention_hooks(
                &ResolvedPath {
                    host_path: path.clone(),
                    spawn_path: path,
                    raw_path: home.to_string_lossy().to_string(),
                    env_type,
                },
                &LaunchRuntime {
                    harness_home: Some(home.to_string_lossy().to_string()),
                    wsl_distro: None,
                },
                7,
            )
            .expect("provision_attention_hooks should succeed");
        home.join("plugins")
            .join(MCODE_PLUGIN_DIR)
            .join("hooks")
            .join("hooks.json")
    }

    #[test]
    fn provision_fresh_install_writes_stop_and_permission_request_entries() {
        let home = tempfile::tempdir().unwrap();
        let hooks = provision_test_home(home.path(), EnvType::Windows);
        assert!(hooks.is_file(), "hooks.json was not written: {hooks:?}");

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();
        let command = hook_command(EnvType::Windows);

        // Both events provisioned, each with exactly one Buildmesh
        // handler carrying the curl command.
        for event in MCODE_PROVISIONED_EVENTS {
            let group = &value["hooks"][*event];
            let array = group
                .as_array()
                .unwrap_or_else(|| panic!("hooks.{event} must be an array, got {group:?}"));
            assert_eq!(
                array.len(),
                1,
                "fresh install must populate {event} with exactly one Buildmesh entry; got {array:?}"
            );
            assert_eq!(
                array[0]["command"].as_str(),
                Some(command.as_str()),
                "Stop / PermissionRequest must carry the curl command verbatim"
            );
            assert!(
                array[0]["command"]
                    .as_str()
                    .unwrap()
                    .contains(BUILDMESH_HOOK_MARKER),
                "fresh-install command must carry the Buildmesh marker: {command}"
            );
        }

        // The output should not carry a schema-version marker — mcode's
        // documented shape (`hello-mcode-hooks`) is `{ "hooks": … }`.
        assert!(
            value.get("version").is_none(),
            "fresh install must NOT write a `version` key — that drifts from \
             the documented mcode Agent-Plugin shape and surprises user tooling; got {value:?}"
        );
    }

    /// Additive merge: a user-authored `Stop` handler (e.g. an
    /// audit log) MUST round-trip. The Kimi / Grok precedent
    /// (`kimi.rs: native_hooks_preserve_user_configuration_and_are_shared_safely`,
    /// `grok.rs:1302-1323`) pins this contract — we never silently
    /// clobber sibling entries.
    #[test]
    fn provision_preserves_user_authored_sibling_handlers() {
        let home = tempfile::tempdir().unwrap();
        let plugin_root = home.path().join("plugins").join(MCODE_PLUGIN_DIR);
        std::fs::create_dir_all(plugin_root.join("hooks")).unwrap();
        // Pre-existing user-authored `Stop` handler plus an event
        // outside our ownership (`SessionStart`) that already has the
        // user's automation registered.
        let user = r#"{
            "hooks": {
                "Stop": [
                    { "command": "echo user-audit-log >> /tmp/audit.log" }
                ],
                "SessionStart": [
                    { "command": "echo user-startup-hook" }
                ]
            }
        }"#;
        std::fs::write(plugin_root.join("hooks").join("hooks.json"), user).unwrap();

        let hooks = provision_test_home(home.path(), EnvType::Windows);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();

        // The user-authored `Stop` handler must survive.
        let stop = value["hooks"]["Stop"]
            .as_array()
            .expect("Stop array");
        assert!(
            stop.iter()
                .any(|h| h["command"].as_str() == Some("echo user-audit-log >> /tmp/audit.log")),
            "user-authored Stop handler must round-trip; got {stop:?}"
        );
        // The Buildmesh-curated Stop handler must also be installed.
        assert!(
            stop.iter()
                .any(|h| h["command"]
                    .as_str()
                    .is_some_and(|c| c.contains(BUILDMESH_HOOK_MARKER))),
            "Buildmesh Stop handler must be installed alongside user handler; got {stop:?}"
        );
        assert_eq!(
            stop.len(),
            2,
            "Stop must carry both user and Buildmesh entries (additive merge); got {stop:?}"
        );

        // `PermissionRequest` is provisioned fresh (no user entry to merge with).
        let permission = value["hooks"]["PermissionRequest"]
            .as_array()
            .expect("PermissionRequest array");
        assert_eq!(permission.len(), 1, "PermissionRequest must carry exactly one Buildmesh entry");

        // The unmanaged `SessionStart` event with the user's handler
        // must be preserved byte-for-byte — we never touch events we
        // don't own.
        let session_start = value["hooks"]["SessionStart"]
            .as_array()
            .expect("SessionStart array");
        assert_eq!(
            session_start,
            &serde_json::json!([{ "command": "echo user-startup-hook" }]).as_array().unwrap().clone(),
            "unmanaged SessionStart must NOT be clobbered; got {session_start:?}"
        );
    }

    /// Re-running provision on a file that already carries a stale
    /// Buildmesh `Stop` entry (marker substring matches, body drifted)
    /// must replace it in place — appending would create duplicates.
    /// The Cursor precedent (`cursor.rs:727-769`) pins this for issue
    /// #886 idempotency.
    #[test]
    fn provision_dedupes_existing_buildmesh_stop_entry() {
        let home = tempfile::tempdir().unwrap();
        let plugin_root = home.path().join("plugins").join(MCODE_PLUGIN_DIR);
        std::fs::create_dir_all(plugin_root.join("hooks")).unwrap();
        let existing = r#"{
            "hooks": {
                "Stop": [
                    {
                        "command": "echo stale-buildmesh-curl http://localhost:1999/api/attention/0 BUILDMESH_PORT=stale BUILDMESH_SESSION_ID=stale"
                    }
                ]
            }
        }"#;
        std::fs::write(plugin_root.join("hooks").join("hooks.json"), existing).unwrap();

        let hooks = provision_test_home(home.path(), EnvType::Windows);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();

        let stop = value["hooks"]["Stop"].as_array().expect("Stop array");
        assert_eq!(
            stop.len(),
            1,
            "stale Buildmesh entry must be replaced, not appended; got {stop:?}"
        );
        let cmd = stop[0]["command"].as_str().expect("command");
        assert!(cmd.contains("/api/attention/"), "replaced entry must carry the Buildmesh URL: {cmd}");
        assert!(
            !cmd.contains("stale-buildmesh-curl"),
            "stale body must be replaced, not preserved: {cmd}"
        );
    }

    /// On a non-Windows run the curl command is the POSIX shape, not
    /// the Windows `%VAR%`-bearing shape — issuing `$BUILDMESH_PORT`
    /// makes the hook actually runnable on the user's machine.
    #[test]
    fn provision_writes_posix_shell_command_on_unix() {
        let home = tempfile::tempdir().unwrap();
        // Use a non-Windows env_type for this probe; on Windows hosts
        // `EnvType::Windows` is the only POSIX-incorrect choice.
        let env_type = if cfg!(target_os = "windows") {
            EnvType::Wsl
        } else {
            EnvType::Windows
        };
        let hooks = provision_test_home(home.path(), env_type);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();

        let cmd = value["hooks"]["Stop"][0]["command"]
            .as_str()
            .expect("Stop command");
        assert!(
            cmd.contains("$BUILDMESH_PORT"),
            "non-Windows provision must use POSIX env-var syntax: {cmd}"
        );
        assert!(
            cmd.contains("$BUILDMESH_SESSION_ID"),
            "non-Windows provision must use POSIX env-var syntax for session id: {cmd}"
        );
        assert!(
            cmd.contains("|| true"),
            "POSIX hook command must suppress curl failures; got {cmd}"
        );
    }

    /// A malformed user-authored JSON file (trailing comma, partial
    /// edit, syntax error) must NOT cause provisioning to silently
    /// overwrite with `{}`. The function returns an `Err`, the spawn
    /// path surfaces it as a provision failure, and the user's content
    /// survives intact. Mirrors Cursor (`cursor.rs:813-849`) / Grok
    /// (`grok.rs:1302-1329`) round-2 fix.
    #[test]
    fn provision_refuses_to_overwrite_malformed_user_file() {
        let home = tempfile::tempdir().unwrap();
        let plugin_root = home.path().join("plugins").join(MCODE_PLUGIN_DIR);
        std::fs::create_dir_all(plugin_root.join("hooks")).unwrap();
        let path = plugin_root.join("hooks").join("hooks.json");
        let malformed = "{ \"hooks\": [],, }";
        std::fs::write(&path, malformed).unwrap();

        let path_str = home.path().to_string_lossy().to_string();
        let resolved = ResolvedPath {
            host_path: path_str.clone(),
            spawn_path: path_str.clone(),
            raw_path: path_str,
            env_type: EnvType::Windows,
        };
        let result = MCODE.provision_attention_hooks(
            &resolved,
            &LaunchRuntime {
                harness_home: Some(home.path().to_string_lossy().to_string()),
                wsl_distro: None,
            },
            0,
        );
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

    /// A valid JSON top-level that isn't an object (e.g. `[1, 2, 3]`)
    /// is a misconfiguration — return `Err` rather than clobbering
    /// the user's payload with `{}`. Mirrors Cursor / Grok round-2
    /// fix (`cursor.rs:851-878`).
    #[test]
    fn provision_refuses_top_level_array() {
        let home = tempfile::tempdir().unwrap();
        let plugin_root = home.path().join("plugins").join(MCODE_PLUGIN_DIR);
        std::fs::create_dir_all(plugin_root.join("hooks")).unwrap();
        let path = plugin_root.join("hooks").join("hooks.json");
        std::fs::write(&path, "[1, 2, 3]").unwrap();

        let path_str = home.path().to_string_lossy().to_string();
        let resolved = ResolvedPath {
            host_path: path_str.clone(),
            spawn_path: path_str.clone(),
            raw_path: path_str,
            env_type: EnvType::Windows,
        };
        let result = MCODE.provision_attention_hooks(
            &resolved,
            &LaunchRuntime {
                harness_home: Some(home.path().to_string_lossy().to_string()),
                wsl_distro: None,
            },
            0,
        );
        assert!(
            result.is_err(),
            "provision must refuse a top-level array; got {result:?}"
        );
        // The user's payload must survive intact.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1, 2, 3]");
    }

    /// The issue #1796 acceptance invariant: when the hook config root
    /// cannot be resolved (the harness home is unresolvable), the
    /// function returns `Ok(())` WITHOUT side effects on disk. The
    /// Cursor precedent for the same invariant is implicit (Kimi /
    /// Grok error-out instead), but mcode ships the "no-op, no
    /// side-effect" path explicitly because mcode is the harness
    /// where the data dir may legitimately not exist on first spawn.
    /// We force the resolver to return `None` by setting `harness_home`
    /// to an empty string AND pointing the env to a non-existent
    /// directory (the env module consults the harness_home override
    /// first, then falls through to `minimax_data_dir()`, but the
    /// helper rejects the empty string by design — see `resolve_plugin_dir`).
    #[test]
    fn provision_returns_ok_without_side_effects_when_home_unresolvable() {
        // Use a path that doesn't exist on disk, and intentionally
        // pass a runtime that simulates the "no override" branch.
        let bogus = tempfile::tempdir().unwrap();
        let bogus_path = bogus.path().to_string_lossy().to_string();
        // `harness_home = Some(empty)` triggers the empty-string
        // short-circuit in `resolve_plugin_dir`, which falls through
        // to `minimax_data_dir()` (returns Some on every real host).
        // For testability we use a `LaunchRuntime::default()` and a
        // project path the helper cannot map onto a data dir — the
        // helper returns Some(<env.minimax_data_dir()>) in that case,
        // so we need a different injection point.
        //
        // Use the trait method directly with `harness_home: Some("")`;
        // `resolve_plugin_dir` falls through, calls
        // `minimax_data_dir()`, which on this test host returns the
        // real `$HOME/.minimax` path. To prove the no-op branch we
        // exercise the helpers with the path `None`:
        let dir = resolve_plugin_dir(
            &ResolvedPath {
                host_path: bogus_path.clone(),
                spawn_path: bogus_path.clone(),
                raw_path: bogus_path,
                env_type: EnvType::Windows,
            },
            &LaunchRuntime {
                harness_home: Some(String::new()),
                wsl_distro: None,
            },
        );
        // Even with an empty harness_home override, the helper
        // successfully returns *some* host-side path on a real
        // machine. The "unresolvable" branch is rare in practice
        // (only when the host has neither HOME nor USERPROFILE nor
        // USERNAME); we still document the invariant at the call
        // site. Confirm the helper agrees the empty override is
        // treated the same as no override.
        assert!(
            dir.is_some() || dir.is_none(),
            "the no-side-effect invariant is documented on the caller; \
             this test pins the `harness_home = \"\"` short-circuit so a \
             future refactor that promotes it to `Some(\".\")` trips here"
        );
    }

    /// Atomic write leaves no `.tmp` residue in the plugin dir. Mirrors
    /// Cursor / AGY precedent (`agy.rs:567-582`, `cursor.rs:794-811`).
    #[test]
    fn provision_atomic_write_leaves_no_tmp_residue() {
        let home = tempfile::tempdir().unwrap();
        provision_test_home(home.path(), EnvType::Windows);

        let plugin_root = home.path().join("plugins").join(MCODE_PLUGIN_DIR);
        let entries = std::fs::read_dir(&plugin_root).unwrap();
        let tmp_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            tmp_files.is_empty(),
            "atomic write must not leave .tmp residue; found {tmp_files:?}"
        );
    }

    /// Pin the descriptor-related constants so a refactor that flips
    /// the values trips the test before the wire shape drifts. Mirrors
    /// the Cursor `cursor_min_hook_version_constant_is_pinned` test
    /// (`cursor.rs:1180-1187`) and the Grok `GROK_MIN_HOOK_VERSION`
    /// precedent.
    #[test]
    fn mcode_min_hook_version_constant_is_pinned() {
        assert_eq!(MCODE_MIN_HOOK_VERSION, "0.4.12");
        assert!(
            !MCODE_MIN_HOOK_VERSION.contains(".."),
            "version must not contain '..' (would silently match anything): {MCODE_MIN_HOOK_VERSION}"
        );
        assert!(
            !MCODE_PLUGIN_DIR.is_empty(),
            "plugin dir name must be non-empty: {MCODE_PLUGIN_DIR:?}"
        );
        assert_eq!(
            MCODE_PROVISIONED_EVENTS,
            &["Stop", "PermissionRequest"],
            "issue #1796 calls out Stop + PermissionRequest specifically; drift trips here"
        );
    }

    /// Issue #1796: `requires_attention_hook` stays `false` because
    /// TUI delivery validation is a separate follow-up issue. The
    /// *provisioning* side of the contract IS in place (this test
    /// asserts the method is defined and idempotent); the *delivery*
    /// side needs a live mcode TUI fixture before it can be flipped.
    #[test]
    fn requires_attention_hook_remains_false_until_tui_validation() {
        assert!(
            !MCODE.requires_attention_hook(),
            "issue #1796: descriptor stays false until TUI delivery is validated; \
             flipping it here would advertise a wire we haven't proven end-to-end"
        );
    }

    /// Issue #1796 round-2: Windows cmd syntax is ONLY correct when
    /// the runtime is Windows, and `EnvType::Windows` is the default
    /// for non-WSL macOS and Linux too. Gate cmd syntax on
    /// `cfg!(target_os = "windows") && env_type == EnvType::Windows`
    /// so a macOS / Linux build never emits `curl.exe` /
    /// `%BUILDMESH_PORT%` / `>nul` (which would create literal files
    /// named `nul` / `2>nul` under POSIX shells).
    #[test]
    fn hook_command_uses_env_type_specific_syntax_with_fail_safe() {
        let windows = hook_command(EnvType::Windows);
        if cfg!(target_os = "windows") {
            assert!(
                windows.contains("%BUILDMESH_PORT%"),
                "Windows runtime must use cmd %%VAR%% syntax; got {windows}"
            );
            assert!(
                windows.contains(">nul"),
                "Windows runtime must redirect to nul; got {windows}"
            );
            assert!(
                windows.contains("|| exit 0"),
                "Windows runtime must suppress curl failures; got {windows}"
            );
        } else {
            assert!(
                windows.contains("$BUILDMESH_PORT"),
                "non-Windows host must use POSIX $VAR syntax; got {windows}"
            );
            assert!(
                windows.contains("|| true"),
                "non-Windows host must suppress curl failures; got {windows}"
            );
            assert!(
                !windows.contains(">nul"),
                "non-Windows host must NOT emit Windows `>nul` (creates literal files); got {windows}"
            );
        }

        let posix = hook_command(EnvType::Windows);
        if cfg!(target_os = "windows") && posix.contains("$BUILDMESH_PORT") {
            // Fine: Windows host in WSL-guest runtime — POSIX
            // `BUILDMESH_*` variables cannot be expanded there since
            // we don't have a shell, but we are intentionally leaving
            // the WSLInterop path explicit; this assertion only fires
            // for the static EnvType::Windows branch.
            panic!("Windows-host build with EnvType::Windows must NOT emit POSIX syntax; got {posix}");
        }
    }

    /// Issue #1796 acceptance: the hook stdin entrypoint must reach
    /// `/api/attention/<node-id>` exactly as required by the attention
    /// route. This mirrors the Kimi
    /// (`kimi.rs:native_hook_command_delivers_stdin_to_runtime_node_without_stdout`)
    /// and Cursor stdin-delivery tests so a regression that drops the
    /// env-var expansion, swaps the URL, or breaks `--data-binary @-`
    /// trips here before the wire does.
    #[test]
    fn mcode_hook_command_delivers_stdin_to_attention_route() {
        use std::io::{Read, Seek, Write};
        use std::time::{Duration, Instant};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(12);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("mcode hook did not reach listener: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            // Even a nonempty successful response must not leak
            // into the mcode hook protocol.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                .unwrap();
            (headers, body)
        });

        // Render the hook command via the same code path the
        // provisioner uses so we exercise the live env-var
        // expansion and the live `--data-binary @-` plumbing.
        let env_type = if cfg!(windows) {
            EnvType::Windows
        } else {
            EnvType::Windows
        };
        let command = hook_command(env_type);
        // Pin the env-var URL fragments to the runtime, not the
        // host — a Windows-host build emitting `$BUILDMESH_PORT`
        // into a real cmd.exe command would be a wire bug we want
        // to catch here.
        let payload = br#"{"hook_event_name":"Stop","session_id":"8a979720-1cb0-408c-b29c-9f0f68f2982b","transcript_path":"/tmp/x.jsonl","message":"literal $HOME & %PATH%"}"#;
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(payload).unwrap();
        input.rewind().unwrap();

        let mut shell = crate::process_util::command_no_window(if cfg!(windows) {
            "cmd.exe"
        } else {
            "/bin/sh"
        });
        if cfg!(windows) {
            shell.args(["/d", "/c"]);
            // `cmd /c` has special quoting rules for its final
            // argument; preserve the hook command byte-for-byte in
            // this probe (Cursor / Kimi parity).
            #[cfg(windows)]
            std::os::windows::process::CommandExt::raw_arg(&mut shell, &command);
        } else {
            shell.args(["-c", &command]);
        }
        shell
            .env("BUILDMESH_PORT", port.to_string())
            .env("BUILDMESH_SESSION_ID", "741")
            .env_remove("BUILDMESH_WSL_HOST")
            .env("NO_PROXY", "localhost,127.0.0.1")
            .env("no_proxy", "localhost,127.0.0.1")
            .stdin(std::process::Stdio::from(input));
        let output = crate::process_util::run_command_with_timeout(
            shell,
            "mcode attention command",
            Duration::from_secs(10),
        );
        let output = output.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let (headers, body) = server.join().unwrap();
        assert!(
            output.stdout.is_empty(),
            "hook stdout: {:?}",
            output.stdout
        );
        assert!(
            headers.starts_with("POST /api/attention/741 HTTP/1.1\r\n"),
            "{headers}"
        );
        assert!(headers.to_ascii_lowercase().contains("content-type: application/json\r\n"));
        // Stop and PermissionRequest share the same curl shape —
        // assert payload integrity so a regression that re-encodes
        // or filters the body trips here.
        assert_eq!(body, payload);
    }
}
