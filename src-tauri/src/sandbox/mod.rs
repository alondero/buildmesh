//! OS process sandboxing for agent PTY nodes (GH #498).
//!
//! On Windows, agent processes can be confined to a restricted
//! [`AppContainer`](appcontainer) so a rogue or prompt-injected agent cannot
//! read host files outside its worktree, write the registry, or run arbitrary
//! system tools. The per-mesh `sandbox` flag (Phase 1, GH #498/#497) opts in;
//! this module is the execution side.
//!
//! Layout mirrors the macOS Seatbelt sibling (#497):
//!   * [`sandbox_enabled`] — the single policy seam that decides, for one
//!     spawn, whether to take the sandboxed path. Keep this the *only* place
//!     that makes the call so the rule stays in one spot.
//!   * [`appcontainer`] — Windows AppContainer profile + security capabilities,
//!     inline `extern "system"` FFI (no `windows-sys`/`winapi` dep), mirroring
//!     `process_util::JobHandle`.
//!
//! The native ConPTY spawn that *consumes* an [`appcontainer::AppContainerProfile`]
//! lands in the follow-up slice; this module is its foundation.
//!
//! ## Experimental: developer-gated (#2034)
//!
//! The sandbox is **not** a shipped feature. Confinement is incomplete
//! (Windows denies no filesystem access — #542; Linux has no backend — #828;
//! WSL launches are not contained), so the whole feature sits behind
//! [`DEV_SANDBOX_ENV`] until those gaps close. Shipped builds ignore the
//! persisted `meshes.sandbox` column entirely, so a flag left on by an earlier
//! dev build cannot silently start confining a released user.

#[cfg(target_os = "windows")]
pub mod appcontainer;

#[cfg(target_os = "windows")]
pub mod conpty;

#[cfg(target_os = "windows")]
pub mod restricted_token;

#[cfg(target_os = "windows")]
pub mod acl;

#[cfg(target_os = "windows")]
pub mod spawn;

/// Environment variable a developer sets to opt into the experimental agent
/// sandbox. Unset in shipped builds — the same shape as
/// `BUILDMESH_DISABLE_CRASH_WATCHDOG`, so the gate is discoverable from the
/// codebase rather than invented here.
pub const DEV_SANDBOX_ENV: &str = "BUILDMESH_SANDBOX";

/// Has the developer opted into the experimental sandbox for this process?
///
/// This is the *authority* for the whole feature, deliberately placed ahead of
/// the persisted `meshes.sandbox` column rather than beside it. A release must
/// not confine a user who once ticked the toggle in a dev build, so the column
/// alone can never be sufficient: it is only consulted after this gate opens.
/// The UI reads the same predicate through the `sandbox_dev_mode_enabled`
/// command purely to decide whether to *offer* the toggle; hiding it there is
/// convenience, this function is the guarantee.
pub fn dev_sandbox_enabled() -> bool {
    std::env::var(DEV_SANDBOX_ENV).as_deref() == Ok("1")
}

/// Platform-agnostic half of the decision: did this spawn ask for a sandbox,
/// and is the developer gate open?
///
/// macOS containment is assembled in [`crate::agent::spawn_environment::wrap`]
/// rather than in [`sandbox_enabled`], which is why the two need a shared
/// predicate instead of each re-deriving the rule.
pub fn sandbox_requested(mesh_sandbox: bool) -> bool {
    dev_sandbox_enabled() && mesh_sandbox
}

