//! Typed delivery of a user-facing spawn's initial prompt for a harness that
//! has no CLI prefill (Freebuff, Kimi Code, Perch, DeepSeek Harness).
//!
//! Issue, pull-request, prompt and handover spawns build their prompt once
//! (`SpawnIntent::initial_prompt`). A harness with CLI prefill receives it on
//! argv. Any other interactive harness launches plain, and this module types
//! the prompt into the live PTY once the TUI has booted, followed by Enter.
//!
//! Readiness is a heuristic, not a handshake. The node is armed before
//! launch so the PTY reader records its boot output here (the circuit
//! evaluator does not buffer ordinary nodes). Delivery waits until that
//! output has been quiet for [`BOOT_QUIET`] **and** the adapter's
//! [`ready_for_initial_prompt`] accepts the cleaned boot tail; a harness that
//! keeps reading nothing after its first quiet gap (network or auth work
//! before raw mode) needs to override that hook with its own ready marker.
//! At [`BOOT_DEADLINE`] the prompt is typed whatever the screen shows, so a
//! TUI that never settles is not stranded.
//!
//! Submission is not verified: ordinary nodes have no submit observer
//! (`circuit::delivery::press_enter_until_output_guarded` sends one Enter),
//! so a harness that flushes its input buffer while entering raw mode would
//! lose the prompt silently.
//!
//! The prompt is not typed over the user: the input stamp captured when the
//! process started guards the whole paste and Enter, and a keystroke into
//! the terminal before delivery abandons the automated prompt.
//!
//! Circuits keep their own first-turn path (`services::circuit_worker::spawn`).
//!
//! [`ready_for_initial_prompt`]: crate::agent::provider::AgentProvider::ready_for_initial_prompt

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Output silence that reads as "the TUI finished drawing its first screen".
const BOOT_QUIET: Duration = Duration::from_millis(1_500);
/// Longest wait before typing into a TUI that never went quiet.
const BOOT_DEADLINE: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(250);
/// Raw boot output kept for the adapter's ready marker.
const TAIL_BYTES: usize = 8 * 1024;

#[derive(Debug, Default)]
struct Boot {
    last_output: Option<Instant>,
    tail: String,
}

static ARMED: Lazy<Mutex<HashMap<i64, Boot>>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// Lets the PTY reader skip the lock while no node is booting.
static ANY_ARMED: AtomicBool = AtomicBool::new(false);

/// Start recording boot output for a node about to launch with a typed prompt.
pub(super) fn arm(node_id: i64) {
    let mut armed = ARMED.lock().unwrap();
    armed.insert(node_id, Boot::default());
    ANY_ARMED.store(true, Ordering::Relaxed);
}

pub(super) fn disarm(node_id: i64) {
    let mut armed = ARMED.lock().unwrap();
    armed.remove(&node_id);
    ANY_ARMED.store(!armed.is_empty(), Ordering::Relaxed);
}

/// PTY reader hook. Without an armed node it is one relaxed atomic load.
pub(super) fn on_output(node_id: i64, text: &str) {
    if !ANY_ARMED.load(Ordering::Relaxed) {
        return;
    }
    if let Some(boot) = ARMED.lock().unwrap().get_mut(&node_id) {
        boot.last_output = Some(Instant::now());
        boot.tail.push_str(text);
        if boot.tail.len() > TAIL_BYTES {
            let mut start = boot.tail.len() - TAIL_BYTES;
            while !boot.tail.is_char_boundary(start) {
                start += 1;
            }
            boot.tail.drain(..start);
        }
    }
}

/// Time since the last boot output, and the boot tail without ANSI escapes.
fn observed(node_id: i64) -> (Option<Duration>, String) {
    let armed = ARMED.lock().unwrap();
    let Some(boot) = armed.get(&node_id) else {
        return (None, String::new());
    };
    let tail = crate::session_naming::ANSI_ESCAPE
        .replace_all(&boot.tail, "")
        .into_owned();
    (boot.last_output.map(|at| at.elapsed()), tail)
}

/// Pure delivery rule: type once boot output has gone quiet and the adapter
/// accepts the screen, or at the deadline whatever the screen is doing.
fn ready_to_type(quiet: Option<Duration>, adapter_ready: bool, waited: Duration) -> bool {
    (adapter_ready && quiet.is_some_and(|quiet| quiet >= BOOT_QUIET)) || waited >= BOOT_DEADLINE
}

/// Why a scheduled prompt was not typed.
#[derive(Debug, PartialEq, Eq)]
enum Abandoned {
    ProcessGone,
    UserTyped,
}

/// Wait until the launched TUI is ready for typed input, while the process
/// lives and nobody has typed past `stamp`.
fn wait_for_boot(
    node_id: i64,
    adapter: &dyn crate::agent::provider::AgentProvider,
    stamp: &str,
) -> Result<(), Abandoned> {
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
        let (quiet, tail) = observed(node_id);
        let adapter_ready = quiet.is_some() && adapter.ready_for_initial_prompt(&tail);
        if ready_to_type(quiet, adapter_ready, started.elapsed()) {
            return Ok(());
        }
    }
}

/// Type `prompt` into the node's PTY once its harness has booted, then press
/// Enter. Runs on its own thread; the spawn has already returned.
pub(super) fn deliver_after_boot(
    app: tauri::AppHandle,
    node_id: i64,
    provider: crate::models::Provider,
    prompt: String,
) {
    let stamp = match crate::agent::process::PROCESS_REGISTRY.input_stamp_result(node_id) {
        Ok(stamp) => stamp,
        Err(reason) => {
            disarm(node_id);
            tracing::warn!(
                "startup_prompt({node_id}): terminal input unavailable ({reason:?}); \
                 initial prompt not typed"
            );
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name(format!("startup-prompt-{node_id}"))
        .spawn(move || {
            let booted = wait_for_boot(node_id, provider.adapter(), &stamp);
            disarm(node_id);
            if let Err(reason) = booted {
                tracing::info!("startup_prompt({node_id}): initial prompt not typed: {reason:?}");
                return;
            }
            let result = crate::circuit::delivery::PreparedPrompt::prepare(node_id, &prompt)
                .and_then(|prepared| {
                    crate::circuit::delivery::write_prompt_to_pty_guarded(
                        &crate::agent::process::PROCESS_REGISTRY,
                        &prepared,
                        &app,
                        Some(&stamp),
                    )
                });
            match result {
                Ok(true) => tracing::info!(
                    "startup_prompt({node_id}): initial prompt typed and Enter sent \
                     (submission is not observed)"
                ),
                Ok(false) => tracing::info!(
                    "startup_prompt({node_id}): terminal input changed; initial prompt left to the user"
                ),
                Err(error) => {
                    tracing::warn!(
                        "startup_prompt({node_id}): initial prompt not submitted: {error}"
                    );
                    crate::commands::attention::mark_attention(node_id, &app);
                }
            }
        });
    if let Err(error) = spawned {
        disarm(node_id);
        tracing::warn!("startup_prompt({node_id}): could not start delivery thread: {error}");
    }
}

#[cfg(test)]
#[path = "startup_prompt_tests.rs"]
mod tests;
