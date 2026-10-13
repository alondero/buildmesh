//! OS-axis seam — wraps a provider's `SpawnRecipe` in the right shell for the
//! runtime environment.
//!
//! - WSL on Windows: `wsl.exe -d <distro> --cd <path> --exec sh -lc ...`
//! - WSL on Linux: direct invocation
//! - macOS, sandbox requested: `sandbox-exec -f <profile.sb> <binary> <args...>`
//!   (Seatbelt containment to the worktree — see `agent::sandbox`, issue #497)
//! - macOS, sandbox not requested: direct invocation
//! - Windows native + PowerShell shell: `powershell.exe -NoLogo -EncodedCommand <base64>`
//!   (used by Codex so ANSI escapes propagate correctly through ConPTY). When
//!   the node's directory is UNC and the executable is a batch shim, this
//!   becomes `cmd.exe /d /c <temp-batch.cmd>`, where the batch `pushd`s the
//!   quoted directory and then loads a companion PowerShell script — cmd.exe
//!   rejects a UNC current directory, and a batch file is the only place a
//!   `&`-containing directory can be quoted so cmd parses it as one argument.
//!   See [`needs_unc_pushd`] and [`mapped_launch_batch`].
//! - Windows native + Cmd shell: `cmd.exe /c "<binary> <args>"`
//!   (used by node-shim providers whose binary is a `.cmd` batch file)
//! - Windows native + Direct: spawn the binary directly (rare; mainly for tests)
//!
//! "Sandbox requested" means the Mesh flag is on *and* the developer gate is
//! open ([`crate::sandbox::sandbox_requested`]); the feature is experimental
//! and inert in shipped builds (#2034). Because a requested sandbox is a
//! promise, [`wrap`] is fallible: the macOS Seatbelt profile is written during
//! command assembly, so a write failure returns `Err` rather than degrading to
//! an unsandboxed spawn.
//!
//! The module also owns spawn-environment hygiene that is not a harness's to
//! decide: [`CLAUDE_SESSION_MARKER_ENV_VARS`] and the scrub that drops them
//! from both the PTY spawn ([`wrap`]) and the pipe-based background spawn
//! ([`background_command`]), so an agent Buildmesh launched from inside a
//! Claude Code session does not inherit that session's markers (#2136).

use crate::agent::provider::{SpawnRecipe, WindowsShell};
use crate::models::EnvType;
use crate::pty;
use portable_pty::CommandBuilder;

/// Environment a launching Claude Code session hands to every process it
/// spawns, so each can tell it belongs to *that* session.
///
/// Buildmesh inherits the whole set whenever the app itself was started from a
/// Claude Code session's shell — an agent running `scripts\run-dev.ps1` for
/// `/use`, `/verify` or `/verify-ui` — and then hands it straight back to the
/// agents it spawns. `CLAUDE_CODE_CHILD_SESSION` is the damaging one: Claude
/// Code reads it and starts with "Transcript saving is off — inherited
/// CLAUDE_CODE_CHILD_SESSION marker", writes no
/// `~/.claude/projects/<dir>/<session>.jsonl`, and every transcript consumer
/// downstream (a Circuit's first gate, session recovery) parks on a transcript
/// that will never exist. The rest leak the launching session's identity and,
/// for the messaging token and socket, its credentials into another agent
/// process. Issue #2136.
///
/// This is an explicit list, never a `CLAUDE*` prefix rule. Buildmesh sets
/// `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` and
/// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` on purpose for the MiniMax naming
/// side-channel (`provider_conf::minimax_backend_env`), and `CLAUDE_CONFIG_DIR`
/// for the Windows sandbox; a prefix rule would silently undo them. A
/// deliberately-set value also survives by ordering rather than by exclusion:
/// the harness environment policy and per-profile backend env are layered on
/// top in `agent::spawn::command` *after* this scrub has run.
pub const CLAUDE_SESSION_MARKER_ENV_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// Drop the launching session's markers from a PTY agent spawn.
///
/// `portable_pty::CommandBuilder` seeds itself from the live process
/// environment, so `env_remove` clears an *inherited* value, not just one this
/// process set — that is the whole mechanism, and the reason a caller cannot
/// fix this by not setting the variables.
fn strip_claude_session_markers(cmd: &mut CommandBuilder) {
    for key in CLAUDE_SESSION_MARKER_ENV_VARS {
        cmd.env_remove(key);
    }
}

/// The same scrub for the pipe-based background launches (`agent::background`:
/// session naming, circuit classifiers). Those build a `std::process::Command`
/// rather than a `CommandBuilder`, so they cannot share the loop above, and
/// they are a second instance of the same leak — a backgrounded
/// `claude --print` inherits the same markers the interactive agent must not.
fn strip_claude_session_markers_from_command(cmd: &mut std::process::Command) {
    for key in CLAUDE_SESSION_MARKER_ENV_VARS {
        cmd.env_remove(key);
    }
}

/// Encode a command string for PowerShell's -EncodedCommand parameter.
/// -EncodedCommand decodes the Base64 payload into a PowerShell *script* and
/// runs it. That avoids PowerShell's argument tokenizer (which would mangle
/// `<>()` and backticks), but the decoded text is still parsed as PowerShell,
/// so newlines and special tokens at the script's top level become statements.
/// Always pair this with [`format_powershell_command`] to keep prefill text
/// inside a single-quoted string literal.
fn encode_for_powershell(cmd: &str) -> String {
    crate::env::encode_powershell(cmd)
}

/// Quote a single PowerShell argument as a single-quoted string literal.
/// Shared PowerShell quoting escapes ASCII and smart apostrophes. Newlines,
/// backticks, brackets and `$` remain literal inside the quoted string.
fn ps_single_quote(s: &str) -> String {
    crate::env::powershell_literal(s)
}

/// Build a PowerShell script that invokes `binary` with `args`, with every
/// token wrapped in single quotes and dispatched via the call operator (`&`).
/// This ensures arguments containing newlines or PowerShell-significant chars
/// (backticks, `<>`, parentheses, `|`, `;`, `$`, `#`, `&`, brackets, list
/// markers like `1.`) are passed through as literal strings rather than being
/// parsed as new statements when the script is decoded by `-EncodedCommand`.
fn format_powershell_command(binary: &str, args: &[String]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(args.len() + 1);
    parts.push(ps_single_quote(binary));
    for a in args {
        parts.push(ps_single_quote(a));
    }
    format!("& {}", parts.join(" "))
}

/// True when `path` is a Windows UNC path in either separator style — the form
/// both cmd.exe and PowerShell treat as "not a drive".
fn is_unc_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

/// True when the file can only be executed by handing it to `cmd.exe`.
fn is_batch_shim(executable: &str) -> bool {
    matches!(
        std::path::Path::new(executable)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd" | "bat")
    )
}

/// Whether this spawn has to establish a UNC directory through cmd's `pushd`
/// rather than inherit one.
///
/// PowerShell runs a `.cmd`/`.bat` shim by delegating to `cmd.exe /c`, and
/// cmd.exe rejects a UNC current directory: it reports that UNC paths are
/// unsupported and silently continues in `C:\Windows`, leaving the agent with
/// no repository, no worktree, and no transcript where Buildmesh expects one.
/// A node whose runtime is Windows but whose directory lives in WSL is spawned
/// with exactly such a directory, because `apply_harness_runtime` hands the
/// spawn that directory's UNC host path.
///
/// A `.exe` or a bare stem is launched by PowerShell itself and never reaches
/// cmd.exe, so it needs no mapping, and PowerShell accepts a UNC current
/// directory. A drive-letter directory is not UNC at all, and the `.cmd`
/// launch works there today.
///
/// This is deliberately a property of the *command* and not a filesystem
/// probe. On the `WindowsInterop` arm Buildmesh runs on the Linux host, so
/// asking whether a Windows path exists would query the wrong filesystem and
/// silently decline — which is what an earlier revision of this fix did, and
/// why it never fired on the route that needed it.
///
/// This covers the PowerShell arms only. The `Cmd` arms run their own `pushd`
/// with the directory as a plain command-line argument and so keep the
/// metacharacter exposure cmd brings with that — a pre-existing defect that
/// reaches every npm-shimmed harness, not just Codex. It is recorded here
/// rather than fixed here because that arm has its own live WSL test, and
/// reshaping it belongs in a change that can exercise it end to end.
fn needs_unc_pushd(executable: &str, spawn_path: &str) -> bool {
    is_unc_path(spawn_path) && is_batch_shim(executable)
}

/// The batch file the mapped spawn runs.
///
/// The node directory is quoted inside the file rather than carried on the
/// outer command line, where portable-pty's argument quoting leaves some cmd
/// metacharacters exposed. Percent signs are doubled because cmd expands them
/// inside batch files even within quotes. The batch carries the directory
/// itself, so this route needs no directory variable forwarded through WSLENV.
///
/// Only the short, encoded loader is inline. Encoding the full spawn script
/// here would spend cmd's 8,191-character batch-line budget on Base64 expansion.
fn mapped_launch_batch(encoded: &str, directory: &str) -> String {
    format!(
        "@echo off\r\n\
         rem Keep the directory quoted in the file and escape batch percent signs.\r\n\
         rem Fail closed if the share cannot be mapped, so the agent reports it\r\n\
         rem instead of quietly running somewhere else.\r\n\
         pushd \"{}\"\r\n\
         if errorlevel 1 exit /b 1\r\n\
         powershell.exe -NoLogo -NoProfile -EncodedCommand {encoded}\r\n",
        directory.replace('%', "%%").replace('"', "\"\"")
    )
}

/// Where the one-shot batch file is written, and the name `cmd.exe` is given.
///
/// The two differ on `WindowsInterop`. Buildmesh runs on the Linux host and
/// writes through `/mnt/c/...`, which is the same directory as
/// `C:\Users\...` — but `cmd.exe` is a Windows process and cannot resolve a
/// `/mnt/c/...` path. Passing the host path is how this route hung with a
/// Windows-format argument that Windows could not open: `cmd.exe` reported an
/// unrecognised path and then blocked on a PTY the spawner was not reading.
struct MappedLaunchTarget {
    /// The filesystem Buildmesh writes the file through.
    write_path: std::path::PathBuf,
    /// The path handed to `cmd.exe /c`, in Windows form.
    windows_path: String,
}

impl MappedLaunchTarget {
    fn native() -> Self {
        let directory = std::env::temp_dir();
        Self {
            windows_path: directory.to_string_lossy().into_owned(),
            write_path: directory,
        }
    }
}

/// Resolve a Windows-visible directory for the companion launch files.
/// A Linux host's `/tmp` is not openable by Windows-side cmd.exe.
fn mapped_launch_directory(env_type: EnvType) -> Result<MappedLaunchTarget, String> {
    // `is_wsl_host` is the discriminator, not just the env type. On a Windows
    // host an interop spawn is not a real configuration, so the native shape is
    // harmless and keeps this path testable there; on a WSL host `/tmp` is the
    // guest's and must never be handed to cmd.exe.
    if env_type != EnvType::WindowsInterop || !crate::env::is_wsl_host() {
        return Ok(MappedLaunchTarget::native());
    }
    let home = crate::env::windows_home().ok_or_else(|| {
        "a mapped WindowsInterop spawn needs a Windows-visible batch file, but the Windows \
         user profile could not be resolved from the WSL host"
            .to_string()
    })?;
    // Built as a Windows path, and deliberately *not* run through
    // `windows_cli_home`: that returns the host-side spelling of a Windows
    // path, which is correct for opening a file from Linux and wrong for
    // telling Windows where it is.
    let windows_dir = format!(
        "{}\\{}",
        home.trim_end_matches('\\'),
        "AppData\\Local\\Temp"
    );
    let write_path = std::path::PathBuf::from(crate::env::to_host_path(&windows_dir));
    Ok(MappedLaunchTarget {
        windows_path: windows_dir,
        write_path,
    })
}

/// Characters cmd.exe treats as something other than path text when they appear
/// on its command line.
///
/// Unlike the node directory written inside the batch, this path is passed as
/// `cmd.exe /c <path>` — command text, which cmd expands before it runs. A temp
/// directory containing a segment like `%USERNAME%` is therefore
/// substituted before the script is even opened, and the spawn fails or reaches
/// the wrong file. `!` is included for the same reason: delayed expansion is off
/// by default, but it is a policy setting rather than a guarantee.
fn has_cmd_metacharacter(path: &str) -> bool {
    path.chars().any(|c| "&|<>()^\"%!".contains(c))
}

