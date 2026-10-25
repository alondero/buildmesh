//! Codex attention hook handlers, launch configuration, and legacy hook retirement.

use super::*;
/// Codex launches hook commands through PowerShell (Windows; probed against
/// 0.162, where `commandWindows` sees `$PSVersionTable` but not
/// `%CMDCMDLINE%`) or `$SHELL -lc` (Unix), then `env_clear()`s down to a Core
/// inherit snapshot. A cmd.exe-only construct (`& echo {}`, `2>NUL >NUL`) is
/// therefore a PowerShell syntax error and shows as "Hook failed ... exited
/// with code 1", and a nested `cmd.exe /c "%BUILDMESH_PORT%"` never expands
/// and never sees stdin. Bake the loopback callback URL and let Codex's own
/// shell run curl. Discard the HTTP response body and print `{}`: Codex Stop requires
/// JSON on stdout. Attention callbacks are best-effort notifications, so a
/// stopped server or stale node must not fail the Codex hook itself.
///
/// The command carries no quote characters (`printf {}`, not `printf '{}'`):
/// it travels as a `-c` launch argument through shells that re-quote it, and
/// Windows PowerShell 5.1 drops embedded double quotes on the way to a native
/// program.
pub(super) fn attention_hook_unix_command(url: &str) -> String {
    format!(
        "curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST --data-binary @- {url} 2>/dev/null || true; printf {{}}"
    )
}

/// A Windows host driving a *WSL* Codex runs a Linux binary, so `command`
/// reaches `$SHELL -lc` — not cmd.exe. `curl.exe` leaves WSL to reach the
/// Windows loopback listener the node's Buildmesh owns; a Linux curl would
/// resolve `localhost` inside the distro and find nothing.
pub(super) fn attention_hook_wsl_shell_command(url: &str) -> String {
    format!(
        "if command -v curl.exe >/dev/null 2>&1; then curl.exe -fsS --connect-timeout 2 --max-time 10 -o NUL -X POST --data-binary @- {url} 2>/dev/null || true; else curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST --data-binary @- {url} 2>/dev/null || true; fi; printf {{}}"
    )
}

/// The callback a Windows Codex runs. Codex resolves a Windows binary's
/// command hook to `commandWindows` and runs it in PowerShell, so the value is
/// PowerShell statement text built by [`crate::env::windows_hook_callback_command`]
/// — quote-free and newline-free, because it rides a `-c` launch argument
/// through this module's own PowerShell script and then through the `cmd.exe`
/// that runs an npm `codex.CMD` shim.
pub(super) fn attention_hook_windows_command(url: &str) -> String {
    crate::env::windows_hook_callback_command(url)
}

pub(super) const CODEX_HOOK_RELAY_ENV: &str = "BUILDMESH_CODEX_HOOK_RELAY_URL";

pub(super) fn attention_hook_url(node_id: i64, app_port: u16, relay_url: Option<&str>) -> String {
    let app_path = format!("/api/attention/{node_id}");
    if let Some(origin) = relay_url.and_then(valid_loopback_relay_origin) {
        return format!("{origin}{app_path}?forward_port={app_port}");
    }
    format!("http://localhost:{app_port}{app_path}")
}

pub(super) fn valid_loopback_relay_origin(value: &str) -> Option<&str> {
    let origin = value.trim().trim_end_matches('/');
    let authority = origin.strip_prefix("http://")?;
    let (host, port) = authority.rsplit_once(':')?;
    if host != "127.0.0.1" || port.parse::<u16>().ok().filter(|port| *port != 0).is_none() {
        return None;
    }
    Some(origin)
}

/// The `command` Codex runs when it does not prefer `commandWindows`.
///
/// Codex 0.131.0+ resolves a command hook with
/// `command_windows.unwrap_or(command)` under `cfg!(windows)`
/// (`codex-rs/hooks/src/engine/discovery.rs`), so only a Windows binary reads
/// the override. An older binary ignores the unknown key and hands `command`
/// to `cmd.exe /C`, which fails on a POSIX script with `-v was unexpected at
/// this time.` — so `command` has to be written for the shell that will really
/// parse it, which is the *target* Codex binary, not the Buildmesh host.
pub(super) fn attention_hook_default_command(url: &str, env_type: EnvType) -> String {
    if env_type == EnvType::Wsl && cfg!(windows) {
        return attention_hook_wsl_shell_command(url);
    }
    // Every other runtime whose Codex is a Windows binary — spawned natively,
    // or reached through WSL interop — parses `command` with cmd.exe.
    let native = cfg!(windows) && env_type == EnvType::Windows;
    if native || env_type == EnvType::WindowsInterop {
        return attention_hook_windows_command(url);
    }
    attention_hook_unix_command(url)
}

