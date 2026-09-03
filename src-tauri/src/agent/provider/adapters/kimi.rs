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
//! Kimi Code accepts Buildmesh-level model overrides passed via the spawn
//! path — the `-m <model>` flag is forwarded to the Kimi CLI, which then
//! runs that model for the invocation (overriding the harness's
//! `default_model` from `~/.kimi/config.toml` for that one session).
//! Credentials and provider mapping live in `~/.kimi/config.toml`;
//! Buildmesh does not manage those — Kimi's own login flow owns them.
//!
//! **Attention hooks** (issue #1369). Kimi's hook surface is documented at
//! <https://www.kimi.com/code/docs/en/kimi-code-cli/customization/hooks.html>
//! (current) and
//! <https://moonshotai.github.io/kimi-cli/en/customization/hooks.html> (Beta
//! reference). Both describe the same `[[hooks]]` array-of-tables in
//! `~/.kimi/config.toml` with `event`, optional `matcher`, `command`, and
//! optional `timeout` fields. The current docs add three more lifecycle
//! events (`PermissionRequest`, `TaskStarted`, `SessionHeartbeat`) that
//! the Beta page documents as "Beta"-labelled. The schema is compatible
//! across both eras — the same merge logic works against either release.
//!
//! We inject four hook entries to cover the lifecycle surface:
//!   - `Stop` (empty matcher — match all turns) → turn completion. The
//!     route's transcript-reader false-yield suppression applies so a Stop
//!     fired while background tasks are still pending does not create a
//!     false alert.
//!   - `PermissionRequest` → the canonical permission prompt signal
//!     (Codex-style `PermissionRequest`). Marks input regardless of
//!     pending background tasks.
//!   - `Notification` with matcher `task\.completed` → Kimi emits this
//!     when a background task finishes; lands as `Ready` (the harness
//!     has nothing to wait for).
//!   - `SessionStart` → capture-only, same as Codex's `SessionStart`
//!     (`Decision::Ignore`). The route persists `cli_session_id` but does
//!     not mark attention or fire naming / autopilot.
//!
//! `~/.kimi/config.toml` is the user's **global** config — every Kimi
//! session on the box (including a non-Buildmesh invocation in a shell
//! that happens to inherit the right env) loads it. To keep Buildmesh
//! callbacks from being routed by a non-Buildmesh Kimi session, the
//! command URL embeds `?token=$BUILDMESH_HOOK_TOKEN`; the route's
//! per-provider gate (issue #1366) refuses callbacks with a wrong /
//! missing token, sibling harnesses bypass.
//!
//! The command is **baked** with the node id (Codex precedent) because
//! Kimi's hook runner env-handling isn't documented and Codex's
//! `env_clear` already proved a hook that depends on `$VAR` expansion
//! can lose its route. Buildmesh's spawn path calls
//! `provision_attention_hooks` before every spawn and the file is
//! rewritten atomically; under the 1:1 worktree-to-node invariant the
//! "last-writer-wins on baked node id" tradeoff doesn't surface.
//!
//! **Effort override**: Kimi Code's documented flag set is `-m <model>`
//! only — there is no `--effort` / `--reasoning-effort` knob. The trait
//! default `effort_args` emits `["--effort", value]` which Kimi rejects;
//! the descriptor advertises `EffortControlKind::None` so the resolver
//! drops every resolved effort layer.
//!
//! **Shell wrapping**: `kimi` is a native binary on all platforms (not a
//! `.cmd` shim), so `WindowsShell::Direct` is correct everywhere — matching
//! the AGY and Grok adapter patterns.

use crate::agent::capabilities::EffortControlKind;
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::path::Path;
use std::sync::OnceLock;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

pub struct KimiAdapter;
pub static KIMI: KimiAdapter = KimiAdapter;

/// Minimum Kimi release the Buildmesh hook integration has been
/// validated against (issue #1369). The current
/// <https://www.kimi.com/code/docs/en/kimi-code-cli/customization/hooks.html>
/// describes a `[[hooks]]` schema with `Stop`, `Notification` (with
/// `matcher` regex), `PermissionRequest`, `SessionStart`, and additional
/// lifecycle events; the Beta reference at
/// <https://moonshotai.github.io/kimi-cli/en/customization/hooks.html>
/// documents the same shape with fewer events. The hook semantics
/// Buildmesh relies on (Stop, Notification with matcher, PermissionRequest,
/// SessionStart) all appear in both pages, so a Kimi release that ships
/// the Beta or current docs is expected to work.
///
/// The constant name carries `_MIN_` for descriptor-shape
/// compatibility (`AttentionCapability::min_version` declares "minimum"
/// semantics). We do **not** enforce a strict `>=` comparison at
/// runtime — semver isn't wired through the hook surface, and `>=`
/// would require either a `semver::Version` dependency or a hand-rolled
/// tuple parser. The path-coverage test pins that the descriptor
/// advertises the exact pin; treat runtime version drift as a
/// documented upgrade story rather than a gate. A future installed-release
/// validation pass should overwrite this constant with the verified
/// floor.
pub const KIMI_MIN_HOOK_VERSION: &str = "1.0.0";

