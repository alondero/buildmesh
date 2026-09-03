//! Kimi Code provider adapter — Moonshot AI's full-screen interactive coding
//! agent, installed on PATH as a single `kimi` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. The non-interactive `-p <prompt>` mode
//! exists but is *not* used here: the #914 prototype verified that Buildmesh's
//! PTY backend (ConPTY on Windows, native PTY on macOS/Linux) fully supports
//! full-screen TUI rendering, so we launch in interactive mode everywhere.
//!
//! **Session resumption** uses `-S [<id>]` / `--session [<id>]` (cwd-scoped,
//! both forms optional-id selector or explicit resumption) or `-c` /
//! `--continue` for the most-recent session. Kimi auto-assigns its own
//! session ids (captured from PTY output by `session_naming`), so
//! `self_assigns_session_id()` is `true` and `session_assign_args()` is a no-op.
//!
//! **Model override** uses `-m <model-id>` / `--model <model-id>` (Kimi's
//! `--help` advertises the short form first, so the adapter emits `-m`).
//!
//! **Attention hooks** (issue #1369). Kimi's hook surface is documented at
//! <https://www.kimi.com/code/docs/en/kimi-code-cli/customization/hooks.html>
//! (current) and <https://moonshotai.github.io/kimi-cli/en/customization/hooks.html>
//! (Beta reference). Both describe the same `[[hooks]]` array-of-tables
//! shape; the current docs add three more lifecycle events
//! (`PermissionRequest`, `TaskStarted`, `SessionHeartbeat`) that the Beta
//! page documents as "Beta"-labelled. The schema is compatible across
//! both eras — the same merge logic works against either release.
//!
//! **Scoping (issue #1369, follow-up review).** Each Buildmesh-managed
//! Kimi node lives behind its own `KIMI_CODE_HOME` directory rooted under
//! the node's worktree (`<worktree>/.kimi-buildmesh/<node_id>/`). Kimi's
//! runner honours `KIMI_CODE_HOME` for config discovery, so each node
//! loads only its own `config.toml`. The spawn path exports
//! `KIMI_CODE_HOME=<that path>` on the child (`hook_env_vars`); the dir
//! is owned by Buildmesh and removed when the worktree itself is cleaned
//! up on node delete. This avoids:
//! - **Global mutation:** the user's personal `~/.kimi/config.toml` is
//!   never touched — their CLI sessions behave normally even with
//!   Buildmesh running.
//! - **Multi-tenant routing:** two simultaneous Kimi nodes get two
//!   different `KIMI_CODE_HOME` directories, so their `config.toml`
//!   files never collide (their hook URLs are baked to their own
//!   `node_id`).
//! - **Host intrusion / teardown:** the per-node dir is under the
//!   worktree, so the worktree-cleanup flow drops it for free — no
//!   extra teardown registration, no risk of orphaned directories
//!   hijacking the user's `~/.kimi/config.toml` after the app exits.
//!
//! We wire four hook entries per node:
//! - `Stop` (no matcher) → turn completion. The route's transcript-scan
//!   false-yield suppression applies so a Stop fired while background
//!   tasks are still pending does not create a false alert.
//! - `PermissionRequest` → canonical permission prompt signal
//!   (Codex-style). Marks input regardless of background work.
//! - `Notification` with matcher `task\.completed` → Kimi emits this
//!   when a background task finishes; lands as `Ready`.
//! - `SessionStart` → capture-only, same as Codex's `SessionStart`
//!   (`Decision::Ignore`). The route persists `cli_session_id` but does
//!   not mark attention or fire naming / autopilot.
//!
//! The hook command fully bakes the URL (`node_id` + runtime port +
//! runtime-scoped `?token=<minted>`). Kimi's hook runner env-handling
//! isn't documented (matching the Codex "bake the URL" precedent), so
//! the per-node `KIMI_CODE_HOME` directory is the multi-tenant
//! isolation boundary even when URLs collide. The token query + the
//! `verify_attention_token` per-provider gate in the route are
//! layered defences for same-box non-Buildmesh Kimi callbacks.
//!
//! **Effort override**: Kimi Code has no `--effort` /
//! `--reasoning-effort` flag in the documented surface. The descriptor
//! advertises `EffortControlKind::None` so the resolver drops every
//! resolved effort layer.
//!
//! **Shell wrapping**: `kimi` is a native binary on all platforms (not a
//! `.cmd` shim), so `WindowsShell::Direct` is correct everywhere —
//! matching the AGY and Grok adapter patterns.

use crate::agent::capabilities::EffortControlKind;
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::path::{Path, PathBuf};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

pub struct KimiAdapter;
pub static KIMI: KimiAdapter = KimiAdapter;

/// Minimum Kimi release the Buildmesh hook integration has been
/// validated against (issue #1369). The current Kimi Code docs and the
/// Beta reference both describe the same `[[hooks]]` schema — both
/// expose the four Buildmesh-owned events (Stop, PermissionRequest,
/// SessionStart, Notification-with-matcher), so a Kimi release that
/// ships either docs' surface is expected to work.
///
/// Pin format mirrors the AGY / Grok precedents: the descriptor's
/// `AttentionCapability::min_version` declares "minimum" semantics,
/// not a strict `>=` enforcement at runtime (semver isn't wired
/// through the hook surface). A future installed-release validation
/// pass should overwrite this with the verified floor.
pub const KIMI_MIN_HOOK_VERSION: &str = "1.0.0";

