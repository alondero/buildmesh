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
//! **Model override** uses `--model <provider/model>` for the launched
//! session, including `--session <id>` resumes. References may include
//! `#variant`; the adapter validates their syntax before save and spawn,
//! restricting each part to ASCII letters, digits, `.`, `_` and `-` so the
//! Windows Cmd wrapper cannot interpret model text. Accepted references are
//! forwarded unchanged. An omitted model
//! leaves mcode's own default intact, and the override does not change its
//! global model configuration.
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
//!    Codex, so the shared callback URL bakes the loopback port. Because the
//!    manifest is one user-global file, it bakes each running instance's port
//!    rather than the spawning one's, and the fan-out is what keeps the hub and
//!    a dev instance from stealing each other's MiniMax nodes' callbacks
//!    (issue #2343).
//!
//! The merge is idempotent and additive: sibling events and user-authored
//! handlers round-trip untouched, a malformed user file fails closed rather
//! than being overwritten silently, and an unresolvable data dir returns
//! `Ok(())` with no side effects.
//!
//! **Full Access** is pinned on every spawn. mcode's TUI accepts no permission
//! flag in the installed help; `--permission` exists on `mcode exec` only,
//! which we never spawn for interactive nodes — and mcode reads no environment
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

use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode, PermissionModeOption};
use crate::agent::managed_config;
use crate::agent::provider::{
    shell_for, AgentProvider, LaunchRuntime, Platform, ResolvedPath, SpawnRecipe, UiMeta,
};
// The recipe tests assert the shell each platform produces; nothing outside
// `cfg(test)` names the type now that `shell_for` is shared.
#[cfg(test)]
use crate::agent::provider::WindowsShell;
use crate::agent::session_lifecycle::LifecycleKind;
use crate::env::windows_attention_command_for_urls;
use crate::models::EnvType;
use std::path::{Path, PathBuf};

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
const _: () = assert!(MCODE_HOOK_TIMEOUT_SECONDS > 0);

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

/// Build the `(command, args)` pair mcode's hook runner executes for one
/// event. mcode passes `args` to `command` **without shell interpretation**
/// (0.4+ plugin spec), so a bare `curl` invocation cannot express stdin
/// piping or output suppression — the invocation therefore names a shell
/// explicitly and hands it a single command line.
///
/// The URLs are **baked**, never expanded from the environment: mcode
/// `env_clear()`s the `BUILDMESH_*` variables before running a hook (the live
/// 0.4.12 validation observed `%BUILDMESH_PORT%` arriving verbatim), which is
/// the same constraint Codex has. And there is more than one URL: the manifest
/// is a single user-global file, so baking only the spawning instance's port
/// let the most recent MiniMax spawn steal every other instance's callbacks —
/// those nodes then stayed `Running` forever because the callback landed on an
/// instance that does not own the node (issue #2343). Naming every live
/// instance needs no per-instance plugin dir: curl reads the stdin body once
/// and POSTs it to each URL, and the node-independent route lets the owning
/// instance accept the callback while the others discard it as unknown. A curl
/// failure must never surface as a non-zero hook exit — the documented contract
/// is that a hook which cannot reach Buildmesh still exits successfully — so
/// each branch ends in an explicit success: `|| true` under `sh`, `& exit 0`
/// under `cmd.exe`, which a redirection alone does not give, and the same in the
/// interop relay below.
///
/// The `WindowsInterop` case is the mirror image: Buildmesh runs *inside* the
/// WSL/Linux host and mcode is the **Windows** executable reached through
/// interop (`EnvType::WindowsInterop` = "Windows executable reached through
/// interoperability from a Linux WSL host"). The callback is therefore
/// relayed back into the guest by `windows_attention_command_for_urls`, whose
/// `powershell.exe … -EncodedCommand <b64>` line runs
/// `wsl.exe -d <distro> --exec curl …`; that curl lands on the Linux-side
/// listener's own loopback, so this direction needs no mirrored networking.
/// Its single-line command is split into the `command` + `args` shape.
fn attention_invocation(env_type: EnvType, urls: &[String]) -> (String, Vec<String>) {
    if env_type == EnvType::WindowsInterop {
        if let Some(command) = windows_attention_command_for_urls(urls) {
            let mut parts = command.split(' ');
            let executable = parts.next().unwrap_or("powershell.exe").to_string();
            return (executable, parts.map(str::to_string).collect());
        }
    }
    let targets = urls.join(" ");
    if cfg!(target_os = "windows") && env_type == EnvType::Windows {
        (
            "cmd.exe".to_string(),
            vec![
                "/c".to_string(),
                // `& exit 0` is load-bearing, not decoration: a redirection does
                // not touch the exit code, and `cmd.exe /c` reports curl's. With
                // several URLs curl's status is the last transfer's, so one
                // unreachable sibling — the normal case while it shuts down, or
                // while every instance is gone — would fail the hook and mcode
                // would surface it as a hook error (#2343 review). The POSIX
                // branch's `|| true` is the same guarantee.
                format!(
                    "curl.exe -sf --connect-timeout 1 --max-time 2 -X POST --data-binary @- {targets} >nul 2>nul & exit 0"
                ),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "curl -sf --connect-timeout 1 --max-time 2 -X POST --data-binary @- {targets} >/dev/null 2>&1 || true"
                ),
            ],
        )
    }
}