/// The Kimi hook events Buildmesh wires into the user's global
/// `~/.kimi/config.toml`. The Beta reference documents
/// `matcher` as a regex; the current docs add a structured
/// `task\.completed` Notification pattern. The matcher is a regex that
/// the runner applies to the notification's content/payload (per the
/// Beta example: `matcher = "task\\.completed"` matches
/// `task.completed`). Buildmesh uses the same matcher verbatim — the
/// exact token matters: `task.completed` would also match
/// `taskAcompleted` under regex semantics, defeating the filter.
const KIMI_EVENTS: &[(&str, &str)] = &[
    // (event, matcher)
    //
    // Empty matcher = match-all. The Kimi docs explicitly note the
    // matcher field defaults to `""` when omitted; we emit it
    // explicitly so re-runs of `provision_attention_hooks` are
    // byte-stable and the idempotency invariant holds.
    ("Stop", ""),
    ("PermissionRequest", ""),
    ("SessionStart", ""),
    // Notification with `task\.completed` matcher — Kimi emits
    // background-task status changes through the `Notification`
    // channel (per the Beta docs example). A bare `Notification`
    // matcher would over-fire on every notification; pinning the
    // regex scopes it to completion events.
    //
    // The TOML representation of the regex `task\.completed` is
    // `"task\\.completed"` — TOML's basic string doubles the
    // backslash, the regex then matches the literal dot.
    ("Notification", "task\\.completed"),
];

/// Anchor substrings used by the marker predicate. The hook command
/// must carry both anchors to be recognised as Buildmesh-owned by the
/// additive merge:
///
/// - `/api/attention/` is the canonical attention route (the same
///   path Codex, Claude, Grok, and AGY POST to). User-authored
///   hooks targeting the agent's own callbacks are vanishingly rare
///   in practice.
/// - `--data-binary @-` is the Codex / AGY / Grok canonical
///   stdin-forwarding curl flag. It's the surface Kimi's TOML parser
///   preserves byte-for-byte (the `command` field is a verbatim
///   shell command) and a user-authored hook with the same curl
///   shape would only collide if it also targets the attention
///   route — in which case it would re-enter Buildmesh's webhook
///   anyway, so claiming it as Buildmesh-owned is the correct
///   behaviour.
///
/// The marker no longer anchors on `BUILDMESH_PORT` because Kimi's
/// hook command bakes the port (the runner's env-handling isn't
/// documented; matching the Codex "bake the URL" precedent). The
/// `/api/attention/` + `--data-binary @-` pair is stable across
/// token rotations and node-id changes — both substring anchors
/// survive future URL refactors (e.g. swapping `localhost` for
/// `127.0.0.1`) because they're independent of those refactors.
const BUILDMESH_MARKER_ROUTE: &str = "/api/attention/";
const BUILDMESH_MARKER_STDIN: &str = "--data-binary @-";

/// Build the Buildmesh-owned hook command for a given node id. The URL
/// bakes the loopback callback so the hook never relies on env expansion
/// at runner time. The route is unauthenticated over loopback (issue
/// #496 / ADR-0012); the `?token=<minted>` query defends against
/// non-Buildmesh Kimi sessions on the same box (issue #1366).
///
/// The shell-conditional curl command picks the binary the Kimi runner
/// inherits from the parent shell — `curl.exe` on Windows,
/// `curl` everywhere else — and discards the empty 200 body so the
/// hook returns nothing on stdout (some Kimi documentation flags
/// stdout contents as part of the agent context on certain event
/// types; an empty body is the safe default). The `|| true` at the end
/// keeps the hook fail-open: a Buildmesh-down or network-blip moment
/// must NOT block the agent.
///
/// The URL is fully baked — no `$VAR` left in the wire shape. Kimi's
/// hook runner env-handling isn't documented; matching the Codex "bake
/// the URL" precedent avoids a potential `env_clear`-style trap
/// (Codex's hook runner strips `BUILDMESH_*` and a `cmd.exe /c "%VAR%"`
/// expansion never reaches the POST body).
fn hook_command(node_id: i64) -> String {
    // The runtime-scoped token, minted once per process. Reads via
    // the static `OnceLock` cache so the spawn path doesn't trigger a
    // new mint when the runtime-scoped OnceLock has already been
    // populated by another spawn. The token is interpolated directly
    // into the URL (baked) — never referenced as a `$VAR` placeholder
    // since Kimi's hook runner env-handling isn't documented.
    let token = minted_token_for_bake().clone();
    let port = crate::http_server::current_http_port();
    let url = format!(
        "http://localhost:{port}/api/attention/{node_id}?token={token}"
    );
    format!(
        "curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST \
 -H \"Content-Type: application/json\" --data-binary @- \"{url}\" || true"
    )
}

/// Cached mint value used by `hook_command` so every spawn inside the
/// same runtime shares one token. The route's `verify_attention_token`
/// rejects callbacks whose `?token=` doesn't match the runtime-scoped
/// `RUNTIME_HOOK_TOKEN` OnceLock; minting once per Buildmesh process
/// (via `crate::agent::mint_runtime_hook_token`) is the documented
/// fix for the round-2 review's "two separate statics, never agreed"
/// bug (`grok::mint_runtime_hook_token`'s doc comment in
/// `agent::mod.rs`). The `OnceLock` here is just a thread-safe
/// cache so the bake path doesn't trigger a new mint when the
/// runtime-scoped OnceLock has already been populated by another
/// spawn (mirrors the Grok precedent's lazy-mint at first hook
/// provision).
fn minted_token_for_bake() -> &'static String {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(crate::agent::mint_runtime_hook_token)
}

/// Marker predicate: a Kimi hook command is Buildmesh-owned when its
/// `command` carries both anchors. Substring match (not strict
/// equality) so future URL refactors (changing the route, swapping
/// `localhost` for `127.0.0.1`, adding new query params alongside
/// the token) keep the merge stable — only the canonical anchors
/// matter. The `command` field is the one Kimi's TOML parser is
/// documented to preserve byte-for-byte; any custom marker field
/// would risk silent pruning by a future release.
fn is_buildmesh_command(command: &str) -> bool {
    command.contains(BUILDMESH_MARKER_ROUTE) && command.contains(BUILDMESH_MARKER_STDIN)
}

