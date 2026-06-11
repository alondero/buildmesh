//! Real-PTY integration test for the agent spawn pipeline (issue #154).
//!
//! The unit tests mock `invoke` or re-implement the arg-vector logic locally, so
//! nothing in CI exercises a real spawn. This test drives the actual production
//! seams end-to-end:
//!   * `spawn_environment::wrap` builds the OS-wrapped `CommandBuilder`
//!     (PowerShell `-EncodedCommand`, `cmd.exe /c`, or a direct spawn)
//!   * `open_pty_pair` + `spawn_child` start a real child under a real PTY
//!   * the child is tracked in the real global `PROCESS_REGISTRY`
//!   * output is drained via `pump_pty_output` — the same read loop the
//!     production reader thread uses
//!
//! Asserts that the child's stdout reaches us through the PTY ("hello"), that the
//! child exits cleanly, that the reader signals it is no longer alive (production
//! maps this to `SessionStatus::Idle`), and that the registry entry is cleaned up.
//!
//! The Windows shell-recipe coverage is the regression surface for the spawn
//! fixes called out in the issue: the PowerShell BOM fix (30380d9) and the
//! `cmd.exe /c` batch wrapping (ee6472f).

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use buildmesh_lib::agent::process::{AgentProcess, PROCESS_REGISTRY};
use buildmesh_lib::agent::provider::{SpawnRecipe, WindowsShell};
use buildmesh_lib::agent::spawn::{open_pty_pair, pump_pty_output, spawn_child};
use buildmesh_lib::agent::spawn_environment;
use buildmesh_lib::models::EnvType;

struct ChunkedReader {
    chunks: VecDeque<Vec<u8>>,
}

impl ChunkedReader {
    fn new(chunks: Vec<Vec<u8>>) -> Self {
        Self { chunks: chunks.into() }
    }
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(chunk) = self.chunks.pop_front() else {
            return Ok(0);
        };
        assert!(chunk.len() <= buf.len(), "test chunk must fit read buffer");
        buf[..chunk.len()].copy_from_slice(&chunk);
        Ok(chunk.len())
    }
}

/// Spawn `recipe` under a real PTY, drain its output through the production read
/// loop, wait for the child to exit and `expected` to arrive, then close the
/// PTY and clean up the registry. Returns the captured output and whether the
/// child exited successfully.
///
/// `expected` is the output marker to wait for after the child exits. ConPTY on
/// current Windows builds (observed 10.0.28120, 2026-06) no longer EOFs the
/// master read pipe when the child exits — conhost holds it open until the
/// pseudoconsole itself is closed — so the reader thread only finishes once we
/// drop the master. We therefore drain until `expected` shows up (bounded),
/// close the PTY the way `kill_agent` does (dropping the registry entry), and
/// only then join the reader.
fn run_recipe_through_pty(session_id: i64, recipe: SpawnRecipe, expected: &str) -> (String, bool) {
    // `wrap` applies the OS-axis shell wrapping. `EnvType::Windows` keeps us on
    // the `windows_shell` branch on Windows; on Unix that branch falls through
    // to a direct spawn of the binary, which is what we want for `/bin/sh`.
    let cwd = std::env::current_dir().unwrap();
    let cmd = spawn_environment::wrap(recipe, EnvType::Windows, &cwd.to_string_lossy(), session_id);

    // Stage markers (visible with --nocapture) so a hang in the ConPTY stack
    // points at the exact call rather than reading as a silent stall.
    eprintln!("[pty-test {session_id}] opening pty pair");
    let pair = open_pty_pair(24, 80).expect("open pty pair");
    eprintln!("[pty-test {session_id}] spawning child");
    let child = spawn_child(&pair, cmd).expect("spawn child");
    eprintln!("[pty-test {session_id}] child spawned, wiring io");

    let reader = pair.master.try_clone_reader().expect("clone reader");
    let writer = pair.master.take_writer().expect("take writer");
    let master = pair.master;
    // Drop the slave handle so the master read sees EOF once the child exits.
    drop(pair.slave);

    let reader_alive = Arc::new(AtomicBool::new(true));
    PROCESS_REGISTRY.insert(
        session_id,
        AgentProcess {
            child: Arc::new(Mutex::new(child)),
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(master)),
            reader_alive: reader_alive.clone(),
            job: None,
        },
    );

    // Drain output on a background thread using the production read loop.
    let collected = Arc::new(Mutex::new(Vec::new()));
    let collected_w = collected.clone();
    let reader_alive_thread = reader_alive.clone();
    let reader_handle = std::thread::spawn(move || {
        pump_pty_output(reader, |chunk| {
            collected_w.lock().unwrap().extend_from_slice(chunk);
        });
        // Mirror the production reader thread: liveness ends when the PTY closes.
        reader_alive_thread.store(false, Ordering::SeqCst);
    });

    // Wait for the child to exit. A trivial echo command exits promptly; if it
    // ever hangs, the test stalls loudly rather than passing silently.
    let entry = PROCESS_REGISTRY.get(&session_id).expect("entry registered");
    eprintln!("[pty-test {session_id}] waiting for child exit");
    let exit_ok = {
        let mut child = entry.child.lock().unwrap();
        child.wait().map(|s| s.success()).unwrap_or(false)
    };
    drop(entry);

    // The child has exited, but conhost may still be flushing its output
    // through the pipe. Wait (bounded) for the expected marker before tearing
    // the PTY down, so closing the pseudoconsole can't race the flush. On
    // timeout we fall through — the caller's assertion then reports whatever
    // output actually arrived.
    eprintln!("[pty-test {session_id}] child exited (ok={exit_ok}), draining output");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !String::from_utf8_lossy(&collected.lock().unwrap()).contains(expected)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Close the PTY the same way `kill_agent` does — dropping the registry
    // entry drops the master, which closes the pseudoconsole and EOFs the
    // reader. (Pre-2026-06 Windows builds EOF'd on child exit by themselves;
    // current conhost only releases the pipe when the pseudoconsole closes.)
    PROCESS_REGISTRY.remove(&session_id);
    assert!(
        !PROCESS_REGISTRY.contains(&session_id),
        "PROCESS_REGISTRY entry for session {session_id} should be cleaned up"
    );

    eprintln!("[pty-test {session_id}] pty closed, joining reader");
    reader_handle.join().expect("reader thread joins");
    assert!(
        !reader_alive.load(Ordering::SeqCst),
        "reader should report not-alive after the PTY closes (production -> SessionStatus::Idle)"
    );

    let out = String::from_utf8_lossy(&collected.lock().unwrap()).into_owned();
    (out, exit_ok)
}

