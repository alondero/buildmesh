//! Cancelling a verification must signal only that shell's process group.
//!
//! Ubuntu 24.04 procps `kill -KILL -<pid>` (no `--`) keeps the first digit of a
//! negative id. A pid starting with 1 becomes -1, and that signal reaches every
//! process the user can kill. On a GitHub-hosted runner, that includes the runner agent
//! (issue #2103).

use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

#[test]
fn cancelling_a_verification_does_not_signal_an_unrelated_process_group() {
    let _env = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_string_lossy().into_owned();
    let mut bystander = crate::process_util::command_no_window("sleep");
    bystander
        .arg("30")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut bystander = bystander
        .spawn()
        .expect("spawn a bystander in its own process group");
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let (result_tx, result_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        result_tx
            .send(super::run_verification_command(
                &path,
                "printf started > ready; while :; do sleep 1; done",
                &token,
                &AtomicBool::new(false),
            ))
            .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.path().join("ready").exists() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    let started = directory.path().join("ready").exists();
    cancelled.store(true, Ordering::Release);
    assert!(!result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("cancelled verification must exit"));
    worker.join().unwrap();
    assert!(
        started,
        "real shell command must reach its running state before cancellation"
    );
    assert!(
        bystander.try_wait().unwrap().is_none(),
        "cancelling one verification must not signal an unrelated process group"
    );
    crate::process_util::kill_process_group(bystander.id());
    let _ = bystander.wait();
}
