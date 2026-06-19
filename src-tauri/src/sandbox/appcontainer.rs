//! Windows AppContainer profile + security capabilities for sandboxing agent
//! PTY processes (GH #498).
//!
//! An AppContainer is a low-integrity security sandbox: a process launched with
//! an AppContainer SID is denied access to any securable object that doesn't
//! explicitly grant that SID (or the `ALL APPLICATION PACKAGES` SID). That's how
//! we confine an agent to its worktree — the worktree dir is granted the
//! container SID (a later slice), everything else stays denied by default.
//!
//! This module owns the *profile + SID* lifecycle. The native ConPTY spawn that
//! launches a process *into* the container (via a `STARTUPINFOEX`
//! `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` attribute) consumes
//! [`AppContainerProfile::security_capabilities`].
//!
//! FFI is inline `extern "system"` with no `windows-sys`/`winapi` dependency,
//! mirroring [`crate::process_util::JobHandle`]. A throwaway spike (GH #498
//! Phase 0) validated the equivalent calls via the `windows` crate on this
//! platform before this hand-rolled version was written.

use std::os::raw::c_void;

/// A Windows `PSID`.
pub type Psid = *mut c_void;

/// `SID_AND_ATTRIBUTES` — one capability granted to the container.
#[repr(C)]
pub struct SidAndAttributes {
    pub sid: Psid,
    pub attributes: u32,
}

/// `SECURITY_CAPABILITIES` — passed as the
/// `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` value at `CreateProcess`. Held
/// by the consumer (`sandbox::conpty`) only for the duration of that call; its
/// pointers borrow from the owning [`AppContainerProfile`].
#[repr(C)]
pub struct SecurityCapabilities {
    pub app_container_sid: Psid,
    pub capabilities: *const SidAndAttributes,
    pub capability_count: u32,
    pub reserved: u32,
}

mod ffi {
    use super::{Psid, SidAndAttributes};
    use std::os::raw::c_void;

    /// `SE_GROUP_ENABLED` — a capability must be enabled to take effect.
    pub const SE_GROUP_ENABLED: u32 = 0x0000_0004;

    /// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)` — `CreateAppContainerProfile`
    /// returns this when the named profile already exists (e.g. a prior run);
    /// the caller falls back to deriving the SID by name.
    pub const ERROR_ALREADY_EXISTS_HRESULT: i32 = 0x8007_00B7u32 as i32;

    pub const S_OK: i32 = 0;

    #[link(name = "userenv")]
    extern "system" {
        pub fn CreateAppContainerProfile(
            app_container_name: *const u16,
            display_name: *const u16,
            description: *const u16,
            capabilities: *const SidAndAttributes,
            capability_count: u32,
            sid: *mut Psid,
        ) -> i32; // HRESULT (uses the hoisted super:: types)
        pub fn DeriveAppContainerSidFromAppContainerName(
            app_container_name: *const u16,
            sid: *mut Psid,
        ) -> i32; // HRESULT
        pub fn DeleteAppContainerProfile(app_container_name: *const u16) -> i32; // HRESULT
    }

    #[link(name = "advapi32")]
    extern "system" {
        /// Allocates a SID from a string form (e.g. `S-1-15-3-1`). The result is
        /// freed with `LocalFree` (NOT `FreeSid`).
        pub fn ConvertStringSidToSidW(string_sid: *const u16, sid: *mut Psid) -> i32; // BOOL
        /// Frees a SID returned by `CreateAppContainerProfile` /
        /// `DeriveAppContainerSidFromAppContainerName`.
        pub fn FreeSid(sid: Psid) -> Psid;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }
}