/// The single policy seam: should this spawn be sandboxed?
///
/// Windows-only backend (macOS Seatbelt is the #497 sibling, which
/// [`sandbox_requested`] gates separately). Keeping the decision in one
/// function means the per-node override (a later slice) extends exactly one
/// call site rather than scattering `cfg!` checks through the spawn path.
pub fn sandbox_enabled(mesh_sandbox: bool) -> bool {
    cfg!(target_os = "windows") && sandbox_requested(mesh_sandbox)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::DEV_SANDBOX_ENV;

    /// Runs `body` with the developer gate set to `value`, restoring the
    /// previous environment even if `body` panics.
    ///
    /// The process environment is shared by every thread in the test binary, so
    /// a test that sets the gate and returns — or panics — must not leave
    /// `BUILDMESH_SANDBOX=1` behind for whichever test runs next. `with_env_vars`
    /// wraps the body in `catch_unwind` and restores before re-panicking, and
    /// `ENV_LOCK` is held across that whole window, so restoration cannot race a
    /// sibling test. The lock is released only after `with_env_vars` returns.
    pub(crate) fn with_dev_gate_result<T>(
        value: Option<&str>,
        body: impl FnOnce() -> T,
    ) -> T {
        let _env = crate::env::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::env::with_env_vars(
            &[(DEV_SANDBOX_ENV, value.map(std::ffi::OsStr::new))],
            body,
        )
    }

    /// Unit variant of [`with_dev_gate_result`].
    pub(crate) fn with_dev_gate(value: Option<&str>, body: impl FnOnce()) {
        with_dev_gate_result(value, body);
    }

    /// Removes a directory when the guard drops, so a failing assertion cannot
    /// leave a scratch path behind to corrupt the next run.
    pub(crate) struct RemoveDirOnDrop(std::path::PathBuf);

    impl RemoveDirOnDrop {
        pub(crate) fn new(path: impl Into<std::path::PathBuf>) -> Self {
            Self(path.into())
        }
    }

    impl Drop for RemoveDirOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Blocks a file write at `path` by creating a directory there, returning a
    /// guard that removes it again.
    ///
    /// Portable (no chmod, no root), which is what lets the fail-closed
    /// contract be asserted on every CI host rather than only where Seatbelt
    /// exists.
    pub(crate) fn block_writes_at(path: impl Into<std::path::PathBuf>) -> RemoveDirOnDrop {
        let path = path.into();
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("a directory at {} must be creatable: {e}", path.display()));
        RemoveDirOnDrop::new(path)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::with_dev_gate;
    use super::*;

    #[test]
    fn disabled_when_mesh_opts_out() {
        with_dev_gate(Some("1"), || assert!(!sandbox_enabled(false)));
    }

    /// The release guarantee (#2034): a persisted `sandbox = 1` row must not
    /// confine anything unless a developer explicitly opened the gate. This is
    /// the regression guard for the flag outliving the build that set it.
    #[test]
    fn persisted_flag_is_inert_without_the_developer_gate() {
        with_dev_gate(None, || {
            assert!(!dev_sandbox_enabled());
            assert!(!sandbox_requested(true));
            assert!(!sandbox_enabled(true));
        });
    }

    #[test]
    fn a_non_one_value_does_not_open_the_gate() {
        // `BUILDMESH_SANDBOX=true`, `=0`, or an empty string are all someone
        // guessing the variable name; none of them opt a release build in.
        for value in ["true", "0", "", "yes"] {
            with_dev_gate(Some(value), || {
                assert!(!dev_sandbox_enabled(), "{value:?} must not open the gate");
                assert!(!sandbox_requested(true), "{value:?} must not request a sandbox");
            });
        }
    }

    #[test]
    fn follows_platform_when_mesh_opts_in_and_the_gate_is_open() {
        // With the gate open the opt-in is honoured on Windows; elsewhere
        // there is no restricted-token backend, so that seam stays closed.
        with_dev_gate(Some("1"), || {
            assert!(sandbox_requested(true));
            assert_eq!(sandbox_enabled(true), cfg!(target_os = "windows"));
        });
    }

    /// macOS builds its Seatbelt command in `spawn_environment::wrap`, which
    /// gates on `sandbox_requested` rather than `sandbox_enabled`. That
    /// predicate must therefore be platform-agnostic — a `cfg!` here would
    /// silently re-open the macOS fail-open.
    #[test]
    fn request_predicate_is_platform_agnostic() {
        with_dev_gate(Some("1"), || {
            assert!(sandbox_requested(true));
            assert!(!sandbox_requested(false));
        });
    }
}