/// Delete batch files abandoned by an earlier run, once per process.
///
/// The file cannot be deleted by the batch itself: cmd.exe reads a batch file
/// incrementally, so `del "%~f0"` removes it and the *next* read fails with
/// *The batch file cannot be found*. Nor can the child clean up after itself.
/// So the leftovers are swept instead, by age — long enough that no live
/// agent's script is a candidate, and a locked file simply fails to delete.
fn sweep_stale_mapped_batches(directory: &std::path::Path) {
    static SWEPT: std::sync::Once = std::sync::Once::new();
    SWEPT.call_once(|| {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        let cutoff =
            std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(24 * 60 * 60));
        let Some(cutoff) = cutoff else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(MAPPED_BATCH_PREFIX)
                || !(name.ends_with(".cmd") || name.ends_with(".ps1"))
            {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if modified < cutoff {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
}

const MAPPED_BATCH_PREFIX: &str = "buildmesh-mapped-spawn-";

/// Write a batch file and its companion PowerShell script.
///
/// The name carries a random suffix from `tempfile`: a pid-and-counter name
/// restarts its counter after a process restart, so the same pid could
/// overwrite a file a still-running `cmd.exe` holds open.
fn write_mapped_launch_batch(
    target: &MappedLaunchTarget,
    script: &str,
    directory: &str,
) -> Result<String, String> {
    sweep_stale_mapped_batches(&target.write_path);
    let mut file = tempfile::Builder::new()
        .prefix(MAPPED_BATCH_PREFIX)
        .suffix(".cmd")
        .tempfile_in(&target.write_path)
        .map_err(|error| {
            format!(
                "creating the mapped spawn script in {}: {error}",
                target.write_path.display()
            )
        })?;
    let name = file
        .path()
        .file_name()
        .ok_or_else(|| "the mapped spawn script has no file name".to_string())?;
    let windows_path = format!("{}\\{}", target.windows_path, name.to_string_lossy());
    if has_cmd_metacharacter(&windows_path) {
        // Unlike the node directory, this path cannot travel in an environment
        // variable: it is what tells cmd.exe which script to run. A Windows
        // account name containing `&` would otherwise split the command line
        // and execute the tail, so refuse rather than mis-spawn.
        return Err(format!(
            "the temp directory for a mapped spawn contains a character cmd.exe treats as a \
             command operator: {windows_path}"
        ));
    }
    let script_path = file.path().with_extension("ps1");
    let script_handle = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&script_path)
        .map_err(|error| format!("creating the mapped PowerShell script: {error}"))?;
    let mut script_file = tempfile::NamedTempFile::from_parts(
        script_handle,
        tempfile::TempPath::try_from_path(&script_path)
            .map_err(|error| format!("tracking the mapped PowerShell script: {error}"))?,
    );
    std::io::Write::write_all(&mut script_file, script.as_bytes())
        .map_err(|error| format!("writing the mapped PowerShell script: {error}"))?;
    let windows_script = format!(
        "{}\\{}",
        target.windows_path,
        script_path
            .file_name()
            .ok_or_else(|| "the mapped PowerShell script has no file name".to_string())?
            .to_string_lossy()
    );
    // Read as UTF-8 through .NET and invoke as a script block: -File would add
    // an execution-policy dependency, and Windows PowerShell's default file
    // decoding would corrupt non-ASCII prompts without a BOM.
    let loader = format!(
        "$ErrorActionPreference = 'Stop'; & ([scriptblock]::Create([IO.File]::ReadAllText({}))); exit $LASTEXITCODE",
        ps_single_quote(&windows_script)
    );
    let encoded = encode_for_powershell(&loader);
    let spent = POWERSHELL_ENCODED_PREFIX.len() + encoded.len();
    if spent > CMD_EXE_COMMAND_LINE_CHARS {
        return Err(format!(
            "the mapped spawn loader needs a {spent} character PowerShell command line, over \
             cmd.exe's {CMD_EXE_COMMAND_LINE_CHARS} character batch-line limit"
        ));
    }
    std::io::Write::write_all(
        &mut file,
        mapped_launch_batch(&encoded, directory).as_bytes(),
    )
    .map_err(|error| format!("writing the mapped spawn batch: {error}"))?;
    let (_script_handle, kept_script) = script_file
        .keep()
        .map_err(|error| format!("releasing the mapped PowerShell script: {error}"))?;
    if let Err(error) = file.keep() {
        let _ = std::fs::remove_file(kept_script);
        return Err(format!("releasing the mapped spawn batch: {error}"));
    }
    Ok(windows_path)
}

/// Windows' per-process command-line limit. CreateProcess accepts at most
/// 32,767 characters on one command line, and there is no shell-side
/// workaround: a launch over it cannot run at all.
pub(crate) const WINDOWS_PROCESS_COMMAND_LINE_CHARS: usize = 32_767;

/// `cmd.exe`'s own, much tighter limit. It parses only the first 8,191
/// characters of the command line it is handed and answers the rest with *The
/// command line is too long.* on its output stream. PowerShell reaches a
/// `.cmd` / `.bat` shim — which is how an npm-installed Codex is shipped —
/// by handing exactly such a line to `cmd.exe /c`, so a PowerShell launch
/// crosses two limits, not one (#2184).
pub(crate) const CMD_EXE_COMMAND_LINE_CHARS: usize = 8_191;

/// The fixed part of a `powershell.exe -EncodedCommand` launch's command line.
const POWERSHELL_ENCODED_PREFIX: &str = "powershell.exe -NoLogo -NoProfile -EncodedCommand ";

/// Return the number of UTF-16 code units needed for one Windows command-line
/// argument using the quoting rules used by `Command`. Quotes and backslashes
/// before quotes are doubled, so a byte-count approximation can accept a line
/// that `cmd.exe` still rejects near its limit.
fn windows_argument_command_line_chars(argument: &str) -> usize {
    let needs_quotes = argument.is_empty()
        || argument
            .chars()
            .any(|character| character.is_whitespace() || character == '"');
    if !needs_quotes {
        return argument.encode_utf16().count();
    }

    let mut chars = 2; // surrounding quotes
    let mut backslashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            backslashes += 1;
        } else if character == '"' {
            chars += backslashes * 2 + 2;
            backslashes = 0;
        } else {
            chars += backslashes + character.len_utf16();
            backslashes = 0;
        }
    }
    chars + backslashes * 2
}

fn windows_command_line_chars(executable: &str, args: &[String]) -> usize {
    windows_argument_command_line_chars(executable)
        + args
            .iter()
            .map(|arg| 1 + windows_argument_command_line_chars(arg))
            .sum::<usize>()
}

fn powershell_command_line_chars(encoded_script: &str) -> usize {
    windows_command_line_chars(
        "powershell.exe",
        &[
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-EncodedCommand".into(),
            encoded_script.into(),
        ],
    )
}

fn cmd_command_line_chars(args: &[String]) -> usize {
    windows_command_line_chars("cmd.exe", args)
}

fn command_line_error(executable: &str, spent: usize, limit: usize, via: &str) -> String {
    format!(
        "the launch of {executable} spends {spent} characters on {via}, over Windows' \
         {limit}-character command-line limit; shorten the initial prompt or the \
         harness's extra arguments"
    )
}

fn ensure_powershell_command_line_fits(script: &str) -> Result<(), String> {
    let encoded = encode_for_powershell(script);
    let spent = powershell_command_line_chars(&encoded);
    if spent > WINDOWS_PROCESS_COMMAND_LINE_CHARS {
        return Err(command_line_error(
            "powershell.exe",
            spent,
            WINDOWS_PROCESS_COMMAND_LINE_CHARS,
            "the encoded PowerShell process command line",
        ));
    }
    Ok(())
}

fn ensure_cmd_command_line_fits(executable: &str, args: &[String]) -> Result<(), String> {
    let mut command_args = vec!["/d".into(), "/c".into(), executable.into()];
    command_args.extend_from_slice(args);
    ensure_cmd_arguments_fit(&command_args, executable)
}

fn ensure_cmd_arguments_fit(command_args: &[String], executable: &str) -> Result<(), String> {
    let spent = cmd_command_line_chars(command_args);
    if spent > WINDOWS_PROCESS_COMMAND_LINE_CHARS {
        return Err(command_line_error(
            executable,
            spent,
            WINDOWS_PROCESS_COMMAND_LINE_CHARS,
            "the cmd.exe process command line",
        ));
    }
    if spent > CMD_EXE_COMMAND_LINE_CHARS {
        return Err(command_line_error(
            executable,
            spent,
            CMD_EXE_COMMAND_LINE_CHARS,
            "the cmd.exe command line that runs it",
        ));
    }
    Ok(())
}

/// Refuse a Windows launch whose assembled command line cannot fit the limits
/// above, instead of spawning a shell that prints *The command line is too
/// long.* into the node's terminal and exits (#2184).
///
/// A launch's arguments travel twice under the `PowerShell` arm: once inside
/// the Base64 script this process hands `powershell.exe`, and again inside the
/// line PowerShell builds for the child it starts. Only the child's line has a
/// second, tighter limit — and only when the child has to be reached through
/// `cmd.exe`, which is how PowerShell runs a batch shim.
fn ensure_windows_command_line_fits(
    windows_shell: WindowsShell,
    executable: &str,
    args: &[String],
    encoded_script: Option<&str>,
) -> Result<(), String> {
    match windows_shell {
        WindowsShell::PowerShell => {
            if let Some(script) = encoded_script {
                ensure_powershell_command_line_fits(script)?;
            }
            if is_batch_shim(executable) {
                ensure_cmd_command_line_fits(executable, args)?;
            }
        }
        WindowsShell::Cmd => ensure_cmd_command_line_fits(executable, args)?,
        WindowsShell::Direct => {
            let spent = windows_command_line_chars(executable, args);
            if spent > WINDOWS_PROCESS_COMMAND_LINE_CHARS {
                return Err(command_line_error(
                    executable,
                    spent,
                    WINDOWS_PROCESS_COMMAND_LINE_CHARS,
                    "the process command line",
                ));
            }
        }
    }
    Ok(())
}

/// Build the Windows-native invocation for a PowerShell-wrapped spawn.
///
/// Normally that is `powershell.exe -EncodedCommand <script>`. When `mapped`
/// carries a directory, cmd's `pushd` establishes it first and PowerShell runs
/// inside the drive it mapped: cmd.exe can only be given a drive-letter current
/// directory, so cmd is the shell that has to do the mapping. That mapping runs
/// from [`mapped_launch_batch`] so the directory never appears on a command
/// line. The agent's argv stays in the companion script, preserving the
/// literal PowerShell arguments from `format_powershell_command`.
///
/// The batch file is swept by age rather than deleted when the child exits:
/// cmd.exe reads a batch file incrementally, so neither the batch's own
/// `del "%~f0"` nor an early delete works — both leave cmd unable to read the
/// next line. See [`sweep_stale_mapped_batches`].
fn powershell_spawn(
    env_type: EnvType,
    script: &str,
    mapped: Option<&str>,
) -> Result<CommandBuilder, String> {
    // -NoProfile skips loading the user's PowerShell profile, which can add
    // hundreds of ms (modules, prompt frameworks) to *every* agent spawn. This
    // shell only needs to relay the agent CLI's ANSI output through ConPTY — it
    // never touches profile state, and the agent binary is already resolved to
    // an absolute path.
    let Some(directory) = mapped else {
        let encoded = encode_for_powershell(script);
        tracing::info!("spawn_environment: building powershell.exe for the spawn script");
        let mut c = CommandBuilder::new("powershell.exe");
        c.args(["-NoLogo", "-NoProfile", "-EncodedCommand", &encoded]);
        return Ok(c);
    };
    tracing::info!("spawn_environment: mapping the UNC spawn directory through a cmd pushd script");
    let launch = mapped_launch_directory(env_type)?;
    let batch = write_mapped_launch_batch(&launch, script, directory)?;
    let mut c = CommandBuilder::new("cmd.exe");
    c.args(["/d", "/c", &batch]);
    Ok(c)
}

/// The single decision of how a Windows-side spawn establishes its working
/// directory, consumed by all three sites that need to agree on it: the
/// interop branch that builds the script, the native `PowerShell` arm that
/// builds the command, and the gate that decides whether the process inherits a
/// working directory at all.
///
/// They have to agree. A mapped spawn that still receives the UNC makes cmd.exe
/// print *Defaulting to Windows directory* and continue, so the agent looks
/// broken while quietly working; an unmapped spawn that drops the directory
/// falls back to `USERPROFILE` and opens in the home folder instead of the
/// node's. `WindowsShell::Direct` is the case that proves it — it establishes
/// no location of its own, so it must both map and keep the directory.
///
/// Only the PowerShell arms map. The `Cmd` arms run `pushd` inside their own
/// command already and keep the working directory they have always had; see
/// the note in [`needs_unc_pushd`] about why that arm still hands cmd.exe a
/// UNC path.
fn maps_directory_with_pushd(
    env_type: EnvType,
    windows_shell: WindowsShell,
    executable: &str,
    spawn_path: &str,
) -> bool {
    match env_type {
        // Built by `wsl.exe`; the agent runs in the guest, where a Windows
        // drive mapping would mean nothing.
        EnvType::Wsl => false,
        EnvType::Windows | EnvType::WindowsInterop => {
            windows_shell == WindowsShell::PowerShell && needs_unc_pushd(executable, spawn_path)
        }
    }
}

