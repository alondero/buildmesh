//! Utility for spawning background processes without a visible console window on Windows.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Create a `Command` that won't flash a console window on Windows.
///
/// On Windows GUI apps (windows_subsystem = "windows"), spawning a console process
/// allocates a new visible console unless CREATE_NO_WINDOW is passed.
pub fn command_no_window(program: &str) -> Command {
    let cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(target_os = "windows"))]
    {
        cmd
    }
}

/// Run a `Command` to completion with a wall-clock timeout. If the child
/// hasn't exited by `timeout`, it is killed and reaped, and the function
/// returns an `Err`.
///
/// This exists for the issue #762 follow-up: `git fetch` / `git pull`
/// shell-outs can wedge indefinitely on a half-open connection (laptop
/// sleep/resume, dropped Wi-Fi). Even on the blocking pool (which has
/// ~512 threads, so pool exhaustion is unlikely), a wedged subprocess
/// still leaks a thread forever. The bound here is the resource-leak
/// backstop — the request-side [`crate::services::github`] HTTP client
/// also has its own `.timeout()` for the actual network round-trip, but
/// a stuck `git fetch` shell-out bypasses that.
///
/// **Why polling instead of `tokio::time::timeout`:** the caller (e.g.
/// [`crate::git::sync::do_sync`]) runs on the blocking pool under a
/// per-Mesh mutex, NOT on a tokio worker, so async timeouts aren't
/// available. Polling `try_wait` with a 100ms tick is precise enough for
/// the minute-scale timeouts we use here (a 30s tolerance is well within
/// the 100ms tick) and avoids pulling in an extra dependency.
///
/// **Pipe-buffer caveat:** the child must have its stdout/stderr piped
/// (caller's responsibility — the helper pipes them automatically). If
/// either pipe fills (~64KB on Linux) the child blocks on write and we
/// never see it exit; polling kills it on deadline anyway, but the exit
/// is from the timeout rather than the natural completion. `git fetch`
/// / `git pull` output is well under the pipe limit so this is fine in
/// practice.
pub fn run_command_with_timeout(
    mut cmd: Command,
    op_name: &str,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {op_name}: {e}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break, // exited — fall through to wait_with_output
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Kill the process tree on timeout, not just the direct
                    // child. `git fetch` over HTTPS routinely spawns
                    // `git-credential-manager` (or `ssh-askpass` over SSH)
                    // as a separate process — `child.kill()` only signals
                    // the immediate child, leaving the helper alive and
                    // confusing the user's next spawn (ghost auth dialogs).
                    // `kill_process_tree` walks the tree on Windows via
                    // `taskkill /F /T`; on Unix it's a no-op because
                    // closing the PTY master already `SIGHUP`s the
                    // foreground process group.
                    kill_process_tree(child.id());
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{op_name} timed out after {}s",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(format!("{op_name} try_wait failed: {e}"));
            }
        }
    }

    child
        .wait_with_output()
        .map_err(|e| format!("{op_name} wait_with_output failed: {e}"))
}