pub(super) fn attention_hook_handler(node_id: i64, env_type: EnvType) -> serde_json::Value {
    let port = crate::http_server::current_http_port();
    let relay_url = std::env::var(CODEX_HOOK_RELAY_ENV).ok();
    let url = attention_hook_url(node_id, port, relay_url.as_deref());
    serde_json::json!({
        "type": "command",
        "command": attention_hook_default_command(&url, env_type),
        "commandWindows": attention_hook_windows_command(&url),
        "statusMessage": BUILDMESH_HOOK_STATUS_MESSAGE,
    })
}

pub(super) const BUILDMESH_HOOK_STATUS_MESSAGE: &str = "Buildmesh attention callback";
pub(super) static ATTENTION_CONFIG_WRITE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// The Codex events Buildmesh listens to. Every event is catch-all except
/// `PreToolUse`, which is matched to the native question tool; `PostToolUse`
/// stays catch-all so an approved permission can correlate by tool name.
pub(super) const ATTENTION_HOOK_EVENTS: [&str; 7] = [
    "SessionStart",
    "Stop",
    "PermissionRequest",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Interrupt",
];

/// Codex 0.162 asks through `request_user_input_async`; older builds use
/// `request_user_input`. A prefix rather than an alternation: the matcher
/// rides the launch command line through cmd.exe, where `|` and `()` are
/// shell syntax.
pub(super) const QUESTION_TOOL_MATCHER: &str = "^request_user_input";

pub(super) fn attention_hook_matcher(event: &str) -> Option<&'static str> {
    (event == "PreToolUse").then_some(QUESTION_TOOL_MATCHER)
}

/// Stand-in `command` for a Windows runtime. Codex on Windows runs
/// `commandWindows` and never reads `command`, but the field is mandatory.
/// Repeating the full callback there would double the launch's size, and on
/// Windows the whole launch is re-encoded into one PowerShell script that has
/// to fit in a 32,767-character process command line. The fallback to
/// `command` that issue #2036 guards is a pre-0.131.0 Codex, which cannot
/// deliver these hooks anyway (the attention capability needs 0.154.0).
pub(super) const WINDOWS_COMMAND_STUB: &str = "echo {}";

pub(super) fn attention_hook_launch_handler(node_id: i64, env_type: EnvType) -> serde_json::Value {
    let mut handler = attention_hook_handler(node_id, env_type);
    if matches!(env_type, EnvType::Windows | EnvType::WindowsInterop) {
        handler["command"] = serde_json::json!(WINDOWS_COMMAND_STUB);
    }
    handler
}

/// A TOML string for a `-c` value. A literal (single-quoted) string keeps the
/// argument free of double quotes, which Windows PowerShell 5.1 drops on the
/// way to a native program; a value that cannot be literal falls back to an
/// escaped basic string.
pub(super) fn toml_launch_string(value: &str) -> String {
    if !value.contains(['\'', '\n', '\r']) {
        return format!("'{value}'");
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{escaped}\"")
}

pub(super) fn attention_hook_toml_value(event: &str, handler: &serde_json::Value) -> String {
    let fields = ["type", "command", "commandWindows", "statusMessage"]
        .into_iter()
        .filter_map(|key| {
            handler[key]
                .as_str()
                .map(|value| format!("{key}={}", toml_launch_string(value)))
        })
        .collect::<Vec<_>>()
        .join(",");
    let matcher = attention_hook_matcher(event)
        .map(|matcher| format!("matcher={},", toml_launch_string(matcher)))
        .unwrap_or_default();
    format!("[{{{matcher}hooks=[{{{fields}}}]}}]")
}

/// Deliver the attention hooks as per-launch `-c` config overrides instead of
/// project files. From a git worktree Codex reads the *main checkout's*
/// `.codex/hooks.json` and ignores the worktree's own, so a file written
/// beside the node is never loaded and the main checkout's file would be
/// shared by every worktree node. Launch arguments carry this node's id, work
/// from any directory, and leave nothing behind to go stale. Codex combines
/// them with the user's own `hooks.json` hooks instead of replacing those.
pub(super) fn attention_hook_launch_args(node_id: i64, env_type: EnvType) -> Vec<String> {
    let handler = attention_hook_launch_handler(node_id, env_type);
    let mut args = vec!["-c".to_string(), "features.hooks=true".to_string()];
    for event in ATTENTION_HOOK_EVENTS {
        args.push("-c".into());
        args.push(format!(
            "hooks.{event}={}",
            attention_hook_toml_value(event, &handler)
        ));
    }
    args
}

