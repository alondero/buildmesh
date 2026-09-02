//! Grok Code provider adapter — xAI's full-screen interactive coding agent,
//! installed on PATH as a single `grok` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. The non-interactive `-p <prompt>` mode
//! exists but is *not* used here: the #914 prototype verified that Buildmesh's
//! PTY backend (ConPTY on Windows, native PTY on macOS/Linux) fully supports
//! Grok's TUI, so we launch in interactive mode everywhere.
//!
//! **Session IDs** follow ADR-0024. Fresh spawns mint a UUID and pass
//! `--session-id <uuid>` (Grok 1.0.5: create-only; errors if the ID already
//! exists under the cwd). Resume uses `--resume <id>`. `--continue` exists
//! but is unused — auto-resume always passes the stored id explicitly.
//!
//! **Prefill** is the trailing positional `[PROMPT]` on the interactive TUI
//! (`grok "fix the bug"`). There is no `--prefill` flag; `-p`/`--single` is
//! headless (print and exit) and is not used here.
//!
//! **Model override** uses `-m <model-id>` / `--model <model-id>` (`grok
//! --help` advertises the long form; the adapter emits it). Grok Code
//! accepts Buildmesh-level model overrides passed via the spawn path —
//! the `--model <model>` flag is forwarded to the Grok CLI, which then
//! runs that model for the invocation (overriding the harness's
//! `[model.<name>]` default in `~/.grok/config.toml` for that one
//! session). Custom models are configured via `[model.<name>]` blocks
//! in `~/.grok/config.toml`; Buildmesh does not manage those.
//!
//! **Attention hooks** (issue #1282). Grok's hook surface
//! (`~/.grok/docs/user-guide/10-hooks.md`) exposes both `Notification`
//! (idle_prompt / permission_prompt / task_complete) and `Stop` events
//! with a native HTTP handler — no curl wrapper. We inject a single
//! global hook file at `~/.grok/hooks/buildmesh-attention.json`,
//! always trusted. Project-local `.grok/hooks/` requires folder trust
//! (`/hooks-trust` or `--trust`); `--trust` is documented but **not**
//! in `grok --help` 1.0.5, so there is no flag we can pass at spawn to
//! unblock the project path. The URL expands `$BUILDMESH_PORT` and
//! `$BUILDMESH_SESSION_ID` at hook runner time (set per-agent by
//! `spawn_environment`), so the file is reusable across nodes — every
//! runner carries the session id and port from its own environment.
//!
//! **Effort override** uses the documented `--effort` alias of
//! `--reasoning-effort <level>` (`grok --help`). Grok accepts the seven
//! canonical levels (`none | minimal | low | medium | high | xhigh | max`)
//! across the interactive TUI and headless mode — see
//! `docs/learning/grok-harness-capabilities.md` and the
//! `GROK_EFFORT_ALLOWED` constant in `agent::capabilities`. The trait
//! default `effort_args` already emits `["--effort", value]`, so no
//! override is required for the recipe shape — only the closed vocabulary
//! needs advertising via `effort_control()`. Issue #1280.
//!
//! **Shell wrapping**: `grok` is a native binary on all platforms (not a
//! `.cmd` shim), so `WindowsShell::Direct` is correct everywhere — matching
//! the AGY adapter pattern.

use crate::agent::capabilities::{EffortControlKind, GROK_EFFORT_ALLOWED};
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::path::Path;

pub struct GrokAdapter;
pub static GROK: GrokAdapter = GrokAdapter;

/// Minimum Grok release the Buildmesh hook integration has been
/// validated against (issue #1366). The detailed event contract
/// (`Notification` with `notificationType` =
/// `permission_prompt` / `idle_prompt` / `task_complete`, `Stop`
/// with `reason`, `$VAR` expansion in `url`) is documented in the
/// bundled `~/.grok/docs/user-guide/10-hooks.md` reference shipped
/// with 1.0.5 but is *not* fully exposed on the public xAI docs.
/// Pinned here so the `AttentionCapability` descriptor advertises
/// the supported version and a release that drops any of these
/// fields surfaces a visible capability/health change.
///
/// Note: the constant name carries `_MIN_` for descriptor-shape
/// compatibility (the `AttentionCapability::min_version` field
/// declares "minimum" semantics). We do **not** enforce a strict
/// `>=` comparison at runtime — semver isn't wired through the hook
/// surface, and `>=` would require either a `semver::Version`
/// dependency or a hand-rolled tuple parser. The path-coverage test
/// pins that the descriptor advertises the exact pin; treat runtime
/// version drift as a documented upgrade story rather than a gate.
pub const GROK_MIN_HOOK_VERSION: &str = "1.0.5";

/// The HTTP URL the Grok hook runner POSTs the event envelope to. The
/// `$BUILDMESH_PORT`, `$BUILDMESH_SESSION_ID`, and `$BUILDMESH_HOOK_TOKEN`
/// tokens expand at hook-run time (set per-agent by
/// `spawn_environment`) — the file is therefore reusable across nodes
/// without rewriting the literal values. The Grok docs
/// (`~/.grok/docs/user-guide/10-hooks.md`, "Using variables in
/// `command` and `url` fields") explicitly state that both `command`
/// and `url` support `${VAR}` and `$VAR` expansion.
///
/// The trailing `?token=$BUILDMESH_HOOK_TOKEN` carries the
/// runtime-scoped token (issue #1366) so the attention route can
/// reject non-Buildmesh callbacks even on a same-box collision. The
/// marker predicate in `is_buildmesh_handler` matches on the
/// canonical `/api/attention/` + `BUILDMESH_PORT` anchors, so this
/// URL change does not disturb the additive merge.
const HOOK_URL: &str = "http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID?token=$BUILDMESH_HOOK_TOKEN";

