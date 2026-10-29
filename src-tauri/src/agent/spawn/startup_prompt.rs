//! Typed delivery of a user-facing spawn's initial prompt for a harness that
//! has no CLI prefill (Freebuff, Kimi Code, Perch, DeepSeek Harness).
//!
//! Issue, pull-request, prompt and handover spawns build their prompt once
//! (`SpawnIntent::initial_prompt`). A harness with CLI prefill receives it on
//! argv. Any other interactive harness launches plain, and this module types
//! the prompt into the live PTY once the TUI has booted, followed by Enter —
//! the same result as a startup prefill, through the path the user's own
//! keystrokes take.
//!
//! "Booted" is observed, not assumed: the node is armed before launch so the
//! PTY reader records its first output, and delivery waits until that output
//! has been quiet for [`BOOT_QUIET`]. A TUI that keeps animating, or never
//! draws, is given the prompt at [`BOOT_DEADLINE`] instead of being stranded.
//!
//! The prompt does not overwrite the user: the input stamp is captured when the
//! process starts, and a keystroke into the terminal before delivery (or
//! during the paste/Enter sequence) abandons the automated prompt.
//!
//! Circuits keep their own first-turn path
//! (`services::circuit_worker::spawn`), which carries run fences and
//! evidence this module has no use for.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Output silence that reads as "the TUI finished drawing its first screen".
const BOOT_QUIET: Duration = Duration::from_millis(1_500);
/// Longest wait before typing into a TUI that never went quiet.
const BOOT_DEADLINE: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
struct Boot {
    last_output: Option<Instant>,
}

static ARMED: Lazy<Mutex<HashMap<i64, Boot>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Start recording boot output for a node about to launch with a typed prompt.
pub(super) fn arm(node_id: i64) {
    ARMED.lock().unwrap().insert(node_id, Boot::default());
}

pub(super) fn disarm(node_id: i64) {
    ARMED.lock().unwrap().remove(&node_id);
}

/// PTY reader hook: one map lookup for nodes that are not armed.
pub(super) fn on_output(node_id: i64) {
    if let Some(boot) = ARMED.lock().unwrap().get_mut(&node_id) {
        boot.last_output = Some(Instant::now());
    }
}

fn quiet_for(node_id: i64) -> Option<Duration> {
    ARMED
        .lock()
        .unwrap()
        .get(&node_id)
        .and_then(|boot| boot.last_output)
        .map(|at| at.elapsed())
}

/// Pure delivery rule: type once boot output has gone quiet, or at the
/// deadline whatever the screen is doing.
fn ready_to_type(quiet: Option<Duration>, waited: Duration) -> bool {
    quiet.is_some_and(|quiet| quiet >= BOOT_QUIET) || waited >= BOOT_DEADLINE
}

/// Why a scheduled prompt was not typed.
#[derive(Debug, PartialEq, Eq)]
enum Abandoned {
    ProcessGone,
    UserTyped,
}

/// Wait until the launched TUI is ready for typed input, while the process
/// lives and nobody has typed past `stamp`.
fn wait_for_boot(node_id: i64, stamp: &str) -> Result<(), Abandoned> {
    let registry = &crate::agent::process::PROCESS_REGISTRY;
    let started = Instant::now();
    loop {
        std::thread::sleep(POLL);
        if !registry.is_alive(&node_id) {
            return Err(Abandoned::ProcessGone);
        }
        if registry.input_stamp(node_id).as_deref() != Some(stamp) {
            return Err(Abandoned::UserTyped);
        }
        if ready_to_type(quiet_for(node_id), started.elapsed()) {
            return Ok(());
        }
    }
}

/// Type `prompt` into the node's PTY once its harness has booted, then press
/// Enter. Runs on its own thread; the spawn has already returned.
pub(super) fn deliver_after_boot(app: tauri::AppHandle, node_id: i64, prompt: String) {
    let Some(stamp) = crate::agent::process::PROCESS_REGISTRY.input_stamp(node_id) else {
        disarm(node_id);
        tracing::warn!("startup_prompt({node_id}): no live process; initial prompt not typed");
        return;
    };
    std::thread::spawn(move || {
        let booted = wait_for_boot(node_id, &stamp);
        disarm(node_id);
        if let Err(reason) = booted {
            tracing::info!("startup_prompt({node_id}): initial prompt not typed: {reason:?}");
            return;
        }
        let result = crate::circuit::delivery::PreparedPrompt::prepare(node_id, &prompt).and_then(
            |prepared| {
                crate::circuit::delivery::write_prompt_to_pty_guarded(
                    &crate::agent::process::PROCESS_REGISTRY,
                    &prepared,
                    &app,
                    Some(&stamp),
                )
            },
        );
        match result {
            Ok(true) => tracing::info!("startup_prompt({node_id}): initial prompt submitted"),
            Ok(false) => tracing::info!(
                "startup_prompt({node_id}): terminal input changed; initial prompt left to the user"
            ),
            Err(error) => {
                tracing::warn!("startup_prompt({node_id}): initial prompt not submitted: {error}");
                crate::commands::attention::mark_attention(node_id, &app);
            }
        }
    });
}

#[cfg(test)]
#[path = "startup_prompt_tests.rs"]
mod tests;