/// Pipe-based background tasks still need the adapter's Windows shell for npm shims.
pub(crate) fn background_command(
    recipe: &SpawnRecipe,
    executable: Option<&std::path::Path>,
) -> Result<std::process::Command, String> {
    let executable = executable
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| recipe.binary.into());
    let args: Vec<String> = recipe.argv().map(str::to_owned).collect();
    let mut cmd = if cfg!(windows) && recipe.windows_shell == WindowsShell::PowerShell {
        let script = format!(
            "{}; exit $LASTEXITCODE",
            format_powershell_command(&executable, &args)
        );
        ensure_windows_command_line_fits(
            WindowsShell::PowerShell,
            &executable,
            &args,
            Some(&script),
        )?;
        let mut cmd = crate::process_util::command_no_window("powershell.exe");
        cmd.args([
            "-NoLogo",
            "-NoProfile",
            "-EncodedCommand",
            &encode_for_powershell(&script),
        ]);
        cmd
    } else {
        let mut cmd = if cfg!(windows) && recipe.windows_shell == WindowsShell::Cmd {
            ensure_windows_command_line_fits(WindowsShell::Cmd, &executable, &args, None)?;
            let mut cmd = crate::process_util::command_no_window("cmd.exe");
            cmd.args(["/d", "/c", &executable]);
            cmd
        } else {
            if cfg!(windows) {
                ensure_windows_command_line_fits(WindowsShell::Direct, &executable, &args, None)?;
            }
            crate::process_util::command_no_window(executable)
        };
        cmd.args(args);
        cmd
    };
    // The recipe's own deliberate env is applied by the caller afterwards, so
    // scrubbing here clears only what the launching session contributed.
    strip_claude_session_markers_from_command(&mut cmd);
    // The guard signals this process's group (`kill(-pid)`). A child that
    // merely inherited our group has no group whose id is its pid, so that
    // signal is ESRCH and cancellation would leave descendants running.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(cmd)
}

pub fn wrap(
    mut recipe: SpawnRecipe,
    env_type: EnvType,
    wsl_distro: Option<&str>,
    executable_override: Option<&str>,
    spawn_path: &str,
    session_id: i64,
    sandbox: bool,
) -> Result<CommandBuilder, String> {
    recipe
        .base_args
        .extend(std::mem::take(&mut recipe.trailing_args));
    let executable = executable_override.unwrap_or(recipe.binary);
    // Captured before the arms below move `base_args` out of the recipe.
    let windows_shell = recipe.windows_shell;
    // One decision, read by the interop script, the native arm, and the
    // working-directory gate below.
    let mapped = maps_directory_with_pushd(env_type, windows_shell, executable, spawn_path);
    let mut cmd = if env_type == EnvType::WindowsInterop {
        let cmd_args = if windows_shell == WindowsShell::Cmd {
            let mut args = vec![
                "/d".into(),
                "/c".into(),
                "pushd".into(),
                spawn_path.into(),
                "&&".into(),
                executable.into(),
            ];
            args.extend(recipe.base_args.clone());
            Some(args)
        } else {
            None
        };
        let script = if let Some(args) = &cmd_args {
            format_powershell_command("cmd.exe", args)
        } else if mapped {
            // `pushd` already established the directory. Re-issuing
            // `Set-Location` to the UNC form would hand the cmd.exe that runs
            // the shim the very directory cmd refuses.
            format_powershell_command(executable, &recipe.base_args)
        } else {
            format!(
                "Set-Location -LiteralPath {}; {}",
                ps_single_quote(spawn_path),
                format_powershell_command(executable, &recipe.base_args)
            )
        };
        let launch_script = format!("{script}; exit $LASTEXITCODE");
        if let Some(args) = &cmd_args {
            ensure_cmd_arguments_fit(args, executable)?;
            ensure_powershell_command_line_fits(&launch_script)?;
        } else {
            ensure_windows_command_line_fits(
                WindowsShell::PowerShell,
                executable,
                &recipe.base_args,
                (!mapped).then_some(launch_script.as_str()),
            )?;
        }
        let mut command = powershell_spawn(env_type, &launch_script, mapped.then_some(spawn_path))?;
        if let Ok(distro) = std::env::var("WSL_DISTRO_NAME") {
            command.env("BUILDMESH_WSL_HOST", distro);
        }
        Ok(command)
    } else if env_type == EnvType::Wsl && !cfg!(windows) {
        let mut c = CommandBuilder::new(executable);
        c.args(recipe.base_args);
        Ok(c)
    } else if env_type == EnvType::Wsl {
        tracing::info!("spawn_environment: building WSL command via wsl.exe");
        let mut c = CommandBuilder::new("wsl.exe");
        let default_distro = crate::env::get_default_wsl_distro();
        let mut wsl_args = Vec::new();
        if let Some(distro) = wsl_distro.or(default_distro.as_deref()) {
            wsl_args.extend(["-d".into(), distro.into()]);
        }
        // Use the same login environment as discovery. Positional parameters
        // preserve arbitrary prompts without evaluating them as shell code.
        wsl_args.extend([
            "--cd".into(),
            spawn_path.into(),
            "--exec".into(),
            "sh".into(),
            "-lc".into(),
            "export PATH=\"$HOME/.local/bin:$HOME/.npm-global/bin:$PATH\"; exec \"$@\"".into(),
            "buildmesh".into(),
            executable.into(),
        ]);
        wsl_args.extend(recipe.base_args);
        if cfg!(windows) {
            ensure_windows_command_line_fits(WindowsShell::Direct, "wsl.exe", &wsl_args, None)?;
        }
        c.args(wsl_args);
        Ok(c)
    } else if cfg!(target_os = "macos") {
        // macOS Seatbelt sandbox (issue #497). When the Mesh has the sandbox
        // toggle on AND the developer gate is open, launch the agent through
        // `sandbox-exec -f <profile>` so it can only read/write the worktree
        // (see `agent::sandbox`).
        //
        // Writing the profile is the only fallible step, and this path **fails
        // closed** (#2034): a requested sandbox that cannot be set up must not
        // quietly become an unsandboxed launch. The caller turns this `Err`
        // into a spawn error the user sees, which is the only outcome that
        // keeps "sandboxed" and "sandboxing was requested" the same statement.
        if crate::sandbox::sandbox_requested(sandbox) {
            // `executable`, not `recipe.binary`: every other branch above runs
            // the resolved override, and passing the raw recipe name here
            // would confine and launch a *different* program than the one
            // routing selected. Identical when no override is present.
            crate::agent::sandbox::seatbelt_command(
                executable,
                &recipe.base_args,
                spawn_path,
                session_id,
            )
            .inspect(|_| {
                tracing::info!(
                    "spawn_environment: building sandboxed macOS command (sandbox-exec) for {}",
                    executable
                );
            })
            .map_err(|e| {
                tracing::error!(
                    "spawn_environment: failed to write Seatbelt profile for session {} ({}); \
                     refusing to launch {} unsandboxed",
                    session_id,
                    e,
                    recipe.binary
                );
                crate::agent::sandbox::setup_failure_message(session_id, recipe.binary, &e)
            })
        } else {
            tracing::info!(
                "spawn_environment: building macOS command for {}",
                executable
            );
            let mut c = CommandBuilder::new(executable);
            c.args(recipe.base_args);
            Ok(c)
        }
    } else {
        Ok(match recipe.windows_shell {
            WindowsShell::PowerShell => {
                tracing::info!(
                    "spawn_environment: building Windows powershell.exe for {}",
                    executable
                );
                // Build a PowerShell script that calls the binary with each arg
                // single-quoted, then Base64/UTF-16LE encode it for
                // -EncodedCommand. Quoting matters: multi-line prefill text
                // (e.g. handover prefills containing `1. ...` numbered lists or
                // backticks) would otherwise be parsed as separate PowerShell
                // statements after newline boundaries.
                let cmd_str = format_powershell_command(executable, &recipe.base_args);
                if cfg!(windows) {
                    ensure_windows_command_line_fits(
                        windows_shell,
                        executable,
                        &recipe.base_args,
                        (!mapped).then_some(cmd_str.as_str()),
                    )?;
                }
                powershell_spawn(env_type, &cmd_str, mapped.then_some(spawn_path))?
            }
            WindowsShell::Cmd => {
                tracing::info!(
                    "spawn_environment: building Windows cmd.exe /c for {}",
                    executable
                );
                let mut command_args = if is_unc_path(spawn_path) {
                    vec![
                        "/d".into(),
                        "/c".into(),
                        "pushd".into(),
                        spawn_path.into(),
                        "&&".into(),
                        executable.into(),
                    ]
                } else {
                    vec!["/c".into(), executable.into()]
                };
                command_args.extend(recipe.base_args);
                if cfg!(windows) {
                    ensure_cmd_arguments_fit(&command_args, executable)?;
                }
                let mut c = CommandBuilder::new("cmd.exe");
                c.args(command_args);
                c
            }
            WindowsShell::Direct => {
                tracing::info!(
                    "spawn_environment: building direct Windows spawn for {}",
                    executable
                );
                if cfg!(windows) {
                    ensure_windows_command_line_fits(
                        windows_shell,
                        executable,
                        &recipe.base_args,
                        None,
                    )?;
                }
                let mut c = CommandBuilder::new(executable);
                c.args(recipe.base_args);
                c
            }
        })
    }?;

    // A mapped spawn hands cmd.exe the very directory cmd refuses, which makes
    // it print "UNC paths are not supported. Defaulting to Windows directory."
    // and continue in `C:\Windows` — so the agent would look broken while
    // quietly working. It must not also receive it as a current directory.
    //
    // Every other spawn keeps the working directory it has always had, which
    // is not optional for several arms: `Direct` never establishes a
    // location, and a PowerShell spawn of a native executable or bare stem
    // neither `pushd`s nor `Set-Location`s. Suppressing the cwd for all of
    // them would make portable-pty fall back to `USERPROFILE` and open those
    // agents in the home folder.
    if !maps_directory_with_pushd(env_type, windows_shell, executable, spawn_path) {
        if (cfg!(windows) && env_type == EnvType::Wsl) || env_type == EnvType::WindowsInterop {
            cmd.cwd(crate::env::to_host_path(spawn_path));
        } else {
            cmd.cwd(spawn_path);
        }
    }
    if crate::env::is_wsl_host() && env_type != EnvType::WindowsInterop {
        if let Some(path) = crate::agent::detection::native_wsl_path() {
            cmd.env("PATH", path);
        }
    }
    cmd.env("BUILDMESH_SESSION_ID", session_id.to_string());
    cmd.env(
        "BUILDMESH_PORT",
        crate::http_server::current_http_port().to_string(),
    );
    // Issue #1366 round-2 fix: the runtime hook token is minted
    // lazily by the Grok adapter's `provision_attention_hooks`
    // BEFORE `wrap()` runs (the orchestrator orders: provision →
    // spawn). If the token has been minted AND we are spawning a
    // descendant of that runtime, propagate it as
    // `BUILDMESH_HOOK_TOKEN` so the Grok command hook can expand it
    // into the callback request. Gated on
    // `Some(token)` so non-Grok agents never see the variable
    // (preserves the round-1 fix: Claude / Codex / AGY POST URLs
    // carry no `?token=` and the route's per-provider gate
    // recognises them as legitimate).
    if let Some(token) = crate::agent::runtime_hook_token() {
        cmd.env("BUILDMESH_HOOK_TOKEN", token);
    }
    pty::strip_git_env_vars(&mut cmd);
    // Issue #2136: a Buildmesh launched from inside a Claude Code session
    // hands that session's markers to every agent it spawns, and an inherited
    // `CLAUDE_CODE_CHILD_SESSION` turns the agent's transcript off — which
    // silently parks every downstream transcript reader. Runs before the
    // harness environment policy, so a value Buildmesh sets on purpose is
    // layered back on afterwards.
    strip_claude_session_markers(&mut cmd);
    pty::apply_interactive_tty_env(&mut cmd);

    Ok(cmd)
}

/// Carry command-defined and adapter-declared environment variables across a
/// WSL boundary. The adapter owns the inherited-variable declaration; this
/// module owns the platform-specific `WSLENV` representation.
pub(crate) fn apply_wsl_env(
    cmd: &mut CommandBuilder,
    env_type: EnvType,
    command_variables: &[&str],
    inherited_variables: &[&str],
) {
    if !matches!(env_type, EnvType::Wsl | EnvType::WindowsInterop) {
        return;
    }
    let direction = if env_type == EnvType::WindowsInterop {
        "/w"
    } else {
        "/u"
    };
    let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
    // `wrap` installs these callback values on the outer `wsl.exe` command.
    // They must also be listed in WSLENV or the guest hook process cannot see
    // the port/session that identifies its attention callback (issue #1366).
    //
    for key in ["BUILDMESH_PORT", "BUILDMESH_SESSION_ID"] {
        set_wslenv_direction(&mut wslenv, key, direction);
    }
    if cmd.get_env("BUILDMESH_HOOK_TOKEN").is_some() {
        set_wslenv_direction(&mut wslenv, "BUILDMESH_HOOK_TOKEN", direction);
    }
    for key in command_variables {
        set_wslenv_direction(&mut wslenv, key, direction);
    }
    for key in inherited_variables {
        if std::env::var_os(key).is_some() {
            set_wslenv_direction(&mut wslenv, key, direction);
        }
    }
    if env_type == EnvType::WindowsInterop {
        set_wslenv_direction(&mut wslenv, "BUILDMESH_WSL_HOST", direction);
    }
    if !wslenv.is_empty() {
        cmd.env("WSLENV", wslenv);
    }
}