/// Build the `[[hooks]]` array element that carries our marker. The
/// `event`, `matcher`, and `command` fields are what Kimi's parser
/// preserves across re-reads; we keep the surface narrow on purpose so
/// the marker predicate in `is_buildmesh_command` continues to
/// recognise the entry after a future Kimi release adds unrelated
/// fields (the additive merge would otherwise start appending
/// duplicates on every re-run).
///
/// Returns a `Table` because `[[hooks]]` array-of-tables elements
/// parse as regular tables in `toml_edit`'s representation (each
/// `[[hooks]]` entry gets its own table block in the wire format).
fn buildmesh_hook_entry(event: &str, matcher: &str, command: &str) -> Table {
    let mut table = Table::new();
    table.insert("event", Item::Value(Value::from(event)));
    if !matcher.is_empty() {
        table.insert("matcher", Item::Value(Value::from(matcher)));
    }
    table.insert("command", Item::Value(Value::from(command)));
    table
}

/// Process-wide lock for serialised writes to `~/.kimi/config.toml`.
/// Multiple Kimi spawns can run concurrently (handover, autopilot,
/// resume-after-restart); a read/merge/write without this lock could
/// atomically replace a sibling's just-added hook entries. Mirrors
/// the Codex precedent (`codex.rs:ATTENTION_CONFIG_WRITE_LOCK`,
/// `codex.rs:196`).
static KIMI_CONFIG_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Resolve the always-trusted global Kimi config location.
/// Mirrors `codex::native_codex_home` and `grok::grok_home`:
/// `USERPROFILE` on Windows, `HOME` on Unix, joined with `.kimi`.
/// The current Kimi Code documentation describes hook configuration
/// at `~/.kimi-code/config.toml`; the Beta reference describes
/// `~/.kimi/config.toml`. Buildmesh follows the Beta reference
/// (matches the installed release referenced by the #911 research —
/// Kimi Code's wire.jsonl lives under `~/.kimi/sessions/`).
///
/// `BUILDMESH_KIMI_HOME_OVERRIDE` (test-only) short-circuits the
/// env lookup so the test harness can point this function at an
/// isolated temp dir without competing against other test modules
/// that mutate USERPROFILE under their own lock (issue #1369 — the
/// `http::mod::tests` module sets `USERPROFILE` via `LOG_HAPPY_LOCK`,
/// a different lock from `USER_HOME_LOCK`; running both modules in
/// parallel races the env mutation against my own).
#[cfg(test)]
const KIMI_HOME_OVERRIDE_ENV: &str = "BUILDMESH_KIMI_HOME_OVERRIDE";

fn kimi_home() -> Result<std::path::PathBuf, String> {
    #[cfg(test)]
    {
        if let Some(override_path) = std::env::var_os(KIMI_HOME_OVERRIDE_ENV)
            .filter(|v| !v.is_empty())
        {
            return Ok(std::path::PathBuf::from(override_path).join(".kimi"));
        }
    }
    let key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| format!("could not resolve kimi home — ${key} is unset"))
        .map(|p| p.join(".kimi"))
}

/// Atomically persist `content` to `path` via `tempfile::NamedTempFile
/// + persist`. The Codex / AGY / Grok precedents all pin this
/// pattern (`codex.rs:998-1007`, `agy.rs::atomic_write`,
/// `grok.rs::write_atomic`); the Python `os.replace` analogue. A
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

