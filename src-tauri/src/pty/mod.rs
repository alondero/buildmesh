pub mod batch;
pub mod lifecycle;
mod registry;
pub mod sink;
#[cfg(all(test, windows))]
mod conpty_tests;
pub use registry::PtyRegistry;

use portable_pty::CommandBuilder;

/// Strip inherited Git environment variables from a `CommandBuilder` so a spawned
/// process can't accidentally operate on the parent repository or another worktree.
/// Used by agent spawn and build/run spawn paths.
pub fn strip_git_env_vars(cmd: &mut CommandBuilder) {
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd.env_remove("GIT_INDEX_FILE");
    cmd.env_remove("GIT_OBJECT_DIRECTORY");
    cmd.env_remove("GIT_COMMON_DIR");
}

/// Parent processes launched from CI, Grok, or a pipe often carry
/// `TERM=dumb`, `NO_COLOR=1`, and `FORCE_COLOR=0`. Agent TUIs inherit
/// that and render without colour. A ConPTY / xterm.js child is a real
/// terminal, so give it a colour-capable TERM and drop the disable flags.
pub fn apply_interactive_tty_env(cmd: &mut CommandBuilder) {
    cmd.env_remove("NO_COLOR");
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("FORCE_COLOR", "3");
}