/// Forcefully terminate a process and all of its descendants.
///
/// On Windows, `TerminateProcess` (what portable-pty's `Child::kill` calls)
/// only kills the targeted process. The PTY child is a shell, so the agent CLI
/// it spawns survives and keeps its working directory pinned — which blocks
/// removing the agent's worktree on close. `taskkill /T` walks the whole tree.
///
/// On Unix this is a no-op: closing the PTY master already `SIGHUP`s the
/// foreground process group, and a process's CWD never blocks `rmdir`.
#[cfg(target_os = "windows")]
pub fn kill_process_tree(pid: u32) {
    let _ = command_no_window("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .output();
}

#[cfg(not(target_os = "windows"))]
pub fn kill_process_tree(_pid: u32) {}

/// A Windows Job Object that contains an agent's entire process tree.
///
/// `taskkill /T` walks the *live* parent→child links at the instant it runs, so
/// it can never reach a descendant whose parent has already exited — e.g. a dev
/// server the agent launched then orphaned (Claude Code backgrounding `npm run
/// dev`). That orphan keeps the worktree's directory pinned as its CWD, so the
/// deferred worktree removal on close fails indefinitely.
///
/// A Job Object is a *containment boundary*, not a tree walk: every process the
/// contained process spawns after assignment joins the job and cannot escape it,
/// however it detaches. [`terminate`](JobHandle::terminate) then kills the whole
/// set at once, independent of parentage. The job is created kill-on-close, so if
/// buildmesh exits without an orderly shutdown the tree dies with it rather than
/// orphaning a dev server.
///
/// On non-Windows this is an inert handle: closing the PTY master already
/// `SIGHUP`s the foreground process group and a CWD never blocks `rmdir`.
pub struct JobHandle {
    #[cfg(target_os = "windows")]
    handle: isize,
}

#[cfg(target_os = "windows")]
mod job_ffi {
    use std::os::raw::c_void;

    pub type Handle = *mut c_void;

    #[repr(C)]
    pub struct BasicLimitInformation {
        pub per_process_user_time_limit: i64,
        pub per_job_user_time_limit: i64,
        pub limit_flags: u32,
        pub minimum_working_set_size: usize,
        pub maximum_working_set_size: usize,
        pub active_process_limit: u32,
        pub affinity: usize,
        pub priority_class: u32,
        pub scheduling_class: u32,
    }

    #[repr(C)]
    pub struct IoCounters {
        pub read_operation_count: u64,
        pub write_operation_count: u64,
        pub other_operation_count: u64,
        pub read_transfer_count: u64,
        pub write_transfer_count: u64,
        pub other_transfer_count: u64,
    }

    #[repr(C)]
    pub struct ExtendedLimitInformation {
        pub basic_limit_information: BasicLimitInformation,
        pub io_info: IoCounters,
        pub process_memory_limit: usize,
        pub job_memory_limit: usize,
        pub peak_process_memory_used: usize,
        pub peak_job_memory_used: usize,
    }

    pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    pub const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    pub const PROCESS_TERMINATE: u32 = 0x0001;
    pub const PROCESS_SET_QUOTA: u32 = 0x0100;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> Handle;
        pub fn SetInformationJobObject(
            job: Handle,
            class: i32,
            info: *const c_void,
            len: u32,
        ) -> i32;
        pub fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        pub fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        pub fn CloseHandle(handle: Handle) -> i32;
    }
}

impl JobHandle {
    /// Create a kill-on-close job and assign the process `pid` to it. Every
    /// process `pid` spawns *after* this call joins the job automatically.
    ///
    /// Returns `None` when there's nothing to do (non-Windows) or any OS call
    /// fails (e.g. the process exited, or it's already in an incompatible job);
    /// callers fall back to [`kill_process_tree`].
    ///
    /// Assign as soon as possible after spawn: a child the contained process
    /// spawned *before* assignment isn't pulled in. In the agent pipeline the
    /// contained process is the PTY shell and the assign happens within
    /// microseconds of spawn — long before the shell launches the agent CLI, let
    /// alone any dev server — so the whole tree is covered in practice.
    #[cfg(target_os = "windows")]
    pub fn contain(pid: u32) -> Option<JobHandle> {
        use job_ffi::*;
        use std::ptr;

        // SAFETY: each handle returned is checked for null and freed on every
        // exit path; the limit struct is zero-initialised plain-old-data passed
        // with its exact size.
        unsafe {
            let job = CreateJobObjectW(ptr::null_mut(), ptr::null());
            if job.is_null() {
                return None;
            }

            let mut info: ExtendedLimitInformation = std::mem::zeroed();
            info.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                &info as *const _ as *const std::os::raw::c_void,
                std::mem::size_of::<ExtendedLimitInformation>() as u32,
            ) == 0
            {
                CloseHandle(job);
                return None;
            }

            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                CloseHandle(job);
                return None;
            }
            let assigned = AssignProcessToJobObject(job, process);
            CloseHandle(process);
            if assigned == 0 {
                CloseHandle(job);
                return None;
            }

            Some(JobHandle { handle: job as isize })
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn contain(_pid: u32) -> Option<JobHandle> {
        None
    }