/// The well-known `internetClient` capability SID — outbound network access.
/// Required for the Anthropic API and for `git` over HTTPS inside the sandbox.
const INTERNET_CLIENT_SID: &str = "S-1-15-3-1";

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A live Windows AppContainer profile and its security capabilities.
///
/// Owns the container SID (freed with `FreeSid` on drop) and any capability SIDs
/// (freed with `LocalFree`). The profile itself persists on disk until
/// [`delete`](Self::delete) is called — create-or-derive is idempotent, so a
/// re-spawn reuses the same profile rather than leaking a new one each time.
pub struct AppContainerProfile {
    name: Vec<u16>,
    sid: Psid,
    /// Capability SIDs, kept alive (and freed on drop) because `capabilities`
    /// holds raw pointers into them.
    capability_sids: Vec<Psid>,
    capabilities: Vec<SidAndAttributes>,
}

// The raw SID pointers are owned by this struct and only handed out as short-
// lived borrows during a `CreateProcess` call on the spawning thread.
unsafe impl Send for AppContainerProfile {}

impl AppContainerProfile {
    /// Create (or, if it already exists, derive) the AppContainer profile named
    /// `name`, granting the `internetClient` capability when `with_internet` is
    /// set. `name` must be ≤ 64 chars and is the stable identity of the
    /// container — reuse the same name to reuse the same SID and on-disk profile.
    ///
    /// Returns `Err` (rather than falling back to an unsandboxed spawn here) so
    /// the caller decides the failure policy explicitly.
    pub fn create_or_derive(name: &str, with_internet: bool) -> Result<Self, String> {
        let wname = to_wide(name);

        // Build capabilities first so their SIDs outlive the array that points
        // at them (both stored in the returned struct).
        let mut capability_sids: Vec<Psid> = Vec::new();
        if with_internet {
            capability_sids.push(convert_string_sid(INTERNET_CLIENT_SID)?);
        }
        let capabilities: Vec<SidAndAttributes> = capability_sids
            .iter()
            .map(|&sid| SidAndAttributes {
                sid,
                attributes: ffi::SE_GROUP_ENABLED,
            })
            .collect();

        let caps_ptr = if capabilities.is_empty() {
            std::ptr::null()
        } else {
            capabilities.as_ptr()
        };
        let display = to_wide("Buildmesh Agent Sandbox");
        let desc = to_wide("Confines a Buildmesh agent process to its worktree (GH #498)");

        // SAFETY: name/display/desc are null-terminated wide strings that
        // outlive the call; `sid` is written only on success and freed on drop.
        let sid = unsafe {
            let mut sid: Psid = std::ptr::null_mut();
            let hr = ffi::CreateAppContainerProfile(
                wname.as_ptr(),
                display.as_ptr(),
                desc.as_ptr(),
                caps_ptr,
                capabilities.len() as u32,
                &mut sid,
            );
            if hr == ffi::ERROR_ALREADY_EXISTS_HRESULT {
                // Profile from a previous run — derive its SID by name.
                let hr2 = ffi::DeriveAppContainerSidFromAppContainerName(wname.as_ptr(), &mut sid);
                if hr2 != ffi::S_OK {
                    free_capability_sids(&capability_sids);
                    return Err(format!("DeriveAppContainerSidFromAppContainerName failed: HRESULT 0x{:08X}", hr2));
                }
            } else if hr != ffi::S_OK {
                free_capability_sids(&capability_sids);
                return Err(format!("CreateAppContainerProfile failed: HRESULT 0x{:08X}", hr));
            }
            sid
        };

        Ok(Self {
            name: wname,
            sid,
            capability_sids,
            capabilities,
        })
    }

    /// The `SECURITY_CAPABILITIES` to attach to a `STARTUPINFOEX` attribute list
    /// at `CreateProcess` time. The returned struct borrows raw pointers from
    /// `self`, so `self` must outlive the `CreateProcess` call.
    pub fn security_capabilities(&self) -> SecurityCapabilities {
        SecurityCapabilities {
            app_container_sid: self.sid,
            capabilities: if self.capabilities.is_empty() {
                std::ptr::null()
            } else {
                self.capabilities.as_ptr()
            },
            capability_count: self.capabilities.len() as u32,
            reserved: 0,
        }
    }