fn set_wslenv_direction(wslenv: &mut String, key: &str, direction: &str) {
    let flags = wslenv
        .split(':')
        .find(|part| part.split('/').next() == Some(key))
        .and_then(|entry| entry.split_once('/'))
        .map(|(_, flags)| flags.replace(['u', 'w'], ""))
        .unwrap_or_default();
    let mut entries: Vec<_> = wslenv
        .split(':')
        .filter(|entry| !entry.is_empty() && entry.split('/').next() != Some(key))
        .map(str::to_string)
        .collect();
    entries.push(format!(
        "{key}/{flags}{}",
        direction.trim_start_matches('/')
    ));
    *wslenv = entries.join(":");
}

/// Append a WSLENV entry by base name, preserving any existing suffix flags.
pub(crate) fn append_to_wslenv(wslenv: &mut String, key: &str, suffix: &str) {
    if wslenv
        .split(':')
        .any(|part| part.split('/').next() == Some(key))
    {
        return;
    }
    let entry = format!("{key}{suffix}");
    if wslenv.is_empty() {
        wslenv.push_str(&entry);
    } else {
        wslenv.push(':');
        wslenv.push_str(&entry);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_to_wslenv, apply_wsl_env, encode_for_powershell, format_powershell_command,
        CLAUDE_SESSION_MARKER_ENV_VARS,
    };
    use crate::agent::provider::{SpawnRecipe, WindowsShell};
    use crate::models::EnvType;
    use base64::Engine;

    /// Every marker as an explicit "present with this value" env override, so a
    /// test can assert the scrub cleared something that was genuinely
    /// inherited rather than something the command never carried.
    fn markers_in_the_launching_session() -> Vec<(&'static str, Option<&'static std::ffi::OsStr>)> {
        CLAUDE_SESSION_MARKER_ENV_VARS
            .iter()
            .map(|key| {
                (
                    *key,
                    Some(std::ffi::OsStr::new("inherited-from-parent-session")),
                )
            })
            .collect()
    }

    /// The regression from issue #2136: an agent spawned by a Buildmesh that
    /// was itself launched from a Claude Code session inherits
    /// `CLAUDE_CODE_CHILD_SESSION`, starts with "Transcript saving is off",
    /// and writes no transcript for a Circuit gate to read.
    #[test]
    fn agent_spawn_drops_the_launching_claude_code_sessions_markers() {
        let _env_guard = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        crate::env::with_env_vars(&markers_in_the_launching_session(), || {
            let recipe = SpawnRecipe {
                binary: "claude.exe",
                base_args: vec!["--dangerously-skip-permissions".into()],
                trailing_args: vec![],
                windows_shell: WindowsShell::Direct,
            };
            let cmd = super::wrap(recipe, EnvType::Windows, None, None, ".", 4242, false)
                .expect("an unsandboxed command always assembles");
            for key in CLAUDE_SESSION_MARKER_ENV_VARS {
                assert!(cmd.get_env(key).is_none(), "{key} reached a spawned agent: an agent launched from inside a Claude Code session would inherit the launching session's identity and, for CLAUDE_CODE_CHILD_SESSION, save no transcript (issue #2136)");
            }
        });
    }

    /// Node 5351: a Codex reviewer spawned for PR #2261 printed *The command
    /// line is too long.* into its own terminal and exited. Its launch —
    /// attention hooks, model flags and the review policy prompt — crossed the
    /// 8,191-character line `cmd.exe` accepts from the PowerShell that runs an
    /// npm `codex.CMD` shim. The launch is now modelled against both budgets,
    /// so it fits, and one that genuinely cannot is refused up front.
    #[test]
    fn a_realistic_codex_launch_fits_the_windows_command_line_budgets() {
        let adapter = crate::models::Provider::Codex.adapter();
        let mut base_args = vec![
            "--sandbox".to_string(),
            "danger-full-access".into(),
            "--dangerously-bypass-hook-trust".into(),
            "--model".into(),
            "gpt-6-luna".into(),
        ];
        base_args.extend(adapter.launch_hook_args(5351, EnvType::Windows));
        let prompt = format!(
            "Review PR #2261\n{}\nhttps://github.com/alondero/buildmesh/pull/2261",
            crate::review_contract::REVIEW_POLICY
        );
        base_args.push(prompt);
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args,
            trailing_args: vec![],
            windows_shell: WindowsShell::PowerShell,
        };
        let cmd = super::wrap(
            recipe,
            EnvType::Windows,
            None,
            Some(r"C:\Users\alond\AppData\Roaming\npm\codex.CMD"),
            ".",
            5351,
            false,
        )
        .expect("a Codex review launch fits both command-line budgets");
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "powershell.exe", "{argv:?}");
    }

    #[test]
    fn mapped_codex_launch_accepts_a_prompt_that_fits_the_child_budget() {
        let env_types = if cfg!(windows) {
            &[EnvType::Windows, EnvType::WindowsInterop][..]
        } else {
            &[EnvType::WindowsInterop][..]
        };
        for &env_type in env_types {
            let args = vec![format!("{}x", "review homelab\n".repeat(229))];
            let executable = r"C:\Users\alond\AppData\Roaming\npm\codex.CMD";
            super::ensure_cmd_command_line_fits(executable, &args).unwrap();
            let script = super::format_powershell_command(executable, &args);
            assert_eq!(
                super::powershell_command_line_chars(&super::encode_for_powershell(&script)),
                9350
            );
            let recipe = SpawnRecipe {
                binary: "codex",
                base_args: args,
                trailing_args: vec![],
                windows_shell: WindowsShell::PowerShell,
            };
            let command = super::wrap(
                recipe,
                env_type,
                None,
                Some(executable),
                UNC_SPAWN_PATH,
                5406,
                false,
            )
            .expect("Base64 expansion must not consume the mapped batch-line budget");
            assert_maps_through_pushd(&command, UNC_SPAWN_PATH, &encoded_script(&command));
            remove_mapped_batch(&command);
        }
    }

    #[test]
    fn mapped_script_does_not_spend_the_encoded_process_budget_on_literal_quotes() {
        let executable = r"C:\Users\alond\AppData\Roaming\npm\codex.CMD";
        let args = vec!["'".repeat(7_000)];
        super::ensure_cmd_command_line_fits(executable, &args).unwrap();
        let script = super::format_powershell_command(executable, &args);
        assert!(
            super::ensure_powershell_command_line_fits(&script).is_err(),
            "doubled literal quotes exceed the inline encoded process budget"
        );
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args: args,
            trailing_args: vec![],
            windows_shell: WindowsShell::PowerShell,
        };
        let command = super::wrap(
            recipe,
            EnvType::WindowsInterop,
            None,
            Some(executable),
            UNC_SPAWN_PATH,
            5406,
            false,
        )
        .expect("the mapped script is on disk, so only its loader occupies the process line");
        assert_eq!(
            encoded_script(&command),
            format!("{script}; exit $LASTEXITCODE")
        );
        remove_mapped_batch(&command);
    }

    /// The guard's other half: an oversized launch fails here, naming the
    /// budget it crossed, instead of handing a shell a line it will refuse and
    /// leaving the node to report a cryptic message.
    #[cfg(windows)]
    #[test]
    fn a_launch_over_the_cmd_exe_budget_is_refused_with_that_budget() {
        for spawn_path in [".", UNC_SPAWN_PATH] {
            let mut base_args = vec!["--sandbox".into(), "danger-full-access".into()];
            base_args.push("review this".repeat(1_000));
            let recipe = SpawnRecipe {
                binary: "codex",
                base_args,
                trailing_args: vec![],
                windows_shell: WindowsShell::PowerShell,
            };
            let error = super::wrap(
                recipe,
                EnvType::Windows,
                None,
                Some(r"C:\Users\alond\AppData\Roaming\npm\codex.CMD"),
                spawn_path,
                5351,
                false,
            )
            .expect_err("a launch cmd.exe cannot parse must be refused before it is spawned");
            assert!(
                error.contains("cmd.exe") && error.contains("8191"),
                "{error}"
            );
        }
    }

    #[test]
    fn windows_interop_cmd_launch_accounts_for_the_outer_powershell_script() {
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args: vec!["--model".into(), "gpt-6-luna".into()],
            trailing_args: vec![],
            windows_shell: WindowsShell::Cmd,
        };
        // Single quotes are valid Windows path characters. PowerShell doubles
        // each one in the encoded script, making the outer process line cross
        // 32,767 while the inner cmd.exe line remains below 8,191.
        let spawn_path = format!("C:\\{}", "'".repeat(7_000));
        let error = super::wrap(
            recipe,
            EnvType::WindowsInterop,
            None,
            Some(r"C:\\Users\\alond\\AppData\\Roaming\\npm\\codex.CMD"),
            &spawn_path,
            5351,
            false,
        )
        .expect_err("the encoded Windows interop PowerShell script exceeds CreateProcess");
        assert!(error.contains("32767"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn cmd_budget_accounts_for_windows_quote_and_backslash_escaping() {
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args: vec![std::iter::repeat("\\\"").take(2_200).collect::<String>()],
            trailing_args: vec![],
            windows_shell: WindowsShell::Cmd,
        };
        let error = super::wrap(recipe, EnvType::Windows, None, None, ".", 5351, false)
            .expect_err("serialized cmd.exe arguments exceed the shell budget");
        assert!(error.contains("8191"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn background_windows_launch_over_the_process_budget_is_refused() {
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args: vec!["x".repeat(40_000)],
            trailing_args: vec![],
            windows_shell: WindowsShell::PowerShell,
        };
        let error = super::background_command(&recipe, None)
            .expect_err("background launches must use the same Windows process budget");
        assert!(error.contains("32767"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn native_cmd_unc_launch_accounts_for_its_pushd_path() {
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args: vec!["--model".into(), "gpt-6-luna".into()],
            trailing_args: vec![],
            windows_shell: WindowsShell::Cmd,
        };
        let spawn_path = format!("\\\\server\\repo-{}", "x".repeat(8_200));
        let error = super::wrap(
            recipe,
            EnvType::Windows,
            None,
            Some(r"C:\Users\alond\AppData\Roaming\npm\codex.CMD"),
            &spawn_path,
            5351,
            false,
        )
        .expect_err("cmd.exe must budget the UNC pushd command it actually receives");
        assert!(error.contains("8191"), "{error}");
    }

    #[cfg(not(windows))]
    #[test]
    fn native_linux_direct_launch_does_not_use_windows_command_line_limits() {
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args: vec!["x".repeat(40_000)],
            trailing_args: vec![],
            windows_shell: WindowsShell::Direct,
        };
        super::wrap(recipe, EnvType::Windows, None, None, ".", 5351, false)
            .expect("native Linux direct launches do not use Windows command-line limits");
    }

    /// The background launch path is a second instance of the same leak: it
    /// builds a `std::process::Command`, which does not share `wrap`'s loop.
    #[test]
    fn background_launch_drops_the_same_markers() {
        let recipe = SpawnRecipe {
            binary: "claude.exe",
            base_args: vec!["--print".into()],
            trailing_args: vec![],
            windows_shell: WindowsShell::Direct,
        };
        let cmd = super::background_command(&recipe, None).expect("assemble background command");
        // `get_envs` reports the command's explicit overrides, where a removal
        // is the entry `(key, None)`.
        let cleared: std::collections::BTreeMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for key in CLAUDE_SESSION_MARKER_ENV_VARS {
            assert_eq!(
                cleared.get(*key),
                Some(&None),
                "{key} must be explicitly removed from the background launch env"
            );
        }
    }

    /// `BackgroundProcessGuard` signals the group led by the child. Background
    /// inference is built here, so this command has to be that leader. Otherwise
    /// `kill(-pid)` names a group that does not exist and cancellation leaves
    /// the CLI's descendants running.
    #[cfg(unix)]
    #[test]
    fn background_inference_leads_its_own_group_so_cancel_reaps_descendants() {
        use std::io::{BufRead, Read};
        use std::process::Stdio;

        let _env = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let recipe = SpawnRecipe {
            binary: "sh",
            base_args: vec![
                "-c".into(),
                "echo ready; sh -c 'echo child-ready; sleep 60' & wait".into(),
            ],
            trailing_args: vec![],
            windows_shell: WindowsShell::Direct,
        };
        let mut command =
            super::background_command(&recipe, None).expect("assemble background command");
        command.stdout(Stdio::piped());
        let mut child = Reap(
            command
                .spawn()
                .expect("spawn background inference in its own process group"),
        );
        let pid = child.0.id();
        // SAFETY: `getpgid` only reads the group id of this live child. A pid
        // that has already exited returns -1, which the assertion rejects.
        let pgid = unsafe { getpgid(i32::try_from(pid).unwrap()) };
        let mut reader = std::io::BufReader::new(child.0.stdout.take().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ready");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "child-ready");
        assert_eq!(
            pgid,
            i32::try_from(pid).unwrap(),
            "background inference must lead its own process group (pgid {pgid}, pid {pid})"
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let drain = std::thread::spawn(move || {
            let mut remaining = String::new();
            tx.send(reader.read_to_string(&mut remaining)).unwrap();
        });
        drop(crate::agent::background::BackgroundProcessGuard::new(pid));
        let closed = rx.recv_timeout(std::time::Duration::from_secs(5));
        closed
            .expect("cancelling background inference must reap descendants")
            .unwrap();
        drain.join().unwrap();
    }

    #[cfg(unix)]
    struct Reap(std::process::Child);

    #[cfg(unix)]
    impl Drop for Reap {
        fn drop(&mut self) {
            crate::process_util::kill_process_group(self.0.id());
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(unix)]
    extern "C" {
        fn getpgid(pid: i32) -> i32;
    }

    /// The scrub is an explicit list precisely because Buildmesh sets `CLAUDE_*`
    /// variables on purpose. A prefix rule over `CLAUDE*` would look tidier and
    /// would silently undo the MiniMax naming side-channel and the Windows
    /// sandbox config dir; this test is what keeps the list explicit.
    #[test]
    fn the_scrub_never_covers_a_variable_buildmesh_sets_deliberately() {
        for deliberate in [
            // provider_conf::minimax_backend_env — the naming side-channel.
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
            // sandbox::spawn — the Windows agent sandbox.
            "CLAUDE_CONFIG_DIR",
        ] {
            assert!(!CLAUDE_SESSION_MARKER_ENV_VARS.contains(&deliberate), "{deliberate} is set by buildmesh on purpose and must survive the session-marker scrub");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "requires WSL with Windows interop enabled"]
    fn live_wsl_host_launches_windows_harnesses() {
        assert!(crate::env::is_wsl_host());
        let windows_temp = crate::env::windows_cli_home("AppData/Local/Temp").unwrap();
        let script_dir = tempfile::tempdir_in(&windows_temp).unwrap();
        let script = script_dir.path().join("probe script.cmd");
        std::fs::write(
            &script,
            "@echo off\r\necho %BUILDMESH_SESSION_ID%> probe.txt\r\n",
        )
        .unwrap();
        for root in [std::path::Path::new("/tmp"), windows_temp.as_path()] {
            let directory = tempfile::Builder::new()
                .prefix("buildmesh reverse ")
                .tempdir_in(root)
                .unwrap();
            let spawn_path = crate::env::windows_path_from_wsl(directory.path().to_str().unwrap());
            for shell in [WindowsShell::PowerShell, WindowsShell::Cmd] {
                let (binary, args) = if shell == WindowsShell::Cmd {
                    (
                        crate::env::windows_path_from_wsl(script.to_str().unwrap()),
                        vec![],
                    )
                } else {
                    ("powershell.exe".to_string(), vec!["-NoProfile".into(), "-EncodedCommand".into(),
                        encode_for_powershell("[IO.File]::WriteAllText((Join-Path $PWD.ProviderPath 'probe.txt'), $env:BUILDMESH_SESSION_ID)")])
                };
                let recipe = SpawnRecipe {
                    binary: "probe",
                    base_args: args,
                    trailing_args: vec![],
                    windows_shell: shell,
                };
                let mut command = super::wrap(
                    recipe,
                    EnvType::WindowsInterop,
                    None,
                    Some(&binary),
                    &spawn_path,
                    8125,
                    false,
                )
                .expect("an unsandboxed command always assembles");
                apply_wsl_env(&mut command, EnvType::WindowsInterop, &[], &[]);
                let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
                let mut child = crate::agent::spawn::spawn_child(&pair, command).unwrap();
                use std::io::{Read, Write};
                let mut reader = pair.master.try_clone_reader().unwrap();
                let mut writer = pair.master.take_writer().unwrap();
                drop(pair.slave);
                let drain = std::thread::spawn(move || {
                    let mut buffer = [0; 4096];
                    let mut output = Vec::new();
                    while let Ok(count) = reader.read(&mut buffer) {
                        if count == 0 {
                            break;
                        }
                        output.extend_from_slice(&buffer[..count]);
                        if output.ends_with(b"\x1b[6n") {
                            writer.write_all(b"\x1b[1;1R").unwrap();
                        }
                    }
                    output
                });
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let status = loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        break status;
                    }
                    if std::time::Instant::now() >= deadline {
                        child.kill().unwrap();
                        panic!("Windows PTY timed out: {shell:?} {spawn_path}");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                };
                assert!(
                    status.success(),
                    "{shell:?}: {spawn_path}: {}",
                    String::from_utf8_lossy(&drain.join().unwrap())
                );
                let result = directory.path().join("probe.txt");
                assert_eq!(std::fs::read_to_string(&result).unwrap().trim(), "8125");
                std::fs::remove_file(result).unwrap();
            }
        }
    }

    #[test]
    fn wslenv_reverses_direction_preserving_path_flags() {
        let mut value = "TOKEN/u:CODEX_HOME/pu:OTHER/l".to_string();
        super::set_wslenv_direction(&mut value, "TOKEN", "w");
        super::set_wslenv_direction(&mut value, "CODEX_HOME", "w");
        assert_eq!(value, "OTHER/l:TOKEN/w:CODEX_HOME/pw");
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires an installed default WSL distribution"]
    fn live_wsl_pty_preserves_cwd_environment_and_literal_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let mut resolved = crate::env::resolve_raw_path(directory.path().to_str().unwrap());
        crate::env::apply_harness_runtime(&mut resolved, crate::models::EnvType::Wsl);
        let payload = "spaces 'quotes' \"double\" $HOME `literal`\nsecond line";
        let recipe = crate::agent::provider::SpawnRecipe {
            binary: "sh",
            base_args: vec![
                "-c".into(),
                "printf '%s\\n' \"$PWD\" \"$BUILDMESH_SESSION_ID\" \"$1\" > probe.txt".into(),
                "probe".into(),
                payload.into(),
            ],
            trailing_args: vec![],
            windows_shell: crate::agent::provider::WindowsShell::Direct,
        };
        let mut command = super::wrap(
            recipe,
            resolved.env_type,
            None,
            None,
            &resolved.spawn_path,
            8123,
            false,
        )
        .unwrap();
        apply_wsl_env(&mut command, resolved.env_type, &[], &[]);
        let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
        let mut child = crate::agent::spawn::spawn_child(&pair, command).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("probe.txt")).unwrap(),
            format!("{}\n8123\n{payload}\n", resolved.spawn_path)
        );
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires an installed default WSL distribution"]
    fn live_wsl_directory_runs_windows_cmd_harness() {
        let home = crate::env::wsl_home().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("buildmesh-native-")
            .tempdir_in(crate::env::to_host_path(&home.to_string_lossy()))
            .unwrap();
        let script_dir = tempfile::tempdir().unwrap();
        let script = script_dir.path().join("native probe.cmd");
        std::fs::write(&script, "@echo off\r\necho native-in-guest> probe.txt\r\n").unwrap();
        let mut resolved = crate::env::resolve_raw_path(directory.path().to_str().unwrap());
        crate::env::apply_harness_runtime(&mut resolved, crate::models::EnvType::Windows);
        let recipe = crate::agent::provider::SpawnRecipe {
            binary: "probe",
            base_args: vec![],
            trailing_args: vec![],
            windows_shell: crate::agent::provider::WindowsShell::Cmd,
        };
        let command = super::wrap(
            recipe,
            resolved.env_type,
            None,
            script.to_str(),
            &resolved.spawn_path,
            8124,
            false,
        )
        .unwrap();
        let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
        let mut child = crate::agent::spawn::spawn_child(&pair, command).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("probe.txt"))
                .unwrap()
                .trim(),
            "native-in-guest"
        );
    }

    /// The same Windows-runtime-in-a-WSL-directory node as
    /// `live_wsl_directory_runs_windows_cmd_harness`, but on the
    /// `WindowsShell::PowerShell` arm Codex uses. PowerShell runs the `.cmd`
    /// shim by delegating to cmd.exe, which rejects the UNC directory and
    /// starts the agent in `C:\Windows`; the fix maps the share with `pushd`
    /// first, so the probe must land in the node's directory.
    #[test]
    #[cfg(windows)]
    #[ignore = "requires an installed default WSL distribution"]
    fn live_wsl_directory_runs_windows_powershell_harness_in_the_node_directory() {
        let home = crate::env::wsl_home().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("buildmesh-native-ps-")
            .tempdir_in(crate::env::to_host_path(&home.to_string_lossy()))
            .unwrap();
        let npm_dir = tempfile::tempdir().unwrap();
        let cmd_shim = npm_dir.path().join("probe.cmd");
        std::fs::write(
            &cmd_shim,
            "@echo off\r\necho shim-ran-in-node-dir> probe.txt\r\necho %~1> prompt.txt\r\n",
        )
        .unwrap();
        let mut resolved = crate::env::resolve_raw_path(directory.path().to_str().unwrap());
        crate::env::apply_harness_runtime(&mut resolved, crate::models::EnvType::Windows);
        let prompt = "Review homelab. ".repeat(240);
        let recipe = crate::agent::provider::SpawnRecipe {
            binary: "probe",
            base_args: vec![prompt.clone()],
            trailing_args: vec![],
            windows_shell: crate::agent::provider::WindowsShell::PowerShell,
        };
        let command = super::wrap(
            recipe,
            resolved.env_type,
            None,
            cmd_shim.to_str(),
            &resolved.spawn_path,
            8125,
            false,
        )
        .unwrap();
        run_and_expect_probe(&command, &directory.path().join("probe.txt"));
        // Existence first: a `read_to_string().unwrap()` here would report
        // NotFound, which is exactly the symptom and hides the cause.
        let probe = directory.path().join("probe.txt");
        assert!(
            probe.is_file(),
            "the .cmd shim never wrote into the node directory at {} — cmd.exe rejected the \
             UNC working directory and ran in its own default instead",
            probe.display()
        );
        assert_eq!(
            std::fs::read_to_string(&probe).unwrap().trim(),
            "shim-ran-in-node-dir"
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("prompt.txt"))
                .unwrap()
                .trim_end(),
            prompt.trim_end()
        );
    }

    /// The command `wrap` built, so a test can assert on the working directory as
    /// well as the argv. `CommandBuilder::get_cwd` is the only place the
    /// decision is observable — a mapped spawn leaves it unset on purpose.
    fn wrap_command(
        executable: &str,
        spawn_path: &str,
        env_type: EnvType,
        windows_shell: WindowsShell,
    ) -> portable_pty::CommandBuilder {
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args: vec!["--sandbox".into(), "danger-full-access".into()],
            trailing_args: vec![],
            windows_shell,
        };
        super::wrap(
            recipe,
            env_type,
            None,
            Some(executable),
            spawn_path,
            8126,
            false,
        )
        .unwrap()
    }

    /// The argv `wrap` built, so a test can assert on the chosen shell as well
    /// as on the decoded `-EncodedCommand` payload.
    fn wrap_argv(
        executable: &str,
        spawn_path: &str,
        env_type: EnvType,
        windows_shell: WindowsShell,
    ) -> Vec<String> {
        wrap_command(executable, spawn_path, env_type, windows_shell)
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// The whole batch file a mapped command runs.
    fn batch_file(command: &portable_pty::CommandBuilder) -> String {
        let path =
            mapped_batch_path(command).expect("the mapped arm passes a batch file to cmd.exe");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading the mapped batch {}: {error}", path.display()))
    }

    /// The decoded PowerShell script, wherever in the command it ended up: the
    /// plain arm carries it as an argument, the mapped arm buries it in a batch
    /// file that argv never shows.
    fn encoded_script(command: &portable_pty::CommandBuilder) -> String {
        let argv: Vec<String> = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        if let Some(index) = argv.iter().position(|arg| arg == "-EncodedCommand") {
            return decode_ps(&argv[index + 1]);
        }
        // Mapped arm: the original script is beside the batch, at the host
        // spelling of the Windows path in argv.
        let batch =
            mapped_batch_path(command).expect("the mapped arm passes a batch file to cmd.exe");
        std::fs::read_to_string(batch.with_extension("ps1"))
            .expect("the mapped batch has a companion PowerShell script")
    }

    /// Assert the spawn maps the UNC directory through cmd's ``pushd`` without
    /// ever putting the directory on a command line.
    ///
    /// The absence assertions are the load-bearing ones. portable-pty quotes an
    /// argument only when it contains whitespace or a quote, so ``&``, ``|``,
    /// ``<``, ``>``, ``^``, ``(``, ``)`` and ``%`` in a node's directory reach
    /// cmd.exe bare, where cmd treats them as operators — a node in ``R&D``
    /// would run whatever followed the ampersand. Quoting inside the argument
    /// is not available either: portable-pty escapes a ``"`` inside an
    /// argument, so a pre-quoted path arrives as ``"\"<dir>\""`` and ``pushd``
    /// rejects the line outright. The directory is therefore quoted inside
    /// the batch, with percent signs doubled to prevent expansion. Only the
    /// batch file's path travels on the outer command line.
    fn assert_maps_through_pushd(
        command: &portable_pty::CommandBuilder,
        spawn_path: &str,
        script: &str,
    ) {
        let argv: Vec<String> = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv.first().map(String::as_str),
            Some("cmd.exe"),
            "a batch shim in a UNC directory has to be reached through cmd.exe, because \
             PowerShell delegates to it and cmd.exe is what rejects the directory; got: {argv:?}"
        );
        assert!(
            !argv.iter().any(|arg| arg.contains(spawn_path)),
            "the node directory must not appear on the command line at all: cmd parses \
             & | < > ^ ( ) % in an argument as operators once portable-pty leaves it \
             unquoted. It belongs quoted inside the batch file; got: {argv:?}"
        );
        assert!(
            batch_file(command).contains(&format!("pushd \"{}\"", spawn_path.replace('%', "%%"))),
            "the batch must pushd the node directory as one quoted argument, which is the \
             only form cmd parses without splitting on a metacharacter; got: {}",
            batch_file(command)
        );
        assert!(
            !script.contains("Set-Location"),
            "once pushd has mapped the share, re-setting the location to the UNC form would \
             hand the inner cmd.exe the directory it rejects; got: {script}"
        );
    }
    /// Actually runs the mapped form: cmd.exe `pushd`s the directory and the
    /// PowerShell inside it writes a probe.
    ///
    /// This is the test that catches a malformed argument, and it is the one
    /// the earlier revision was missing — `get_argv()` is the serialiser's
    /// *input*, so an argv assertion cannot see quoting applied afterwards. A
    /// directory with a space is used deliberately, because that is the case
    /// that tempts a fix into pre-quoting the path.
    ///
    /// The directory is local, so the test needs neither WSL nor a share and
    /// runs on any Windows machine. `pushd` treats a local path and a UNC one
    /// the same way, which is the whole of the difference that matters here.
    ///
    /// It runs through `spawn_child`, the production spawn, rather than
    /// re-serialising the argv through `std::process::Command`. That re-serialisation
    /// tests an equivalent quoting algorithm, not the one that actually runs:
    /// portable-pty has its own. Reintroducing the pre-quoted `pushd` argument
    /// fails this test with cmd's own error.
    #[test]
    #[cfg(windows)]
    fn mapped_powershell_spawn_runs_the_script_in_the_mapped_directory() {
        // A space, which portable-pty quotes on its own.
        let directory = tempfile::Builder::new()
            .prefix("bm mapped pushd ")
            .tempdir()
            .unwrap();
        let script = "[IO.File]::WriteAllText((Join-Path ($PWD).Path 'probe.txt'), 'mapped')";
        let command = super::powershell_spawn(
            EnvType::Windows,
            script,
            Some(directory.path().to_str().unwrap()),
        )
        .expect("the mapped spawn should be buildable");
        run_and_expect_probe(&command, &directory.path().join("probe.txt"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("probe.txt")).unwrap(),
            "mapped"
        );
    }

    /// The same run with a directory name carrying `&` and **no** space.
    ///
    /// The space is what makes this matter: portable-pty quotes an argument
    /// that contains whitespace, so a round-3 test whose directory had one
    /// would have passed even with the directory on the command line. `&` with
    /// no whitespace is the case cmd splits on, and the one the node's real
    /// directory decides.
    #[test]
    #[cfg(windows)]
    fn mapped_powershell_spawn_survives_shell_metacharacters_in_the_directory() {
        // `&` is legal in a Windows directory name and is the character cmd
        // splits on. (`|` and `>` are not legal at all, so they cannot be
        // tested this way — which is a small comfort: a node's real directory
        // cannot contain them either.)
        let directory = tempfile::Builder::new()
            .prefix("bm&R&D-")
            .tempdir()
            .unwrap();
        assert!(
            !directory.path().to_string_lossy().contains(' '),
            "a space here would let portable-pty quote the argument and hide the defect \
             this test exists to catch"
        );
        let script = "[IO.File]::WriteAllText((Join-Path ($PWD).Path 'probe.txt'), 'mapped')";
        let command = super::powershell_spawn(
            EnvType::Windows,
            script,
            Some(directory.path().to_str().unwrap()),
        )
        .expect("the mapped spawn should be buildable");
        run_and_expect_probe(&command, &directory.path().join("probe.txt"));
    }

    /// The batch must fail closed when the share cannot be mapped.
    ///
    /// The alternative is the failure this guard exists for: the agent starts in
    /// whatever directory cmd fell back to and reports success, so the node
    /// looks launched while silently working somewhere else. The host is on a
    /// reserved `.invalid` TLD and resolves to NXDOMAIN, so `pushd` fails
    /// immediately instead of waiting on a real network share.
    #[test]
    #[cfg(windows)]
    fn mapped_spawn_of_an_unmappable_directory_fails_closed() {
        let unmappable = ["\\\\", "nope.invalid", "share", "directory"].join("\\");
        let command = super::powershell_spawn(EnvType::Windows, "exit 0", Some(&unmappable))
            .expect("the mapped spawn should be buildable");
        let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
        let mut child = crate::agent::spawn::spawn_child(&pair, command.clone()).unwrap();
        let success = child.wait().unwrap().success();
        remove_mapped_batch(&command);
        assert!(
            !success,
            "a directory cmd cannot pushd must fail the spawn rather than start the agent \
             somewhere else and report success"
        );
    }

    /// The same runtime check for the interop route, through `wrap` rather than
    /// the builder, so the `Set-Location`/`exit` wrapper is exercised too. It
    /// needs no WSL: a drive-letter directory is not mapped, so the interop arm
    /// takes its normal path and the agent still has to land in the node's
    /// directory.
    #[test]
    #[cfg(windows)]
    fn interop_drive_letter_spawn_runs_in_the_node_directory() {
        let directory = tempfile::Builder::new()
            .prefix("bm R&D interop ")
            .tempdir()
            .unwrap();
        let shim = directory.path().join("probe.cmd");
        std::fs::write(&shim, "@echo off\r\necho shim-ran> probe.txt\r\n").unwrap();
        let command = wrap_command(
            shim.to_str().unwrap(),
            directory.path().to_str().unwrap(),
            EnvType::WindowsInterop,
            WindowsShell::PowerShell,
        );
        run_and_expect_probe(&command, &directory.path().join("probe.txt"));
    }

    /// Spawn `command` through the production PTY path and assert it wrote the
    /// probe where it was supposed to be, then remove the batch file the child
    /// could not remove for itself.
    #[cfg(windows)]
    fn run_and_expect_probe(command: &portable_pty::CommandBuilder, probe: &std::path::Path) {
        let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
        let mut child = crate::agent::spawn::spawn_child(&pair, command.clone()).unwrap();
        let status = child.wait().unwrap().success();
        // The batch cannot delete itself — cmd reads it incrementally, so a
        // `del "%~f0"` leaves cmd unable to read the next line — and in
        // production the file is swept by age instead. A test knows its child
        // has exited, so it can clean up directly rather than leaving 28 files
        // behind per run.
        remove_mapped_batch(command);
        assert!(
            status,
            "the mapped spawn exited non-zero, so pushd or the agent failed"
        );
        assert!(
            probe.is_file(),
            "no probe at {}. The node directory never took effect: a directory name \
             containing & is a command separator to cmd.exe unless the mapping script \
             quotes it, so this is the metacharacter defect.",
            probe.display()
        );
    }

    /// The batch file a mapped command runs, as a path **this host** can open.
    ///
    /// The argv entry is a Windows path — it has to be, since it is what
    /// `cmd.exe` is told — but a Linux host opens the same file through its
    /// host spelling. Converting is not belt-and-braces: reading the Windows
    /// spelling directly is exactly what broke these tests on Linux CI.
    ///
    /// Not gated on Windows: this is path inspection with no platform API, and
    /// the structural tests that call it run on every platform.
    fn mapped_batch_path(command: &portable_pty::CommandBuilder) -> Option<std::path::PathBuf> {
        let windows = command.get_argv().last()?;
        let host = crate::env::to_host_path(&windows.to_string_lossy());
        // Structural tests can build the native Windows arm on Linux: its
        // batch path combines the Linux temp directory with a Windows separator.
        let host = if cfg!(windows) {
            host
        } else {
            host.replace('\\', "/")
        };
        let host = std::path::PathBuf::from(host);
        let is_batch = host.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with(super::MAPPED_BATCH_PREFIX)
        });
        is_batch.then_some(host)
    }

    /// Remove a batch file a test created but never ran. The child cannot clean
    /// up after itself and production reclaims these by age, so a test that only
    /// inspects the command would otherwise leave one behind per run.
    fn remove_mapped_batch(command: &portable_pty::CommandBuilder) {
        if let Some(batch) = mapped_batch_path(command) {
            let _ = std::fs::remove_file(batch.with_extension("ps1"));
            let _ = std::fs::remove_file(batch);
        }
    }

    #[test]
    #[cfg(windows)]
    fn mapped_long_script_preserves_unicode_and_literal_prompt_text() {
        let directory = tempfile::Builder::new()
            .prefix("bm R&D long script ")
            .tempdir()
            .unwrap();
        let prompt = "homelab: café 🦀 'quoted' `code` $env:PATH <tag> | & ;\n".repeat(80);
        let script = format!(
            "[IO.File]::WriteAllText((Join-Path ($PWD).Path 'probe.txt'), {})",
            super::ps_single_quote(&prompt)
        );
        assert!(
            super::powershell_command_line_chars(&super::encode_for_powershell(&script))
                > super::CMD_EXE_COMMAND_LINE_CHARS
        );
        let command = super::powershell_spawn(
            EnvType::Windows,
            &script,
            Some(directory.path().to_str().unwrap()),
        )
        .expect("a long script should be loaded from disk");
        let batch = batch_file(&command);
        assert!(batch
            .lines()
            .all(|line| line.encode_utf16().count() <= super::CMD_EXE_COMMAND_LINE_CHARS));
        run_and_expect_probe(&command, &directory.path().join("probe.txt"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("probe.txt")).unwrap(),
            prompt
        );
    }

    #[test]
    #[cfg(windows)]
    fn powershell_prompt_preserves_smart_apostrophes_through_the_pty() {
        let prompt = "don't; don\u{2018}t; don\u{2019}t; don\u{201a}t; don\u{201b}t";
        for mapped in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let probe = directory.path().join("probe.txt");
            let script = format!(
                "[IO.File]::WriteAllText({}, {})",
                super::ps_single_quote(probe.to_str().unwrap()),
                super::ps_single_quote(prompt)
            );
            let command = super::powershell_spawn(
                EnvType::Windows,
                &script,
                mapped.then_some(directory.path().to_str().unwrap()),
            )
            .unwrap();
            run_and_expect_probe(&command, &probe);
            assert_eq!(std::fs::read_to_string(&probe).unwrap(), prompt);
        }
    }

    #[test]
    #[cfg(windows)]
    fn mapped_percent_directory_reaches_the_harness_literally() {
        for prefix in ["bm 50%off ", "bm %USERNAME% "] {
            let directory = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
            let command = super::powershell_spawn(
                EnvType::Windows,
                "[IO.File]::WriteAllText((Join-Path ($PWD).Path 'probe.txt'), 'literal directory')",
                Some(directory.path().to_str().unwrap()),
            )
            .unwrap();
            let probe = directory.path().join("probe.txt");
            run_and_expect_probe(&command, &probe);
            assert_eq!(
                std::fs::read_to_string(&probe).unwrap(),
                "literal directory"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn mapped_loader_propagates_child_failure_and_missing_script_errors() {
        let directory = tempfile::tempdir().unwrap();
        for remove_script in [false, true] {
            let command = super::powershell_spawn(
                EnvType::Windows,
                "cmd.exe /d /c exit 23",
                Some(directory.path().to_str().unwrap()),
            )
            .unwrap();
            if remove_script {
                std::fs::remove_file(mapped_batch_path(&command).unwrap().with_extension("ps1"))
                    .unwrap();
            }
            let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
            let mut child = crate::agent::spawn::spawn_child(&pair, command.clone()).unwrap();
            let status = child.wait().unwrap();
            remove_mapped_batch(&command);
            assert!(!status.success());
            if !remove_script {
                assert_eq!(status.exit_code(), 23);
            }
        }
    }

    /// The mapped interop route end to end: Buildmesh on the Linux host, a
    /// Windows-side batch shim, and a node directory reached through the WSL
    /// boundary.
    ///
    /// The companion files must be written through a host path Windows can
    /// open and referenced by their Windows spelling. The directory is quoted
    /// in the batch with percent signs escaped; it needs no directory variable
    /// forwarded through WSLENV. A long prompt must reach the shim intact in
    /// the mapped directory. This test needs a Linux Buildmesh host in WSL to
    /// exercise interop process launch and PTY behavior.
    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "requires WSL with Windows interop enabled"]
    fn live_wsl_mapped_interop_spawn_runs_in_the_node_directory() {
        assert!(crate::env::is_wsl_host());
        // The Windows user profile is what makes this test possible at all: the
        // batch file has to land somewhere Windows can open. A bare `unwrap`
        // here would report only "called Option::unwrap() on a None value", so
        // name the real cause — `windows_home` runs PowerShell through interop
        // under a 10s timeout, and a cold WSL start can miss it.
        let windows_temp =
            crate::env::windows_cli_home("AppData/Local/Temp").unwrap_or_else(|| {
                panic!(
                    "the Windows user profile did not resolve from WSL, so no Windows-visible \
                 batch file can be written. PowerShell interop either failed or missed its \
                 10s timeout; a warm start usually resolves this."
                )
            });
        let npm_dir = tempfile::tempdir_in(&windows_temp).unwrap();
        let shim = npm_dir.path().join("probe.cmd");
        std::fs::write(
            &shim,
            "@echo off\r\necho %~1> prompt.txt\r\necho shim-ran> probe.txt\r\n",
        )
        .unwrap();
        // PowerShell is a Windows process, so it needs the *Windows* spelling
        // of the shim. `windows_cli_home` returns the host spelling — correct
        // for writing the file, wrong for telling Windows where it is — which
        // is the same trap the batch file had.
        let shim_windows = crate::env::windows_path_from_wsl(shim.to_str().unwrap());
        // A WSL-side directory whose Windows form is UNC, with an `&` in the
        // name so a directory that reached cmd.exe on the command line would be
        // split rather than mapped.
        let directory = tempfile::Builder::new()
            .prefix("buildmesh mapped interop R&D 50%off %USERNAME%-")
            .tempdir()
            .unwrap();
        let spawn_path = crate::env::windows_path_from_wsl(directory.path().to_str().unwrap());
        let prompt = "Review homelab. ".repeat(240);
        let full_script =
            super::format_powershell_command(&shim_windows, std::slice::from_ref(&prompt));
        assert!(
            super::powershell_command_line_chars(&super::encode_for_powershell(&full_script))
                > super::CMD_EXE_COMMAND_LINE_CHARS
        );
        let recipe = SpawnRecipe {
            binary: "probe",
            base_args: vec![prompt.clone()],
            trailing_args: vec![],
            windows_shell: WindowsShell::PowerShell,
        };
        let mut command = super::wrap(
            recipe,
            EnvType::WindowsInterop,
            None,
            Some(&shim_windows),
            &spawn_path,
            8127,
            false,
        )
        .expect("an unsandboxed command always assembles");
        apply_wsl_env(&mut command, EnvType::WindowsInterop, &[], &[]);
        // The batch file carries the directory itself, quoted, so this route
        // needs no WSLENV entry for it and cannot be broken by a forwarding
        // regression. Assert the batch is where cmd.exe can actually open it:
        // a Windows path on the command line, even though Buildmesh wrote the
        // file through the host spelling.
        let batch = command
            .get_argv()
            .last()
            .map(|arg| arg.to_string_lossy().into_owned())
            .expect("the mapped spawn passes a batch file to cmd.exe");
        assert!(
            batch.starts_with(r"C:\") || batch.contains(":\\"),
            "cmd.exe must be given a Windows path, not the host spelling; got: {batch}"
        );
        assert!(
            std::path::Path::new(&crate::env::to_host_path(&batch)).exists(),
            "the batch file was written through the host spelling, which is the same directory; \
             it should be openable from here: {batch}"
        );
        let script = std::fs::read_to_string(crate::env::to_host_path(&batch)).unwrap();
        assert!(
            script.contains(&format!("pushd \"{}\"", spawn_path.replace('%', "%%"))),
            "the batch must pushd the node directory quoted, which is the only form cmd parses \
             without splitting on a metacharacter; got: {script}"
        );
        let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
        // Stands in for xterm.js: the only thing that answers the child's
        // cursor-position query. `take_writer` may be called once per master.
        let mut console = pair
            .master
            .take_writer()
            .expect("a PTY master hands out one writer");
        let mut master = pair.master.try_clone_reader().unwrap();
        let mut child = crate::agent::spawn::spawn_child(&pair, command.clone()).unwrap();
        // Drain the PTY. Without this the console output is discarded, and a
        // failure inside the batch — `pushd` refusing a share, `powershell.exe`
        // not resolving — is invisible: all the test can report is that the
        // probe never appeared.
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&captured);
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            while let Ok(n) = master.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut text) = sink.lock() {
                    text.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
        });
        // Poll for the probe rather than waiting on the child. cmd.exe holds the
        // console open for the life of an interactive agent — that is normal
        // for a node, not a failure — so `wait()` would never return here even
        // on a completely successful spawn. What matters is whether the shim
        // ran *in the node's directory*, which is what the probe records.
        //
        // Answer the cursor-position query while polling. cmd.exe opens with
        // `ESC[6n` ("Device Status Report": where is your cursor?) and blocks
        // until a terminal replies. In the app that reply comes from xterm.js
        // in the frontend; a bare PTY has no terminal behind it and so nobody
        // answers, which is the whole of this test's hang. Answering it here is
        // emulating the frontend, not patching the spawn.
        const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";
        use std::io::Write as _;
        // Reply once. The captured text keeps containing the query, so a sticky
        // check spams the reply and buries the real output under repaints.
        let mut answered_cursor_query = false;
        let probe = directory.path().join("probe.txt");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            if probe.is_file() {
                break;
            }
            if !answered_cursor_query {
                let asked = captured
                    .lock()
                    .map(|text| text.contains("\u{1b}[6n"))
                    .unwrap_or(false);
                if asked {
                    answered_cursor_query = true;
                    if console.write_all(CURSOR_REPLY).is_err() {
                        break;
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let ran_in_node_directory = probe.is_file();
        let _ = child.kill();
        drop(pair);
        let _ = reader.join();
        remove_mapped_batch(&command);
        // Tail only: the console is full of repaint escapes once PowerShell is
        // up, and the error that matters is the last thing it printed.
        let tail: String = captured
            .lock()
            .map(|text| {
                text.chars()
                    .rev()
                    .take(1500)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            ran_in_node_directory,
            "the mapped interop spawn did not run the shim in the node directory at {}.\n\
             Console output (tail):\n{tail}",
            directory.path().display()
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("prompt.txt"))
                .unwrap()
                .trim_end(),
            prompt.trim_end()
        );
    }

    /// The batch path is passed as `cmd.exe /c` command text, which cmd expands
    /// before it runs. Unlike the node directory inside the batch, this path
    /// cannot rely on doubled percent signs in the file.
    #[test]
    fn a_batch_path_carrying_percent_or_bang_is_refused() {
        for path in [
            r"C:\Users\%USERNAME%\Temp\launch.cmd",
            r"C:\Users\me!x\Temp\launch.cmd",
            r"C:\Users\R&D\Temp\launch.cmd",
            r"C:\Users\a|b\Temp\launch.cmd",
            r"C:\Users\a(b)\Temp\launch.cmd",
            r"C:\Users\a>b\Temp\launch.cmd",
            r"C:\Users\a^b\Temp\launch.cmd",
            "C:\\Users\\a\"b\\Temp\\launch.cmd",
        ] {
            assert!(
                super::has_cmd_metacharacter(path),
                "{path} reaches cmd.exe as command text and must be refused"
            );
        }
        assert!(
            !super::has_cmd_metacharacter(r"C:\Users\alond\AppData\Local\Temp\launch.cmd"),
            "an ordinary temp path must still be accepted"
        );
    }

    /// A `Direct` shell establishes no location of its own, so the spawn must
    /// carry the node's working directory. Suppressing it makes portable-pty
    /// fall back to `USERPROFILE` and opens the agent in the home folder —
    /// which is what regressed for every non-mapped arm.
    #[test]
    fn direct_shell_keeps_the_unc_working_directory() {
        let argv = wrap_argv(
            "claude.exe",
            UNC_SPAWN_PATH,
            EnvType::Windows,
            WindowsShell::Direct,
        );
        assert_eq!(argv.first().map(String::as_str), Some("claude.exe"));

        let command = wrap_command(
            "claude.exe",
            UNC_SPAWN_PATH,
            EnvType::Windows,
            WindowsShell::Direct,
        );
        assert_eq!(
            command
                .get_cwd()
                .map(|cwd| cwd.to_string_lossy().into_owned()),
            Some(UNC_SPAWN_PATH.to_string()),
            "a Direct shell never calls Set-Location or pushd, so it depends entirely on \
             the inherited working directory"
        );
    }

    /// PowerShell runs a native executable directly and accepts a UNC current
    /// directory, so this arm maps nothing — and must therefore still receive
    /// the directory.
    #[test]
    fn powershell_spawn_of_a_native_executable_keeps_the_unc_working_directory() {
        let bin_dir = tempfile::tempdir().unwrap();
        let native = bin_dir.path().join("codex.exe");
        std::fs::write(&native, "MZ").unwrap();

        let command = wrap_command(
            native.to_str().unwrap(),
            UNC_SPAWN_PATH,
            EnvType::Windows,
            WindowsShell::PowerShell,
        );

        assert_eq!(
            command
                .get_cwd()
                .map(|cwd| cwd.to_string_lossy().into_owned()),
            Some(UNC_SPAWN_PATH.to_string()),
            "this arm neither pushds nor Set-Locations, so without the cwd the agent opens \
             in USERPROFILE instead of the node's directory"
        );
    }

    /// The mapped arm drops the working directory, because `pushd` replaces it
    /// with a drive-letter mapping. The arms that must *keep* theirs are
    /// covered by the tests above.
    #[test]
    fn mapped_spawn_drops_the_working_directory() {
        let npm_dir = tempfile::tempdir().unwrap();
        let shim = npm_dir.path().join("codex.cmd");
        std::fs::write(&shim, "@echo off\r\n").unwrap();

        let command = wrap_command(
            shim.to_str().unwrap(),
            UNC_SPAWN_PATH,
            EnvType::Windows,
            WindowsShell::PowerShell,
        );

        assert!(
            command.get_cwd().is_none(),
            "cmd.exe must not be handed the UNC it is about to pushd, or it prints \
             'Defaulting to Windows directory' and the spawn looks broken"
        );
        remove_mapped_batch(&command);
    }

    /// The interop arms keep the working directory they have always had for a
    /// drive-letter directory; only the mapped case drops it.
    #[test]
    fn interop_drive_letter_spawn_keeps_its_working_directory() {
        let npm_dir = tempfile::tempdir().unwrap();
        let shim = npm_dir.path().join("codex.cmd");
        std::fs::write(&shim, "@echo off\r\n").unwrap();

        let command = wrap_command(
            shim.to_str().unwrap(),
            npm_dir.path().to_str().unwrap(),
            EnvType::WindowsInterop,
            WindowsShell::PowerShell,
        );

        assert!(
            command.get_cwd().is_some(),
            "a drive-letter interop directory is not the shape this change addresses, and \
             the in-script Set-Location must not become the only thing establishing it"
        );
    }

    /// A generic UNC share, not a WSL one: the behaviour under test is "the
    /// spawn directory is UNC", which cmd.exe rejects for any share, so the
    /// test stays hermetic and platform-independent.
    const UNC_SPAWN_PATH: &str = r"\\server\share\project";

    /// The regression, native arm: Codex is the one `WindowsShell::PowerShell`
    /// holder, and a Windows node whose directory is a UNC host path is exactly
    /// the shape that lost its working directory.
    #[test]
    #[cfg(windows)]
    fn powershell_spawn_in_a_unc_directory_maps_it_through_pushd() {
        let npm_dir = tempfile::tempdir().unwrap();
        let shim = npm_dir.path().join("codex.cmd");
        std::fs::write(&shim, "@echo off\r\n").unwrap();

        let command = wrap_command(
            shim.to_str().unwrap(),
            UNC_SPAWN_PATH,
            EnvType::Windows,
            WindowsShell::PowerShell,
        );
        let script = encoded_script(&command);
        assert_maps_through_pushd(&command, UNC_SPAWN_PATH, &script);
        remove_mapped_batch(&command);
        assert!(
            script.contains(&shim.to_string_lossy().into_owned()),
            "the agent is still the resolved .cmd shim — the fix maps the directory rather \
             than substituting a different executable; got: {script}"
        );
    }

    /// The same shape on the `WindowsInterop` arm, where Buildmesh runs on the
    /// Linux host. A filesystem probe cannot answer there (a Windows path is
    /// not on the Linux filesystem), so this arm must be driven by the command
    /// itself. `CodexProxy` is the route that puts a `.cmd` here at all, and it
    /// is the one that was still broken.
    #[test]
    #[cfg(windows)]
    fn windows_interop_powershell_spawn_in_a_unc_directory_maps_it_through_pushd() {
        let npm_dir = tempfile::tempdir().unwrap();
        let shim = npm_dir.path().join("codex.cmd");
        std::fs::write(&shim, "@echo off\r\n").unwrap();

        let command = wrap_command(
            shim.to_str().unwrap(),
            UNC_SPAWN_PATH,
            EnvType::WindowsInterop,
            WindowsShell::PowerShell,
        );
        let script = encoded_script(&command);
        assert_maps_through_pushd(&command, UNC_SPAWN_PATH, &script);
        remove_mapped_batch(&command);
        assert!(
            script.contains(&shim.to_string_lossy().into_owned()),
            "the interop arm must still launch the resolved shim; got: {script}"
        );
    }

    /// The control that keeps this narrow: a drive-letter directory is not the
    /// shape cmd.exe rejects, so it must keep today's direct launch.
    #[test]
    fn powershell_spawn_in_a_drive_letter_directory_is_not_mapped() {
        let npm_dir = tempfile::tempdir().unwrap();
        let shim = npm_dir.path().join("codex.cmd");
        std::fs::write(&shim, "@echo off\r\n").unwrap();

        let argv = wrap_argv(
            shim.to_str().unwrap(),
            npm_dir.path().to_str().unwrap(),
            EnvType::Windows,
            WindowsShell::PowerShell,
        );

        assert_eq!(
            argv.first().map(String::as_str),
            Some("powershell.exe"),
            "a drive-letter directory launches through the shim exactly as before; got: {argv:?}"
        );
        assert!(
            !argv.iter().any(|arg| arg == "pushd"),
            "no share mapping is needed for a local directory; got: {argv:?}"
        );
    }

    /// A native executable never reaches cmd.exe — PowerShell runs it directly
    /// and accepts a UNC current directory — so it must not be mapped.
    #[test]
    fn powershell_spawn_of_a_native_executable_is_not_mapped() {
        let bin_dir = tempfile::tempdir().unwrap();
        let native = bin_dir.path().join("codex.exe");
        std::fs::write(&native, "MZ").unwrap();

        let command = wrap_command(
            native.to_str().unwrap(),
            UNC_SPAWN_PATH,
            EnvType::Windows,
            WindowsShell::PowerShell,
        );
        let script = encoded_script(&command);

        assert_eq!(
            command
                .get_argv()
                .first()
                .map(|arg| arg.to_string_lossy().into_owned())
                .as_deref(),
            Some("powershell.exe")
        );
        assert!(
            script.contains(&native.to_string_lossy().into_owned()),
            "a .exe is executed by PowerShell directly; got: {script}"
        );
    }

    /// A bare stem is resolved by PowerShell itself, which prefers the `.ps1`
    /// twin over the `.cmd` shim — so cmd.exe never enters the chain and there
    /// is nothing to map. This is the interop non-proxy route.
    #[test]
    fn powershell_spawn_of_a_bare_stem_is_not_mapped() {
        let argv = wrap_argv(
            "codex",
            UNC_SPAWN_PATH,
            EnvType::WindowsInterop,
            WindowsShell::PowerShell,
        );

        assert_eq!(argv.first().map(String::as_str), Some("powershell.exe"));
        assert!(
            !argv.iter().any(|arg| arg == "pushd"),
            "PowerShell resolves a bare stem without cmd.exe, so the UNC directory is \
             never handed to it; got: {argv:?}"
        );
    }

    /// Both spellings the code accepts but no test covered: an uppercase
    /// batch extension, and the forward-slash UNC form external tools emit.
    #[test]
    #[cfg(windows)]
    fn uppercase_batch_extension_and_forward_slash_unc_still_map() {
        let npm_dir = tempfile::tempdir().unwrap();
        let shim = npm_dir.path().join("codex.CMD");
        std::fs::write(&shim, "@echo off\r\n").unwrap();
        let forward_slash = "//server/share/project";

        let command = wrap_command(
            shim.to_str().unwrap(),
            forward_slash,
            EnvType::Windows,
            WindowsShell::PowerShell,
        );

        assert_maps_through_pushd(&command, forward_slash, &encoded_script(&command));
        remove_mapped_batch(&command);
    }

    #[test]
    fn apply_wsl_env_carries_attention_callback_variables() {
        let mut command = portable_pty::CommandBuilder::new("wsl.exe");
        apply_wsl_env(&mut command, crate::models::EnvType::Wsl, &[], &[]);

        let wslenv = command
            .get_env("WSLENV")
            .expect("WSLENV should be configured for WSL")
            .to_string_lossy();
        assert!(wslenv
            .split(':')
            .any(|entry| entry.split('/').next() == Some("BUILDMESH_PORT")));
        assert!(wslenv
            .split(':')
            .any(|entry| entry.split('/').next() == Some("BUILDMESH_SESSION_ID")));
        command.env("BUILDMESH_HOOK_TOKEN", "test-token");
        apply_wsl_env(&mut command, crate::models::EnvType::Wsl, &[], &[]);
        assert!(command
            .get_env("WSLENV")
            .unwrap()
            .to_string_lossy()
            .split(':')
            .any(|entry| entry.split('/').next() == Some("BUILDMESH_HOOK_TOKEN")));
    }

    #[test]
    fn append_to_wslenv_deduplicates_by_base_name() {
        let mut wslenv = "SSH_AUTH_SOCK/up:CODEX_HOME/u".to_string();
        append_to_wslenv(&mut wslenv, "CODEX_HOME", "/u");
        assert_eq!(wslenv, "SSH_AUTH_SOCK/up:CODEX_HOME/u");
        append_to_wslenv(&mut wslenv, "BUILDMESH_PORT", "/u");
        assert_eq!(wslenv, "SSH_AUTH_SOCK/up:CODEX_HOME/u:BUILDMESH_PORT/u");
    }

    /// #2034 — the end-to-end fail-closed assertion on the platform that owns
    /// the Seatbelt branch. Before this, a profile-write failure logged and
    /// returned a *direct* command, so the requested sandbox silently became an
    /// unsandboxed launch. `Err` here is what stops that: `launch_process`
    /// propagates it before `spawn_child`, so no agent process is created.
    ///
    /// macOS-only because the branch is `cfg!(target_os = "macos")`; the
    /// portable half of the contract — profile-write failure yields `Err` — is
    /// pinned in `agent::sandbox` on every host.
    #[cfg(target_os = "macos")]
    #[test]
    fn requested_sandbox_without_a_writable_profile_yields_no_command() {
        let session_id = -97_512 - (std::process::id() as i64 % 100_000);
        let profile = std::env::temp_dir().join(format!("buildmesh-sandbox-{session_id}.sb"));
        // RAII: the gate restores the previous env value and the scratch
        // directory is removed even if an assertion below panics.
        let _scratch = crate::sandbox::test_support::block_writes_at(&profile);

        let result = crate::sandbox::test_support::with_dev_gate_result(Some("1"), || {
            let recipe = crate::agent::provider::SpawnRecipe {
                binary: "claude",
                base_args: vec!["--dangerously-skip-permissions".into()],
                trailing_args: vec![],
                windows_shell: crate::agent::provider::WindowsShell::Direct,
            };
            super::wrap(
                recipe,
                EnvType::Windows,
                None,
                None,
                &std::env::temp_dir().to_string_lossy(),
                session_id,
                true,
            )
        });

        let error = match result {
            Ok(_) => panic!("a failed sandbox setup must not yield a launchable command"),
            Err(error) => error,
        };
        assert!(error.contains("sandbox setup failed"), "{error}");
        // The remediation must name the real cause — a temp-directory write
        // failure — not tell the user to re-enable a gate that was already
        // open for this to be reachable.
        assert!(error.contains("temporary directory"), "{error}");
    }

    /// #2034 — the developer gate is authoritative. Even with the Mesh flag on,
    /// a process started without `BUILDMESH_SANDBOX=1` must build the ordinary
    /// direct command rather than a `sandbox-exec` wrapper.
    #[cfg(target_os = "macos")]
    #[test]
    fn mesh_flag_alone_does_not_reach_the_seatbelt_wrapper() {
        let command = crate::sandbox::test_support::with_dev_gate_result(None, || {
            let recipe = crate::agent::provider::SpawnRecipe {
                binary: "claude",
                base_args: vec![],
                trailing_args: vec![],
                windows_shell: crate::agent::provider::WindowsShell::Direct,
            };
            super::wrap(
                recipe,
                EnvType::Windows,
                None,
                None,
                &std::env::temp_dir().to_string_lossy(),
                -97_513 - (std::process::id() as i64 % 100_000),
                true,
            )
        })
        .expect("the unsandboxed path always assembles");
        let argv = command.get_argv();
        assert_ne!(
            argv.first().map(|s| s.to_string_lossy().into_owned()),
            Some("sandbox-exec".to_string()),
            "a persisted mesh flag must not open the sandbox in a release build: {argv:?}"
        );
    }

    fn decode_ps(encoded: &str) -> String {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64");
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).expect("valid utf-16")
    }

    /// PowerShell's -EncodedCommand expects Base64 of UTF-16LE bytes with NO BOM.
    /// A BOM (or worse, the wrong-endian BOM) prepends a U+FEFF/U+FFFE code unit to
    /// the decoded command and breaks every Windows PowerShell spawn.
    #[test]
    fn encode_for_powershell_produces_no_bom_utf16le() {
        let encoded = encode_for_powershell("echo hi");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .expect("valid base64");

        // First two bytes must be the UTF-16LE encoding of 'e' (0x65 0x00), not a BOM.
        assert_eq!(
            &bytes[..2],
            &[0x65, 0x00],
            "leading bytes should be 'e' as UTF-16LE, not a BOM"
        );

        let decoded = decode_ps(&encoded);
        assert_eq!(decoded, "echo hi");
    }

    /// The formatter must use PowerShell's call operator `&` with each arg
    /// wrapped in single quotes, so multi-line prefill text (e.g. selected text
    /// from a Handover containing backticks, brackets, numbered lists) is
    /// treated as a single string literal — not parsed as separate PowerShell
    /// statements line-by-line.
    ///
    /// Regression originally surfaced when an early issue-spawn prefill body
    /// contained backticks (PowerShell's line-continuation character), which
    /// PowerShell parsed as the start of a new statement and rejected with
    /// `Unexpected token '<word>'`. The issue path now ships just a URL +
    /// title — see memory: buildmesh-issue-spawn-url-only — but the handover
    /// path still ships arbitrary multi-line text, so this guarantee still
    /// matters.
    #[test]
    fn format_powershell_command_quotes_multiline_prefill_safely() {
        let body = "Currently, when spawning a new agent...\n\
                    1. `default_provider` (per-mesh override)\n\
                    2. Buildmesh-wide default\n\
                    3. Anthropic (hardcoded fallback)";
        let args = vec![
            "--anthropic".to_string(),
            "--prefill".to_string(),
            body.to_string(),
        ];
        let cmd_str = format_powershell_command("claude", &args);

        // Must start with the call operator so PowerShell treats it as command invocation.
        assert!(
            cmd_str.starts_with("& "),
            "command must use PowerShell call operator: {}",
            cmd_str
        );

        // The binary and every arg must be wrapped in single quotes. After the
        // leading `& 'claude' `, there must be no bare newline outside of a quoted
        // string — i.e. every newline in the prefill stays inside the single-quoted
        // arg, not at the top level of the script.
        let after_call = cmd_str.strip_prefix("& ").unwrap();
        assert!(
            after_call.starts_with("'claude'"),
            "binary must be single-quoted: {}",
            cmd_str
        );

        // The prefill body's newlines must appear inside a single-quoted region —
        // i.e. between an odd-numbered ' and the next '. We verify by checking
        // that every newline is preceded by an odd number of single quotes.
        for (i, _) in cmd_str.match_indices('\n') {
            let quotes_before = cmd_str[..i].chars().filter(|c| *c == '\'').count();
            assert!(
                quotes_before % 2 == 1,
                "newline at byte {} is outside a quoted string — PowerShell will parse it as a new statement.\nCommand: {}",
                i, cmd_str
            );
        }
    }

    /// Single quotes inside an argument must be escaped by doubling them ('')
    /// per PowerShell single-quoted string rules.
    #[test]
    fn format_powershell_command_escapes_embedded_single_quotes() {
        let args = vec!["--prefill".to_string(), "it's a test".to_string()];
        let cmd_str = format_powershell_command("claude", &args);
        assert!(
            cmd_str.contains("'it''s a test'"),
            "expected doubled-quote escaping, got: {}",
            cmd_str
        );
    }
}
