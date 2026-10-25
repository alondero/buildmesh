//! Bounded native/WSL Codex executable, version, home, and capability probes.

use super::*;
pub(super) fn parse_version(output: &str) -> Option<(u32, u32, u32, String)> {
    let token = output
        .split_whitespace()
        .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let normalized = token.split('-').next()?;
    let mut parts = normalized.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch, normalized.to_string()))
}

pub(super) fn validate_proxy_cli_help(fresh_help: &str, resume_help: &str) -> Result<(), String> {
    for (invocation, help) in [("fresh", fresh_help), ("resume", resume_help)] {
        let missing = ["--profile", "--model"]
            .into_iter()
            .filter(|flag| !help.contains(flag))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "Codex {invocation} invocation does not support required proxy flags: {}",
                missing.join(", ")
            ));
        }
    }
    Ok(())
}

/// Wall-clock bound for one Codex capability probe (`--version`, `--help`).
///
/// These are short, local, non-interactive reads that finish in well under a
/// second, but a bare `.output()` waits forever for a child that never exits:
/// a wedged `wsl.exe` (paused VM, busy LxssManager) or a `.cmd` shim blocked on
/// a console handle returns no output and no exit code. That pinned a
/// blocking-pool thread for the life of the process, so one stuck probe left
/// the Settings → Providers tab stuck on "loading" with no way to recover
/// (this command runs 4 discovery chains per tab open — see
/// `provider_menu::available_providers`). 20s matches the bound the
/// WindowsInterop branch already used and is far above the real cost.
pub(super) const CODEX_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Bound for the executable-location and `CODEX_HOME` lookups below. These are
/// plain host/guest reads with no PowerShell in the path, so they get a tighter
/// bound than [`CODEX_PROBE_TIMEOUT`]; the first `wsl.exe` call after a cold
/// VM start can take seconds, and 10s leaves ample headroom.
pub(super) const CODEX_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub(super) fn codex_output(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    args: &[&str],
) -> Result<std::process::Output, String> {
    let (mut command, op_name) = if env_type == EnvType::WindowsInterop {
        let args = args
            .iter()
            .map(|arg| crate::env::powershell_literal(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let command =
            crate::env::powershell_command(&format!("& codex {args}; exit $LASTEXITCODE"));
        return crate::process_util::run_command_with_timeout(
            command,
            "Windows Codex probe",
            CODEX_PROBE_TIMEOUT,
        );
    } else if env_type == EnvType::Wsl {
        let mut command = crate::process_util::command_no_window("wsl.exe");
        command.args([
            "-d",
            wsl_distro.ok_or_else(|| "WSL distribution identity is unavailable".to_string())?,
            "--exec",
            "codex",
        ]);
        (command, "WSL Codex probe")
    } else if cfg!(target_os = "windows") {
        // npm installs Codex as a `.cmd` shim. `std::process::Command` cannot
        // execute batch files directly on Windows, so capability probes use
        // the same non-interactive cmd relay as other shim-backed providers.
        let mut command = crate::process_util::command_no_window("cmd.exe");
        command.args(["/d", "/c", "codex"]);
        (command, "Windows Codex probe")
    } else {
        (
            crate::process_util::command_no_window("codex"),
            "Codex probe",
        )
    };
    command.args(args);
    // `run_command_with_timeout` names the operation and the failure in its
    // own error strings ("WSL Codex probe timed out after 20s"), which is more
    // actionable than the old blanket "Codex executable is unavailable" —
    // the timeout case is not an availability problem.
    crate::process_util::run_command_with_timeout(command, op_name, CODEX_PROBE_TIMEOUT)
}

pub(super) fn successful_help(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    args: &[&str],
    label: &str,
) -> Result<String, String> {
    let output = codex_output(env_type, wsl_distro, args)?;
    if !output.status.success() {
        return Err(format!("Codex {label} capability check failed"));
    }
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// Distinct wrappers for probe results.
///
/// Left as bare `String`s, all three identity probes (and both help probes)
/// share one signature, so passing them to a joiner out of order type-checks
/// while silently changing which error a broken install surfaces. With these
/// wrappers any permutation is a type error, so the precedence the tests pin
/// cannot regress at a call site (issue #1934).
#[derive(Debug)]
pub(super) struct CodexVersion(pub(super) String);
#[derive(Debug)]
pub(super) struct CodexExecutable(pub(super) String);
#[derive(Debug)]
pub(super) struct CodexHome(pub(super) String);
#[derive(Debug)]
pub(super) struct FreshHelp(pub(super) String);
#[derive(Debug)]
pub(super) struct ResumeHelp(pub(super) String);

/// Join a probe thread, re-raising a panic with its original payload.
///
/// `join().unwrap()` replaces the payload with `called 'Result::unwrap()' on an
/// 'Err' value: Any { .. }`, which would reach `run_blocking` without the
/// message naming the probe that failed. `resume_unwind` propagates the
/// original payload, which is what the serial chain did.
pub(crate) fn join_probe<T>(probe: std::thread::ScopedJoinHandle<'_, T>) -> T {
    match probe.join() {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Issue the three independent Codex identity probes for one runtime
/// concurrently and collect them in discovery order.
///
/// They read the same CLI but do not depend on each other: the executable
/// location and `CODEX_HOME` each have their own process spawn, and only the
/// capability key further down needs the version. Chaining them made one WSL
/// discovery pay three `wsl.exe` starts in sequence, so the wall clock was
/// their sum rather than their max.
///
/// **Error precedence stays the serial order.** A missing `codex` fails all
/// three probes at once, and the user must still see `Codex version check
/// failed` — the root cause — rather than the location/home failures that only
/// follow from it. Every result is joined before any is surfaced, then they are
/// reported earliest-step-first. The distinct result types above make that order
/// a compile-time property of this signature, not a convention.
pub(super) fn probe_codex_identity_concurrently(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    version: impl FnOnce(EnvType, Option<&str>) -> Result<CodexVersion, String> + Send,
    executable: impl FnOnce(EnvType, Option<&str>) -> Result<CodexExecutable, String> + Send,
    home: impl FnOnce(EnvType, Option<&str>) -> Result<CodexHome, String> + Send,
) -> Result<(CodexVersion, CodexExecutable, CodexHome), String> {
    std::thread::scope(|scope| {
        let version = scope.spawn(|| version(env_type, wsl_distro));
        let executable = scope.spawn(|| executable(env_type, wsl_distro));
        let home = scope.spawn(|| home(env_type, wsl_distro));
        let version = join_probe(version);
        let executable = join_probe(executable);
        let home = join_probe(home);
        Ok((version?, executable?, home?))
    })
}

/// `codex --version`, parsed and range-checked against the proxied-CLI floor.
pub(super) fn probe_codex_version(
    env_type: EnvType,
    wsl_distro: Option<&str>,
) -> Result<CodexVersion, String> {
    let output = codex_output(env_type, wsl_distro, &["--version"])?;
    if !output.status.success() {
        return Err("Codex version check failed".into());
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let (major, minor, patch, version) = parse_version(&raw)
        .ok_or_else(|| format!("could not parse Codex version from {:?}", raw.trim()))?;
    if (major, minor, patch) < MIN_PROXY_CODEX_VERSION {
        return Err(format!(
            "proxied Codex requires codex-cli >= 0.144.0; found {version}"
        ));
    }
    Ok(CodexVersion(version))
}

/// The first line of a Windows executable-locator answer that names a runnable
/// file, or `""` when it names none.
///
/// `where.exe` and `Get-Command -CommandType Application` both answer with
/// *every* match, and an npm Windows prefix holds `codex.cmd` beside the
/// extensionless POSIX shim npm also writes there. Taking the whole answer put
/// a CRLF inside the single-quoted PowerShell literal this path is destined
/// for, so the resolved executable was unusable — the `WindowsInterop` Codex
/// probe read the entire stdout, and the native arm read every line it could
/// not filter.
pub(super) fn first_runnable_windows_path(candidates: &str) -> &str {
    candidates
        .lines()
        .map(str::trim)
        .find(|path| {
            [".exe", ".cmd", ".bat", ".com"]
                .iter()
                .any(|extension| path.to_ascii_lowercase().ends_with(extension))
        })
        .unwrap_or_default()
}

/// Step 2: resolve the absolute path of this runtime's Codex executable.
///
/// Both locators answer with *every* match on PATH, and an npm Windows prefix
/// holds both `codex.cmd` and the extensionless POSIX shim beside it, so each
/// answer is filtered by [`first_runnable_windows_path`] rather than taken
/// whole. An empty result is left to the caller's emptiness check.
pub(super) fn probe_codex_executable(
    env_type: EnvType,
    wsl_distro: Option<&str>,
) -> Result<CodexExecutable, String> {
    let executable = if env_type == EnvType::WindowsInterop {
        let command = crate::env::powershell_command("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); (Get-Command codex -CommandType Application -ErrorAction Stop).Source");
        let output = crate::process_util::run_command_with_timeout(
            command,
            "Windows Codex location",
            CODEX_LOOKUP_TIMEOUT,
        )?;
        first_runnable_windows_path(&String::from_utf8_lossy(&output.stdout)).to_string()
    } else if env_type == EnvType::Wsl {
        let mut locate = crate::process_util::command_no_window("wsl.exe");
        locate.args([
            "-d",
            wsl_distro.expect("WSL distribution was resolved"),
            "--exec",
            "sh",
            "-lc",
            "printf '__BUILDMESH_WSL_CODEX_EXECUTABLE__%s\\n' \"$(command -v codex)\"",
        ]);
        let out = crate::process_util::run_command_with_timeout(
            locate,
            "WSL Codex executable location",
            CODEX_LOOKUP_TIMEOUT,
        )?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix("__BUILDMESH_WSL_CODEX_EXECUTABLE__"))
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .unwrap_or_default()
            .to_string()
    } else {
        let locator = if cfg!(target_os = "windows") {
            "where.exe"
        } else {
            "which"
        };
        let mut locate = crate::process_util::command_no_window(locator);
        locate.arg("codex");
        let out = crate::process_util::run_command_with_timeout(
            locate,
            "Codex executable location",
            CODEX_LOOKUP_TIMEOUT,
        )?;
        let candidates = String::from_utf8_lossy(&out.stdout);
        if cfg!(target_os = "windows") {
            first_runnable_windows_path(&candidates).to_string()
        } else {
            candidates
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        }
    };
    if executable.is_empty() {
        return Err("Codex executable identity is unavailable".into());
    }
    Ok(CodexExecutable(executable))
}

/// Step 3: resolve this runtime's `CODEX_HOME`.
pub(super) fn probe_codex_home(
    env_type: EnvType,
    wsl_distro: Option<&str>,
) -> Result<CodexHome, String> {
    if env_type == EnvType::WindowsInterop {
        let home = crate::env::codex_dir_for_env(env_type, "")
            .ok_or_else(|| "Windows Codex home unavailable".to_string())?;
        return Ok(CodexHome(crate::env::windows_path_from_wsl(
            &home.to_string_lossy(),
        )));
    }
    if let Some(distro) = wsl_distro {
        let mut command = crate::process_util::command_no_window("wsl.exe");
        command.args(["-d", distro, "--exec", "sh", "-lc", WSL_CODEX_HOME_SCRIPT]);
        let output = crate::process_util::run_command_with_timeout(
            command,
            "WSL Codex home resolution",
            CODEX_LOOKUP_TIMEOUT,
        )?;
        let Some(home) = crate::env::parse_wsl_codex_home_output(&output.stdout) else {
            return Err("WSL Codex home identity is unavailable".into());
        };
        if !output.status.success() {
            return Err("WSL Codex home identity is unavailable".into());
        }
        return Ok(CodexHome(home.to_string_lossy().into_owned()));
    }
    Ok(CodexHome(
        native_codex_home()?.to_string_lossy().into_owned(),
    ))
}

/// Run the two proxy-capability `--help` probes concurrently.
///
/// `fresh` and `resume` are independent reads of the same CLI, and each one is
/// a separate process spawn, so the pair was paying double for one answer. They
/// run after the identity trio because the capability key needs the parsed
/// version, so they overlap each other but not the trio. Error precedence stays
/// in probe order, which is also the order [`validate_proxy_cli_help`] names a
/// missing flag in.
pub(super) fn probe_proxy_cli_help_concurrently(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    fresh: impl FnOnce(EnvType, Option<&str>) -> Result<FreshHelp, String> + Send,
    resume: impl FnOnce(EnvType, Option<&str>) -> Result<ResumeHelp, String> + Send,
) -> Result<(FreshHelp, ResumeHelp), String> {
    std::thread::scope(|scope| {
        let fresh = scope.spawn(|| fresh(env_type, wsl_distro));
        let resume = scope.spawn(|| resume(env_type, wsl_distro));
        let fresh = join_probe(fresh);
        let resume = join_probe(resume);
        Ok((fresh?, resume?))
    })
}

pub(super) fn discover_supported_install_uncached(
    env_type: EnvType,
) -> Result<CodexInstall, String> {
    let wsl_distro = if env_type == EnvType::Wsl {
        Some(
            crate::env::detect_default_wsl_distro()
                .ok_or_else(|| "default WSL distribution is unavailable".to_string())?,
        )
    } else {
        None
    };
    let (CodexVersion(version), CodexExecutable(executable), CodexHome(codex_home)) =
        probe_codex_identity_concurrently(
            env_type,
            wsl_distro.as_deref(),
            probe_codex_version,
            probe_codex_executable,
            probe_codex_home,
        )?;
    let runtime = if let Some(distro) = wsl_distro.as_deref() {
        wsl_runtime_identity(distro, &codex_home)
    } else {
        runtime_identity(env_type).to_string()
    };
    let capability_key = format!("{}\0{}\0{}", runtime, executable, version);
    let capabilities_are_cached = CLI_CAPABILITY_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&capability_key);
    if !capabilities_are_cached {
        let (FreshHelp(fresh_help), ResumeHelp(resume_help)) = probe_proxy_cli_help_concurrently(
            env_type,
            wsl_distro.as_deref(),
            |env_type, wsl_distro| {
                successful_help(env_type, wsl_distro, &["--help"], "fresh").map(FreshHelp)
            },
            |env_type, wsl_distro| {
                successful_help(env_type, wsl_distro, &["resume", "--help"], "resume")
                    .map(ResumeHelp)
            },
        )?;
        validate_proxy_cli_help(&fresh_help, &resume_help)?;
        // The cache-miss path stays idempotent: two runtimes discovering the
        // same `runtime\0executable\0version` concurrently may both probe and
        // both insert, which is harmless. No single-flight assumption is built
        // on top of it.
        CLI_CAPABILITY_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(capability_key);
    }
    Ok(CodexInstall {
        executable,
        version,
        runtime_identity: runtime,
        codex_home,
        wsl_distro,
    })
}