/// Kimi hook events Buildmesh wires into each node's `KIMI_CODE_HOME`.
///
/// The empty matcher on `Stop`, `PermissionRequest`, and `SessionStart`
/// is the Kimi-documented "match-all" default; we emit it explicitly
/// so re-runs of `provision_attention_hooks` produce byte-stable entries
/// (idempotency invariant).
///
/// The Beta reference documents `task\.completed` as the canonical
/// background-task completion matcher; the regex matches a literal
/// `.`. In TOML the double-backslash escape survives `toml_edit`
/// round-trips verbatim, so the wire shape is `matcher =
/// "task\\.completed"` — what the docs example shows.
const KIMI_EVENTS: &[(&str, &str)] = &[
    ("Stop", ""),
    ("PermissionRequest", ""),
    ("SessionStart", ""),
    ("Notification", "task\\.completed"),
];

/// Anchor substrings used by the marker predicate to identify
/// Buildmesh-owned hook entries during the additive merge. Both
/// substrings are stable across URL refactors (adding a query, swapping
/// `localhost` for `127.0.0.1`); the marker survives future refactors
/// that touch the URL shape.
const MARKER_ROUTE: &str = "/api/attention/";
const MARKER_STDIN: &str = "--data-binary @-";

/// Per-node config dir under the worktree. Self-contained — never
/// touches the user's `~/.kimi/config.toml`. Teardown comes for free
/// when the worktree itself is removed (the existing
/// `process_pending_removals` drain sweeps the directory recursively).
fn kimi_home_from(spawn_path: &Path, node_id: i64) -> PathBuf {
    spawn_path
        .join(".kimi-buildmesh")
        .join(node_id.to_string())
}

/// Marker predicate: a Kimi hook command is Buildmesh-owned when its
/// `command` carries both anchors. Substring match (not strict
/// equality) so future URL refactors (changing the route, swapping
/// `localhost` for `127.0.0.1`) keep the merge stable — only the
/// canonical anchors matter.
fn is_buildmesh_command(command: &str) -> bool {
    command.contains(MARKER_ROUTE) && command.contains(MARKER_STDIN)
}

/// Field accessor for `[[hooks]]` entries. `toml_edit`'s
/// `ArrayOfTables` yields `&Table` items (each `[[hooks]]` block
/// becomes one table); `Table::get` returns `Option<&Item>`, so the
/// read path is `Item::as_value` → `Value::as_str`. Used by both the
/// merge logic and the test code so the read pattern lives in one place.
fn table_field<'a>(table: &'a Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(Item::as_value).and_then(Value::as_str)
}

/// Build a Buildmesh-owned `[[hooks]]` entry as a `Table`. The
/// `event`, `matcher`, and `command` fields are what Kimi's parser
/// preserves across re-reads; we keep the surface narrow on purpose so
/// the marker predicate in `is_buildmesh_command` continues to
/// recognise the entry after a future Kimi release adds unrelated
/// fields.
fn buildmesh_hook_entry(event: &str, matcher: &str, command: &str) -> Table {
    let mut table = Table::new();
    table.insert("event", Item::Value(Value::from(event)));
    if !matcher.is_empty() {
        table.insert("matcher", Item::Value(Value::from(matcher)));
    }
    table.insert("command", Item::Value(Value::from(command)));
    table
}

/// Atomically persist `content` to `path` via `tempfile::NamedTempFile +
/// persist`. The Codex / AGY / Grok precedents all pin this pattern.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map(|_| ()).map_err(|error| error.error)
}

/// Construct the Buildmesh-owned hook command for a given node. The URL
/// is fully baked (port + node_id + token); Kimi's hook runner
/// env-handling isn't documented, and per-node isolation is supplied by
/// `KIMI_CODE_HOME` so a future env-var-expansion change wouldn't
/// disturb multi-tenant routing.
///
/// The `curl` shell-conditional picks the binary the Kimi runner
/// inherits from the parent shell (`curl.exe` on Windows, `curl` on
/// macOS/Linux). `-o /dev/null` discards the empty 200 body. The
/// trailing `|| true` keeps the hook fail-open: a Buildmesh-down or
/// network-blip moment must NOT block the agent.
fn hook_command(node_id: i64, port: u16, token: &str) -> String {
    let url = format!(
        "http://localhost:{port}/api/attention/{node_id}?token={token}"
    );
    format!(
        "curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST \
 -H \"Content-Type: application/json\" --data-binary @- \"{url}\" || true"
    )
}