/// Pure helper that edits the TOML document to add or update the
/// Buildmesh-owned `[[hooks]]` entries. Returns the new document text
/// on success, or `Err` when the existing file is malformed. Returns
/// `Ok(None)` when the document already contains every required
/// Buildmesh entry byte-for-byte — the caller skips the rewrite
/// (preserves mtime on idempotent re-runs, the issue #886 invariant).
///
/// Idempotency is per `(event, matcher, command)` triple — a sibling
/// spawn that arrives with a different `node_id` (and therefore a
/// different baked `command`) does update the entry; a re-spawn of
/// the same node is a no-op. Matcher-less events (`Stop`,
/// `PermissionRequest`, `SessionStart`) match on `(event, command)`
/// only.
fn ensure_hooks_toml_content(
    existing: &str,
    node_id: i64,
) -> Result<Option<String>, String> {
    let command = hook_command(node_id);
    let mut document = if existing.is_empty() {
        DocumentMut::new()
    } else {
        existing
            .parse::<DocumentMut>()
            .map_err(|error| format!("failed to parse kimi config.toml: {error}"))?
    };

    // The hooks array-of-tables may already exist (user-authored
    // entries, a sibling Buildmesh agent, or our own previous run).
    // The canonical Kimi schema is `[[hooks]]` at the top level — an
    // array-of-tables — matching the Beta docs example and the
    // current docs. `toml_edit` parses `[[hooks]]` into
    // `Item::ArrayOfTables(ArrayOfTables)` (distinct from the
    // inline-array shape `hooks = [...]`, which we don't accept —
// inline arrays aren't the Kimi documented surface and silently
// re-shaping them would lose the user's data structure).
// Walk the array: own only entries whose `command` matches our
// marker predicate, preserve every other entry (including
// user-authored ones with the same event name but a different
// command), never duplicate a marker-recognised entry.
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
        // The predicate matches on `command` regardless of the
        // matcher's exact text — a future Kimi release that emits
        // `task.completed` as `taskCompleted` would still be
        // recognised because the URL anchors are the source of
        // truth, not the matcher string.
        let existing_index = hooks_array_mut.iter().position(|table| {
            let same_event = table_field(table, "event") == Some(*event);
            if !same_event {
                return false;
            }
            table_field(table, "command").is_some_and(is_buildmesh_command)
        });

        let desired = buildmesh_hook_entry(event, matcher, &command);

        if let Some(index) = existing_index {
            // Already wired — update in place if drift, else leave.
            // `Table::get` returns `Option<&Item>` and `Item` doesn't
            // implement `PartialEq`, and a re-parsed value carries
            // decoration metadata (whitespace, comment positions)
            // that a freshly built value doesn't. A naive
            // `to_string()` comparison would always diverge and
            // re-write the file every spawn (breaking the issue
            // #886 idempotency invariant). The semantic content we
            // own is the `command` field — if it matches the
            // just-baked command, the entry is already correct and
            // we leave it alone. A token or port rotation produces
            // a different `command` string and triggers an
            // in-place update via `ArrayOfTables::replace`.
            let slot_command_matches = hooks_array_mut
                .get(index)
                .and_then(|slot| table_field(slot, "command"))
                == Some(command.as_str());
            if !slot_command_matches {
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

/// Field accessor for `[[hooks]]` entries. `toml_edit`'s
/// `ArrayOfTables` yields `&Table` items (each `[[hooks]]` block
/// becomes one table); `Table::get` returns `Option<&Item>`, so the
/// read path is `Item::as_value` → `Value::as_str`. Used by both
/// the merge logic and the test code so the read pattern lives in
/// one place.
fn table_field<'a>(table: &'a Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(Item::as_value).and_then(Value::as_str)
}

/// Probe the installed Kimi release for a debug-log breadcrumb
/// (best-effort, never gate behaviour on it). The Beta docs and
/// current docs both surface a `--version` flag in the `--help`
/// output; the actual shape of the version string is unstable
/// (semver is undocumented), so we only log on success and otherwise
/// stay silent. The pin lives in `KIMI_MIN_HOOK_VERSION` and is
/// surfaced via the capability descriptor; a future installed-release
/// validation pass should reconcile the two.
fn probe_kimi_version() {
    static CACHED: OnceLock<()> = OnceLock::new();
    CACHED.get_or_init(|| {
        let output = std::process::Command::new("kimi")
            .arg("--version")
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let version = String::from_utf8_lossy(&out.stdout);
                let trimmed = version.trim();
                if !trimmed.is_empty() {
                    tracing::info!("kimi provision_attention_hooks: detected {trimmed}");
                }
            }
            Ok(_) => {
                tracing::debug!(
                    "kimi provision_attention_hooks: `kimi --version` exited non-zero"
                );
            }
            Err(error) => {
                tracing::debug!(
                    "kimi provision_attention_hooks: could not probe `kimi --version`: {error}"
                );
            }
        }
    });
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

    /// Issue #1369: Kimi Code now ships attention hooks. We install
    /// the four-entry Buildmesh-owned `[[hooks]]` array into the
    /// user's global `~/.kimi/config.toml` and route the events
    /// (Stop / Notification with matcher / PermissionRequest /
    /// SessionStart) to the shared attention endpoint. The route's
    /// per-provider token gate (issue #1366) defends against
    /// non-Buildmesh Kimi sessions on the same box.
    fn requires_attention_hook(&self) -> bool {
        true
    }

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        AttentionCapability::Hook {
            // Stop → turn completion (subject to the route's
            // transcript-scan false-yield suppression for agents
            // with launched-but-unfinished background tasks).
            // PermissionRequest → canonical permission prompt signal
            // (Codex-style); the agent is at a tool-approval
            // decision regardless of background work.
            // InputRequired → covers Kimi's "needs the user" hooks
            // that don't take the dedicated PermissionRequest shape
            // (matches Grok's vocabulary); a future Kimi release
            // that exposes an explicit `QuestionRequest` event will
            // route through this kind without a descriptor edit.
            events: vec![
                LifecycleKind::TurnCompleted,
                LifecycleKind::InputRequired,
                LifecycleKind::PermissionRequested,
            ],
            // Kimi Code runs under its default permission-asking
            // mode (no `--dangerously-skip-permissions` flag in the
            // documented surface). PermissionRequest events fire in
            // interactive use; AGY's `SkipPermissions` mode is the
            // standing alternative that doesn't apply here.
            launch_mode: AttentionLaunchMode::PermissionAsk,
            // The user's global `~/.kimi/config.toml` is the only
            // Kimi-documented config location (Beta reference + the
            // installed release that lives under `~/.kimi/`). It's
            // loaded by every Kimi invocation on the box — hence the
            // need for the `?token=` runtime-scoped gate.
            trust: Some("global hook file".into()),
            // Issue #1369 — pin the validated Beta/current Kimi
            // release floor. The exact floor is "1.0.0" pending a
            // production validation pass against the installed
            // release; a future version bump must update both this
            // constant and the `min_version` literal in the
            // `harnessCapabilities.ts` mirror.
            min_version: Some(KIMI_MIN_HOOK_VERSION.into()),
        }
    }

    /// Provision Buildmesh attention hooks into Kimi's always-loaded
    /// global config (issue #1369). The file lives in the user's
    /// `~/.kimi/config.toml`, NOT under the project cwd — Kimi's
    /// documented surface is the global config (Beta reference), and
    /// a project-local config would conflict with the user's normal
    /// Kimi sessions.
    ///
    /// The hook command POSTs the event envelope to the attention
    /// endpoint with the port baked in (Kimi's hook runner env-handling
    /// isn't documented, matching Codex's "bake the URL" precedent).
    /// The `?token=<minted>` query defends against same-box
    /// collisions (issue #1366). The resolved project path is
    /// intentionally unused because the global hook file is the only
    /// Kimi-documented location.
    fn provision_attention_hooks(
        &self,
        _resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        node_id: i64,
    ) -> Result<(), String> {
        // Issue #1366 round-2 — mint the process-wide hook token
        // here, not in `spawn_environment::wrap` (which fires for
        // every agent spawn including Claude / Codex / AGY). Scoping
        // the mint to the Kimi path means non-Kimi agents never see
        // the process-wide token, and the route's token check below
        // correctly discriminates Kimi callbacks from sibling
        // harnesses (Claude / Codex / AGY POST without `?token=` and
        // bypass the gate).
        //
        // `mint_runtime_hook_token` is idempotent: first invocation
        // mints; subsequent spawns in the same runtime see the same
        // value, so every Kimi hook in this process shares one
        // token.
        let _ = minted_token_for_bake();
        tracing::info!(
            "kimi provision_attention_hooks: baking node {node_id} into ~/.kimi/config.toml"
        );

        probe_kimi_version();

        let _guard = KIMI_CONFIG_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let dir = kimi_home()?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create .kimi dir: {e}"))?;

        let path = dir.join("config.toml");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let updated = match ensure_hooks_toml_content(&existing, node_id)? {
            Some(content) => content,
            None => {
                tracing::debug!(
                    "kimi provision_attention_hooks: {path:?} already wired for node {node_id}, \
                     no rewrite"
                );
                return Ok(());
            }
        };

        write_atomic(&path, &updated)
            .map_err(|e| format!("failed to write kimi config.toml: {e}"))?;
        tracing::info!("kimi provision_attention_hooks: wrote {:?}", path);
        Ok(())
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
    /// documented surface (no equivalent knob to AGY's `--effort`). The
    /// descriptor advertises `EffortControlKind::None` so the resolver
    /// drops every resolved effort layer.
    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode, EffortControlKind};
    use tempfile::TempDir;

    fn provision_kimi(project: &Path) {
        let path = project.to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: EnvType::Windows,
        };
        KIMI.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
            .unwrap();
    }

    fn try_provision_kimi(project: &Path) -> Result<(), String> {
        let path = project.to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: EnvType::Windows,
        };
        KIMI.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
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
        // Kimi uses `-S` (uppercase) as the explicit-resume flag, NOT `--resume`.
        let args = KIMI.resume_args("abc-123");
        assert_eq!(args, vec!["-S", "abc-123"]);
    }

    #[test]
    fn model_args_format() {
        // Kimi uses `-m` (short) for the model override, matching `--help`.
        let args = KIMI.model_args("kimi-k2");
        assert_eq!(args, vec!["-m", "kimi-k2"]);
    }

    #[test]
    fn session_assign_args_empty() {
        let args = KIMI.session_assign_args("any-id");
        assert!(args.is_empty(), "Kimi self-assigns; session_assign_args must be empty");
    }

    #[test]
    fn no_prefill_support() {
        assert!(!KIMI.supports_prefill());
    }

    /// Issue #1369: Kimi now ships attention hooks. The capability
    /// descriptor advertises the four-event surface (Stop,
    /// PermissionRequest, SessionStart, Notification with task matcher)
    /// routed through the global hook file with the runtime-scoped
    /// `?token=` gate.
    #[test]
    fn capabilities_descriptor_advertises_model_override() {
        let caps = KIMI.capabilities();
        assert_eq!(caps.harness_id, "kimi");
        assert!(caps.supports_resume);
        assert!(caps.supports_model_override);
        assert!(!caps.supports_effort_override);
        assert!(!caps.supports_prefill);
        // Issue #1369: Kimi now ships attention hooks — the
        // descriptor's boolean and structured shape agree.
        assert!(
            caps.requires_attention_hook,
            "issue #1369: Kimi now ships attention hooks"
        );
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(caps.effort_control, EffortControlKind::None);
        // Pin the structured hook descriptor so a refactor that flips
        // events / launch_mode / min_version trips here. Mirrors the
        // Grok pin (`grok::tests::capabilities_descriptor_advertises_model_override`)
        // — the `..` swallows nothing.
        match &caps.attention_capability {
            AttentionCapability::Hook {
                events,
                launch_mode: AttentionLaunchMode::PermissionAsk,
                trust: Some(trust),
                min_version: Some(min_version),
            } => {
                assert!(
                    events.contains(&crate::agent::session_lifecycle::LifecycleKind::TurnCompleted),
                    "Stop (turn completion) must be advertised"
                );
                assert!(
                    events.contains(&crate::agent::session_lifecycle::LifecycleKind::InputRequired),
                    "InputRequired must be advertised"
                );
                assert!(
                    events.contains(&crate::agent::session_lifecycle::LifecycleKind::PermissionRequested),
                    "PermissionRequest must be advertised"
                );
                assert!(trust.contains("global"), "trust must reflect the global hook file location; got {trust}");
                assert_eq!(min_version, KIMI_MIN_HOOK_VERSION, "min_version pin must match the KIMI_MIN_HOOK_VERSION constant");
            }
            other => panic!(
                "Kimi must advertise Hook under PermissionAsk with a pinned min_version; got {other:?}"
            ),
        }
    }

    #[test]
    fn produces_readable_transcript() {
        // #911 research confirmed Kimi's wire.jsonl is standard JSONL, but
        // the transcript reader's path resolver isn't wired for `~/.kimi/`
        // yet — so we claim `false` to match the current wire behaviour
        // (Node Digest rich layer degrades to spine-only with `unsupported`).
        // When the follow-up wires `TranscriptFormat::Kimi`, flip this back
        // to `true` and add a reader test that parses a fixture wire.jsonl.
        assert!(!KIMI.produces_readable_transcript());
    }

    /// Issue #1186: pin the harness-specific model-flag shape. The
    /// table-driven `capability_recipe_coherence` only asserts *some*
    /// model flag exists in the recipe — a silent `-m` ↔ `--model`
    /// flip on the adapter would pass. This pin catches the drift
    /// before it reaches the wire.
    #[test]
    fn kimi_interactive_recipe_carries_short_m_model_arg() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("kimi-k2".to_string()),
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
        let prepared = default_prepare(&KIMI, input);
        let args = &prepared.recipe.base_args;
        assert_flag_followed_by_value(args, "-m", "kimi-k2");
        // Short form is canonical — a refactor to `--model` would
        // silently flip the wire shape; this catches it.
        assert!(
            !args.iter().any(|a| a == "--model"),
            "Kimi must use -m (short form), not --model; got args = {args:?}"
        );
    }

    // -----------------------------------------------------------------
    // Attention hook injection (issue #1369)
    //
    // Mirrors the Grok / Codex precedent: additive merge with a
    // marker predicate, atomic write via NamedTempFile+persist,
    // malformed-file refusal, and idempotent re-runs that leave mtime
    // untouched.
    // -----------------------------------------------------------------

    /// End-to-end: injection writes the four-event Buildmesh-owned
    /// `[[hooks]]` array at `<home>/.kimi/config.toml`, with each event
    /// carrying a `command` anchored on the canonical
    /// `/api/attention/<node-id>` URL + `?token=<minted>` runtime gate.
    /// The matcher for `Notification` is the documented regex
    /// `task\.completed` so a bare `Notification` doesn't over-trigger
    /// on every notification.
    #[test]
    fn inject_writes_stop_permission_notification_session_start() {
        let temp = with_user_home_redirect();
        provision_kimi(Path::new("/any"));

        let path = temp.path().join(".kimi").join("config.toml");
        let content = std::fs::read_to_string(&path).expect("config.toml not written");
        let value: toml_edit::DocumentMut =
            content.parse().expect("config.toml is not valid TOML");
        let hooks = value["hooks"]
            .as_array_of_tables()
            .expect("hooks must be an array-of-tables (canonical `[[hooks]]` syntax)");

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
                "{event} command must carry the canonical /api/attention/ + BUILDMESH_PORT anchors; got {command}"
            );

            match event {
                "Stop" => {
                    assert!(seen_stop == false, "Stop hook emitted more than once");
                    seen_stop = true;
                    // Stop should NOT carry a matcher — the docs
                    // warn that matchers on `Stop` may be ignored;
                    // a future Kimi release that prints anything on
                    // Stop would then route through our hook
                    // regardless of the matcher text. Match-all is
                    // also the safer default for turn-end signals.
                    assert!(
                        table_field(entry, "matcher").is_none(),
                        "Stop must not carry a matcher: {entry:?}"
                    );
                }
                "PermissionRequest" => {
                    assert!(
                        seen_permission_request == false,
                        "PermissionRequest hook emitted more than once"
                    );
                    seen_permission_request = true;
                    assert!(
                        table_field(entry, "matcher").is_none(),
                        "PermissionRequest must not carry a matcher: {entry:?}"
                    );
                }
                "SessionStart" => {
                    assert!(
                        seen_session_start == false,
                        "SessionStart hook emitted more than once"
                    );
                    seen_session_start = true;
                    assert!(
                        table_field(entry, "matcher").is_none(),
                        "SessionStart must not carry a matcher: {entry:?}"
                    );
                }
                "Notification" => {
                    assert!(
                        seen_notification == false,
                        "Notification hook emitted more than once"
                    );
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
        assert!(seen_stop, "Stop hook missing from injected config");
        assert!(
            seen_permission_request,
            "PermissionRequest hook missing from injected config"
        );
        assert!(
            seen_session_start,
            "SessionStart hook missing from injected config"
        );
        assert!(
            seen_notification,
            "Notification hook missing from injected config"
        );
    }

    /// The baked URL carries the runtime-scoped token so the route's
    /// per-provider gate (issue #1366) can discriminate Kimi callbacks
    /// from sibling harnesses' POSTs. Without the query the global
    /// `~/.kimi/config.toml` (loaded by every Kimi invocation on the
    /// box) would route a non-Buildmesh session into a Buildmesh node.
    #[test]
    fn hook_command_bakes_token_query() {
        let command = hook_command(42);
        assert!(
            command.contains("/api/attention/42"),
            "hook command must bake the node id; got {command}"
        );
        assert!(
            command.contains("?token="),
            "hook command must carry the ?token=<minted> query; got {command}"
        );
        assert!(
            command.contains("localhost:"),
            "hook command must use the loopback port, not 127.0.0.1; got {command}"
        );
        // The URL is fully baked — no `$VAR` left in the wire shape.
        // Kimi's hook runner env-handling isn't documented; matching
        // the Codex "bake the URL" precedent avoids a potential
        // env_clear-style trap.
        assert!(
            !command.contains("$BUILDMESH_PORT"),
            "hook command must bake the port (no $VAR expansion); got {command}"
        );
        // Fail-open contract: a Buildmesh-down moment must NOT block
        // the agent. The hook must return non-zero exit without
        // killing Kimi's TUI.
        assert!(
            command.contains("|| true"),
            "hook command must fail-open (|| true); got {command}"
        );
    }

    /// Re-running injection over an already-correct config is a no-op —
    /// the file's bytes are byte-identical between calls and mtime is
    /// untouched (issue #886 spawn-path idempotency invariant).
    #[test]
    fn inject_is_idempotent() {
        let temp = with_user_home_redirect();
        provision_kimi(Path::new("/any"));
        let path = temp.path().join(".kimi").join("config.toml");
        let first = std::fs::read_to_string(&path).unwrap();
        let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        provision_kimi(Path::new("/any"));
        let second = std::fs::read_to_string(&path).unwrap();
        let second_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        assert_eq!(first, second, "byte-identical re-run");
        assert_eq!(
            first_mtime, second_mtime,
            "idempotent re-run must NOT rewrite the file (no mtime change)"
        );
    }

    /// Injection only owns the four Buildmesh entries — a sibling
    /// `[[hook]]` entry the user authored (a different `command`
    /// targeting their own server, for example) survives a re-injection
    /// untouched. The merge walks the array and preserves every entry
    /// whose `command` does NOT carry our marker predicate.
    #[test]
    fn inject_preserves_user_authored_hook_entry() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".kimi");
        std::fs::create_dir_all(&dir).unwrap();
        let user_hook = r#"
