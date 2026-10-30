//! Bounded native version probes shared by deferred discovery and spawn preflight.
use crate::agent::provider::{shell_for, Platform, SpawnRecipe};
use std::{path::Path, time::Duration};

pub(crate) fn major(text: &str) -> Option<u64> {
    let text = text
        .trim()
        .strip_prefix("opencode ")
        .unwrap_or(text.trim())
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
    let recipe = SpawnRecipe {
        binary: "opencode",
        base_args: vec!["--version".into()],
        trailing_args: vec![],
        windows_shell: shell_for(Platform::current()),
    };
    let command =
        crate::agent::spawn_environment::background_command(&recipe, Some(executable)).ok()?;
    let output = run_private_command(command, "OpenCode version", Duration::from_secs(5)).ok()?;
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
    use std::{process::Stdio, time::Instant};
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn {label}: {e}"))?;
    let guard = crate::agent::background::BackgroundProcessGuard::new(child.id());
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            result => {
                guard.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(e) => format!("{label} wait failed: {e}"),
                    _ => format!("{label} timed out after {}s", timeout.as_secs()),
                });
            }
        }
    }
    drop(guard);
    child
        .wait_with_output()
        .map_err(|e| format!("{label} output failed: {e}"))
}

pub(crate) fn require_major(executable: &Path, expected: u64) -> Result<(), String> {
    match probe(executable) {
        Some(actual) if actual == expected => Ok(()),
        actual => Err(format!("This harness requires OpenCode {expected}; executable {} reported major {actual:?}. Pin the matching CLI executable in Settings.", executable.display())),
    }
}

#[cfg(test)]
#[path = "opencode_version_tests.rs"]
mod tests;
