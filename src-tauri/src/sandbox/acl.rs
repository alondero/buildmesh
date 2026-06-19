//! Grant an AppContainer SID access to a directory (GH #498).
//!
//! A sandboxed agent is denied everything not explicitly granted to its
//! container SID, so before spawning we grant the node's worktree (and the
//! user-profile toolchain dirs that lack the `ALL APPLICATION PACKAGES` ACE) to
//! that SID. The grant is reversed on close so profiles/ACEs don't accumulate.
//!
//! Implemented via `icacls` (same mechanism the GH#498 Phase 0 spike used):
//! granting a user-owned directory needs no elevation. System dirs
//! (`C:\Program Files\…`, msys2) would need admin — the spawn path avoids them
//! by curating PATH toward `C:\Program Files\Git` (which already carries the
//! app-package ACE) instead.

use std::path::Path;

/// Access level to grant the container SID on a directory.
#[derive(Clone, Copy)]
pub enum Access {
    /// Read + execute (toolchain dirs the agent only runs).
    ReadExecute,
    /// Full control (the worktree the agent reads and writes).
    Full,
}

impl Access {
    fn icacls_perms(self) -> &'static str {
        match self {
            // (OI)(CI) = object- and container-inherit, so files/subdirs created
            // later (e.g. `git init`'s objects) inherit the grant.
            Access::ReadExecute => "(OI)(CI)(RX)",
            Access::Full => "(OI)(CI)(F)",
        }
    }
}

/// Grant `sid` (string form, e.g. `S-1-15-2-…`) the given access on `dir`.
pub fn grant_dir(dir: &Path, sid: &str, access: Access) -> Result<(), String> {
    let spec = format!("*{}:{}", sid, access.icacls_perms());
    let output = crate::process_util::command_no_window("icacls")
        .arg(dir)
        .arg("/grant")
        .arg(&spec)
        .output()
        .map_err(|e| format!("failed to run icacls: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "icacls /grant {} on {} failed: {}",
            spec,
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Remove `sid`'s grant from `dir`. Best-effort cleanup on close — a failure is
/// reported but a stale ACE on a directory that's about to be deleted is not
/// worth blocking close over.
pub fn revoke_dir(dir: &Path, sid: &str) -> Result<(), String> {
    let output = crate::process_util::command_no_window("icacls")
        .arg(dir)
        .arg("/remove")
        .arg(format!("*{}", sid))
        .output()
        .map_err(|e| format!("failed to run icacls: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "icacls /remove on {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