    /// The container's SID, for ACL grants (a later slice grants the worktree
    /// dir to this SID).
    pub fn app_container_sid(&self) -> Psid {
        self.sid
    }

    /// Delete the on-disk profile. Call when the owning node is removed so
    /// profiles don't accumulate. Idempotent enough for cleanup — a missing
    /// profile is reported but not fatal to the caller.
    pub fn delete(&self) -> Result<(), String> {
        // SAFETY: `name` is the null-terminated wide name used to create it.
        let hr = unsafe { ffi::DeleteAppContainerProfile(self.name.as_ptr()) };
        if hr == ffi::S_OK {
            Ok(())
        } else {
            Err(format!("DeleteAppContainerProfile failed: HRESULT 0x{:08X}", hr))
        }
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        // SAFETY: `sid` came from Create/Derive (freed with FreeSid); each
        // capability SID came from ConvertStringSidToSidW (freed with LocalFree).
        unsafe {
            if !self.sid.is_null() {
                ffi::FreeSid(self.sid);
            }
        }
        free_capability_sids(&self.capability_sids);
    }
}

fn convert_string_sid(s: &str) -> Result<Psid, String> {
    let wide = to_wide(s);
    // SAFETY: `wide` is a null-terminated wide string; `sid` is written only on
    // a non-zero (success) return and freed by the owner with LocalFree.
    unsafe {
        let mut sid: Psid = std::ptr::null_mut();
        if ffi::ConvertStringSidToSidW(wide.as_ptr(), &mut sid) == 0 {
            return Err(format!("ConvertStringSidToSidW failed for {}", s));
        }
        Ok(sid)
    }
}

fn free_capability_sids(sids: &[Psid]) {
    for &sid in sids {
        if !sid.is_null() {
            // SAFETY: each came from ConvertStringSidToSidW, freed once.
            unsafe {
                ffi::LocalFree(sid as *mut c_void);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creating a profile yields a non-null SID, and the profile can be deleted.
    /// Runs against the real Win32 AppContainer APIs (no elevation needed for a
    /// user-owned profile), so it pins the inline FFI signatures against the OS.
    #[test]
    fn create_yields_sid_then_deletes() {
        let name = "com.alond.buildmesh.test-appcontainer";
        let profile = AppContainerProfile::create_or_derive(name, true)
            .expect("create AppContainer profile");
        assert!(!profile.app_container_sid().is_null(), "SID must be non-null");

        // Capabilities requested → the SECURITY_CAPABILITIES reflects them.
        let caps = profile.security_capabilities();
        assert_eq!(caps.capability_count, 1, "internetClient capability present");
        assert!(!caps.app_container_sid.is_null());

        profile.delete().expect("delete AppContainer profile");
    }

    /// Create-or-derive is idempotent: a second call on the same name succeeds
    /// (deriving the existing profile's SID) rather than erroring.
    #[test]
    fn create_or_derive_is_idempotent() {
        let name = "com.alond.buildmesh.test-appcontainer-idem";
        let first = AppContainerProfile::create_or_derive(name, false)
            .expect("first create");
        let second = AppContainerProfile::create_or_derive(name, false)
            .expect("second create-or-derive must succeed via the derive fallback");
        assert!(!first.app_container_sid().is_null());
        assert!(!second.app_container_sid().is_null());
        drop(first);
        drop(second);
        // Clean up the on-disk profile.
        let cleanup = AppContainerProfile::create_or_derive(name, false).unwrap();
        cleanup.delete().expect("cleanup delete");
    }

    #[test]
    fn no_capabilities_when_internet_not_requested() {
        let name = "com.alond.buildmesh.test-appcontainer-nocaps";
        let profile = AppContainerProfile::create_or_derive(name, false).unwrap();
        let caps = profile.security_capabilities();
        assert_eq!(caps.capability_count, 0);
        assert!(caps.capabilities.is_null());
        profile.delete().ok();
    }
}
