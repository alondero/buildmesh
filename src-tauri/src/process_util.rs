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
