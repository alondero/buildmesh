//! MiniMax Code CLI provider adapter — MiniMax's full-screen interactive
//! coding agent, installed on PATH as a single `mcode` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. Buildmesh's PTY backend (ConPTY on
//! Windows, native PTY on macOS/Linux) fully supports full-screen TUI rendering,
//! so we launch in interactive mode everywhere.
//!
//! **Session resumption** uses `--session [<id>]` or `-c` / `--continue`.
//! MiniMax Code auto-assigns its own session ids. No PTY banner shape is
//! verified for the TUI, so PTY capture stays off. SessionStart is provisioned
//! to capture the workspace and `mvs_<hex>` identity, but delivery is not
//! validated against the installed TUI. Stop delivery was validated on 0.4.12
//! using the then-provisioned `/api/attention/<node-id>` callback. The current
//! node-independent `/api/attention/mcode` route is provisioned but its native
//! delivery has not been validated. Consequently
//! `http::request::parse_mcode_session_id` must accept that shape or the id is
//! silently discarded. `self_assigns_session_id()` is `true` and
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
//! **Attention** (issue #1797, validating #1796): a Buildmesh Agent-Plugin is
//! provisioned into `<dataDir>/plugins/io.buildmesh.attention/`. A live
//! `mcode` 0.4.12 TUI validated `Stop` delivery of the
//! (`hook_event_name`, `session_id`, `transcript_path`) envelope to the then-
//! configured `/api/attention/<node-id>` callback. Current delivery to
//! `/api/attention/mcode` and SessionStart delivery remain unvalidated. Three
//! mcode-0.4.x constraints shape the write:
//!
//! 1. The manifest lives at `.claude-plugin/plugin.json` and `hooks` is
//!    **inlined on the manifest**. A separate `io.minimax.mcode/hooks/hooks.json`
//!    document is ignored by 0.4.x, and a plugin directory with no manifest is
//!    skipped silently (no plugin, no diagnostic).
//! 2. `command` + `args` are executed **without shell interpretation**, so the
//!    invocation names a shell (`cmd.exe` / `sh`) and hands it one command line.
//! 3. mcode `env_clear()`s `BUILDMESH_*` before running a hook — exactly like
//!    Codex, so the shared callback URL bakes the loopback port.
//!
//! The merge is idempotent and additive: sibling events and user-authored
//! handlers round-trip untouched, a malformed user file fails closed rather
//! than being overwritten silently, and an unresolvable data dir returns
//! `Ok(())` with no side effects.
//!
//! **Full Access** is pinned on every spawn. mcode's TUI accepts no permission
//! flag — `mcode --help` offers `--model`, `--lane`, `--session`,
//! `--continue` and `--tui-mode` and nothing else; `--permission` exists on
//! `mcode exec` only, which we never spawn — and mcode reads no environment
//! variable for the mode. The sole lever is the `permissionMode` key in
//! `<dataDir>/config.yaml`, so [`pin_permission_mode`] sets it to
//! `bypassPermissions` (Full Access) alongside the attention plugin. This
//! matches how Buildmesh already treats Codex
//! (`--ask-for-approval never --sandbox danger-full-access`): nobody is
//! watching a circuit's PTY, so an approval prompt is a stall. That function
//! also documents why the edit is surgical and why it never fabricates a
//! config.
//!
//! **Transcript**: `messages.jsonl` canonical history is parsed via
//! `TranscriptFormat::Mcode`, so the Coordinator Node Digest rich layer,
//! the archived-node resume picker, and circuit assistant reports all work.

use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
use crate::agent::provider::{AgentProvider, LaunchRuntime, Platform, ResolvedPath, SpawnRecipe, UiMeta, WindowsShell};
use crate::agent::session_lifecycle::LifecycleKind;
use crate::env::windows_attention_command;
use crate::models::EnvType;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct McodeAdapter;
pub static MCODE: McodeAdapter = McodeAdapter;

/// Minimum mcode release the Buildmesh attention hook has been validated
/// against (issue #1797). Validated on a live `0.4.12` TUI: `Stop` fires at
/// turn end and POSTs the mcode envelope to the then-provisioned
/// `/api/attention/<node-id>` callback. Current provisioning uses
/// `/api/attention/mcode`, but delivery to that route remains unvalidated.
/// The pin is descriptor-shape only — we do not gate the spawn on a runtime
/// version probe because mcode does not expose a semver-ish header through
/// the hook surface. Like `CURSOR_MIN_HOOK_VERSION` / `GROK_MIN_HOOK_VERSION`.
pub const MCODE_MIN_HOOK_VERSION: &str = "0.4.12";

/// Buildmesh-owned mcode Agent-Plugin directory under the user's data
/// root (`<dataDir>/plugins/<dir>`). mcode scans every `<dataDir>/plugins/*/`
/// for a plugin manifest; installing this plugin makes Buildmesh's attention
/// callbacks fire without disturbing any other plugin the user has registered.
/// Follows the reverse-DNS convention used by the upstream examples
/// (`io.<publisher>.<plugin>`).
const MCODE_PLUGIN_DIR: &str = "io.buildmesh.attention";

/// Plugin `name` in the manifest. mcode requires lowercase letters, digits
/// and single hyphens — the reverse-domain directory name above is not a
/// legal manifest name, so the two intentionally differ.
const MCODE_PLUGIN_NAME: &str = "buildmesh-attention";

const MCODE_PLUGIN_VERSION: &str = "1.0.0";

/// mcode's settings file, at `<dataDir>/config.yaml`. This is a **user-owned**
/// document — it holds the provider block, the API key and the model catalog —
/// not a Buildmesh-owned artifact like the plugin dir. See
/// [`pin_permission_mode`] for what we do, and deliberately do not do, to it.
const MCODE_CONFIG_FILE: &str = "config.yaml";

/// Top-level `config.yaml` key that selects mcode's permission policy.
///
/// mcode validates it against `["default", "bypassPermissions", "auto",
/// "off"]` and silently discards anything else, falling back to its compiled
/// default of `"auto"`. The TUI surfaces the same four values under the labels
/// Ask, Full access and Auto (`/permission`, or `approval-mode` in the status
/// bar), with `full` mapping to `bypassPermissions`.
const MCODE_PERMISSION_MODE_KEY: &str = "permissionMode";

/// The value that means **Full Access**. Chosen over leaving mcode on its
/// `"auto"` default for the same reason Codex is launched with
/// `--ask-for-approval never --sandbox danger-full-access` (`codex.rs:43-61`):
/// nobody is watching a circuit's PTY, so an approval prompt is a stall, and
/// `auto` still interrupts for anything its policy does not recognise. This is
/// also MiniMax's own recommendation for unattended runs.
const MCODE_PERMISSION_MODE_VALUE: &str = "bypassPermissions";

const MCODE_PLUGIN_DESCRIPTION: &str =
    "Buildmesh attention observer: reports turn completion to the local Buildmesh node.";

/// Per-handler timeout, in seconds (the mcode 0.4+ unit; the v0.3.x spec used
/// milliseconds and 0.4+ accepts either — we pin one).
const MCODE_HOOK_TIMEOUT_SECONDS: u64 = 5;

/// Events Buildmesh provisions into the mcode plugin manifest. `Stop` (turn
/// finished) is the validated signal that drives Node Digest turn completion.
/// `PermissionRequest` is provisioned for completeness but is **not**
/// advertised in `attention_capability`: Buildmesh launches mcode in Full
/// Access (`permissionMode: bypassPermissions`, pinned by
/// [`pin_permission_mode`] on every spawn), so no approval prompt is ever
/// raised and the event is never observed.
const MCODE_PROVISIONED_EVENTS: &[&str] = &["SessionStart", "Stop", "PermissionRequest"];

/// Marker substring identifying the Buildmesh-owned handler inside the
/// manifest's inline `hooks` map. Used to detect a stale Buildmesh entry on
/// re-provision (the issue #886 idempotency invariant): a re-run that finds
/// an existing handler carrying this substring replaces it in place (so the
/// array never grows), a re-run that finds no Buildmesh handler appends a
/// fresh one, and a re-run that already carries the exact handler leaves the
/// file untouched.
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

