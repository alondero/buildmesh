pub mod batch;
mod registry;
pub mod sink;
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