/// One inline handler entry for an event in the manifest's `hooks` map. The
/// mcode 0.4.0+ shape nests the executable entry under a `hooks` array beside
/// a `matcher` (a `"*"` matcher matches every occurrence).
fn attention_handler(_node_id: i64, env_type: EnvType) -> serde_json::Value {
    let urls = attention_targets();
    let (command, args) = attention_invocation(env_type, &urls);
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

/// The node-independent attention route on every Buildmesh instance listening
/// on this machine, this one included. See [`attention_invocation`] for why a
/// baked handler cannot name only the instance that spawned the node.
fn attention_targets() -> Vec<String> {
    crate::http::instances::live_ports(crate::http::current_http_port())
        .into_iter()
        .map(|port| format!("http://localhost:{port}/api/attention/mcode"))
        .collect()
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
    let eol = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let Some((key_line, comment)) = find_top_level_key(&existing, MCODE_PERMISSION_MODE_KEY) else {
        let mut out = existing;
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push_str(eol);
            }
            // Blank separator so the appended key reads as its own setting.
            out.push_str(eol);
        }
        out.push_str(&format!(
            "{MCODE_PERMISSION_MODE_KEY}: {MCODE_PERMISSION_MODE_VALUE}{eol}"
        ));
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
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        // The character right after the name must be a `:` (optionally
        // spaced) — this is what rejects a longer sibling key.
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(value_and_comment) = rest.strip_prefix(':') else {
            continue;
        };
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
        let start = (0..i)
            .rev()
            .find(|j| !bytes[*j].is_ascii_whitespace())
            .map_or(0, |j| j + 1);
        return &value_and_comment[start..];
    }
    ""
}

/// True when the located line already carries `value`, i.e. nothing to do.
/// Compares only the text between the `:` and any comment, trimmed, so
/// `permissionMode:  bypassPermissions ` counts as already pinned.
fn already_pinned(line: &str, comment: &str, value: &str) -> bool {
    let value_end = line.len().saturating_sub(comment.len());
    line[..value_end]
        .split_once(':')
        .map(|(_, found)| found.trim() == value)
        .unwrap_or(false)
}