/// Remove Buildmesh's own handlers from the text of a `hooks.json`. Earlier
/// versions wrote them there; launch arguments deliver them now, and a handler
/// left behind would fire a second, stale callback for the same event (and
/// can be an outright failing one: older shapes exit with code 1 in Codex's
/// PowerShell). The user's own handlers and unrelated keys are untouched.
/// Returns `None` when there is nothing to remove.
pub(super) fn strip_buildmesh_hook_handlers(existing: &str) -> Result<Option<String>, String> {
    if existing.trim().is_empty() {
        return Ok(None);
    }
    let mut settings: serde_json::Value =
        serde_json::from_str(existing).map_err(|e| format!("failed to parse hooks.json: {e}"))?;
    let Some(events) = settings
        .get_mut("hooks")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(None);
    };

    let mut changed = false;
    events.retain(|_, groups| {
        let Some(groups) = groups.as_array_mut() else {
            return true;
        };
        let before = groups.len();
        groups.retain_mut(|group| {
            let Some(handlers) = group
                .get_mut("hooks")
                .and_then(serde_json::Value::as_array_mut)
            else {
                return true;
            };
            let handlers_before = handlers.len();
            handlers.retain(|handler| !is_buildmesh_hook_handler(handler));
            if handlers.len() == handlers_before {
                return true;
            }
            changed = true;
            !handlers.is_empty()
        });
        before == 0 || !groups.is_empty()
    });
    if !changed {
        return Ok(None);
    }
    serde_json::to_string_pretty(&settings)
        .map(Some)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))
}

pub(super) fn is_buildmesh_hook_handler(handler: &serde_json::Value) -> bool {
    handler.get("statusMessage").and_then(|v| v.as_str()) == Some(BUILDMESH_HOOK_STATUS_MESSAGE)
        || handler
            .get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|command| {
                command.contains("BUILDMESH_PORT")
                    && command.contains("BUILDMESH_SESSION_ID")
                    && command.contains("/api/attention/")
            })
}

/// The main checkout a linked git worktree belongs to, from the worktree's
/// `.git` file (`gitdir: <main>/.git/worktrees/<name>`). `None` for an
/// ordinary repository, where `.git` is a directory.
pub(super) fn linked_worktree_main_checkout(dir: &Path, distro: Option<&str>) -> Option<PathBuf> {
    let git_file = dir.join(".git");
    let content = read_runtime_files(&[&git_file], distro)
        .ok()?
        .into_iter()
        .next()?;
    let git_dir = content
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim()
        .replace('\\', "/");
    let (main, _) = git_dir.rsplit_once("/.git/worktrees/")?;
    Some(PathBuf::from(main))
}

/// Clear Buildmesh's file-based hooks from the spawn directory and, for a
/// linked worktree, from the main checkout Codex actually reads. Files are
/// never created: only an existing `hooks.json` is rewritten, and only to drop
/// Buildmesh's own entries. A `hooks.json` Codex cannot parse is left for
/// Codex to report rather than failing the launch.
pub(super) fn retire_project_file_hooks(
    resolved: &ResolvedPath,
    runtime: &LaunchRuntime,
) -> Result<(), String> {
    let _guard = ATTENTION_CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let distro = runtime_wsl_distro(resolved.env_type, runtime)?;
    let spawn_dir = if distro.is_some() {
        PathBuf::from(&resolved.spawn_path)
    } else {
        PathBuf::from(&resolved.host_path)
    };
    let mut hook_files = vec![spawn_dir.join(".codex").join("hooks.json")];
    if let Some(main) = linked_worktree_main_checkout(&spawn_dir, distro.as_deref()) {
        hook_files.push(main.join(".codex").join("hooks.json"));
    }

    let paths: Vec<&Path> = hook_files.iter().map(PathBuf::as_path).collect();
    let existing = read_runtime_files(&paths, distro.as_deref())?;
    let mut rewritten: Vec<(&Path, String)> = Vec::new();
    for (path, content) in paths.iter().zip(&existing) {
        match strip_buildmesh_hook_handlers(content) {
            Ok(Some(updated)) => rewritten.push((path, updated)),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!("leaving {} untouched: {error}", path.display());
            }
        }
    }
    let writes: Vec<(&Path, &str)> = rewritten
        .iter()
        .map(|(path, content)| (*path, content.as_str()))
        .collect();
    write_runtime_files(&writes, distro.as_deref())
}