/// Build the `(command, args)` pair mcode's hook runner executes for one
/// event. mcode passes `args` to `command` **without shell interpretation**
/// (0.4+ plugin spec), so a bare `curl` invocation cannot express stdin
/// piping or output suppression — the invocation therefore names a shell
/// explicitly and hands it a single command line.
///
/// The URL is **baked**, never expanded from the environment: mcode
/// `env_clear()`s the `BUILDMESH_*` variables before running a hook (the live
/// 0.4.12 validation observed `%BUILDMESH_PORT%` arriving verbatim), which is
/// the same constraint Codex has. The event payload is forwarded from the
/// hook's stdin with `--data-binary @-`; a curl failure must never surface as
/// a non-zero hook exit, so both branches swallow it.
///
/// The `WindowsInterop` case is the mirror image: Buildmesh runs *inside* the
/// WSL/Linux host and mcode is the **Windows** executable reached through
/// interop (`EnvType::WindowsInterop` = "Windows executable reached through
/// interoperability from a Linux WSL host"). The callback is therefore
/// relayed back into the guest by `windows_attention_command`, whose
/// `powershell.exe … -EncodedCommand <b64>` line runs
/// `wsl.exe -d <distro> --exec curl …`; that curl lands on the Linux-side
/// listener's own loopback, so this direction needs no mirrored networking.
/// Its single-line command is split into the `command` + `args` shape.
fn attention_invocation(env_type: EnvType, url: &str) -> (String, Vec<String>) {
    if env_type == EnvType::WindowsInterop {
        if let Some(command) = windows_attention_command(Some(url)) {
            let mut parts = command.split(' ');
            let executable = parts.next().unwrap_or("powershell.exe").to_string();
            return (executable, parts.map(str::to_string).collect());
        }
    }
    if cfg!(target_os = "windows") && env_type == EnvType::Windows {
        (
            "cmd.exe".to_string(),
            vec![
                "/c".to_string(),
                format!(
                    "curl.exe -sf --connect-timeout 1 --max-time 2 -X POST --data-binary @- {url} >nul 2>nul"
                ),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "curl -sf --connect-timeout 1 --max-time 2 -X POST --data-binary @- {url} >/dev/null 2>&1 || true"
                ),
            ],
        )
    }
}

/// One inline handler entry for an event in the manifest's `hooks` map. The
/// mcode 0.4.0+ shape nests the executable entry under a `hooks` array beside
/// a `matcher` (a `"*"` matcher matches every occurrence).
fn attention_handler(_node_id: i64, env_type: EnvType) -> serde_json::Value {
    let port = crate::http_server::current_http_port();
    let url = format!("http://localhost:{port}/api/attention/mcode");
    let (command, args) = attention_invocation(env_type, &url);
    serde_json::json!({
        "matcher": "*",
        "hooks": [{
            "type": "command",
            "command": command,
            "args": args,
            "timeout": MCODE_HOOK_TIMEOUT_SECONDS,
        }],
    })
}

/// Resolve mcode's data root — the directory holding both the Buildmesh
/// attention plugin and the user's `config.yaml`. `runtime.harness_home`
/// is the per-launch override (preferred when the caller has already picked a
/// writable data dir); absent that, we route through `cli_dir_for_spawn`
/// (`env::environment.rs:402`) so a WSL-guest mcode spawn writes into the
/// *guest* `$HOME/.minimax` through the environment's host-path conversion —
/// never the Windows host's `%USERPROFILE%\.minimax` (the round-1 / round-2 reviewer
/// correction: silently calling `minimax_data_dir()` ignored
/// `resolved.spawn_path`, writing the plugin into the wrong filesystem and
/// violating the buildmesh hard rule `CLAUDE.md:21` "Never pass Linux/WSL
/// paths to Windows-side APIs"). The permission-mode pin must resolve
/// through this same path, or a WSL guest would be configured from the
/// Windows host's config.
///
/// Returns `None` when no path can be resolved — the exact case the
/// issue #1796 acceptance names "Return Ok(()) without side effects":
/// a Linux host under WSL with no WSL distro discoverable through
/// `wsl_home()` (or a host that has neither HOME nor USERPROFILE)
/// resolves to `None`, and `provision_attention_hooks` consumes that
/// as the "spawn proceeds, attention callback only is lost" signal.
fn resolve_data_dir(resolved: &ResolvedPath, runtime: &LaunchRuntime) -> Option<PathBuf> {
    if let Some(home) = runtime.harness_home.as_deref() {
        let trimmed = home.trim();
        // Treat an empty string the same as "no override" rather than
        // producing a plugin dir under the project root — empty
        // harness_home is the Library's way of saying "I didn't pick
        // one", not "use the cwd".
        if !trimmed.is_empty() {
            return Some(PathBuf::from(crate::env::to_host_path(trimmed)));
        }
    }
    // Route through `cli_dir_for_spawn` so WSL-guest mcode spawns
    // resolve to the *guest* home through the environment's host-path
    // conversion, not the Windows host's `%USERPROFILE%\.minimax`.
    // `cli_dir_for_spawn` itself returns `None` only when the host
    // has no resolvable WSL distro + no fallback home — that is the
    // legitimate "unresolvable hook config root" case for issue
    // #1796's `Ok(())` invariant.
    crate::env::cli_dir_for_spawn(
        crate::env::minimax_data_dir(),
        ".minimax",
        &resolved.spawn_path,
    )
}

/// Issue #1797 review (finding 2): a WSL-guest mcode runs its hook `curl`
/// inside the guest, so reaching the Windows-side Buildmesh depends entirely
/// on WSL mirrored networking. Without it the callback is swallowed (`|| true`)
/// and the node looks permanently dead to Autopilot while the descriptor
/// claims a working hook. Refuse provisioning with an actionable message
/// instead — the spawn still proceeds, it just surfaces as
/// `SignalHealth::Unavailable` rather than a silent black hole, exactly as
/// `grok.rs` does for its command callbacks.
///
/// Only the guest direction needs the check. `WindowsInterop` (Buildmesh in
/// the WSL host, mcode the Windows binary) relays through
/// `windows_attention_command`, whose `curl` runs back inside the guest and so
/// reaches the Linux-side listener over plain loopback.
fn ensure_wsl_callbacks_reachable(
    env_type: EnvType,
    wsl_distro: Option<&str>,
) -> Result<(), String> {
    if !(cfg!(windows) && env_type == EnvType::Wsl) {
        return Ok(());
    }
    let mut command = crate::process_util::command_no_window("wsl.exe");
    command.args(wsl_wslinfo_args(wsl_distro));
    let mode = crate::process_util::run_command_with_timeout(
        command,
        "WSL networking mode",
        std::time::Duration::from_secs(5),
    )
    .ok()
    .filter(|output| output.status.success())
    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());

    if wsl_networking_is_mirrored(mode.as_deref()) {
        return Ok(());
    }
    Err(format!(
        "MiniMax Code's attention hook needs WSL mirrored networking so guest callbacks reach \
         the Buildmesh port; the interactive harness can still run. Enable it by adding \
         `[wsl2]` + `networkingMode=mirrored` to %USERPROFILE%\\.wslconfig, then run \
         `wsl --shutdown` and relaunch the distro (observed networking mode: {}).",
        mode.as_deref().unwrap_or("unavailable")
    ))
}

/// `wslinfo --networking-mode` arguments for the distro the node actually runs
/// in. `wsl.exe` without `-d` inspects the **default** distro, which may be a
/// different distro with different networking settings (or lack `wslinfo`
/// entirely), so the resolved distro is threaded through (issue #1797 review,
/// finding 3). Split out so the argument shape is unit-testable on a host
/// without WSL.
fn wsl_wslinfo_args(wsl_distro: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(distro) = wsl_distro
        .map(str::trim)
        .filter(|distro| !distro.is_empty())
    {
        args.push("-d".to_string());
        args.push(distro.to_string());
    }
    args.extend(["--", "wslinfo", "--networking-mode"].map(str::to_string));
    args
}

/// Pure predicate over `wslinfo --networking-mode` output, split out so the
/// accept/reject decision is unit-testable on a host without WSL.
fn wsl_networking_is_mirrored(mode: Option<&str>) -> bool {
    mode.is_some_and(|mode| mode.trim().eq_ignore_ascii_case("mirrored"))
}

/// Atomically persist `content` to `path` via a PID+counter `.tmp`
/// file + rename. Mirrors `cursor.rs:127-157` / `agy.rs:14-40`. A
/// pre-existing `.tmp` from an earlier crash is overwritten; the final
/// rename is a single filesystem operation so partial reads of the
/// manifest see either the old file or the new one.
fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("plugin.json");
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