/// Pure helper: edit the TOML document to add or update the
/// Buildmesh-owned `[[hooks]]` entries. Returns `Ok(Some(updated))`
/// when at least one entry changed; `Ok(None)` when every entry is
/// already byte-stable (idempotent re-run, preserve mtime); or `Err`
/// when the existing document is malformed (don't clobber a
/// user-authored file).
///
/// Idempotency is the **full (event, matcher, command) triple** — a
/// sibling spawn that arrives with a different node_id (and therefore
/// a different baked `command`) does update the entry; a re-spawn of
/// the same node is a no-op; a future Kimi release that changes
/// Buildmesh's matcher regex also re-asserts the entry correctly.
fn ensure_hooks_toml_content(
    existing: &str,
    node_id: i64,
    port: u16,
    token: &str,
) -> Result<Option<String>, String> {
    let command = hook_command(node_id, port, token);
    let mut document = if existing.is_empty() {
        DocumentMut::new()
    } else {
        existing
            .parse::<DocumentMut>()
            .map_err(|error| format!("failed to parse kimi config.toml: {error}"))?
    };

    // The hooks array-of-tables may already exist (user-authored
    // entries on the same Kimi install, though that's now rare given
    // our per-node KIMI_CODE_HOME, or a leftover from a previous
    // provisioning of the same node). The canonical Kimi schema is
    // `[[hooks]]` at the top level — matching the Beta docs and the
    // current docs. `toml_edit` parses `[[hooks]]` into
    // `Item::ArrayOfTables(ArrayOfTables)` (distinct from the
    // inline-array shape `hooks = [...]`, which we don't accept —
    // inline arrays aren't the Kimi documented surface). Walk the
    // array: own only entries whose `command` matches our marker
    // predicate, preserve every other entry (including user-authored
    // ones with the same event name but a different command), never
    // duplicate a marker-recognised entry.
    let hooks_item = document
        .as_table_mut()
        .entry("hooks")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));

    // If `hooks` is misconfigured as anything other than an
    // array-of-tables (e.g. an int, a string, a regular table, or an
    // inline array), refuse to clobber it — the Codex precedent
    // returns Err rather than silently overwriting the user's data.
    // A user who wants the canonical `[[hooks]]` shape can rename
    // their existing key without Buildmesh interfering.
    let Item::ArrayOfTables(hooks_array_mut) = hooks_item else {
        return Err(
            "kimi config.toml `hooks` value must be a `[[hooks]]` array-of-tables"
                .to_string(),
        );
    };

    let mut changed = false;
    for (event, matcher) in KIMI_EVENTS {
        // Locate an existing Buildmesh-owned entry for this event.
        // Match on the marker predicate regardless of the matcher's
        // exact text — the URL anchors are the source of truth, not
        // the matcher string. We then compare the FULL
        // (event, matcher, command) triple against the desired
        // entry; any drift triggers an in-place replacement.
        let existing_index = hooks_array_mut.iter().position(|table| {
            let same_event = table_field(table, "event") == Some(*event);
            if !same_event {
                return false;
            }
            table_field(table, "command").is_some_and(is_buildmesh_command)
        });

        let desired = buildmesh_hook_entry(event, matcher, &command);

        if let Some(index) = existing_index {
            // Compare every owned field against the desired entry.
            // A re-parsed `Table` carries decoration metadata
            // (whitespace, comment positions) that a freshly built
            // value doesn't, so a naive `to_string()` comparison
            // would always diverge and re-write the file every spawn
            // (breaking the issue #886 idempotency invariant).
            // The semantic content we own is the field-level
            // (event, matcher, command) triple. Crucially, an empty
            // matcher in the desired entry maps to the ABSENCE of
            // the key in the on-disk entry — comparing `None ==
            // Some("")` is wrong.
            let slot_matches = hooks_array_mut.get(index).is_some_and(|slot| {
                let event_matches = table_field(slot, "event") == Some(*event);
                let matcher_matches = if matcher.is_empty() {
                    table_field(slot, "matcher").is_none()
                } else {
                    table_field(slot, "matcher") == Some(*matcher)
                };
                let command_matches =
                    table_field(slot, "command") == Some(command.as_str());
                event_matches && matcher_matches && command_matches
            });
            if !slot_matches {
                hooks_array_mut.replace(index, desired);
                changed = true;
            }
        } else {
            // Fresh inject — append a Buildmesh-owned entry.
            hooks_array_mut.push(desired);
            changed = true;
        }
    }

    if !changed {
        return Ok(None);
    }
    Ok(Some(document.to_string()))
}