    /// Terminate every process currently in the job.
    #[cfg(target_os = "windows")]
    pub fn terminate(&self) {
        // SAFETY: `handle` is a live job handle owned until `Drop`.
        unsafe {
            job_ffi::TerminateJobObject(self.handle as job_ffi::Handle, 1);
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn terminate(&self) {}
}

#[cfg(target_os = "windows")]
impl Drop for JobHandle {
    fn drop(&mut self) {
        // Closing the last handle to a kill-on-close job terminates anything
        // still in it — the backstop for an unclean buildmesh exit.
        // SAFETY: `handle` was created by `CreateJobObjectW` and closed once.
        unsafe {
            job_ffi::CloseHandle(self.handle as job_ffi::Handle);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for [`run_command_with_timeout`] (issue #762).
    //!
    //! The pre-#762 shell-outs (`git fetch`, `git pull --ff-only`, `gh auth
    //! token`) had no wall-clock bound — a wedged subprocess leaked a
    //! blocking-pool thread indefinitely. The fix wraps each shell-out in
    //! `run_command_with_timeout` which polls `try_wait` and `kill`s the
    //! child on deadline. These tests guard the bound itself: a hang
    //! becomes a deterministic `Err(_)` rather than a CI freeze.
    //!
    //! **Cross-platform:** the tests use the OS-native "sleep then exit"
    //! helper (`timeout` on Windows, `sleep` on Unix) so they don't depend
    //! on POSIX-only tools. Each test runs the helper with a short timeout
    //! and asserts the `Err` arrives within (timeout + slack) so a
    //! regression that turns the bound into a no-op (e.g. someone
    //! accidentally removes the `kill()` call) fails the test instead of
    //! hanging the test runner.

    use super::*;
    use std::time::{Duration, Instant};

    /// A command that hangs for ~N seconds before exiting.
    ///
    /// On Windows, `ping -n N 127.0.0.1` sends one echo per second
    /// (so `-n 30` takes ~29 seconds; we never wait the full duration
    /// because the test's timeout fires first). On Unix, `sleep N` is
    /// the obvious choice.
    ///
    /// `timeout /T /NOBREAK` was tried first but it exits immediately
    /// on Windows when stdin isn't redirected from a console — the
    /// "Input redirection is not supported" error short-circuits
    /// before the timer starts. `ping` has no such constraint.
    fn hang_cmd(secs: u64) -> Command {
        #[cfg(target_os = "windows")]
        {
            let mut c = command_no_window("ping");
            // `-n N` issues N echo requests at 1-second intervals.
            c.args(["-n", &secs.to_string(), "127.0.0.1"]);
            c
        }
        #[cfg(not(target_os = "windows"))]
        {
            let mut c = command_no_window("sleep");
            c.arg(secs.to_string());
            c
        }
    }

    #[test]
    fn run_command_with_timeout_kills_long_running_child() {
        // Spawn a child that would happily run for 30 seconds. The 1s
        // timeout must fire and the helper must return Err. Without the
        // bound, this test would hang the suite for ~30s.
        let start = Instant::now();
        let result = run_command_with_timeout(hang_cmd(30), "test hang", Duration::from_secs(1));
        let elapsed = start.elapsed();

        assert!(result.is_err(), "expected timeout Err, got {result:?}");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("timed out") && msg.contains("test hang"),
            "error must identify the operation and the timeout, got: {msg}"
        );
        // Bound verification: the kill fires within the deadline + a small
        // slack (we sleep 100ms between polls). 2s is generous enough to
        // absorb CI noise but tight enough that a "no bound" regression
        // (e.g. someone replaces `kill()` with a no-op) fails this.
        assert!(
            elapsed < Duration::from_secs(2),
            "expected kill within ~1s, took {elapsed:?}"
        );
    }

    #[test]
    fn run_command_with_timeout_returns_output_when_child_exits_early() {
        // A child that exits before the deadline must produce a normal
        // `Ok(Output)` — the helper is NOT a kill-on-everything wrapper,
        // just a deadline guard.
        #[cfg(not(target_os = "windows"))]
        let cmd = {
            let mut c = command_no_window("sleep");
            c.arg("0");
            c
        };
        #[cfg(target_os = "windows")]
        let cmd = {
            // `ping -n 1` takes ~1s; using `cmd /c exit 0` for a true
            // instant exit (no console noise).
            let mut c = command_no_window("cmd");
            c.args(["/c", "exit", "0"]);
            c
        };
        let result = run_command_with_timeout(cmd, "test quick exit", Duration::from_secs(5));
        assert!(
            result.is_ok(),
            "early-exit child should return Ok, got {result:?}"
        );
    }

    #[test]
    fn run_command_with_timeout_surfaces_spawn_failure() {
        // A non-existent program must return Err immediately, NOT block on
        // the timeout. This guards against a regression that wraps the
        // timeout around the spawn too.
        let cmd = command_no_window("this-binary-definitely-does-not-exist-1234567890");
        let start = Instant::now();
        let result = run_command_with_timeout(cmd, "spawn-fail test", Duration::from_secs(5));
        let elapsed = start.elapsed();

        assert!(result.is_err(), "missing binary should be Err, got {result:?}");
        assert!(
            elapsed < Duration::from_secs(1),
            "spawn failure must short-circuit, took {elapsed:?}"
        );
    }
}