/// Best-effort Full Access pin, run on every mcode spawn.
///
/// Failure is logged, never propagated. A node that starts in mcode's own
/// `auto` default is working-but-slow, whereas failing the provision would
/// abort a launch the user asked for over a permission-convenience setting.
/// This is the same split `provision_at` makes for an unresolvable hook
/// root: degrade the capability, keep the agent.
fn pin_full_access(data_root: Option<&Path>) {
    let Some(root) = data_root else {
        tracing::debug!("mcode pin_permission_mode: data dir unresolvable; leaving mode alone");
        return;
    };
    if let Err(e) = pin_permission_mode(root) {
        tracing::warn!(
            "mcode pin_permission_mode: {e} — the session starts in mcode's own default \
             mode instead of Full Access"
        );
    }
}

/// Pin mcode's permission policy to Full Access in `<dataDir>/config.yaml`.
///
/// **Why a config write.** The interactive TUI is the only mcode entry point
/// Buildmesh launches, and it accepts no permission flag. `--permission`
/// exists on `mcode exec` only, which this adapter never spawns, and mcode
/// reads no environment variable for the mode. The settings key is the sole
/// lever the CLI offers.
///
/// **Why a surgical line edit.** `config.yaml` is the user's document: it
/// carries their comments, key order, an API key and a model catalog. A
/// parse-and-re-serialise round-trip would strip every comment and reflow the
/// file, so this rewrites exactly one line (or appends one) and leaves every
/// other byte alone. No YAML dependency is needed for that.
///
/// **Invariants**
///
/// * **Never fabricates a config.** mcode bootstraps its own config on first
///   run and *skips* that bootstrap when the file already exists, so creating
///   a file containing only this key would suppress the bootstrap and leave
///   the CLI with no `provider` block at all — a worse failure than running in
///   `auto`. An absent config is left absent; the next spawn (or the user's own
///   first run) finds the file and pins it.
/// * **Only the top-level key.** An indented `permissionMode` belongs to some
///   other mapping and is left alone, as are longer siblings like
///   `permissionModes:` and a commented-out key.
/// * **Idempotent.** A config already at Full Access is not rewritten, so a
///   steady-state spawn does no I/O.
///
/// Returns whether the file changed, or `Err` when a config exists but cannot
/// be read — a config we cannot read is one we must not clobber.
fn pin_permission_mode(data_root: &Path) -> Result<bool, String> {
    let path = data_root.join(MCODE_CONFIG_FILE);
    let existing = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };

    // The line ending this document already uses. mcode itself writes LF
    // (`yaml.dump` never emits CRLF), but a Windows user who hand-edited the
    // file must not get it silently half-converted.
    let eol = if existing.contains("\r\n") { "\r\n" } else { "\n" };

    let Some((key_line, comment)) = find_top_level_key(&existing, MCODE_PERMISSION_MODE_KEY) else {
        let mut out = existing;
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push_str(eol);
            }
            // Blank separator so the appended key reads as its own setting.
            out.push_str(eol);
        }
        out.push_str(&format!("{MCODE_PERMISSION_MODE_KEY}: {MCODE_PERMISSION_MODE_VALUE}{eol}"));
        return write_pinned_config(&path, &out);
    };

    if already_pinned(key_line, comment, MCODE_PERMISSION_MODE_VALUE) {
        return Ok(false);
    }

    // `key_line` has its `\r` stripped and the original terminator still
    // follows it in `existing`, so the replacement must NOT carry one — it
    // would leave a blank line behind. A comment that hugged the value
    // (`permissionMode:# note`) gains a separating space so the two don't fuse.
    let comment = match comment.is_empty() {
        true => String::new(),
        false if comment.starts_with([' ', '\t']) => comment.to_string(),
        false => format!(" {comment}"),
    };
    let replaced = format!("{MCODE_PERMISSION_MODE_KEY}: {MCODE_PERMISSION_MODE_VALUE}{comment}");
    write_pinned_config(&path, &existing.replacen(key_line, &replaced, 1))
}

/// Locate a **top-level** `key: …` line, returning the line without its
/// terminator and, separately, the trailing `# …` comment (leading whitespace
/// included) or `""` when the line carries none.
///
/// A top-level key starts at column 0. That is sound against a false positive
/// from a block scalar: YAML block-scalar content is always indented deeper
/// than its parent key, so it can never begin at column 0.
fn find_top_level_key<'a>(text: &'a str, key: &str) -> Option<(&'a str, &'a str)> {
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // A top-level key starts at column 0, so an indented `permissionMode`
        // is some other mapping's key and a `#`-led line is a comment.
        let Some(rest) = line.strip_prefix(key) else { continue };
        // The character right after the name must be a `:` (optionally
        // spaced) — this is what rejects a longer sibling key.
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(value_and_comment) = rest.strip_prefix(':') else { continue };
        return Some((line, trailing_comment(value_and_comment)));
    }
    None
}

/// The trailing `# …` comment in the part of a line after the `key:`, including
/// the whitespace that separates it from the value, or `""`. A `#` only opens
/// a comment when it starts the value or follows whitespace; anything else is
/// part of the value. mcode's four mode values contain no `#`, so this never
/// splits one. Carrying the whitespace run along keeps a user's column
/// alignment intact across the rewrite.
fn trailing_comment(value_and_comment: &str) -> &str {
    let bytes = value_and_comment.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'#' {
            continue;
        }
        if i != 0 && !bytes[i - 1].is_ascii_whitespace() {
            continue;
        }
        let start = (0..i).rev().find(|j| !bytes[*j].is_ascii_whitespace()).map_or(0, |j| j + 1);
        return &value_and_comment[start..];
    }
    ""
}

/// True when the located line already carries `value`, i.e. nothing to do.
/// Compares only the text between the `:` and any comment, trimmed, so
/// `permissionMode:  bypassPermissions ` counts as already pinned.
fn already_pinned(line: &str, comment: &str, value: &str) -> bool {
    let value_end = line.len().saturating_sub(comment.len());
    line[..value_end].split_once(':').map(|(_, found)| found.trim() == value).unwrap_or(false)
}

/// Persist the pinned config and report that the write happened.
fn write_pinned_config(path: &Path, contents: &str) -> Result<bool, String> {
    atomic_write(path, contents).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    tracing::info!(
        "mcode pin_permission_mode: set {MCODE_PERMISSION_MODE_KEY}={MCODE_PERMISSION_MODE_VALUE} in {:?}",
        path
    );
    Ok(true)
}

/// True when one executable entry resolves to the Buildmesh attention URL.
/// The `WindowsInterop` invocation is a `powershell.exe … -EncodedCommand
/// <b64>` line, so the command and its args are rejoined and the payload
/// decoded before the marker substring is tested.
fn handler_entry_targets_attention(entry: &serde_json::Value) -> bool {
    let mut haystack = String::new();
    if let Some(command) = entry.get("command").and_then(|v| v.as_str()) {
        haystack.push_str(command);
        haystack.push(' ');
    }
    if let Some(args) = entry.get("args").and_then(|v| v.as_array()) {
        for arg in args {
            if let Some(arg) = arg.as_str() {
                haystack.push_str(arg);
                haystack.push(' ');
            }
        }
    }
    let decoded = crate::env::decode_powershell_command(haystack.trim_end());
    let candidate = decoded.as_deref().unwrap_or(&haystack);
    candidate.contains(BUILDMESH_HOOK_MARKER)
}

/// True when `handler` is a Buildmesh-owned entry. Accepts the 0.4.0+ nested
/// shape (`{ matcher, hooks: [{ type, command, args }] }`) and the legacy flat
/// v0.3.x shape (`{ command }`) so a manifest written by an older development
/// build is upgraded in place rather than duplicated.
fn is_buildmesh_handler(handler: &serde_json::Value) -> bool {
    if let Some(inner) = handler.get("hooks").and_then(|v| v.as_array()) {
        return inner.iter().any(handler_entry_targets_attention);
    }
    handler_entry_targets_attention(handler)
}