impl AgentProvider for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Kimi Code".into(),
            color: "#00c4c4".into(),
            icon: "K".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "kimi",
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

    /// Issue #1369: Kimi Code now ships attention hooks via the
    /// per-node `KIMI_CODE_HOME` directory (see module doc comment for
    /// the scoping rationale).
    fn requires_attention_hook(&self) -> bool {
        true
    }

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        AttentionCapability::Hook {
            // Stop → turn completion (subject to the route's
            // transcript-scan false-yield suppression).
            // PermissionRequest → canonical permission prompt signal.
            // InputRequired → covers Kimi's "needs the user" hooks
            // that don't take the dedicated PermissionRequest shape
            // (matches Grok's vocabulary); a future Kimi release that
            // exposes an explicit `QuestionRequest` event will route
            // through this kind without a descriptor edit.
            events: vec![
                LifecycleKind::TurnCompleted,
                LifecycleKind::InputRequired,
                LifecycleKind::PermissionRequested,
            ],
            // Kimi Code runs under its default permission-asking
            // mode (no `--dangerously-skip-permissions` flag in the
            // documented surface).
            launch_mode: AttentionLaunchMode::PermissionAsk,
            // The dir is per-node and lives inside the worktree —
            // Buildmesh owns it, and the worktree cleanup drops it
            // for free. No global mutation of the user's
            // `~/.kimi/config.toml`.
            trust: Some("per-node KIMI_CODE_HOME".into()),
            // Issue #1369 — pin the validated Beta/current Kimi
            // release floor.
            min_version: Some(KIMI_MIN_HOOK_VERSION.into()),
        }
    }

    /// Provision Buildmesh attention hooks into Kimi's per-node
    /// `KIMI_CODE_HOME` directory. The directory lives under the
    /// node's worktree (resolved.spawn_path) — NOT under the user's
    /// global `~/.kimi/`. The runtime-scoped `?token=<minted>`
    /// query in the baked URL defends against same-box collisions;
    /// the route's per-provider gate (`verify_attention_token` in
    /// `http/routes/attention.rs`) requires the token for Kimi.
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        node_id: i64,
    ) -> Result<(), String> {
        // The runtime-scoped token, minted once per process. The
        // route's `verify_attention_token` rejects callbacks whose
        // `?token=` doesn't match `RUNTIME_HOOK_TOKEN` for the
        // `kimi` provider; minting once per Buildmesh process
        // (via `mint_runtime_hook_token`) keeps the per-node URL
        // stable across spawns in the same runtime.
        let token = crate::agent::mint_runtime_hook_token();
        let port = crate::http_server::current_http_port();

        let home = kimi_home_from(Path::new(&resolved.spawn_path), node_id);
        std::fs::create_dir_all(&home)
            .map_err(|e| format!("failed to create KIMI_CODE_HOME {home:?}: {e}"))?;

        // Per-node write lock — concurrent Kimi spawns in the same
        // node can't race on the (event, matcher, command) merge.
        // Different nodes get different homes (no sharing); the
        // lock is a defence-in-depth within a node's own dir.
        let _guard = KIMI_CONFIG_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let path = home.join("config.toml");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        match ensure_hooks_toml_content(&existing, node_id, port, &token)? {
            Some(content) => {
                write_atomic(&path, &content)
                    .map_err(|e| format!("failed to write kimi config.toml: {e}"))?;
                tracing::info!(
                    "kimi provision_attention_hooks: wrote node {node_id} hooks into {path:?}"
                );
            }
            None => {
                tracing::debug!(
                    "kimi provision_attention_hooks: {path:?} already wired for node {node_id}, no rewrite"
                );
            }
        }
        Ok(())
    }

    /// Per-node env-var export: tell the Kimi process to load its
    /// config from the Buildmesh-owned directory instead of the
    /// user's global `~/.kimi/config.toml`. The spawn command layer
    /// (`spawn/command.rs`) calls this AFTER `provision_attention_hooks`
    /// runs, so the directory is guaranteed to exist when the child
    /// reads it. Honours user-supplied values (does NOT clobber an
    /// explicit `KIMI_CODE_HOME` set by the user before launch).
    fn hook_env_vars(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        node_id: i64,
    ) -> Vec<(String, String)> {
        if std::env::var_os("KIMI_CODE_HOME").is_some() {
            // User explicitly set KIMI_CODE_HOME before launching
            // Buildmesh — preserve their choice. The hooks we
            // provision land wherever the orchestrator's resolved
            // path says; we don't second-guess the user.
            return Vec::new();
        }
        let home = kimi_home_from(Path::new(&resolved.spawn_path), node_id);
        vec![(
            "KIMI_CODE_HOME".to_string(),
            home.to_string_lossy().into_owned(),
        )]
    }

    /// Kimi Code stores its session log under `~/.kimi/sessions/wire.jsonl`
    /// in standard JSONL form (#911 research). The on-disk *format* matches
    /// what the shared transcript_reader parses, but the *path* is
    /// `~/.kimi/...` not `~/.claude/projects/<encoded-cwd>/<session>.jsonl`,
    /// and the reader's path resolver isn't wired for Kimi yet — so the
    /// Node Digest rich layer currently degrades to spine-only with the
    /// `unsupported` flag set, not silent omission. Returns `false` to
    /// match the wire behaviour; follow-up wires the Kimi case into
    /// `services::transcript_reader::TranscriptFormat::for_harness`.
    fn produces_readable_transcript(&self) -> bool {
        false
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        false
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    /// Kimi auto-assigns session ids — captured from PTY output.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    /// Kimi's explicit resume flag is `-S <id>` / `--session <id>` (long form
    /// is `--session`, not `--resume`). The bare `-c` / `--continue` form
    /// (cwd-most-recent) is intentionally not modelled here — auto-resume
    /// always passes the captured session id explicitly, so the resolver
    /// never needs to fall back to the implicit selector.
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["-S".into(), id.into()]
    }

    /// Kimi's model flag is `-m <model>` (short) or `--model <model>` (long).
    /// Use the short form — matches Kimi Code's own CLI examples and the
    /// `-m` short flag is what `--help` advertises first.
    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["-m".into(), model.into()]
    }

    /// No `--session-id` flag — Kimi assigns its own.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    /// Kimi has no `--effort` / `--reasoning-effort` flag in the
    /// documented surface. The descriptor advertises
    /// `EffortControlKind::None` so the resolver drops every
    /// resolved effort layer.
    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::None
    }
}