/// Persist the pinned config and report that the write happened.
fn write_pinned_config(path: &Path, contents: &str) -> Result<bool, String> {
    managed_config::atomic_write(path, contents)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
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
    managed_config::carries_anchors(haystack.trim_end(), &[BUILDMESH_HOOK_MARKER])
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
/// (trailing comma, partial edit, syntax error) is refused through the shared
/// `managed_config::read_document` refusal rather than silently clobbered:
/// the user's data must surface to the spawn path as a provision failure so it
/// can be repaired.
///
/// Returns `Ok(())` without rewriting the file when every event already
/// carries the expected handler (the issue #886 idempotency invariant — no
/// spurious mtime bumps).
fn ensure_plugin_manifest(path: &Path, handler: &serde_json::Value) -> Result<(), String> {
    let mut document = managed_config::read_document(path).map_err(|error| error.to_string())?;
    let object = managed_config::root_object(&mut document, "mcode plugin manifest top-level")?;

    let mut changed = false;
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

    let hooks = managed_config::object_entry(object, "hooks", "mcode plugin manifest `hooks`")?;
    for event in MCODE_PROVISIONED_EVENTS {
        let group_array = managed_config::array_entry(
            hooks,
            event,
            &format!("mcode plugin manifest `hooks.{event}`"),
        )?;
        changed |= managed_config::upsert_entry(group_array, &is_buildmesh_handler, handler);
    }

    if !changed {
        return Ok(());
    }
    let content = document
        .to_pretty_json()
        .map_err(|e| format!("serialize mcode plugin manifest failed: {e}"))?;
    managed_config::atomic_write(path, &content)
        .map_err(|e| format!("failed to write mcode plugin manifest: {e}"))?;
    tracing::info!("mcode provision_attention_hooks: wrote {:?}", path);
    Ok(())
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
        // Issue #2151: bare — mcode's TUI accepts no permission flag, so
        // there is no approval argv to strip. Full Access is enforced by
        // the `config.yaml` pin (`pin_full_access`), not by argv.
        SpawnRecipe {
            binary: "mcode",
            base_args: vec![],
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    /// Issue #2151: mcode's TUI accepts no permission flag, so the mode
    /// set is a singleton — Full Access via the `permissionMode:
    /// bypassPermissions` pin in `<dataDir>/config.yaml`
    /// ([`pin_permission_mode`]). There is no prompt entry to offer: the
    /// pin is machine-global (shared with the user's standalone `mcode`
    /// sessions), so merely skipping the pin would not restore prompts
    /// once a previous spawn pinned it. Settings shows this enforced
    /// mode explicitly rather than a choice.
    fn permission_modes(&self) -> Vec<PermissionModeOption> {
        vec![PermissionModeOption::unattended(
            "Full Access (permissionMode: bypassPermissions)",
            "Pinned in mcode's config.yaml on every spawn — the TUI has no permission flag.",
        )]
    }

    fn supports_resume(&self) -> bool {
        true
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
        spawn.base_args = [
            "exec",
            "--input",
            "-",
            "--input-format",
            "text",
            "--permission",
            "smart",
            "--max-steps",
            "1",
            "--timeout",
            "30s",
        ]
        .map(str::to_owned)
        .to_vec();
        Some(BackgroundRecipe::new(
            spawn,
            BackgroundPromptInput::Stdin,
            BackgroundResultOutput::LastMessageFile,
        ))
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

    /// The interactive CLI accepts a session-only `--model` override (0.6.5).
    fn supports_model_override(&self) -> bool {
        true
    }

    fn validate_model_override(&self, model: &str) -> Result<(), String> {
        // A conservative identifier alphabet keeps references literal when
        // the Windows .cmd shim is launched through cmd.exe.
        let identifier = |value: &str| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        };
        let valid = model.split_once('/').is_some_and(|(provider, selection)| {
            let (name, variant) = selection
                .split_once('#')
                .map_or((selection, None), |(name, variant)| (name, Some(variant)));
            identifier(provider) && identifier(name) && variant.is_none_or(identifier)
        });
        if valid {
            Ok(())
        } else {
            Err("MiniMax Code model reference must use provider/model or provider/model#variant; each part allows only ASCII letters, digits, '.', '_' and '-'".into())
        }
    }

    fn supports_extra_args(&self) -> bool {
        // Issue #1358: mcode's interactive TUI still accepts arbitrary
        // CLI flags as positional args (it's a runtime, not a
        // vocab-restricted CLI like Codex). Effort controls remain
        // unsupported; model selection uses the dedicated override.
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
#[path = "mcode_tests.rs"]
mod tests;