/// Add or refresh the Buildmesh-owned handler for each event in
/// `MCODE_PROVISIONED_EVENTS` inside the plugin manifest, while preserving
/// everything else the manifest carries (the user's own events, matchers and
/// sibling handlers).
///
/// The manifest identity fields are owned by Buildmesh — the plugin directory
/// is ours, and mcode requires a legal `name` — while the `hooks` map is only
/// ever merged additively.
///
/// A missing file is the happy path (fresh install). A malformed existing file
/// (trailing comma, partial edit, syntax error) is treated as an explicit
/// `Err` rather than silently clobbered — the Cursor precedent at
/// `cursor.rs:194-198`: the user's data must surface to the spawn path as a
/// provision failure so it can be repaired.
///
/// Returns `Ok(())` without rewriting the file when every event already
/// carries the expected handler (the issue #886 idempotency invariant — no
/// spurious mtime bumps).
fn ensure_plugin_manifest(path: &Path, handler: &serde_json::Value) -> Result<(), String> {
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
        return Err(format!(
            "mcode plugin manifest top-level must be a JSON object; got {}",
            settings_kind(&settings)
        ));
    }

    let mut changed = false;
    {
        let object = settings
            .as_object_mut()
            .expect("manifest verified object above");
        for (key, value) in [
            ("name", serde_json::json!(MCODE_PLUGIN_NAME)),
            ("version", serde_json::json!(MCODE_PLUGIN_VERSION)),
            ("description", serde_json::json!(MCODE_PLUGIN_DESCRIPTION)),
        ] {
            if object.get(key) != Some(&value) {
                object.insert(key.to_string(), value);
                changed = true;
            }
        }

        let hooks = object
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));
        if !hooks.is_object() {
            return Err(format!(
                "mcode plugin manifest `hooks` must be a JSON object; got {}",
                settings_kind(hooks)
            ));
        }
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
                format!(
                    "mcode plugin manifest `hooks.{event}` must be an array; got {group_kind}"
                )
            })?;
            if let Some(existing) = group_array.iter_mut().find(|h| is_buildmesh_handler(h)) {
                if *existing != *handler {
                    *existing = handler.clone();
                    changed = true;
                }
            } else {
                group_array.push(handler.clone());
                changed = true;
            }
        }
    }

    if !changed {
        return Ok(());
    }
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize mcode plugin manifest failed: {e}"))?;
    atomic_write(path, &content)
        .map_err(|e| format!("failed to write mcode plugin manifest: {e}"))?;
    tracing::info!("mcode provision_attention_hooks: wrote {:?}", path);
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