/// Per-node write lock — concurrent Kimi spawns on the same node
/// (rare; usually one node at a time) can't race on the
/// (event, matcher, command) merge. Different nodes get different
/// `KIMI_CODE_HOME` directories (no sharing), so this is a
/// within-a-node defence in depth, not a multi-tenant gate.
static KIMI_CONFIG_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
    use tempfile::TempDir;

    /// Pure helper exposed for tests: provision with a
    /// `KIMI_CONFIG_WRITE_LOCK`-bypassing call. The tests run
    /// helpers directly with fixture paths (no env mutation, no
    /// global locks — the pure helpers are independent of any state
    /// that needs locking).
    fn provision_at(home: &Path, node_id: i64, port: u16, token: &str) {
        std::fs::create_dir_all(home).unwrap();
        let path = home.join("config.toml");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        match ensure_hooks_toml_content(&existing, node_id, port, token).unwrap() {
            Some(content) => std::fs::write(&path, content).unwrap(),
            None => {}
        }
    }

    fn read_hooks(value: &DocumentMut) -> &ArrayOfTables {
        value["hooks"]
            .as_array_of_tables()
            .expect("hooks must be an array-of-tables (canonical `[[hooks]]` syntax)")
    }

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(KIMI.id(), "kimi");
        let ui = KIMI.ui();
        assert_eq!(ui.label, "Kimi Code");
        assert_eq!(ui.color, "#00c4c4");
        assert_eq!(ui.icon, "K");
    }

    #[test]
    fn spawn_recipe_direct_on_all_platforms() {
        for platform in [Platform::Windows, Platform::Linux, Platform::Macos] {
            let recipe = KIMI.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "kimi");
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
        let platforms = KIMI.available_on();
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
        assert!(KIMI.self_assigns_session_id());
    }

    #[test]
    fn resume_args_format() {
        assert_eq!(KIMI.resume_args("abc-123"), vec!["-S", "abc-123"]);
    }

    #[test]
    fn model_args_format() {
        assert_eq!(KIMI.model_args("kimi-k2"), vec!["-m", "kimi-k2"]);
    }

    #[test]
    fn session_assign_args_empty() {
        assert!(KIMI.session_assign_args("any-id").is_empty());
    }

    #[test]
    fn no_prefill_support() {
        assert!(!KIMI.supports_prefill());
    }

    /// Issue #1369: Kimi now ships per-node attention hooks. The
    /// descriptor advertises the four-event surface routed through
    /// per-node `KIMI_CODE_HOME`, plus the `min_version` pin so a
    /// flip-back to `None` trips here (matching the AGY / Grok
    /// `..`-free `matches!` pattern from issue #1366).
    #[test]
    fn capabilities_descriptor_advertises_attention_capability() {
        let caps = KIMI.capabilities();
        assert_eq!(caps.harness_id, "kimi");
        assert!(caps.supports_resume);
        assert!(caps.supports_model_override);
        assert!(!caps.supports_effort_override);
        assert!(!caps.supports_prefill);
        assert!(
            caps.requires_attention_hook,
            "issue #1369: Kimi now ships per-node attention hooks"
        );
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(caps.effort_control, EffortControlKind::None);

        match &caps.attention_capability {
            AttentionCapability::Hook {
                events,
                launch_mode: AttentionLaunchMode::PermissionAsk,
                trust: Some(trust),
                min_version: Some(min_version),
            } => {
                use crate::agent::session_lifecycle::LifecycleKind;
                assert!(events.contains(&LifecycleKind::TurnCompleted));
                assert!(events.contains(&LifecycleKind::InputRequired));
                assert!(events.contains(&LifecycleKind::PermissionRequested));
                assert!(
                    trust.contains("KIMI_CODE_HOME"),
                    "trust must reflect the per-node KIMI_CODE_HOME scoping; got {trust}"
                );
                assert_eq!(
                    min_version, KIMI_MIN_HOOK_VERSION,
                    "min_version pin must match the KIMI_MIN_HOOK_VERSION constant"
                );
            }
            other => panic!(
                "Kimi must advertise Hook under PermissionAsk with a pinned min_version; got {other:?}"
            ),
        }
    }

    /// `kimi_home_from` is a pure helper — same inputs always
    /// produce the same path, regardless of global env state.
    /// Pin the per-worktree + per-node shape.
    #[test]
    fn kimi_home_from_is_per_worktree_and_per_node() {
        let spawn = Path::new("/worktrees/wt-A");
        let h10 = kimi_home_from(spawn, 10);
        let h20 = kimi_home_from(spawn, 20);
        assert_eq!(h10, PathBuf::from("/worktrees/wt-A/.kimi-buildmesh/10"));
        assert_eq!(h20, PathBuf::from("/worktrees/wt-A/.kimi-buildmesh/20"));
        assert_ne!(h10, h20, "two nodes must get two different homes");

        let spawn_b = Path::new("/worktrees/wt-B");
        let h10_b = kimi_home_from(spawn_b, 10);
        assert_ne!(
            h10, h10_b,
            "two worktrees must get two different homes for the same node_id"
        );
    }

    /// `hook_env_vars` returns `KIMI_CODE_HOME` for the resolved
    /// spawn path / node. Two nodes on the same worktree get
    /// different `KIMI_CODE_HOME` values — the multi-tenant
    /// isolation boundary. Honours user-set env (returns empty
    /// when the user has chosen their own path).
    #[test]
    fn hook_env_vars_returns_per_node_kimi_code_home() {
        let resolved = ResolvedPath {
            host_path: "/worktrees/wt-A".into(),
            spawn_path: "/worktrees/wt-A".into(),
            raw_path: "/worktrees/wt-A".into(),
            env_type: EnvType::Windows,
        };
        let env = KIMI.hook_env_vars(&resolved, &LaunchRuntime::default(), 10);
        assert_eq!(env.len(), 1);
        let (k, v) = &env[0];
        assert_eq!(k, "KIMI_CODE_HOME");
        // Component-based assertion (path-separator agnostic on
        // Windows where `Path::join` emits backslashes):
        let path = std::path::Path::new(v);
        let components: Vec<_> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            components.last().map(String::as_str),
            Some("10"),
            "tail component must be the node id; got {components:?}"
        );
        assert!(
            path
                .components()
                .any(|c| c.as_os_str() == ".kimi-buildmesh"),
            "KIMI_CODE_HOME must contain a .kimi-buildmesh component; got {components:?}"
        );
    }

    /// End-to-end: injection writes the four-event Buildmesh-owned
    /// `[[hooks]]` array at the per-node `KIMI_CODE_HOME`, with each
    /// event carrying a `command` anchored on the canonical
    /// `/api/attention/<node-id>` URL + `?token=<minted>` runtime
    /// gate. The matcher for `Notification` is the documented regex
    /// `task\.completed` so a bare `Notification` doesn't
    /// over-trigger on every notification.
    #[test]
    fn inject_writes_stop_permission_notification_session_start() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        provision_at(home, 42, 1992, "tok-abcdef");

        let path = home.join("config.toml");
        let content = std::fs::read_to_string(&path).expect("config.toml not written");
        let value: DocumentMut = content.parse().expect("config.toml is not valid TOML");
        let hooks = read_hooks(&value);

        let mut seen_stop = false;
        let mut seen_permission_request = false;
        let mut seen_session_start = false;
        let mut seen_notification = false;
        for entry in hooks {
            let event = table_field(entry, "event")
                .unwrap_or_else(|| panic!("hook entry missing `event`: {entry:?}"));
            let command = table_field(entry, "command")
                .unwrap_or_else(|| panic!("{event} hook missing `command`: {entry:?}"));
            assert!(
                is_buildmesh_command(command),
                "{event} command must carry the canonical /api/attention/ + --data-binary @- anchors; got {command}"
            );

            match event {
                "Stop" => {
                    assert!(!seen_stop, "Stop hook emitted more than once");
                    seen_stop = true;
                    assert!(
                        table_field(entry, "matcher").is_none(),
                        "Stop must not carry a matcher: {entry:?}"
                    );
                }
                "PermissionRequest" => {
                    assert!(!seen_permission_request, "PermissionRequest hook emitted more than once");
                    seen_permission_request = true;
                    assert!(
                        table_field(entry, "matcher").is_none(),
                        "PermissionRequest must not carry a matcher: {entry:?}"
                    );
                }
                "SessionStart" => {
                    assert!(!seen_session_start, "SessionStart hook emitted more than once");
                    seen_session_start = true;
                    assert!(
                        table_field(entry, "matcher").is_none(),
                        "SessionStart must not carry a matcher: {entry:?}"
                    );
                }
                "Notification" => {
                    assert!(!seen_notification, "Notification hook emitted more than once");
                    seen_notification = true;
                    let matcher = table_field(entry, "matcher")
                        .expect("Notification must carry the task\\.completed matcher");
                    assert_eq!(
                        matcher, "task\\.completed",
                        "Notification matcher must match the documented task\\.completed regex"
                    );
                }
                other => panic!("unexpected hook event {other}"),
            }
        }
        assert!(seen_stop);
        assert!(seen_permission_request);
        assert!(seen_session_start);
        assert!(seen_notification);
    }

    /// The baked URL carries the runtime-scoped token + the
    /// per-node id. Pin the exact wire shape so a refactor that
    /// drops the token trips here before reaching the route.
    #[test]
    fn hook_command_bakes_token_and_node_id() {
        let cmd = hook_command(42, 1992, "tok-abcdef");
        assert!(cmd.contains("/api/attention/42"));
        assert!(cmd.contains("?token=tok-abcdef"));
        assert!(cmd.contains("localhost:1992"));
        assert!(
            !cmd.contains("$BUILDMESH_PORT"),
            "URL is fully baked (no env expansion): {cmd}"
        );
        assert!(cmd.contains("|| true"), "fail-open contract: {cmd}");
    }

    /// Re-running injection over an already-correct config is a
    /// no-op — the file's bytes are byte-identical between calls and
    /// mtime is untouched (issue #886 spawn-path idempotency).
    #[test]
    fn inject_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        provision_at(home, 42, 1992, "tok-abcdef");
        let path = home.join("config.toml");
        let first = std::fs::read_to_string(&path).unwrap();
        let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        provision_at(home, 42, 1992, "tok-abcdef");
        let second = std::fs::read_to_string(&path).unwrap();
        let second_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(first, second, "byte-identical re-run");
        assert_eq!(first_mtime, second_mtime, "no mtime change");
    }

    /// User-authored sibling entries survive (someone added a
    /// PostToolUse for prettier, say). The merge only owns
    /// Buildmesh-flagged entries.
    #[test]
    fn inject_preserves_user_authored_hook_entry() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let user_hook = r#"