/// File name we own under `~/.grok/hooks/`. Namespaced so a user's
/// existing hooks (and any future Buildmesh hook with a different
/// shape) never collide on the always-trusted global path.
const HOOK_FILE: &str = "buildmesh-attention.json";

/// Resolve the always-trusted global Grok hooks directory. Mirrors
/// Codex's `native_codex_home_from` pattern: `USERPROFILE` on Windows,
/// `HOME` on Unix. Returns `<home>/.grok/hooks`. The hook file lives
/// here — not under the project cwd — because project-local Grok
/// hooks require folder trust and `--trust` is not in `grok --help`.
fn grok_home() -> Result<std::path::PathBuf, String> {
    let key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| format!("could not resolve grok home — ${key} is unset"))
        .map(|p| p.join(".grok").join("hooks"))
}

/// Atomically persist `content` to `path` via `tempfile::NamedTempFile +
/// persist`. The Codex/AGY precedents both pin this pattern
/// (`codex.rs:998-1007`); the Python `os.replace` analogue. A
/// crash mid-rename or pre-rename leaves the canonical file
/// untouched and the orphan `.tmp` is the only residue — visible at
/// `dir.parent().join("name.*.tmp")` until the OS reclaims it.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map(|_| ()).map_err(|error| error.error)
}

/// Human-readable JSON kind tag for error messages. The codec
/// distinguishes `Object`, `Array`, `String`, `Number`, `Boolean`,
/// `Null`. Non-`Object` values produce the right rejection: the
/// path-coverage test pins the rejection for an Array.
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

/// Marker predicate: a Grok HTTP handler is Buildmesh-owned when its
/// `url` carries both anchors. URL-anchored rather than on a custom
/// `statusMessage` field because Grok's parser tolerance for unknown
/// handler fields is undocumented; the `url` is guaranteed preserved
/// byte-for-byte. Substring match (not strict equality) so future
/// refactors (adding a `?token=` query param, etc.) keep the merge
/// stable — only the canonical anchors matter.
fn is_buildmesh_handler(handler: &serde_json::Value) -> bool {
    handler
        .get("url")
        .and_then(|v| v.as_str())
        .is_some_and(|url| url.contains("/api/attention/") && url.contains("BUILDMESH_PORT"))
}

/// Add or update the Buildmesh-owned handler in a single event's
/// matcher-group array. Returns `true` when something changed
/// (caller decides whether to rewrite the document). User-authored
/// sibling handlers, matcher groups, and the array itself are
/// preserved across the merge — only the Buildmesh entry is created
/// (one per event) or replaced (handler-shape drift).
fn merge_buildmesh_handler(
    groups: &mut Vec<serde_json::Value>,
    new_handler: &serde_json::Value,
) -> bool {
    for group in groups.iter_mut() {
        let Some(handlers) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        if let Some(index) = handlers.iter().position(is_buildmesh_handler) {
            if handlers[index] != *new_handler {
                handlers[index] = new_handler.clone();
                return true;
            }
            // Already correct → no-op. Caller skips the rewrite.
            return false;
        }
    }
    // No Buildmesh entry yet → append a fresh matcher group.
    groups.push(serde_json::json!({ "hooks": [new_handler.clone()] }));
    true
}

/// Write the attention hook file. Idempotent — preserves ALL existing
/// hooks the user authored (sibling matcher groups, sibling handler
/// fields, additional events), atomically (named temp file +
/// `persist`), and only rewrites when the Buildmesh entry actually
/// changes (so re-running over a fresh write is a no-op on mtime and
/// bytes — the spawn-path idempotency invariant from issue #886).
/// Uses Grok's native HTTP handler type — the runner POSTs the event
/// envelope directly as JSON (camelCase: `hookEventName`,
/// `sessionId`, …), no curl wrapper.
///
/// `Notification` is wired with no matcher field at all (Grok docs
/// match-all behaviour); the matcher-group array is the array of
/// matcher groups, each `{"hooks":[handler]}` shaped. `Stop` has no
/// matcher either — Grok's docs warn "A matcher on `Stop` or
/// `UserPromptSubmit` is ignored with a warning".
fn ensure_hooks_json(path: &Path) -> Result<(), String> {
    // If the file already exists, refuse to overwrite a malformed
    // user-authored payload (trailing comma, partial edit, …) — the
    // Codex pattern (`codex.rs:938`) treats parse failure as an
    // explicit `Err` so the spawn path surfaces a provision error
    // rather than silently wiping user data. A missing file is the
    // happy path (fresh install); only an existing-but-unparseable
    // file is the failure case.
    let mut settings: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).map_err(|e| {
            format!(
                "refusing to overwrite malformed {path:?}: {e}. \
                 Repair or remove the file and retry"
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({})
        }
        Err(e) => return Err(format!("failed to read {path:?}: {e}")),
    };
    // Round-2 fix (reviewer point 5): a valid JSON top-level that
    // isn't an object (e.g. an array, a string, a number, `null`)
    // is a misconfiguration — return Err instead of clobbering the
    // user's payload with `{}`. Mirrors `codex.rs:941`.
    if !settings.is_object() {
        return Err(format!(
            "{path:?}: top-level value must be a JSON object; got {}",
            settings_kind(&settings)
        ));
    }

    let new_handler = serde_json::json!({
        "type": "http",
        "url": HOOK_URL,
    });

    let settings_obj = settings
        .as_object_mut()
        .expect("settings coerced to object above");
    let hooks = settings_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "hooks.json `hooks` value must be an object".to_string())?;

    let mut changed = false;
    for event in ["Notification", "Stop"] {
        let groups = hooks_obj
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        let groups_array = groups
            .as_array_mut()
            .ok_or_else(|| format!("hooks.json event `{event}` must be an array"))?;
        changed = merge_buildmesh_handler(groups_array, &new_handler) || changed;
    }

    if !changed {
        return Ok(());
    }

    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))?;
    write_atomic(path, &content).map_err(|e| format!("failed to write hooks.json: {e}"))?;
    tracing::info!("grok provision_attention_hooks: wrote {:?}", path);
    Ok(())
}