/// Inner provisioner used by `AgentProvider::provision_attention_hooks`.
/// Pulled out into its own function so the "no side effects, return
/// `Ok(())` when plugin root is unresolvable" branch can be unit-tested
/// directly — the trait signature cannot inject a `None` plugin root
/// without changing the spawn-path contract (`requires_attention_hook`),
/// so we exercise the inner branch with `plugin_root = None` here.
fn provision_at(plugin_root: Option<&Path>, env_type: EnvType, node_id: i64) -> Result<(), String> {
    let Some(root) = plugin_root else {
        // Issue #1796 acceptance: "Return Ok(()) without side effects
        // when the hook config root is unresolvable". No file system
        // touch — the spawning mcode session proceeds, only the
        // attention callback is lost.
        tracing::debug!(
            "mcode provision_attention_hooks: hook config root unresolvable; \
             skipping with no side effects"
        );
        return Ok(());
    };
    // mcode 0.4.0+ reads the manifest at `<plugin>/.claude-plugin/plugin.json`
    // and requires `hooks` to be inlined on it. A separate
    // `io.minimax.mcode/hooks/hooks.json` document is ignored by 0.4.x, and a
    // plugin directory without a manifest is skipped silently (the issue #1797
    // root cause the live validation exposed).
    let manifest_path = root
        .join("plugins")
        .join(MCODE_PLUGIN_DIR)
        .join(".claude-plugin")
        .join("plugin.json");
    if let Some(parent) = manifest_path.parent() {
        // `create_dir_all` on the parent chain is idempotent; it
        // fails only on permission / read-only-fs errors, which we
        // surface as `Err` (provision failure) rather than swallow.
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create mcode plugin dir {parent:?}: {e}"))?;
    }
    ensure_plugin_manifest(&manifest_path, &attention_handler(node_id, env_type))
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

    fn background_recipe(&self, platform: Platform) -> Option<crate::agent::background::BackgroundRecipe> {
        use crate::agent::{background::BackgroundRecipe, capabilities::{BackgroundPromptInput, BackgroundResultOutput}};
        let mut spawn = self.spawn_recipe(platform, EnvType::Windows);
        spawn.base_args = ["exec", "--input", "-", "--input-format", "text", "--permission", "smart", "--max-steps", "1", "--timeout", "30s"].map(str::to_owned).to_vec();
        Some(BackgroundRecipe::new(spawn, BackgroundPromptInput::Stdin, BackgroundResultOutput::LastMessageFile))
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    /// `true` — the Buildmesh Agent-Plugin is provisioned into
    /// `<dataDir>/plugins/io.buildmesh.attention/.claude-plugin/plugin.json`
    /// and `Stop` delivery was validated against a live 0.4.12 TUI using the
    /// historical per-node callback (issue #1797). The current shared callback
    /// and SessionStart delivery remain unvalidated. This flag opens the
    /// spawn-time gate at `provision.rs:557-569` and, downstream, the Autopilot
    /// compatibility gate; see `attention_capability()` for the contract.
    fn requires_attention_hook(&self) -> bool {
        true
    }

    /// Issue #1797: `Stop` is the only live-validated lifecycle signal. That
    /// 0.4.12 validation used the historical `/api/attention/<node-id>` URL;
    /// current `/api/attention/mcode` and SessionStart delivery remain
    /// unvalidated. Buildmesh launches mcode in Full Access
    /// (`permissionMode: bypassPermissions`, pinned by
    /// [`pin_permission_mode`]), so no approval prompt is raised and
    /// `PermissionRequested` is not advertised — exactly like Cursor under
    /// `--force`. No workspace-trust step is required and none is taken, so
    /// `trust` stays `None`.
    fn attention_capability(&self) -> AttentionCapability {
        AttentionCapability::Hook {
            events: vec![LifecycleKind::TurnCompleted],
            launch_mode: AttentionLaunchMode::SkipPermissions,
            trust: None,
            min_version: Some(MCODE_MIN_HOOK_VERSION.into()),
        }
    }

    /// Provision mcode's Agent-Plugin attention hook (issue #1796; Stop delivery
    /// validated in #1797). Writes
    /// `<mcode-data-dir>/plugins/io.buildmesh.attention/.claude-plugin/plugin.json`
    /// with the `SessionStart`, `Stop`, and `PermissionRequest` handlers inlined on the manifest
    /// in mcode's 0.4.0+ (Claude-compatible) shape.
    ///
    /// The same data dir also gets its `config.yaml` permission mode pinned to
    /// Full Access by [`pin_permission_mode`], because the TUI has no flag for
    /// it. That pin is best-effort — a failure there is logged, never
    /// propagated, so an unpinnable config cannot abort a launch (the node
    /// simply starts in mcode's own default mode). Note it is a
    /// **machine-global** setting, shared with the user's standalone `mcode`
    /// sessions. Callback provisioning also uses one shared manifest.
    ///
    /// The merge is additive (sibling events and user handlers round-trip) and
    /// idempotent (issue #886); a malformed existing file returns `Err` rather
    /// than being overwritten with a fresh payload (the Cursor / Grok round-2
    /// review fix); and an unresolvable data dir returns `Ok(())` without side
    /// effects so an mcode spawn that cannot find its home directory still
    /// proceeds (only the attention callback is lost — the agent remains
    /// usable).
    ///
    /// The manifest is machine-global and mcode clears `BUILDMESH_*` before
    /// invoking handlers. A stable URL lets the route bind the native session
    /// and workspace instead of trusting another spawn's baked node address.
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        runtime: &LaunchRuntime,
        node_id: i64,
    ) -> Result<(), String> {
        // Refuse a WSL-guest spawn whose callbacks cannot reach the Buildmesh
        // port, rather than installing a hook that can only fail silently
        // (issue #1797 review, finding 2). The node's own distro is threaded
        // through so the preflight inspects the right one (finding 3).
        ensure_wsl_callbacks_reachable(resolved.env_type, runtime.wsl_distro.as_deref())?;
        let data_root = resolve_data_dir(resolved, runtime);
        pin_full_access(data_root.as_deref());
        provision_at(data_root.as_deref(), resolved.env_type, node_id)
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

    /// MiniMax Code assigns ids captured by native session/workspace callbacks.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    /// The TUI has no verified session banner, so the labeled-UUID PTY regex
    /// can never reliably match an mcode id — and leaving it on would risk
    /// binding a stray UUID from tool output (same reason AGY opts out).
    /// Capture uses native hooks; timestamps cannot prove session ownership.
    fn captures_session_id_from_pty(&self) -> bool {
        false
    }

    // Identity is captured by workspace-scoped native callbacks. The global
    // manifest has no workspace anchor, so time-only recovery is unsafe.
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
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

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

    /// Native callbacks capture identity; PTY must not bind UUIDs from tool output.
    #[test]
    fn self_assigns_but_does_not_capture_from_pty() {
        assert!(
            MCODE.self_assigns_session_id(),
            "mcode mints its own session ids"
        );
        assert!(
            !MCODE.captures_session_id_from_pty(),
            "mcode ids are not PTY banners; capture is after_fresh_spawn (issue #1798)"
        );
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
    }

    /// Pin the capability descriptor end-to-end: the harness-id,
    /// `supports_model_override = false`, and the absence of effort
    /// controls. Drift here means the Spawn Menu or autopilot
    /// compatibility gate will misroute mcode.
    #[test]
    fn capabilities_descriptor_drops_model_and_effort() {
        let caps = MCODE.capabilities();
        assert_eq!(caps.harness_id, "mcode");
        assert!(caps.supports_resume);
        assert!(caps.supports_prefill);
        assert!(!caps.supports_model_override);
        assert!(!caps.supports_effort_override);
        // Issue #1797 — the hook is provisioned and Stop delivery was
        // validated against a live 0.4.12 TUI.
        assert!(caps.requires_attention_hook);
        assert!(caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
    }

    /// Issue #1797: `Stop` is the only validated lifecycle event. SessionStart is
    /// provisioned for identity capture but remains unvalidated on the TUI.
    /// Buildmesh launches mcode with its default
    /// (auto-approving) permission policy, so no permission prompt is raised
    /// and `PermissionRequested` must NOT be advertised. The `min_version` pin
    /// fails a refactor that drops it.
    #[test]
    fn attention_capability_advertises_validated_stop_only() {
        let caps = MCODE.capabilities();
        assert!(caps.requires_attention_hook);
        let capability = caps.attention_capability;
        match &capability {
            AttentionCapability::Hook {
                events,
                launch_mode,
                trust,
                min_version,
            } => {
                assert_eq!(
                    events,
                    &vec![LifecycleKind::TurnCompleted],
                    "`Stop` is the only validated event; advertising more would \
                     claim a signal we never observed"
                );
                assert!(
                    !events.contains(&LifecycleKind::PermissionRequested),
                    "Buildmesh launches mcode with an auto-approving permission \
                     policy — no approval prompt is raised, so a permission \
                     signal is impossible by construction"
                );
                assert_eq!(*launch_mode, AttentionLaunchMode::SkipPermissions);
                assert!(
                    trust.is_none(),
                    "mcode requires no workspace-trust step and we take none: {trust:?}"
                );
                assert_eq!(min_version.as_deref(), Some(MCODE_MIN_HOOK_VERSION));
            }
            _ => panic!("expected Hook, got {capability:?}"),
        }
    }

    /// The recipe for the default launch mode — even with a (hypothetical)
    /// resolved model in the input — must contain no `--model` flag.
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
    // Issue #1796 provisioning + issue #1797 validation.
    //
    // Contract pinned here:
    //   1. Fresh install writes a mcode 0.4.0+ manifest at
    //      `<data-dir>/plugins/io.buildmesh.attention/.claude-plugin/plugin.json`
    //      with `Stop` AND `PermissionRequest` INLINED on the manifest.
    //   2. Additive merge preserves sibling user-authored handlers and
    //      events outside our ownership — never clobbers user plugins.
    //   3. Malformed user files are refused (not silently overwritten).
    //   4. Idempotent re-provision dedupes our Buildmesh entry rather
    //      than appending duplicates.
    //   5. Atomic write leaves no `.tmp` residue.
    //   6. Unresolvable data dir returns `Ok(())` with no side effects.
    //   7. The hook invocation POSTs stdin JSON to
    //      `/api/attention/mcode` through a real localhost listener,
    //      with NO reliance on inherited environment (mcode env_clears
    //      `BUILDMESH_*`, so the URL is baked).
    // -----------------------------------------------------------------

    fn plugin_manifest_path(home: &Path) -> std::path::PathBuf {
        home.join("plugins")
            .join(MCODE_PLUGIN_DIR)
            .join(".claude-plugin")
            .join("plugin.json")
    }

    /// Drive `provision_attention_hooks` against a temporary directory
    /// masquerading as the mcode data dir, returning the resolved
    /// `.claude-plugin/plugin.json` path so the test can inspect the JSON.
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
        plugin_manifest_path(home)
    }

    /// The executable entry under one event's first handler group.
    fn first_executable(value: &serde_json::Value, event: &str) -> serde_json::Value {
        value["hooks"][event][0]["hooks"][0].clone()
    }

    /// The args of one handler group's first executable, joined for matching.
    fn joined_args(entry: &serde_json::Value) -> String {
        entry["args"]
            .as_array()
            .map(|args| {
                args.iter()
                    .filter_map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    }

    #[test]
    fn provision_fresh_install_writes_manifest_with_inlined_hooks() {
        let home = tempfile::tempdir().unwrap();
        let manifest = provision_test_home(home.path(), EnvType::Windows);
        assert!(
            manifest.is_file(),
            "plugin.json was not written: {manifest:?}"
        );

        // The ignored v0.3.x document must NOT be created — mcode 0.4.0+
        // reads `hooks` from the manifest and skips a directory without one.
        let legacy = home
            .path()
            .join("plugins")
            .join(MCODE_PLUGIN_DIR)
            .join("hooks")
            .join("hooks.json");
        assert!(
            !legacy.exists(),
            "mcode 0.4.0+ ignores `hooks/hooks.json`; writing it would be dead \
             weight that looks like a working integration: {legacy:?}"
        );

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();

        // A manifest is only loaded when it carries a legal name; the
        // reverse-domain directory name is not a legal manifest name.
        assert_eq!(value["name"].as_str(), Some(MCODE_PLUGIN_NAME));
        assert!(
            MCODE_PLUGIN_NAME
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "mcode requires lowercase letters, digits and single hyphens: {MCODE_PLUGIN_NAME}"
        );

        for event in MCODE_PROVISIONED_EVENTS {
            let entry = first_executable(&value, event);
            assert_eq!(
                entry["type"].as_str(),
                Some("command"),
                "{event} must carry the 0.4.0+ command entry: {entry:?}"
            );
            let joined = joined_args(&entry);
            assert!(
                joined.contains(BUILDMESH_HOOK_MARKER),
                "{event} handler must POST to the attention route: {joined}"
            );
            assert!(
                joined.contains("/api/attention/mcode"),
                "{event} must use the shared callback URL: {joined}"
            );
            assert!(
                !joined.contains("BUILDMESH_PORT") && !joined.contains("BUILDMESH_SESSION_ID"),
                "{event} must not rely on inherited env; mcode env_clears \
                 BUILDMESH_*: {joined}"
            );
        }
    }

    /// Additive merge: a user-authored sibling handler (e.g. an audit log)
    /// MUST round-trip. The Kimi / Grok precedent pins this contract — we
    /// never silently clobber sibling entries.
    #[test]
    fn provision_preserves_user_authored_sibling_handlers() {
        let home = tempfile::tempdir().unwrap();
        let manifest = plugin_manifest_path(home.path());
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        // A pre-existing Buildmesh manifest the user has extended: a sibling
        // `Stop` handler plus an event outside our ownership (`SessionStart`).
        let user = r#"{
            "name": "buildmesh-attention",
            "hooks": {
                "Stop": [
                    {
                        "matcher": "*",
                        "hooks": [
                            { "type": "command", "command": "node", "args": ["audit.mjs"] }
                        ]
                    }
                ],
                "SessionStart": [
                    {
                        "matcher": "*",
                        "hooks": [
                            { "type": "command", "command": "node", "args": ["startup.mjs"] }
                        ]
                    }
                ]
            }
        }"#;
        std::fs::write(&manifest, user).unwrap();

        let written = provision_test_home(home.path(), EnvType::Windows);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();

        // The user-authored sibling `Stop` handler must survive.
        let stop = value["hooks"]["Stop"].as_array().expect("Stop array");
        assert!(
            stop.iter()
                .any(|h| h["hooks"][0]["command"].as_str() == Some("node")),
            "user-authored Stop handler must round-trip; got {stop:?}"
        );
        assert!(
            stop.iter().any(is_buildmesh_handler),
            "Buildmesh Stop handler must be installed alongside the user handler; got {stop:?}"
        );
        assert_eq!(
            stop.len(),
            2,
            "Stop must carry both the user and Buildmesh entries (additive merge); got {stop:?}"
        );

        // SessionStart must retain the user handler alongside Buildmesh capture.
        let session_start = value["hooks"]["SessionStart"]
            .as_array()
            .expect("SessionStart array");
        assert_eq!(session_start.len(), 2);
        assert!(session_start.iter().any(is_buildmesh_handler));
        assert_eq!(
            session_start[0]["hooks"][0]["args"][0].as_str(),
            Some("startup.mjs")
        );

        // `PermissionRequest` is provisioned fresh (no user entry to merge with).
        let permission = value["hooks"]["PermissionRequest"]
            .as_array()
            .expect("PermissionRequest array");
        assert_eq!(
            permission.len(),
            1,
            "PermissionRequest must carry exactly one Buildmesh entry"
        );
    }

    /// Re-running provision on a manifest that already carries a stale
    /// Buildmesh `Stop` entry (marker matches, URL drifted) must replace it in
    /// place — appending would create duplicates (issue #886 idempotency).
    #[test]
    fn provision_dedupes_existing_buildmesh_stop_entry() {
        let home = tempfile::tempdir().unwrap();
        let manifest = plugin_manifest_path(home.path());
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let existing = r#"{
            "name": "buildmesh-attention",
            "hooks": {
                "Stop": [
                    {
                        "matcher": "*",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "cmd.exe",
                                "args": ["/c", "curl.exe -s http://localhost:1999/api/attention/0"]
                            }
                        ]
                    }
                ]
            }
        }"#;
        std::fs::write(&manifest, existing).unwrap();

        let written = provision_test_home(home.path(), EnvType::Windows);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();

        let stop = value["hooks"]["Stop"].as_array().expect("Stop array");
        assert_eq!(
            stop.len(),
            1,
            "stale Buildmesh entry must be replaced, not appended; got {stop:?}"
        );
        let joined = joined_args(&first_executable(&value, "Stop"));
        assert!(
            !joined.contains("localhost:1999"),
            "stale URL must be replaced, not preserved: {joined}"
        );
        assert!(
            joined.contains("/api/attention/mcode"),
            "replacement must carry the shared callback URL: {joined}"
        );
    }

    /// A shared plugin must retain the same URL across launches and resumes,
    /// including standalone processes that load the manifest.
    #[test]
    fn provision_is_stable_across_nodes_sharing_one_data_dir() {
        let home = tempfile::tempdir().unwrap();
        let manifest = plugin_manifest_path(home.path());

        let provision = |node_id: i64| -> String {
            let path = home.path().to_string_lossy().to_string();
            MCODE
                .provision_attention_hooks(
                    &ResolvedPath {
                        host_path: path.clone(),
                        spawn_path: path.clone(),
                        raw_path: path,
                        env_type: EnvType::Windows,
                    },
                    &LaunchRuntime {
                        harness_home: Some(home.path().to_string_lossy().to_string()),
                        wsl_distro: None,
                    },
                    node_id,
                )
                .expect("provision_attention_hooks should succeed");
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
            joined_args(&first_executable(&value, "Stop"))
        };

        let first = provision(101);
        assert!(first.contains("/api/attention/mcode"), "{first}");
        let second = provision(202);
        assert_eq!(first, second, "another spawn cannot redirect an existing process");
        assert_eq!(provision(101), first);

        // The merge never accumulates handlers across nodes.
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        for event in MCODE_PROVISIONED_EVENTS {
            assert_eq!(
                value["hooks"][*event].as_array().unwrap().len(),
                1,
                "{event} must hold exactly one Buildmesh handler no matter how many \
                 nodes have provisioned into this data dir"
            );
        }
    }

    /// Issue #1797 review (finding 3): the preflight must inspect the distro
    /// the node actually runs in, not `wsl.exe`'s default. A bare `wsl.exe`
    /// reads whatever `wsl --set-default` points at, which can be a different
    /// distro with different networking (or no `wslinfo` at all).
    #[test]
    fn wsl_preflight_targets_the_nodes_own_distro() {
        assert_eq!(
            wsl_wslinfo_args(Some("Ubuntu-22.04")),
            vec!["-d", "Ubuntu-22.04", "--", "wslinfo", "--networking-mode"]
        );
        assert_eq!(
            wsl_wslinfo_args(Some("  Ubuntu  ")),
            vec!["-d", "Ubuntu", "--", "wslinfo", "--networking-mode"],
            "the distro name is trimmed"
        );
        // No distro resolved (or a blank one) → the default-distro form, which
        // is still the best available target.
        assert_eq!(
            wsl_wslinfo_args(None),
            vec!["--", "wslinfo", "--networking-mode"]
        );
        assert_eq!(
            wsl_wslinfo_args(Some("   ")),
            vec!["--", "wslinfo", "--networking-mode"]
        );
    }

    /// Issue #1797 review (finding 2): the WSL preflight is what turns a silent
    /// callback black hole into an actionable provisioning failure. The accept
    /// predicate is split out so it is testable without a WSL host.
    #[test]
    fn wsl_networking_preflight_only_accepts_mirrored() {
        assert!(wsl_networking_is_mirrored(Some("mirrored")));
        assert!(wsl_networking_is_mirrored(Some("  mirrored\n")));
        assert!(wsl_networking_is_mirrored(Some("Mirrored")));
        assert!(!wsl_networking_is_mirrored(Some("nat")));
        assert!(!wsl_networking_is_mirrored(Some("")));
        assert!(!wsl_networking_is_mirrored(None));
    }

    /// Non-Windows runtimes get the POSIX invocation: `sh -c`, `/dev/null`
    /// redirection and `|| true` — never Windows `>nul` or `curl.exe`.
    #[test]
    fn provision_writes_posix_invocation_on_unix() {
        let url = "http://localhost:2992/api/attention/7";
        let (command, args) = attention_invocation(EnvType::Wsl, url);
        assert_eq!(command, "sh");
        assert_eq!(args[0], "-c");
        assert!(
            args[1].contains(url),
            "POSIX invocation must carry the baked url: {}",
            args[1]
        );
        assert!(
            args[1].contains("|| true"),
            "POSIX hook must suppress curl failures: {}",
            args[1]
        );
        assert!(
            !args[1].contains(">nul"),
            "POSIX hook must not emit Windows `>nul`: {}",
            args[1]
        );
        assert!(
            !args[1].contains("curl.exe"),
            "POSIX hook must not use curl.exe: {}",
            args[1]
        );
    }

    /// A malformed user-authored manifest (trailing comma, partial edit,
    /// syntax error) must NOT cause provisioning to silently overwrite. The
    /// function returns an `Err`, the spawn path surfaces it as a provision
    /// failure, and the user's content survives intact.
    #[test]
    fn provision_refuses_to_overwrite_malformed_user_file() {
        let home = tempfile::tempdir().unwrap();
        let manifest = plugin_manifest_path(home.path());
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let malformed = "{ \"hooks\": [],, }";
        std::fs::write(&manifest, malformed).unwrap();

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
            "provision must refuse a malformed existing manifest; got {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&manifest).unwrap(),
            malformed,
            "malformed manifest content must NOT be overwritten"
        );
    }

    /// A valid JSON top-level that isn't an object is a misconfiguration —
    /// return `Err` rather than clobbering the user's payload with `{}`.
    #[test]
    fn provision_refuses_top_level_array() {
        let home = tempfile::tempdir().unwrap();
        let manifest = plugin_manifest_path(home.path());
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, "[1, 2, 3]").unwrap();

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
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), "[1, 2, 3]");
    }

    /// The issue #1796 acceptance invariant: when the hook config root cannot
    /// be resolved, the function returns `Ok(())` WITHOUT side effects on disk.
    /// We exercise the inner `provision_at` helper directly with
    /// `plugin_root = None` (the unresolvable case) and verify both the return
    /// value AND the absence of any side effects in a sandboxed tempdir.
    #[test]
    fn provision_returns_ok_without_side_effects_when_home_unresolvable() {
        let sandbox = tempfile::tempdir().unwrap();
        let sandbox_root = sandbox.path().to_path_buf();
        let before: std::collections::BTreeSet<_> = walk_dir_files(&sandbox_root)
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let result = provision_at(None, EnvType::Windows, 7);
        assert!(
            result.is_ok(),
            "unresolvable hook config root must return Ok(()) per issue #1796; got {result:?}"
        );

        let after: std::collections::BTreeSet<_> = walk_dir_files(&sandbox_root)
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            after, before,
            "provision_at(plugin_root = None) must NOT touch the filesystem; \
             the sandbox has changed: before={before:?}, after={after:?}"
        );

        for forbidden in [
            "minimax",
            "plugins",
            "io.buildmesh.attention",
            "plugin.json",
        ] {
            let found = walk_dir_files(&sandbox_root)
                .into_iter()
                .any(|p| p.to_string_lossy().contains(forbidden));
            assert!(
                !found,
                "provision_at(None) must NOT create a `{forbidden}` artifact; walked: {:?}",
                walk_dir_files(&sandbox_root)
            );
        }
    }

    /// Recursively walk `root` and collect every file path under it.
    fn walk_dir_files(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        out.push(path);
                    } else if path.is_dir() {
                        visit(&path, out);
                    }
                }
            }
        }
        visit(root, &mut out);
        out
    }

    /// Atomic write leaves no `.tmp` residue beside the manifest.
    #[test]
    fn provision_atomic_write_leaves_no_tmp_residue() {
        let home = tempfile::tempdir().unwrap();
        provision_test_home(home.path(), EnvType::Windows);

        let manifest_dir = plugin_manifest_path(home.path())
            .parent()
            .expect("manifest dir")
            .to_path_buf();
        let entries = std::fs::read_dir(&manifest_dir).unwrap();
        let tmp_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            tmp_files.is_empty(),
            "atomic write must not leave .tmp residue; found {tmp_files:?}"
        );
    }

    // ---- Full Access pin ---------------------------------------------------

    /// The `config.yaml` a temp data dir is expected to hold.
    fn config_path(home: &Path) -> PathBuf {
        home.join(MCODE_CONFIG_FILE)
    }

    /// Write `body` as the data dir's `config.yaml`.
    fn seed_config(home: &Path, body: &str) {
        std::fs::create_dir_all(home).unwrap();
        std::fs::write(config_path(home), body).unwrap();
    }

    /// A representative real-world `config.yaml`: a leading comment, a nested
    /// provider map holding the API key and a model catalog, and a trailing
    /// comment. Every byte must survive the pin except the one line we touch.
    const REALISTIC_CONFIG: &str = "\
logLevel: info
# managed by the MiniMax installer
provider:
  minimax:
    name: MiniMax
    options:
      apiKey: sk-xxx
      baseURL: https://agent.minimax.io/mavis/api/v1/llm/v1
    models:
      - id: MiniMax-M3
        thinking:
          effort: xhigh