[[hooks]]
event = "Stop"
command = "user-owned-stop-handler"

[[hooks]]
event = "PostToolUse"
matcher = "WriteFile|StrReplaceFile"
command = "jq -r '.tool_input.file_path' | xargs prettier --write"
"#;
        std::fs::write(home.join("config.toml"), user_hook).unwrap();

        provision_at(home, 42, 1992, "tok-abcdef");

        let content = std::fs::read_to_string(home.join("config.toml")).unwrap();
        let value: DocumentMut = content.parse().expect("valid TOML");
        let hooks = read_hooks(&value);

        let user_stop_preserved = hooks.iter().any(|entry| {
            table_field(entry, "command") == Some("user-owned-stop-handler")
        });
        assert!(user_stop_preserved, "user Stop hook survives a Buildmesh inject");

        let user_post_tool_use_preserved = hooks
            .iter()
            .any(|entry| table_field(entry, "event") == Some("PostToolUse"));
        assert!(user_post_tool_use_preserved, "user PostToolUse hook survives");

        let buildmesh_stop_present = hooks.iter().any(|entry| {
            table_field(entry, "event") == Some("Stop")
                && table_field(entry, "command").is_some_and(is_buildmesh_command)
        });
        assert!(buildmesh_stop_present, "Buildmesh Stop hook present alongside user Stop");
    }

    /// Repeat injects add no duplicate Buildmesh handlers.
    /// Full (event, matcher, command) triple match (not just
    /// command) — a future release that tightens the matcher
    /// regex updates in place rather than appending a stale
    /// duplicate.
    #[test]
    fn inject_creates_no_duplicates_on_repeat() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        provision_at(home, 42, 1992, "tok-abcdef");
        provision_at(home, 42, 1992, "tok-abcdef");
        provision_at(home, 42, 1992, "tok-abcdef");

        let path = home.join("config.toml");
        let value: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let hooks = read_hooks(&value);

        for event in ["Stop", "PermissionRequest", "SessionStart", "Notification"] {
            let count = hooks
                .iter()
                .filter(|entry| {
                    table_field(entry, "event") == Some(event)
                        && table_field(entry, "command").is_some_and(is_buildmesh_command)
                })
                .count();
            assert_eq!(
                count, 1,
                "{event} must have exactly one Buildmesh entry after 3 injects; got {hooks:#?}"
            );
        }
    }

    /// Drift in any owned field (event, matcher, OR command)
    /// triggers an in-place replacement — not an idempotent skip.
    /// Pin this so a future matcher tightening isn't silently
    /// missed.
    #[test]
    fn inject_matches_full_event_matcher_command_triple() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        // First provision: matcher is `task\.completed`.
        provision_at(home, 42, 1992, "tok-abcdef");

        // Manually mutate the on-disk Notifier entry's matcher to
        // a stale value (a future Buildmesh release that shipped a
        // different regex). Re-running provision must REPLACE the
        // stale entry, not skip it.
        let path = home.join("config.toml");
        let mut value: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let hooks_table: &mut toml_edit::ArrayOfTables = value["hooks"]
            .as_array_of_tables_mut()
            .expect("hooks must be an array-of-tables");
        for entry in hooks_table.iter_mut() {
            if table_field(entry, "event") == Some("Notification")
                && table_field(entry, "command").is_some_and(is_buildmesh_command)
            {
                entry.insert(
                    "matcher",
                    toml_edit::Item::Value(toml_edit::Value::from("task\\.waaay_stale")),
                );
            }
        }
        std::fs::write(&path, value.to_string()).unwrap();

        // Re-provision with the proper matcher.
        provision_at(home, 42, 1992, "tok-abcdef");

        let value: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let hooks = read_hooks(&value);
        let notification_matchers: Vec<_> = hooks
            .iter()
            .filter(|entry| {
                table_field(entry, "event") == Some("Notification")
                    && table_field(entry, "command").is_some_and(is_buildmesh_command)
            })
            .filter_map(|entry| table_field(entry, "matcher"))
            .collect();
        assert_eq!(notification_matchers, vec!["task\\.completed"]);
    }

    /// Round-2 review point 2.3 — refuse to silently wipe a
    /// malformed user-authored file.
    #[test]
    fn inject_refuses_to_overwrite_malformed_user_file() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let path = home.join("config.toml");
        let malformed = "hooks = [[\n"; // unclosed array
        std::fs::write(&path, malformed).unwrap();

        let result = ensure_hooks_toml_content(malformed, 42, 1992, "tok-abcdef");
        assert!(
            result.is_err(),
            "provision must refuse a malformed existing file; got {result:?}"
        );
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, malformed,
            "malformed file content must NOT be overwritten"
        );
    }

    /// Missing file → empty document, written normally.
    #[test]
    fn inject_treats_missing_file_as_empty_document() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        assert!(!home.join("config.toml").exists());

        provision_at(home, 42, 1992, "tok-abcdef");
        let written = std::fs::read_to_string(home.join("config.toml")).unwrap();
        let value: DocumentMut = written.parse().expect("valid TOML");
        let hooks = read_hooks(&value);
        assert!(
            hooks
                .iter()
                .any(|entry| table_field(entry, "event") == Some("Stop")),
            "Stop hook must be present after fresh install"
        );
    }

    /// Non-array `hooks` value is a misconfiguration — refuse.
    #[test]
    fn inject_refuses_non_array_hooks_value() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let path = home.join("config.toml");
        std::fs::write(&path, "hooks = 42\n").unwrap();

        let result = ensure_hooks_toml_content("hooks = 42\n", 42, 1992, "tok-abcdef");
        assert!(
            result.is_err(),
            "provision must refuse a non-array hooks value; got {result:?}"
        );
    }

    /// Marker predicate: anchors on stable substrings, not the
    /// URL or the token. A user-written hook that happens to POST
    /// to `/api/attention/` without stdin-forwarding shouldn't be
    /// claimed by the merge.
    #[test]
    fn is_buildmesh_command_recognises_canonical_anchors() {
        assert!(is_buildmesh_command(&hook_command(42, 1992, "tok")));
        assert!(!is_buildmesh_command(""));
        assert!(!is_buildmesh_command("echo hello"));
        assert!(!is_buildmesh_command("curl http://example.com/"));
        assert!(!is_buildmesh_command(
            "curl -X POST http://localhost:1992/api/attention/42"
        ));
        assert!(!is_buildmesh_command(
            "curl -X POST --data-binary @- http://example.com/somewhere/else"
        ));
    }

    /// Idempotency state machine: a node_id change rebuilds the
    /// command string (different baked URL), so the entry is
    /// replaced in place — not appended as a stale duplicate.
    #[test]
    fn inject_replaces_entry_when_node_id_changes() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        provision_at(home, 42, 1992, "tok-abcdef");

        // Mutate the baked command in the on-disk Stop entry to
        // point at node-id 99 (a different node). Re-running
        // provision for node 42 must restore the proper URL.
        let path = home.join("config.toml");
        let mut value: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let hooks_table: &mut toml_edit::ArrayOfTables = value["hooks"]
            .as_array_of_tables_mut()
            .expect("hooks must be an array-of-tables");
        for entry in hooks_table.iter_mut() {
            if table_field(entry, "event") == Some("Stop")
                && table_field(entry, "command").is_some_and(is_buildmesh_command)
            {
                let stale = "curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST \
                 -H \"Content-Type: application/json\" --data-binary @- \
                 \"http://localhost:1992/api/attention/99?token=tok-abcdef\" || true";
                entry.insert("command", toml_edit::Item::Value(toml_edit::Value::from(stale)));
            }
        }
        std::fs::write(&path, value.to_string()).unwrap();

        provision_at(home, 42, 1992, "tok-abcdef");

        let content = std::fs::read_to_string(&path).unwrap();
        let value: DocumentMut = content.parse().unwrap();
        let hooks = read_hooks(&value);
        let stop_commands: Vec<_> = hooks
            .iter()
            .filter(|entry| {
                table_field(entry, "event") == Some("Stop")
                    && table_field(entry, "command").is_some_and(is_buildmesh_command)
            })
            .filter_map(|entry| table_field(entry, "command"))
            .collect();
        assert_eq!(stop_commands.len(), 1, "one Buildmesh Stop hook");
        assert!(
            stop_commands[0].contains("/api/attention/42"),
            "entry restored to node 42"
        );
    }

    // Pin the convenience helpers — the multi-tenant guarantee
    // lives here. If a future refactor changes the per-node
    // layout, the next two tests trip.

    #[test]
    fn kimi_home_from_uses_dot_kimi_buildmesh_subdir() {
        // Subdir name `.kimi-buildmesh` rather than `.kimi` to
        // signal "Buildmesh-managed" vs the user's personal
        // Kimi dir. Two worktrees never share this path; the
        // worktree-cleanup sweep drops it when the node dies.
        let home = kimi_home_from(Path::new("/wt"), 7);
        assert!(home.starts_with("/wt/"));
        assert!(home.to_string_lossy().contains(".kimi-buildmesh"));
    }

    #[test]
    fn kimi_home_from_converts_node_id_to_string() {
        let h = kimi_home_from(Path::new("/wt"), 1234);
        assert_eq!(h, PathBuf::from("/wt/.kimi-buildmesh/1234"));
    }
}
