//! OS process sandboxing for agent PTY nodes (GH #498).
//!
//! On Windows, agent processes can be confined to a restricted
//! [`AppContainer`](appcontainer) so a rogue or prompt-injected agent cannot
//! read host files outside its worktree, write the registry, or run arbitrary
//! system tools. The per-mesh `use_sandbox` flag (Phase 1) opts in; this module
//! is the execution side.
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

#[cfg(target_os = "windows")]
pub mod appcontainer;

#[cfg(target_os = "windows")]
pub mod conpty;

#[cfg(target_os = "windows")]
pub mod acl;

#[cfg(target_os = "windows")]
pub mod spawn;

/// The single policy seam: should this spawn be sandboxed?
///
/// Sandboxing is Windows-only for now (macOS Seatbelt is the #497 sibling) and
/// strictly opt-in via the mesh's `use_sandbox` flag. Keeping the decision in
/// one function means the per-node override (a later slice) extends exactly one
/// call site rather than scattering `cfg!` checks through the spawn path.
pub fn sandbox_enabled(mesh_use_sandbox: bool) -> bool {
    cfg!(target_os = "windows") && mesh_use_sandbox
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_mesh_opts_out() {
        assert!(!sandbox_enabled(false));
    }

    #[test]
    fn follows_platform_when_mesh_opts_in() {
        // On Windows the opt-in is honoured; elsewhere there is no sandbox
        // backend yet, so the seam stays closed regardless of the flag.
        assert_eq!(sandbox_enabled(true), cfg!(target_os = "windows"));
    }
}
