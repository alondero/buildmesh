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
