//! Bounded native version probes shared by deferred discovery and spawn preflight.
use crate::agent::provider::{shell_for, Platform, SpawnRecipe};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct ExecutableStamp {
    modified: SystemTime,
    len: u64,
}

struct CachedProbe {
    stamp: ExecutableStamp,
    major: Option<u64>,
    checked: Instant,
}

static VERSION_CACHE: LazyLock<Mutex<HashMap<PathBuf, CachedProbe>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn executable_stamp(path: &Path) -> Option<ExecutableStamp> {
    let metadata = path.metadata().ok()?;
    Some(ExecutableStamp {
        modified: metadata.modified().ok()?,
        len: metadata.len(),
    })
}

pub(crate) fn major(text: &str) -> Option<u64> {
    static ANSI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").expect("ANSI CSI pattern")
    });
    let plain = ANSI.replace_all(text, "");
    let text = plain.trim();
    let text = text
        .strip_prefix("opencode ")
        .unwrap_or(text)
        .trim_start_matches('v');
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts
        .next()?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    Some(major)
}

pub(crate) fn probe(executable: &Path) -> Option<u64> {
    probe_with_timeout(executable, Duration::from_secs(30))
}

fn probe_with_timeout(executable: &Path, timeout: Duration) -> Option<u64> {
    let stamp = executable_stamp(executable);
    if let Some(cached) = VERSION_CACHE.lock().unwrap().get(executable) {
        if Some(cached.stamp) == stamp
            && (cached.major.is_some() || cached.checked.elapsed() < Duration::from_secs(5))
        {
            return cached.major;
        }
    }
    // Do not hold the cache lock across subprocess I/O. Unknown results expire
    // quickly so a transient failure does not hide a repaired installation.
    let major = probe_uncached(executable, timeout);
    if let Some(stamp) = stamp.filter(|stamp| Some(*stamp) == executable_stamp(executable)) {
        let mut cache = VERSION_CACHE.lock().unwrap();
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(
            executable.to_path_buf(),
            CachedProbe {
                stamp,
                major,
                checked: Instant::now(),
            },
        );
    }
    major
}

fn probe_uncached(executable: &Path, timeout: Duration) -> Option<u64> {
    let recipe = SpawnRecipe {
        binary: "opencode",
        base_args: vec!["--version".into()],
        trailing_args: vec![],
        windows_shell: shell_for(Platform::current()),
    };
    let command =
        crate::agent::spawn_environment::background_command(&recipe, Some(executable)).ok()?;
    run_version_probe(command, timeout)
}

fn run_version_probe(mut command: std::process::Command, timeout: Duration) -> Option<u64> {
    command.env_remove("FORCE_COLOR").env("NO_COLOR", "1");
    let output = run_private_command(command, "OpenCode version", timeout).ok()?;
    output
        .status
        .success()
        .then(|| major(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

/// These commands may start a private server. Own the whole process tree on
/// success, error and timeout, including a Unix process group created by the
/// background command wrapper. No child can keep an output pipe open forever.
pub(super) fn run_private_command(
    mut command: std::process::Command,
    label: &str,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn {label}: {e}"))?;
    let guard = crate::agent::background::BackgroundProcessGuard::new(child.id());
    let stdout = drain(child.stdout.take().expect("piped stdout"));
    let stderr = drain(child.stderr.take().expect("piped stderr"));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            result => {
                guard.terminate();
                let _ = child.kill();
                let _ = child.wait();
                break Err(match result {
                    Err(e) => format!("{label} wait failed: {e}"),
                    _ => format!("{label} timed out after {}s", timeout.as_secs()),
                });
            }
        }
    };
    drop(guard);
    // Join both readers after terminating descendants, including on timeout.
    let stdout = stdout.join();
    let stderr = stderr.join();
    let read = |result: std::thread::Result<std::io::Result<Vec<u8>>>| {
        result
            .map_err(|_| format!("{label} output reader panicked"))?
            .map_err(|e| format!("{label} output failed: {e}"))
    };
    Ok(std::process::Output {
        status: status?,
        stdout: read(stdout)?,
        stderr: read(stderr)?,
    })
}

fn drain(
    mut pipe: impl Read + Send + 'static,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    })
}

/// V1 discovery and launch preserve the pre-probe contract on unknown output;
/// V2 requires a positively identified matching major.
pub(crate) fn accepts_major(actual: Option<u64>, expected: u64) -> bool {
    actual == Some(expected) || (expected == 1 && actual.is_none())
}

pub(crate) fn require_major(executable: &Path, expected: u64) -> Result<(), String> {
    let actual = probe(executable);
    if accepts_major(actual, expected) {
        return Ok(());
    }
    match actual {
        Some(actual) => Err(format!("This harness requires OpenCode {expected}; executable {} reported major {actual}. Pin the matching CLI executable in Settings.", executable.display())),
        None => Err(format!("Could not determine the OpenCode {expected} version for executable {}. Its --version probe failed, timed out, or returned unrecognized output. Check the CLI and retry.", executable.display())),
    }
}

#[cfg(test)]
#[path = "opencode_version_tests.rs"]
mod tests;
