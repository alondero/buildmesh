//! Job-Object containment test (close-node dev-server orphan, issue follow-up to #243).
//!
//! Closing an agent node must kill *everything* the agent spawned, or a survivor
//! keeps the worktree directory pinned and the deferred removal fails forever.
//! The hard case is a process the agent detached — e.g. Claude Code launching a
//! dev server that outlives its launcher. Such a process becomes an orphan: its
//! parent has exited, so `taskkill /T`'s live parent→child tree walk can never
//! reach it from the agent's root PID.
//!
//! `JobHandle` solves this by *containing* the agent's process tree in a Windows
//! Job Object: every process spawned after assignment stays in the job however it
//! detaches, and `terminate()` kills them all at once — independent of parentage.
//!
//! This test proves that property end-to-end against the real OS: a parent that
//! spawns a detached grandchild and then exits, leaving the grandchild orphaned
//! but contained, which `terminate()` then kills.

#![cfg(windows)]

use std::process::Command;
use std::time::{Duration, Instant};

use buildmesh_lib::process_util::JobHandle;

/// Minimal liveness probe via `kernel32!OpenProcess` + `GetExitCodeProcess`, so
/// the test doesn't depend on parsing `tasklist`'s locale-dependent output.
mod sys {
    use std::os::raw::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(handle: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    pub fn alive(pid: u32) -> bool {
        // SAFETY: handle (if non-null) is closed before returning; the out param
        // is a stack u32 written only on success.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE
        }
    }

    pub fn kill(pid: u32) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }
}

fn wait_until<F: Fn() -> bool>(timeout: Duration, cond: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

#[test]
fn job_terminate_kills_orphaned_detached_descendant() {
    let pid_file = std::env::temp_dir().join(format!("buildmesh_job_test_{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pid_file);

    // The parent sleeps briefly so we win the race to assign it to the job before
    // it spawns the grandchild, then launches a detached long-lived `ping`,
    // records its PID, and exits — orphaning the grandchild.
    let script = format!(
        "Start-Sleep -Milliseconds 700; \
         $g = Start-Process -FilePath ping.exe -ArgumentList '-n','120','127.0.0.1' -PassThru -WindowStyle Hidden; \
         Set-Content -Path '{}' -Value $g.Id -Encoding ascii",
        pid_file.to_string_lossy().replace('\'', "''")
    );

    let mut parent = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .spawn()
        .expect("spawn powershell parent");
    let parent_pid = parent.id();

    // Contain the parent before it spawns the grandchild (it sleeps 700ms first).
    let job = JobHandle::contain(parent_pid).expect("Job Object should contain the parent process");

    // Wait for the grandchild PID to be recorded. Poll the parse, not just
    // `exists()`: PowerShell's `Set-Content` briefly holds the file open, so an
    // eager read races it (sharing violation) or sees an empty file.
    let read_pid = || {
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
    };
    assert!(
        wait_until(Duration::from_secs(15), || read_pid().is_some()),
        "parent never recorded the grandchild PID"
    );
    let grandchild_pid = read_pid().expect("grandchild pid recorded");

    // Cleanup guard: whatever the assertions do, never leak the ping process.
    struct KillGuard(u32, std::path::PathBuf);
    impl Drop for KillGuard {
        fn drop(&mut self) {
            sys::kill(self.0);
            let _ = std::fs::remove_file(&self.1);
        }
    }
    let _guard = KillGuard(grandchild_pid, pid_file.clone());

    // The parent exits right after writing the PID, orphaning the grandchild.
    parent.wait().expect("parent exits");
    assert!(
        wait_until(Duration::from_secs(5), || !sys::alive(parent_pid)),
        "parent should have exited"
    );

    // The orphan is alive and — crucially — unreachable from the parent's PID,
    // so today's `taskkill /T` from the agent root would miss it entirely.
    assert!(
        sys::alive(grandchild_pid),
        "detached grandchild should be running after the parent exits"
    );

    // Killing the job reaches the orphan regardless of the broken parent link.
    job.terminate();

    assert!(
        wait_until(Duration::from_secs(5), || !sys::alive(grandchild_pid)),
        "terminating the job must kill the orphaned, contained grandchild"
    );
}