defaultModel: minimax/MiniMax-M3 # the one we default to
defaultModelThinking:
  effort: xhigh
";

    #[test]
    fn permission_mode_constants_name_a_value_mcode_accepts() {
        // mcode validates the key against ["default", "bypassPermissions",
        // "auto", "off"] and silently discards anything else, falling back to
        // "auto" — the exact mode this pin exists to escape.
        assert_eq!(MCODE_PERMISSION_MODE_KEY, "permissionMode");
        assert_eq!(MCODE_PERMISSION_MODE_VALUE, "bypassPermissions");
        assert_eq!(MCODE_CONFIG_FILE, "config.yaml");
    }

    #[test]
    fn pin_appends_the_key_when_the_config_has_none() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), REALISTIC_CONFIG);

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert!(
            after.starts_with(REALISTIC_CONFIG),
            "every pre-existing byte must survive the append; got:\n{after}"
        );
        assert!(
            after.contains("permissionMode: bypassPermissions\n"),
            "the key must be appended; got:\n{after}"
        );
    }

    #[test]
    fn pin_overwrites_auto_with_full_access() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "logLevel: info\npermissionMode: auto\n");

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert_eq!(after, "logLevel: info\npermissionMode: bypassPermissions\n");
    }

    #[test]
    fn pin_overwrites_every_other_mode() {
        // mcode accepts four values; all three non-Full-Access ones must be
        // replaced, or the fix silently no-ops for someone on "ask".
        for mode in ["default", "auto", "off"] {
            let home = tempfile::tempdir().unwrap();
            seed_config(home.path(), &format!("permissionMode: {mode}\n"));
            assert_eq!(pin_permission_mode(home.path()), Ok(true), "mode {mode}");
            let after = std::fs::read_to_string(config_path(home.path())).unwrap();
            assert_eq!(after, "permissionMode: bypassPermissions\n", "mode {mode}");
        }
    }

    #[test]
    fn pin_is_idempotent_and_leaves_the_file_byte_identical() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "permissionMode: bypassPermissions\n");
        let before = std::fs::read_to_string(config_path(home.path())).unwrap();

        assert_eq!(pin_permission_mode(home.path()), Ok(false));
        assert_eq!(pin_permission_mode(home.path()), Ok(false));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert_eq!(after, before, "a second pin must be a no-op");
    }

    #[test]
    fn pin_tolerates_padded_and_commented_pinned_values() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "permissionMode:  bypassPermissions  \n");

        assert_eq!(
            pin_permission_mode(home.path()),
            Ok(false),
            "extra whitespace around an already-correct value is still correct"
        );
    }

    #[test]
    fn pin_preserves_a_trailing_comment_on_the_key_line() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "permissionMode: ask  # deliberately strict\nlogLevel: info\n");

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert_eq!(
            after, "permissionMode: bypassPermissions  # deliberately strict\nlogLevel: info\n",
            "the user's own comment and its alignment must not be discarded"
        );
    }

    #[test]
    fn pin_leaves_every_other_line_untouched() {
        let home = tempfile::tempdir().unwrap();
        let original = REALISTIC_CONFIG.replace("logLevel: info\n", "logLevel: info\npermissionMode: auto\n");
        seed_config(home.path(), &original);

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert_eq!(
            after,
            original.replace("permissionMode: auto", "permissionMode: bypassPermissions"),
            "the API key, model catalog and comments must round-trip"
        );
    }

    #[test]
    fn pin_ignores_an_indented_nested_permission_mode() {
        // Only the top-level key selects the mode. An indented one belongs to
        // some other mapping; rewriting it would corrupt an unrelated part of
        // the user's config.
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "provider:\n  minimax:\n    permissionMode: ask\n");

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert!(
            after.contains("    permissionMode: ask\n"),
            "the nested key must be untouched; got:\n{after}"
        );
        assert!(
            after.ends_with("permissionMode: bypassPermissions\n"),
            "the top-level key must be appended; got:\n{after}"
        );
    }

    #[test]
    fn pin_does_not_match_a_key_that_merely_starts_the_same() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "permissionModeOverride: keep\npermissionModes: keep\n");

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert!(
            after.contains("permissionModeOverride: keep\n"),
            "a longer sibling key must not be rewritten; got:\n{after}"
        );
        assert!(
            after.contains("permissionModes: keep\n"),
            "a longer sibling key must not be rewritten; got:\n{after}"
        );
    }

    #[test]
    fn pin_does_not_match_a_commented_out_key() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "# permissionMode: ask\n");

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert!(
            after.contains("# permissionMode: ask\n"),
            "a comment is not a key; got:\n{after}"
        );
    }

    #[test]
    fn pin_appends_correctly_when_the_file_has_no_trailing_newline() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "logLevel: info");

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert_eq!(
            after, "logLevel: info\n\npermissionMode: bypassPermissions\n",
            "a missing final newline must not glue the key onto the last value"
        );
    }

    #[test]
    fn pin_preserves_crlf_line_endings() {
        // A Windows user who hand-edited the file must not get it silently
        // half-converted to LF.
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), "logLevel: info\r\npermissionMode: auto\r\n");

        assert_eq!(pin_permission_mode(home.path()), Ok(true));

        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert_eq!(after, "logLevel: info\r\npermissionMode: bypassPermissions\r\n");
        assert!(!after.replace("\r\n", "").contains('\n'), "no bare LF may be introduced; got {after:?}");
    }

    #[test]
    fn pin_never_fabricates_a_config_file_that_does_not_exist() {
        // mcode bootstraps its own config on first run and *skips* that
        // bootstrap when the file already exists. Writing a file holding only
        // `permissionMode` would suppress the bootstrap and leave the CLI with
        // no `provider` block at all — a worse failure than running in "auto".
        // So an absent config is left absent.
        let home = tempfile::tempdir().unwrap();

        assert_eq!(pin_permission_mode(home.path()), Ok(false));
        assert_eq!(pin_permission_mode(home.path()), Ok(false));
        assert!(
            !config_path(home.path()).exists(),
            "no config.yaml may be created; the dir now holds {:?}",
            walk_dir_files(home.path())
        );
    }

    #[test]
    #[cfg(unix)]
    fn pin_reports_an_unreadable_config_rather_than_overwriting_it() {
        // A config we cannot read is a config we must not clobber. `chmod 000`
        // only denies the owner on Unix, and Windows has no equivalent, hence
        // the compile-time gate.
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), REALISTIC_CONFIG);
        std::fs::set_permissions(config_path(home.path()), std::os::unix::fs::PermissionsExt::from_mode(0o000))
            .unwrap();

        let result = pin_permission_mode(home.path());
        std::fs::set_permissions(config_path(home.path()), std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();

        assert!(
            result.is_err(),
            "an unreadable config must surface an error, not Ok(false); got {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(config_path(home.path())).unwrap(),
            REALISTIC_CONFIG,
            "the unreadable file must be left byte-identical"
        );
    }

    #[test]
    fn pin_leaves_no_tmp_residue() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), REALISTIC_CONFIG);
        pin_permission_mode(home.path()).unwrap();

        let tmp_files: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(tmp_files.is_empty(), "atomic write must not leave .tmp residue; found {tmp_files:?}");
    }

    /// The pin runs on every spawn next to the attention provisioner, so the
    /// two must coexist in one data dir.
    #[test]
    fn provision_attention_hooks_pins_full_access_and_writes_the_plugin() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), REALISTIC_CONFIG);

        let manifest = provision_test_home(home.path(), EnvType::Windows);

        assert!(manifest.exists(), "the attention plugin must still be provisioned alongside the pin");
        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert!(
            after.contains("permissionMode: bypassPermissions"),
            "a normal spawn must pin Full Access; got:\n{after}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn provision_attention_hooks_still_provisions_when_the_config_is_unreadable() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), REALISTIC_CONFIG);
        std::fs::set_permissions(config_path(home.path()), std::os::unix::fs::PermissionsExt::from_mode(0o000))
            .unwrap();

        let path = home.path().to_string_lossy().to_string();
        let result = MCODE.provision_attention_hooks(
            &ResolvedPath {
                host_path: path.clone(),
                spawn_path: path.clone(),
                raw_path: path.clone(),
                env_type: EnvType::Windows,
            },
            &LaunchRuntime {
                harness_home: Some(path),
                wsl_distro: None,
            },
            7,
        );
        std::fs::set_permissions(config_path(home.path()), std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();

        assert!(
            result.is_ok(),
            "an unpinnable config must not fail the spawn; got {result:?}"
        );
        assert!(
            plugin_manifest_path(home.path()).exists(),
            "the attention plugin must be provisioned regardless"
        );
    }

    /// Pin the descriptor-related constants so a refactor that flips the values
    /// trips the test before the wire shape drifts.
    #[test]
    fn mcode_hook_constants_are_pinned() {
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
            &["SessionStart", "Stop", "PermissionRequest"],
            "SessionStart is provisioned for capture without publishing Ready, but remains unvalidated; Stop completes a turn"
        );
        assert!(
            MCODE_HOOK_TIMEOUT_SECONDS > 0,
            "hook timeout must be positive: {MCODE_HOOK_TIMEOUT_SECONDS}"
        );
    }

    /// Issue #1797: `requires_attention_hook` is `true` — live mcode 0.4.12
    /// testing validated the Stop event shape on the historical per-node URL.
    /// The current shared URL and SessionStart remain unvalidated. Reverting
    /// this without new evidence would re-close the Autopilot gate.
    #[test]
    fn requires_attention_hook_is_enabled_after_tui_validation() {
        assert!(
            MCODE.requires_attention_hook(),
            "issue #1797 validated historical Stop delivery against a live 0.4.12 TUI; \
             reverting to false would close the Autopilot gate without cause"
        );
    }

    /// The plugin directory is only loaded when it carries a manifest at the
    /// Claude-compatible location; mcode silently skips a bare directory (the
    /// issue #1797 root cause). Pin the path so a refactor back to the ignored
    /// v0.3.x `hooks/hooks.json` trips here.
    #[test]
    fn manifest_path_is_the_claude_plugin_location() {
        let manifest = plugin_manifest_path(Path::new("/tmp/mcode-home"));
        assert_eq!(
            manifest.file_name().and_then(|n| n.to_str()),
            Some("plugin.json")
        );
        assert_eq!(
            manifest
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some(".claude-plugin"),
            "mcode 0.4.0+ reads `.claude-plugin/plugin.json`; got {manifest:?}"
        );
        assert!(
            manifest.to_string_lossy().contains(MCODE_PLUGIN_DIR),
            "manifest must live under our plugin dir: {manifest:?}"
        );
    }

    /// The Windows invocation wraps the baked curl line in `cmd.exe /c` (mcode
    /// applies no shell interpretation of its own). Non-Windows hosts must never
    /// emit the cmd-shape line.
    #[test]
    fn attention_invocation_matches_host_platform() {
        let url = "http://localhost:1992/api/attention/42";
        let (command, args) = attention_invocation(EnvType::Windows, url);
        if cfg!(target_os = "windows") {
            assert_eq!(command, "cmd.exe");
            assert_eq!(args[0], "/c");
            assert!(
                args[1].contains(url),
                "Windows invocation must carry the url: {}",
                args[1]
            );
            assert!(
                args[1].contains("curl.exe"),
                "Windows invocation must use curl.exe: {}",
                args[1]
            );
            assert!(
                args[1].contains(">nul"),
                "Windows invocation must redirect to nul: {}",
                args[1]
            );
        } else {
            assert_eq!(command, "sh", "non-Windows hosts must not emit cmd.exe");
            assert!(
                !args[1].contains(">nul"),
                "non-Windows hosts must NOT emit Windows `>nul` (creates literal files): {}",
                args[1]
            );
        }
    }

    /// Provisioning contract: the hook invocation targets the currently
    /// configured `/api/attention/mcode` URL with stdin JSON as the body and
    /// does not rely on inherited `BUILDMESH_*` environment. This local test
    /// verifies the generated command against a listener; it does not validate
    /// native delivery from the installed mcode TUI.
    #[test]
    fn attention_invocation_delivers_stdin_to_attention_route_without_env() {
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
            // Even a nonempty successful response must not leak into the mcode
            // hook protocol.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                .unwrap();
            (headers, body)
        });

        // Bake the listener's ephemeral port into the URL the way the
        // provisioner bakes the live Buildmesh port.
        let url = format!("http://localhost:{port}/api/attention/741");
        let env_type = if cfg!(target_os = "windows") {
            EnvType::Windows
        } else {
            EnvType::Wsl
        };
        let (command, args) = attention_invocation(env_type, &url);

        let payload = br#"{"hook_event_name":"Stop","session_id":"8a979720-1cb0-408c-b29c-9f0f68f2982b","transcript_path":"/tmp/x.jsonl","permission_mode":"auto","message":"literal $HOME & %PATH%"}"#;
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(payload).unwrap();
        input.rewind().unwrap();

        let mut invocation = crate::process_util::command_no_window(&command);
        invocation.args(&args);
        invocation
            // Deliberately do NOT provide BUILDMESH_*: the URL is baked, so the
            // callback must work under mcode's env_clear().
            .env_remove("BUILDMESH_PORT")
            .env_remove("BUILDMESH_SESSION_ID")
            .env_remove("BUILDMESH_WSL_HOST")
            .env("NO_PROXY", "localhost,127.0.0.1")
            .env("no_proxy", "localhost,127.0.0.1")
            .stdin(std::process::Stdio::from(input));
        let output = crate::process_util::run_command_with_timeout(
            invocation,
            "mcode attention invocation",
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let (headers, body) = server.join().unwrap();
        assert!(output.stdout.is_empty(), "hook stdout: {:?}", output.stdout);
        assert!(
            headers.starts_with("POST /api/attention/741 HTTP/1.1\r\n"),
            "{headers}"
        );
        // Stop and PermissionRequest share the same invocation shape — assert
        // payload integrity so a regression that re-encodes or filters the body
        // trips here.
        assert_eq!(body, payload);
    }
}
