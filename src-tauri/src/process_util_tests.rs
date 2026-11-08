//! Tests for the process-tree kill seam in `process_util`.
//!
//! Split out of `src-tauri/src/process_util.rs` by issue #2220. The conventions this
//! layout depends on — why every `#[path]` module needs an entry in
//! `RUST_PATH_INCLUDES` — are documented once in
//! `docs/development/rust-conventions.md`, under *Rust unit tests live in sibling
//! `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test process_util::process_util_tests

use super::*;

/// The Job Object is the containment boundary, and it must be terminated even
/// when the caller knows no pid. A shell that already exited leaves
/// `process_id()` empty while the descendants it spawned keep running, and
/// those descendants are the processes that pin a worktree's directory - so
/// gating the termination on a pid let exactly the processes the job exists
/// to contain survive.
///
/// The test contains a real process and terminates the job with no pid at
/// all: with the termination gated on the pid, nothing kills the process and
/// the assertion fails.
#[cfg(windows)]
#[test]
fn the_job_is_terminated_even_when_no_pid_is_known() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let mut child = command_no_window("cmd")
        .args(["/d", "/q", "/c", "ping -n 30 127.0.0.1 > nul"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a process to contain");
    let job = JobHandle::contain(child.id()).expect("contain the spawned process");
    assert!(
        job.active_process_count().unwrap_or(0) >= 1,
        "the spawned process must be inside the job"
    );

    // No pid at all: the job alone has to end the process.
    kill_process_tree_contained(Some(&job), None);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut status = child.try_wait().unwrap();
    while status.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        status = child.try_wait().unwrap();
    }
    assert!(
        status.is_some(),
        "the contained process must be terminated by the job alone"
    );
    let _ = child.wait();
}