#[test]
fn pump_pty_output_preserves_split_utf8_sequences() {
    let reader = ChunkedReader::new(vec![vec![0xe2, 0x96], vec![0x88]]);
    let mut collected = Vec::new();

    pump_pty_output(Box::new(reader), |chunk| {
        collected.extend_from_slice(chunk);
    });

    assert_eq!(collected, "█".as_bytes());
}

#[cfg(unix)]
#[test]
fn unix_sh_echo_through_real_pty() {
    let recipe = SpawnRecipe {
        binary: "/bin/sh",
        base_args: vec!["-c".into(), "echo hello".into()],
        windows_shell: WindowsShell::Direct,
    };
    let (out, exit_ok) = run_recipe_through_pty(-915_4001, recipe, "hello");
    assert!(out.contains("hello"), "expected 'hello' in PTY output, got: {out:?}");
    assert!(exit_ok, "child should exit cleanly");
}

/// Direct spawn: `cmd.exe /c echo hello`, no shell wrapper.
#[cfg(windows)]
#[test]
fn windows_direct_echo_through_real_pty() {
    let recipe = SpawnRecipe {
        binary: "cmd.exe",
        base_args: vec!["/c".into(), "echo".into(), "hello".into()],
        windows_shell: WindowsShell::Direct,
    };
    let (out, exit_ok) = run_recipe_through_pty(-915_4002, recipe, "hello");
    assert!(out.contains("hello"), "expected 'hello' in PTY output, got: {out:?}");
    assert!(exit_ok, "child should exit cleanly");
}

/// PowerShell `-EncodedCommand` path. Guards the BOM fix (commit 30380d9): a
/// UTF-16LE BOM in the encoded payload makes powershell.exe fail to parse the
/// script, so "hello" never reaches the PTY.
#[cfg(windows)]
#[test]
fn windows_powershell_encoded_echo_through_real_pty() {
    let recipe = SpawnRecipe {
        binary: "cmd.exe",
        base_args: vec!["/c".into(), "echo".into(), "hello".into()],
        windows_shell: WindowsShell::PowerShell,
    };
    let (out, exit_ok) = run_recipe_through_pty(-915_4003, recipe, "hello");
    assert!(
        out.contains("hello"),
        "PowerShell -EncodedCommand spawn produced no 'hello' (BOM regression?): {out:?}"
    );
    assert!(exit_ok, "child should exit cleanly");
}

/// `cmd.exe /c <batch.cmd>` path. Guards the batch-wrapping fix (ee6472f):
/// spawning a `.cmd` directly (without the `cmd.exe /c` layer) fails with
/// error 193 / 0xc0000142 on Windows, so "hello" never reaches the PTY.
#[cfg(windows)]
#[test]
fn windows_cmd_batch_echo_through_real_pty() {
    let batch = std::env::temp_dir().join(format!("buildmesh_test_{}.cmd", std::process::id()));
    std::fs::write(&batch, "@echo hello\r\n").expect("write batch file");
    // SpawnRecipe.binary is &'static str; leak the temp path for the test's lifetime.
    let binary: &'static str = Box::leak(batch.to_string_lossy().into_owned().into_boxed_str());
    let recipe = SpawnRecipe {
        binary,
        base_args: vec![],
        windows_shell: WindowsShell::Cmd,
    };
    let (out, exit_ok) = run_recipe_through_pty(-915_4004, recipe, "hello");
    std::fs::remove_file(&batch).ok();
    assert!(
        out.contains("hello"),
        "cmd.exe /c batch spawn produced no 'hello' (npm-batch regression?): {out:?}"
    );
    assert!(exit_ok, "child should exit cleanly");
}