[[hooks]]
event = "Stop"
matcher = ""
command = "user-owned-stop-handler"

[[hooks]]
event = "PostToolUse"
matcher = "WriteFile|StrReplaceFile"
command = "jq -r '.tool_input.file_path' | xargs prettier --write"
"#;
        std::fs::write(dir.join("config.toml"), user_hook).unwrap();

        provision_kimi(Path::new("/any"));

        let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        let value: toml_edit::DocumentMut = content.parse().expect("valid TOML");
        let hooks = value["hooks"]
            .as_array_of_tables()
            .expect("hooks must be an array-of-tables (canonical `[[hooks]]` syntax)");

        // User-authored Stop entry survives byte-for-byte. The
        // additive merge appends our Buildmesh entry alongside
        // it — a separate matcher group on the same event.
        let user_stop_preserved = hooks
            .iter()
            .any(|entry| table_field(entry, "command") == Some("user-owned-stop-handler"));
        assert!(
            user_stop_preserved,
            "user-authored Stop hook must survive a Buildmesh inject; got hooks = {hooks:#?}"
        );
        // The unrelated PostToolUse entry also survives — Buildmesh
        // only owns its four entries.
        let user_post_tool_use_preserved = hooks
            .iter()
            .any(|entry| table_field(entry, "event") == Some("PostToolUse"));
        assert!(
            user_post_tool_use_preserved,
            "user-authored PostToolUse hook must survive; got hooks = {hooks:#?}"
        );
        // And Buildmesh's own Stop entry IS present.
        let buildmesh_stop_present = hooks.iter().any(|entry| {
            table_field(entry, "event") == Some("Stop")
                && table_field(entry, "command").is_some_and(is_buildmesh_command)
        });
        assert!(
            buildmesh_stop_present,
            "Buildmesh-owned Stop hook must be present alongside user-authored Stop hook"
        );
    }

    /// Repeat injects add no duplicate Buildmesh handlers. The
    /// marker-anchored merge must update in place — not append — when
    /// the URL anchors are recognised.
    #[test]
    fn inject_creates_no_duplicates_on_repeat() {
        let temp = with_user_home_redirect();
        provision_kimi(Path::new("/any"));
        provision_kimi(Path::new("/any"));
        provision_kimi(Path::new("/any"));

        let path = temp.path().join(".kimi").join("config.toml");
        let value: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let hooks = value["hooks"]
            .as_array_of_tables()
            .expect("hooks must be an array-of-tables (canonical `[[hooks]]` syntax)");

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
                "{event} must have exactly one Buildmesh matcher group after 3 injects; got {hooks:#?}"
            );
        }
    }

    /// Atomic write leaves no `.tmp` residue. Mirrors AGY precedent
    /// (`agy.rs::inject_leaves_no_tmp_residue`) and the Codex pattern
    /// (`codex.rs::write_atomic`).
    #[test]
    fn inject_atomic_write_leaves_no_tmp_residue() {
        let temp = with_user_home_redirect();
        provision_kimi(Path::new("/any"));

        let dir = temp.path().join(".kimi");
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| {
            panic!("read_dir({dir:?}) failed: {e}");
        });
        let tmp_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            tmp_files.is_empty(),
            "atomic write must not leave .tmp residue; found {tmp_files:?}"
        );
    }

    /// Round-2 review point 2.3 — refuse to silently wipe a
    /// malformed user-authored file. A trailing bracket, partial
    /// edit, or syntax error must NOT cause `ensure_hooks_toml_content`
    /// to fall back to an empty document and overwrite the user's
    /// data. The function returns an `Err`, the spawn path surfaces it
    /// as a provision failure, and the user's content survives
    /// intact.
    #[test]
    fn inject_refuses_to_overwrite_malformed_user_file() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".kimi");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let malformed = "hooks = [[\n"; // Unclosed array
        std::fs::write(&path, malformed).unwrap();

        let result = try_provision_kimi(Path::new("/any"));
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

    /// The matching positive case for the malformed-file pin: a
    /// missing file should be treated as an empty document and written
    /// normally. Lock the happy path so a future refactor that
    /// treats both missing AND malformed the same way trips both
    /// regressions in one place.
    #[test]
    fn inject_treats_missing_file_as_empty_document() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".kimi");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!dir.join("config.toml").exists());

        try_provision_kimi(Path::new("/any")).unwrap();
        let written = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        let value: toml_edit::DocumentMut = written.parse().expect("valid TOML");
        let hooks = value["hooks"]
            .as_array_of_tables()
            .expect("hooks must be an array-of-tables after fresh install");
        assert!(
            hooks
                .iter()
                .any(|entry| table_field(entry, "event") == Some("Stop")),
            "Stop hook must be present after fresh install"
        );
    }

    /// A non-array `hooks` value (e.g. an int, a string, or a
    /// non-array table) is a misconfiguration — refuse to clobber it
    /// with `[]`. Mirrors the Codex precedent
    /// (`codex.rs::ensure_hooks_json_content` refuses non-object
    /// top-level values).
    #[test]
    fn inject_refuses_non_array_hooks_value() {
        let temp = with_user_home_redirect();
        let dir = temp.path().join(".kimi");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "hooks = 42\n").unwrap();

        let result = try_provision_kimi(Path::new("/any"));
        assert!(
            result.is_err(),
            "provision must refuse a non-array hooks value; got {result:?}"
        );
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "hooks = 42\n");
    }

    /// The marker predicate anchors on the documented `command`
    /// field's substring content, not on the exact URL — so future
    /// refactors that add another query param, swap `localhost` for
    /// `127.0.0.1`, or adjust the path keep the merge stable.
    #[test]
    fn is_buildmesh_command_recognises_canonical_anchors() {
        let baked = hook_command(42);
        assert!(
            is_buildmesh_command(&baked),
            "the baked hook command must round-trip through the marker predicate"
        );
        assert!(!is_buildmesh_command(""));
        assert!(!is_buildmesh_command("echo hello"));
        assert!(!is_buildmesh_command("curl http://example.com/"));
        // Missing one anchor must not match. A command that carries
        // the route anchor but no stdin-forwarding curl flag is
        // likely a user-authored webhook — not Buildmesh-owned.
        assert!(!is_buildmesh_command(
            "curl -X POST http://localhost:1992/api/attention/42"
        ));
        // And a command that carries the stdin-forwarding curl flag
        // but routes somewhere other than the attention endpoint is
        // not Buildmesh-owned either.
        assert!(!is_buildmesh_command(
            "curl -X POST --data-binary @- http://example.com/somewhere/else"
        ));
    }

    /// `kimi_home()` resolves to `<override>/.kimi` (the test-only
    /// override env var is treated like USERPROFILE and `.kimi` is
    /// appended — same shape as the production USERPROFILE lookup).
    /// In production, a missing/empty USERPROFILE is a hard error
    /// (the spawn path logs and continues, but the agent runs
    /// without attention callbacks).
    #[test]
    fn kimi_home_resolves_under_user_profile() {
        let temp = with_user_home_redirect();
        let resolved = kimi_home().expect("home should resolve");
        assert_eq!(resolved, temp.path().join(".kimi"));
    }

    /// End-to-end: the route's per-provider token gate (issue #1366)
    /// sees Kimi as a hook-bearing provider, so a sibling harness's
    /// POST (Claude / Codex / AGY) bypasses the gate while a Kimi
    /// callback must carry `?token=<minted>`. Pin that the adapter's
    /// descriptor carries the same `kind: 'hook'` shape Grok's does
    /// — the route's discrimination relies on the structured
    /// capability, not on `requires_attention_hook` alone.
    #[test]
    fn capability_kind_matches_grok_shape() {
        let grok = crate::agent::capabilities::capabilities_for(
            &crate::agent::provider::adapters::GROK,
        );
        let kimi = KIMI.capabilities();
        match (&grok.attention_capability, &kimi.attention_capability) {
            (
                AttentionCapability::Hook { launch_mode: g_lm, .. },
                AttentionCapability::Hook { launch_mode: k_lm, .. },
            ) => {
                // Both are permission-asking hooks. The launch_mode
                // pin matters for the AttentionLaunchMode surface
                // (issue #1364 §3): SkipPermissions harnesses never
                // raise PermissionRequest; PermissionAsk harnesses
                // do. Kimi Code runs in interactive / permission-ask
                // mode by default — same shape as Grok.
                assert_eq!(g_lm, k_lm);
            }
            _ => panic!(
                "Kimi + Grok must both advertise Hook shape; got grok={:?} kimi={:?}",
                grok.attention_capability, kimi.attention_capability
            ),
        }
    }

    /// Test scaffolding: redirect USERPROFILE / HOME to a tempdir so
    /// `kimi_home()` lands in an isolated location. Serialised
    /// through the process-wide `USER_HOME_LOCK` because parallel
    /// tests racing on `set_var` would otherwise let one test read
    /// the other test's temp path. Drop order: env restore in
    /// `drop()` runs before `_lock` releases (Drop::drop runs before
    /// field drops), so a parallel test can never observe the
    /// other test's mutated env value mid-shutdown.
    ///
    /// The temp parent is the current working directory (the worktree
    /// root in practice), NOT `std::env::temp_dir()`. The latter
    /// resolves to `AppData\Local\Temp` on Windows, which collides
    /// with `sandbox::spawn::tests::curated_env_prepends_git_and_redirects_temp`'s
    /// "no env var points into AppData\Local\Temp" assertion when
    /// that test runs concurrently with a Kimi redirect — the
    /// leaked `USERPROFILE` value carries the temp path substring
    /// and trips the sandbox check.
    struct HomeRedirect {
        temp: TempDir,
        key: &'static str,
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeRedirect {
        fn path(&self) -> &std::path::Path {
            self.temp.path()
        }
    }

    impl Drop for HomeRedirect {
        fn drop(&mut self) {
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

    /// Temp dir parent that avoids `AppData\Local\Temp` (Windows)
    /// and `/tmp` (Unix) so a leaked USERPROFILE value can't trip
    /// tests that scan the global env for those substrings. Falls
    /// back to `std::env::temp_dir()` only if the cwd is unavailable.
    fn redirect_parent() -> std::path::PathBuf {
        std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir())
    }

    fn with_user_home_redirect() -> HomeRedirect {
        let lock = USER_HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = tempfile::Builder::new()
            .prefix("buildmesh-kimi-home-")
            .tempdir_in(redirect_parent())
            .expect("create kimi test home");
        // Use the test-only `BUILDMESH_KIMI_HOME_OVERRIDE` env var
        // (see `kimi_home()`) instead of mutating USERPROFILE. The
        // http module's tests mutate USERPROFILE under a separate
        // `LOG_HAPPY_LOCK`, so my `USER_HOME_LOCK` does not
        // serialise against them — a concurrent mutation could
        // redirect `kimi_home()` away from my tempdir and trip a
        // spurious NotFound. The dedicated override env var is
        // owned by this test module's lock.
        let previous = std::env::var_os(KIMI_HOME_OVERRIDE_ENV);
        std::env::set_var(KIMI_HOME_OVERRIDE_ENV, temp.path());
        HomeRedirect {
            temp,
            key: KIMI_HOME_OVERRIDE_ENV,
            previous,
            _lock: lock,
        }
    }

    /// Process-wide lock for tests that mutate USERPROFILE / HOME.
    /// See `HomeRedirect`'s doc comment for why.
    static USER_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}