//! Utility for spawning background processes without a visible console window on Windows.

use std::process::Command;

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