impl AgentProvider for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Grok Code".into(),
            // Official xAI brand colour from
            // https://x.ai/legal/brand-guidelines (Feb 14, 2025).
            // Paired with the white Grok Logomark at src/assets/providers/grok.svg
            // for a high-contrast (WCAG AAA) mobile avatar chip.
            color: "#0A0A0A".into(),
            icon: "X".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "grok",
            base_args: vec![],
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        }
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn produces_readable_transcript(&self) -> bool {
        // Issue #1281: Grok Code writes per-session directories at
        // `~/.grok/sessions/<urlencoded-cwd>/<session-id>/{chat_history.jsonl,
        // updates.jsonl}` (and `summary.json`). Buildmesh's transcript
        // reader now parses both files via `TranscriptFormat::Grok`, so the
        // archived-node resume picker surfaces Grok sessions and the
        // Coordinator Node Digest rich layer hydrates them.
        true
    }

    fn requires_attention_hook(&self) -> bool {
        true
    }

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        AttentionCapability::Hook {
            events: vec![
                LifecycleKind::TurnCompleted,
                LifecycleKind::InputRequired,
                LifecycleKind::PermissionRequested,
                LifecycleKind::QuestionRequested,
            ],
            launch_mode: AttentionLaunchMode::PermissionAsk,
            trust: Some("global hook dir".into()),
            // Issue #1366: pin the Grok release the integration has
            // been validated against. A future Grok release that drops
            // `notificationType`, `$VAR` expansion, or the `Stop`
            // envelope would surface as "this is now an
            // unversioned/integration-at-risk harness" via the
            // Inspector capabilities table.
            min_version: Some(GROK_MIN_HOOK_VERSION.into()),
        }
    }

    /// Provision Buildmesh attention hooks into Grok's always-trusted
    /// global hooks directory (issue #1282). The file lives in the
    /// user's `~/.grok/hooks/`, NOT under the project cwd: project-local
    /// `.grok/hooks/` requires folder trust via `/hooks-trust` /
    /// `--trust`, and `--trust` is **not** in `grok --help` 1.0.5 —
    /// there is no spawn flag we can use to bypass the gate.
    ///
    /// The HTTP handler POSTs the event envelope to the attention
    /// endpoint with `$BUILDMESH_PORT` and `$BUILDMESH_SESSION_ID`
    /// expanded at runner time (set per-agent by `spawn_environment`).
    /// The resolved project path is intentionally unused because the global
    /// hook directory is the only trustable Grok location available here.
    fn provision_attention_hooks(
        &self,
        _resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        // Issue #1366 — mint the process-wide hook token **here**, not
        // in `spawn_environment::wrap` (which fires for every agent
        // spawn including Claude / Codex / AGY). Scoping the mint
        // to the Grok path means non-Grok agents never see the
        // process-wide token, and the route's token check below
        // correctly discriminates Grok callbacks from sibling
        // harnesses (Claude / Codex / AGY POST without `?token=` and
        // bypass the gate).
        //
        // The call is idempotent: first invocation mints; subsequent
        // spawns in the same runtime see the same value, so all
        // Grok hooks in this process share one token. The token
        // itself never appears in the JSON file — the URL template
        // stores `$BUILDMESH_HOOK_TOKEN` as a literal and the Grok
        // runner expands `$VAR` at hook-run time (the docs the
        // issue links pin this; the same mechanism is documented
        // for `BUILDMESH_PORT` / `BUILDMESH_SESSION_ID`).
        let token = crate::agent::mint_runtime_hook_token();
        tracing::info!(
            "grok provision_attention_hooks: minted runtime hook token {token}"
        );
        let dir = grok_home()?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create .grok/hooks dir: {e}"))?;
        ensure_hooks_json(&dir.join(HOOK_FILE))
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        // Interactive TUI accepts a trailing positional [PROMPT] as the
        // first turn (`grok "fix the bug"`). There is no `--prefill` flag;
        // override `prefill_args` below. Headless `-p`/`--single` is a
        // different mode (print and exit) and is not used here.
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--resume".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // Trailing positional [PROMPT] on the interactive TUI. The trait
        // default would emit `["--prefill", text]`, which grok rejects.
        vec![text.into()]
    }

    /// Grok 1.0.5 accepts `--reasoning-effort <level>` (alias `--effort`)
    /// with the seven canonical levels documented in
    /// `docs/learning/grok-harness-capabilities.md` and kept in
    /// [`GROK_EFFORT_ALLOWED`]. The trait default `effort_args` emits
    /// `["--effort", value]` — the documented alias — so no recipe-shape
    /// override is needed. Issue #1280.
    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::Closed {
            allowed: GROK_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn provision_grok(project: &Path) {
        let path = project.to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: EnvType::Windows,
        };
        GROK
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
            .unwrap();
    }

    /// Same as `provision_grok` but returns the `Result` so callers can
    /// exercise the `Err` path (the malformed-file test below asserts
    /// `Err`). The `_` variant panics on `Err` — preserved for the
    /// happy-path tests that don't care about the result.
    fn try_provision_grok(project: &Path) -> Result<(), String> {
        let path = project.to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: EnvType::Windows,
        };
        GROK.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
    }

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(GROK.id(), "grok");
        let ui = GROK.ui();
        assert_eq!(ui.label, "Grok Code");
        assert_eq!(ui.color, "#0A0A0A");
        assert_eq!(ui.icon, "X");
    }

    #[test]
    fn spawn_recipe_direct_on_all_platforms() {
        for platform in [Platform::Windows, Platform::Linux, Platform::Macos] {
            let recipe = GROK.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "grok");
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
    fn available_on_all_three_platforms() {
        let platforms = GROK.available_on();
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
    fn assigns_session_id_via_cli_flag() {
        // Grok 1.0.5 accepts `-s/--session-id <UUID>` to create a new
        // session (create-only; resume is `--resume`). ADR-0024: we mint
        // the UUID and pass it, rather than scraping PTY output.
        assert!(!GROK.self_assigns_session_id());
        assert_eq!(
            GROK.session_assign_args("550e8400-e29b-41d4-a716-446655440000"),
            vec!["--session-id", "550e8400-e29b-41d4-a716-446655440000"]
        );
    }

    #[test]
    fn resume_args_format() {
        let args = GROK.resume_args("abc-123");
        assert_eq!(args, vec!["--resume", "abc-123"]);
    }

    #[test]
    fn model_args_format() {
        let args = GROK.model_args("grok-3");
        assert_eq!(args, vec!["--model", "grok-3"]);
    }

    #[test]
    fn supports_prefill_via_positional() {
        // Interactive TUI takes a trailing [PROMPT] as the first turn
        // (`grok "fix the bug"`). There is no `--prefill` flag; emitting
        // the trait default would be rejected upstream.
        assert!(GROK.supports_prefill());
        assert_eq!(GROK.prefill_args("fix the auth bug"), vec!["fix the auth bug"]);
    }

    #[test]
    fn prefill_args_preserves_multiline_text() {
        let multi = "first line\nsecond line\n  indented";
        assert_eq!(GROK.prefill_args(multi), vec![multi]);
    }

    /// Interactive TUI prefill is the trailing positional, never
    /// `--prefill`. The table-driven coherence check accepts either
    /// shape; this pin forbids the Claude-shaped flag.
    #[test]
    fn grok_interactive_recipe_carries_positional_prefill() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("fix the auth bug in handler.rs"),
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        let args = &prepared.recipe.base_args;
        assert_eq!(
            args.last().map(String::as_str),
            Some("fix the auth bug in handler.rs"),
            "Grok prefill must be the trailing positional; got {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--prefill"),
            "Grok has no --prefill flag; got {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "-p" || a == "--single"),
            "Grok prefill must stay on the interactive TUI, not headless -p; got {args:?}"
        );
    }

    /// Fresh spawn (Assign) must pass `--session-id <uuid>` so the
    /// orchestrator owns the ID before launch. `--resume` must not appear.
    #[test]
    fn grok_assign_recipe_carries_session_id_flag() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Assign(id),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        let args = &prepared.recipe.base_args;
        assert_flag_followed_by_value(args, "--session-id", id);
        assert!(
            !args.iter().any(|a| a == "--resume"),
            "Assign must not emit --resume; got {args:?}"
        );
    }

    /// Resume keeps `--resume <id>` (not `-s`) and still appends a
    /// positional prefill as the trailing arg.
    #[test]
    fn grok_resume_recipe_keeps_resume_flag_and_positional_prefill() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("grok-3".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("01a0400a-6ac5-7d90-a1a6-b5397ff81d62"),
            config: &config,
            prefill: Some("continue from the last turn"),
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        let args = &prepared.recipe.base_args;
        assert_flag_followed_by_value(args, "--resume", "01a0400a-6ac5-7d90-a1a6-b5397ff81d62");
        assert_flag_followed_by_value(args, "--model", "grok-3");
        assert_eq!(
            args.last().map(String::as_str),
            Some("continue from the last turn")
        );
        assert!(
            !args.iter().any(|a| a == "--session-id"),
            "Resume must not also assign; got {args:?}"
        );
    }

    /// Issue #1186: pin the harness-specific model-flag shape. The
    /// table-driven `capability_recipe_coherence` only asserts *some*
    /// model flag exists in the recipe — a silent `-m` ↔ `--model`
    /// flip on the adapter would pass. This pin catches the drift
    /// before it reaches the wire.
    #[test]
    fn grok_interactive_recipe_carries_long_model_arg() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("grok-3".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        assert_flag_followed_by_value(&prepared.recipe.base_args, "--model", "grok-3");
    }

    /// Issue #1179 (mirror): end-to-end descriptor pin. The Spawn Menu,
    /// resolver, and autopilot compatibility gate all consume this
    /// descriptor — drift here means the menu misroutes Grok.
    ///
    /// Issue #1280 flipped `effort_control` from `None` to
    /// `Closed { allowed: GROK_EFFORT_ALLOWED }`. The vocabulary is the
    /// seven canonical levels documented at
    /// `docs/learning/grok-harness-capabilities.md`; the resolver mask
    /// drops anything outside it.
    ///
    /// Issue #1281: Grok now writes a transcript Buildmesh can read
    /// (`TranscriptFormat::Grok` in `services::transcript_reader`), so
    /// `produces_readable_transcript` flips to true.
    #[test]
    fn capabilities_descriptor_advertises_model_override() {
        let caps = GROK.capabilities();
        assert_eq!(caps.harness_id, "grok");
        assert!(caps.supports_resume);
        assert!(caps.supports_model_override);
        assert!(caps.supports_effort_override);
        assert!(caps.supports_prefill);
        // Issue #1282: Grok now ships attention hooks.
        assert!(caps.requires_attention_hook, "issue #1282: Grok now ships attention hooks");
        // Issue #1281: Grok's per-session chat_history.jsonl / updates.jsonl
        // are parsed via TranscriptFormat::Grok, so the archived-node picker
        // and Node Digest rich layer surface Grok.
        assert!(caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::Closed {
                allowed: GROK_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
            }
        );
    }

    /// Issue #1280: descriptor advertises the seven canonical Grok levels.
    /// Mirrors the Anthropic-style closed-vocab pin (the AGY precedent is
    /// `agy::tests::capabilities_descriptor_advertises_effort_override`).
    /// A future change that drops `none` or `max` from the list — or that
    /// silently flips the trait default back to `None` — would be caught
    /// here before reaching the Spawn Menu.
    #[test]
    fn capabilities_descriptor_advertises_effort_override() {
        let caps = GROK.capabilities();
        assert!(
            caps.supports_effort_override,
            "Grok 1.0.5 accepts --effort; descriptor must advertise it"
        );
        let allowed: Vec<String> = match &caps.effort_control {
            crate::agent::capabilities::EffortControlKind::Closed { allowed } => allowed.clone(),
            other => panic!(
                "Grok must advertise Closed-vocab effort control after #1280; got {other:?}"
            ),
        };
        let expected: Vec<String> = GROK_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            allowed, expected,
            "Grok effort vocabulary must be exactly GROK_EFFORT_ALLOWED"
        );
    }

    /// Issue #1280: when the resolver forwards an effort layer, the
    /// prepared recipe must carry `--effort <level>` (the documented alias
    /// of `--reasoning-effort`). Without an effort layer, no `--effort`
    /// flag may appear — the resolver mask and `default_prepare` together
    /// guarantee this for every adapter (issue #1179 coherence matrix),
    /// but the per-adapter pin catches future silent drift (e.g. an
    /// override that emits the long form instead of the alias).
    #[test]
    fn grok_recipe_appends_effort_arg_when_resolved() {
        use crate::agent::capabilities::{EffortControlKind, ResolvedAgentConfig, GROK_EFFORT_ALLOWED};
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        // Sanity check the adapter advertises the same vocabulary the
        // constant carries — protects a future refactor that moves the
        // vocabulary off into the constant.
        let caps = GROK.capabilities();
        let advertised: Vec<String> = match &caps.effort_control {
            EffortControlKind::Closed { allowed } => allowed.clone(),
            other => panic!("expected Closed, got {other:?}"),
        };
        let expected: Vec<String> = GROK_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect();
        assert_eq!(advertised, expected);

        // With an effort layer, the recipe carries --effort <level>.
        let config = ResolvedAgentConfig {
            model: None,
            effort: Some("high".into()),
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        assert_flag_followed_by_value(&prepared.recipe.base_args, "--effort", "high");
        // Pin the alias preference — Grok accepts both `--effort` and
        // `--reasoning-effort`; the documented alias is `--effort` and
        // Buildmesh emits it (issue #1280 acceptance criteria).
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--reasoning-effort"),
            "Grok must use the --effort alias, not the long --reasoning-effort form; \
             got {:?}",
            prepared.recipe.base_args
        );

        // Without an effort layer, no --effort flag appears at all
        // (capability mask + default_prepare are the joint guarantee).
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("continue from where we left off"),
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--effort"),
            "no effort layer must produce no --effort flag; got {:?}",
            prepared.recipe.base_args
        );
        assert!(
            !prepared
                .recipe
                .base_args
                .iter()
                .any(|a| a == "--reasoning-effort"),
            "no effort layer must produce no --reasoning-effort flag; got {:?}",
            prepared.recipe.base_args
        );
    }

    /// Issue #1280: end-to-end vocabulary pin via the resolver. A value
    /// inside the seven-level vocabulary passes the mask and reaches the
    /// recipe; a value outside (e.g. `"ultra-mega-high"`, an obvious
    /// typo) is dropped at the resolver and never reaches `default_prepare`.
    /// Mirrors the AGY precedent (`agy::tests::agy_recipe_appends_effort_arg_when_resolved`).
    #[test]
    fn grok_resolver_keeps_in_vocabulary_drops_out_of_vocabulary() {
        use crate::agent::capabilities::{
            resolve_agent_config, AgentConfigInputs, FieldInputs,
        };
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let caps = GROK.capabilities();

        // "xhigh" is in the seven-level vocabulary — must pass through.
        let inputs = AgentConfigInputs {
            model: FieldInputs::default(),
            effort: FieldInputs {
                explicit: Some("xhigh"),
                ..FieldInputs::default()
            },
        };
        let resolved = resolve_agent_config(&caps, inputs, None);
        assert_eq!(resolved.effort.as_deref(), Some("xhigh"));

        // End-to-end: the prepared recipe carries --effort xhigh.
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &resolved,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        assert!(
            prepared.recipe.base_args.windows(2).any(|w| {
                w.first().map(String::as_str) == Some("--effort")
                    && w.get(1).map(String::as_str) == Some("xhigh")
            }),
            "in-vocabulary effort must reach the recipe as --effort xhigh; got {:?}",
            prepared.recipe.base_args
        );

        // "ultra-mega-high" is not in Grok's vocabulary — must be masked out.
        let inputs = AgentConfigInputs {
            model: FieldInputs::default(),
            effort: FieldInputs {
                explicit: Some("ultra-mega-high"),
                ..FieldInputs::default()
            },
        };
        let resolved = resolve_agent_config(&caps, inputs, None);
        assert!(
            resolved.effort.is_none(),
            "out-of-vocabulary effort must be dropped at the resolver; got {:?}",
            resolved.effort
        );

        // End-to-end: a resolved-None effort produces no --effort flag.
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &resolved,
            prefill: Some("tail prompt"),
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--effort"),
            "masked-out effort must not reach the recipe; got {:?}",
            prepared.recipe.base_args
        );
        // ...but the prefill still lands — capability masking is
        // per-field, not "all or nothing".
        assert_eq!(
            prepared.recipe.base_args.last().map(String::as_str),
            Some("tail prompt"),
            "prefill must not be lost when effort is masked; got {:?}",
            prepared.recipe.base_args
        );
    }

    // -----------------------------------------------------------------
    // Attention hook injection (issue #1282)
    // -----------------------------------------------------------------

    /// End-to-end: injection writes the namespaced always-trusted global
    /// file at `<home>/.grok/hooks/buildmesh-attention.json`, with
    /// Notification (empty matcher = catch-all) and Stop entries pointing
    /// at the attention endpoint via the env-var indirection that
    /// `spawn_environment` sets per agent.
    #[test]
    fn inject_writes_notification_and_stop_hooks() {
        let temp = with_user_home_redirect();
        provision_grok(Path::new("/any"));

        let path = temp.path().join(".grok").join("hooks").join("buildmesh-attention.json");
        let content = std::fs::read_to_string(&path).expect("hook file not written");
        let value: serde_json::Value =
            serde_json::from_str(&content).expect("hook file is not valid JSON");
        let hooks = value.get("hooks").expect("hooks key missing");

        for event in ["Notification", "Stop"] {
            let url = hooks[event][0]["hooks"][0]["url"]
                .as_str()
                .unwrap_or_else(|| panic!("{event} hook missing or wrong shape: {value:#}"));
            assert!(
                url.contains("$BUILDMESH_PORT"),
                "{event} url must expand BUILDMESH_PORT at runner time: {url}"
            );
            assert!(
                url.contains("$BUILDMESH_SESSION_ID"),
                "{event} url must expand BUILDMESH_SESSION_ID at runner time: {url}"
            );
            assert!(
                url.contains("/api/attention/"),
                "{event} url must POST to the attention endpoint: {url}"
            );
            assert!(
                hooks[event][0]["hooks"][0]["type"].as_str() == Some("http"),
                "{event} must use the native http handler (no curl wrapper): {value:#}"
            );
        }
    }

    /// Re-running injection is a no-op once the file matches the
    /// expected shape — important because the spawn path calls this on
    /// every fresh spawn (issue #886's idempotency invariant).
    #[test]
    fn inject_is_idempotent() {
        let temp = with_user_home_redirect();
        provision_grok(Path::new("/any"));
        let first = std::fs::read_to_string(
            temp.path().join(".grok").join("hooks").join("buildmesh-attention.json"),
        )
        .unwrap();

        provision_grok(Path::new("/any"));
        let second = std::fs::read_to_string(
            temp.path().join(".grok").join("hooks").join("buildmesh-attention.json"),
        )
        .unwrap();

        assert_eq!(first, second);
    }

    /// Injection only owns the `hooks` key — unrelated top-level keys
    /// the user (or another Buildmesh hook with a different shape)
    /// authored survive.
    #[test]
    fn inject_preserves_unrelated_top_level_keys() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".grok").join("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("buildmesh-attention.json"),
            r#"{"custom":"kept","other":[1,2,3]}"#,
        )
        .unwrap();

        provision_grok(Path::new("/any"));

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("buildmesh-attention.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["custom"], "kept");
        assert_eq!(value["other"], serde_json::json!([1, 2, 3]));
        assert!(value["hooks"]["Notification"].is_array());
        assert!(value["hooks"]["Stop"].is_array());
    }

    /// The URL template is the single source of truth for the
    /// attention endpoint — both events reference the same one, and
    /// all three env vars (port + session id + hook token) appear so
    /// the runner expands them per agent. Issue #1366 added the
    /// `?token=$BUILDMESH_HOOK_TOKEN` query for the runtime-scoped
    /// token gate; without it a non-Buildmesh Grok session could
    /// POST to a Buildmesh node on a box collision.
    #[test]
    fn hook_url_targets_attention_endpoint_with_env_expansion() {
        assert!(HOOK_URL.starts_with("http://localhost:$BUILDMESH_PORT/"));
        assert!(HOOK_URL.contains("/api/attention/$BUILDMESH_SESSION_ID"));
        assert!(
            !HOOK_URL.contains("127.0.0.1"),
            "use `localhost` so the loopback-only peer check accepts the runner: {HOOK_URL}"
        );
        // Issue #1366: the runtime-scoped token template. Pin the
        // exact `?token=$BUILDMESH_HOOK_TOKEN` query so a refactor
        // that drops the token trips here before reaching the wire.
        assert!(
            HOOK_URL.contains("?token=$BUILDMESH_HOOK_TOKEN"),
            "hook URL must carry the runtime-scoped token template: {HOOK_URL}"
        );
        // Pin the canonical anchors the marker predicate matches on
        // — additive merge looks for these to recognise the
        // Buildmesh-owned handler. If either anchor changes, the
        // marker fails to find its own handler and starts appending
        // duplicates on every re-run.
        assert!(HOOK_URL.contains("/api/attention/") && HOOK_URL.contains("BUILDMESH_PORT"));
    }

    /// The hook file lives in the user's home Grok hooks dir, not in
    /// the project cwd — project-local hooks require folder trust,
    /// which we have no spawn flag to bypass.
    #[test]
    fn inject_targets_global_grok_hooks_dir_not_project() {
        let temp = with_user_home_redirect();
        let project = TempDir::new().unwrap();
        provision_grok(project.path());

        // No hook file landed under the project cwd.
        assert!(
            !project.path().join(".grok").exists(),
            "project-local path should be skipped — folder-trust gate has no spawn flag"
        );
        // The global one did land.
        assert!(
            temp.path()
                .join(".grok")
                .join("hooks")
                .join("buildmesh-attention.json")
                .exists()
        );
    }

    /// `grok_home()` resolves to `<USERPROFILE|HOME>/.grok/hooks/`. A
    /// missing/empty env var is a hard error (the spawn path logs and
    /// continues, but the agent runs without attention callbacks).
    #[test]
    fn grok_home_resolves_under_user_profile() {
        let temp = with_user_home_redirect();
        let resolved = grok_home().expect("home should resolve");
        assert_eq!(resolved, temp.path().join(".grok").join("hooks"));
    }

    // -----------------------------------------------------------------
    // Additive merging + atomic write — issue #1366.
    //
    // The pre-#1366 `ensure_hooks_json` wholesale-assigned
    // `settings["hooks"] = expected_hooks`, destroying any
    // user-authored matcher groups, sibling handlers, and unrelated
    // events on every re-run. The fix mirrors Codex's per-event
    // additive merge (codex.rs:931-996): iterate over the event
    // list, locate a Buildmesh-owned handler via a URL-anchored
    // match, update it in place; otherwise append a new matcher
    // group — leaving every user-authored entry untouched.
    //
    // The marker is anchored on the handler's documented `url`
    // field (Grok's parser tolerance for unknown handler fields
    // like `statusMessage` is undocumented; the URL is guaranteed
    // preserved byte-for-byte). A handler is "Buildmesh-owned"
    // when its `url` carries both `/api/attention/` and the
    // `BUILDMESH_PORT` expansion token — the canonical anchors of
    // every Buildmesh attention webhook.
    // -----------------------------------------------------------------

    /// Marker predicate: a Grok HTTP handler is Buildmesh-owned
    /// when its `url` carries both anchors. Substring match (not
    /// strict equality) so future URL refactors (adding a query
    /// param, swapping `localhost` for `127.0.0.1`, etc.) keep the
    /// merge stable — only the canonical anchors matter.
    fn is_buildmesh_handler(handler: &serde_json::Value) -> bool {
        handler
            .get("url")
            .and_then(|v| v.as_str())
            .is_some_and(|url| url.contains("/api/attention/") && url.contains("BUILDMESH_PORT"))
    }

    #[test]
    fn marker_predicate_matches_canonical_buildmesh_url() {
        let buildmesh = serde_json::json!({
            "type": "http",
            "url": HOOK_URL,
        });
        assert!(is_buildmesh_handler(&buildmesh));

        let user = serde_json::json!({
            "type": "http",
            "url": "https://hooks.example.com/user-event",
            "timeout": 30,
        });
        assert!(!is_buildmesh_handler(&user));

        let bare = serde_json::json!({"type": "http"});
        assert!(!is_buildmesh_handler(&bare));

        let command_style = serde_json::json!({
            "type": "command",
            "command": "echo hello",
        });
        assert!(!is_buildmesh_handler(&command_style));
    }

    /// Pre-existing user-authored Notification handler survives a
    /// Buildmesh inject — the user's matcher group is preserved
    /// AND the Buildmesh handler is appended as a sibling matcher
    /// group on the same event.
    #[test]
    fn inject_preserves_user_handler_on_notification() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".grok").join("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        let user_handler = serde_json::json!({
            "type": "http",
            "url": "https://hooks.example.com/user-event",
            "timeout": 30,
        });
        let existing = serde_json::json!({
            "hooks": {
                "Notification": [
                    { "hooks": [user_handler] }
                ]
            }
        });
        std::fs::write(
            dir.join("buildmesh-attention.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        provision_grok(Path::new("/any"));

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("buildmesh-attention.json")).unwrap(),
        )
        .unwrap();
        let notification = value["hooks"]["Notification"]
            .as_array()
            .expect("Notification must be an array");
        assert_eq!(
            notification.len(),
            2,
            "Notification must carry BOTH user and Buildmesh matcher groups; got {value:#}"
        );
        // User handler preserved byte-for-byte (matcher group index 0).
        assert_eq!(
            notification[0]["hooks"][0]["url"].as_str(),
            Some("https://hooks.example.com/user-event")
        );
        // Buildmesh matcher group appended (index 1).
        let buildmesh_url = notification[1]["hooks"][0]["url"]
            .as_str()
            .expect("buildmesh handler appended");
        assert!(
            buildmesh_url.contains("/api/attention/") && buildmesh_url.contains("BUILDMESH_PORT"),
            "buildmesh matcher group must carry the canonical URL anchors: {buildmesh_url}"
        );
    }

    /// Pre-existing user hooks under an event the integration
    /// doesn't touch (e.g. `SessionStart`) survive unchanged. Only
    /// the Buildmesh-owned `Notification` and `Stop` events are
    /// managed.
    #[test]
    fn inject_preserves_unrelated_events_alongside_notification_and_stop() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".grok").join("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        let user_handler = serde_json::json!({
            "type": "http",
            "url": "https://hooks.example.com/user-event",
        });
        let user_other_handler = serde_json::json!({
            "type": "http",
            "url": "https://hooks.example.com/some-other-event",
        });
        let existing = serde_json::json!({
            "hooks": {
                "SessionStart": [{ "hooks": [user_handler] }],
                "UserCustom": [{ "hooks": [user_other_handler] }],
            }
        });
        std::fs::write(
            dir.join("buildmesh-attention.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        provision_grok(Path::new("/any"));

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("buildmesh-attention.json")).unwrap(),
        )
        .unwrap();

        // Unrelated user events survive byte-for-byte.
        assert_eq!(
            value["hooks"]["SessionStart"][0]["hooks"][0]["url"].as_str(),
            Some("https://hooks.example.com/user-event")
        );
        assert_eq!(
            value["hooks"]["UserCustom"][0]["hooks"][0]["url"].as_str(),
            Some("https://hooks.example.com/some-other-event")
        );
        // Notification / Stop are populated by the inject.
        let notification = value["hooks"]["Notification"]
            .as_array()
            .expect("Notification must be present");
        assert_eq!(notification.len(), 1, "single Buildmesh matcher group expected");
        assert!(is_buildmesh_handler(&notification[0]["hooks"][0]));
        let stop = value["hooks"]["Stop"]
            .as_array()
            .expect("Stop must be present");
        assert_eq!(stop.len(), 1, "single Buildmesh matcher group expected");
        assert!(is_buildmesh_handler(&stop[0]["hooks"][0]));
    }

    /// Repeat injects add no duplicate Buildmesh handlers. The
    /// marker-anchored merge must update in place — not append —
    /// when the URL anchor is recognised.
    #[test]
    fn inject_creates_no_duplicates_on_repeat() {
        let temp = with_user_home_redirect();
        provision_grok(Path::new("/any"));
        provision_grok(Path::new("/any"));
        provision_grok(Path::new("/any"));

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                temp.path().join(".grok").join("hooks").join("buildmesh-attention.json"),
            )
            .unwrap(),
        )
        .unwrap();
        for event in ["Notification", "Stop"] {
            let groups = value["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("{event} not an array: {value:#}"));
            assert_eq!(
                groups.len(),
                1,
                "{event} must have exactly one matcher group after 3 injects; got {value:#}"
            );
        }
    }

    /// Atomic write leaves no `.tmp` residue in the hooks dir.
    /// Mirrors AGY precedent (`agy.rs:540-555`,
    /// `inject_leaves_no_tmp_residue`).
    #[test]
    fn inject_atomic_write_leaves_no_tmp_residue() {
        let temp = with_user_home_redirect();
        provision_grok(Path::new("/any"));

        let dir = temp.path().join(".grok").join("hooks");
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

    /// When both events already carry a Buildmesh handler (idempotent
    /// re-run) the file is NOT rewritten. We assert this by checking
    /// the file's `mtime` is unchanged between two injects that find
    /// the integration already wired. (mtime is OS-resolution dependent
    /// — we wrap the writes in a brief sleep to make the assertion
    /// deterministic.)
    #[test]
    fn idempotent_rerun_does_not_rewrite_when_already_wired() {
        let temp = with_user_home_redirect();
        let path = temp.path().join(".grok").join("hooks").join("buildmesh-attention.json");
        provision_grok(Path::new("/any"));
        let first_bytes = std::fs::read(&path).unwrap();
        let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        provision_grok(Path::new("/any"));
        let second_bytes = std::fs::read(&path).unwrap();
        let second_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        assert_eq!(first_bytes, second_bytes, "byte-identical re-run");
        assert_eq!(
            first_mtime, second_mtime,
            "idempotent re-run must NOT rewrite the file (no mtime change)"
        );
    }

    /// Issue #1366 review point 2.3: refuse to silently wipe a
    /// malformed user-authored file. A trailing comma, partial edit,
    /// or syntax error must NOT cause `ensure_hooks_json` to fall
    /// back to `{}` and overwrite the user's data. The function
    /// returns an `Err`, the spawn path surfaces it as a provision
    /// failure, and the user's content survives intact.
    #[test]
    fn inject_refuses_to_overwrite_malformed_user_file() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".grok").join("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("buildmesh-attention.json");
        // Deliberately malformed: trailing comma + unmatched brace.
        let malformed = "{ \"hooks\": { \"Notification\": [],, }";
        std::fs::write(&path, malformed).unwrap();

        let result = try_provision_grok(Path::new("/any"));
        assert!(
            result.is_err(),
            "provision must refuse a malformed existing file; got {result:?}"
        );

        // The user's malformed content must survive intact — the
        // refactor that triggered this test (silently overwriting
        // with `{}`) clobbered user data; the new behaviour leaves
        // it for the user to repair.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, malformed,
            "malformed file content must NOT be overwritten"
        );
    }

    /// The matching positive case for the malformed-file pin: a
    /// missing file should be treated as `{}` (fresh install) and
    /// written normally. Lock the happy path so a future refactor
    /// that treats both missing AND malformed the same way trips
    /// both regressions in one place.
    #[test]
    fn inject_treats_missing_file_as_empty_settings() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".grok").join("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        // Note: no file at buildmesh-attention.json — fresh install.
        assert!(!dir.join("buildmesh-attention.json").exists());

        try_provision_grok(Path::new("/any")).unwrap();
        let written = std::fs::read_to_string(
            dir.join("buildmesh-attention.json"),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert!(value["hooks"]["Notification"].is_array());
        assert!(value["hooks"]["Stop"].is_array());
    }

    /// `with_user_home_redirect` is the test scaffolding for every test
    /// that needs `grok_home()` to land in a temp dir. It serializes
    /// access to USERPROFILE (Windows) / HOME (Unix) via a process-wide
    /// mutex so parallel tests can't stomp on each other's env value —
    /// `grok_home()` reads USERPROFILE, so the only way to keep two
    /// parallel tests deterministic is to let one mutate it at a time.
    /// The lock is held for the lifetime of the returned guard; Drop
    /// restores the prior value and releases the lock together.
    struct HomeRedirect {
        temp: TempDir,
        key: &'static str,
        previous: Option<std::ffi::OsString>,
        /// Declared last so the lock is the last field dropped. Drop::drop
        /// runs before field drops, so the env restore in our impl
        /// happens before this guard releases — without that ordering,
        /// a parallel test could observe the *other* test's temp path.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeRedirect {
        fn path(&self) -> &std::path::Path {
            self.temp.path()
        }
    }

    impl Drop for HomeRedirect {
        fn drop(&mut self) {
            // Drop::drop runs before any field drops, so the env
            // restore here happens before `_lock` releases — the
            // ordering prevents a parallel test from racing on
            // USERPROFILE mid-shutdown.
            match self.previous.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn home_key() -> &'static str {
        if cfg!(target_os = "windows") {
            "USERPROFILE"
        } else {
            "HOME"
        }
    }

    fn with_user_home_redirect() -> HomeRedirect {
        let lock = USER_HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let key = home_key();
        let previous = std::env::var_os(key);
        std::env::set_var(key, temp.path());
        HomeRedirect { temp, key, previous, _lock: lock }
    }

    /// Process-wide lock for tests that mutate USERPROFILE / HOME.
    /// Without this, two tests in parallel racing on `set_var` can
    /// leave the value pointing at the *other* test's temp dir when
    /// either side calls `grok_home()`, which then writes to the
    /// wrong location and fails the assertion.
    static USER_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
