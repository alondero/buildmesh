//! Restricted, low-integrity primary token for confining agent PTY processes
//! (ADR-0014, GH #528).
//!
//! This is the pivot away from the AppContainer SID ([`super::appcontainer`]).
//! An AppContainer confines via a *private object namespace* that denies
//! named-pipe creation on `\Device\NamedPipe` and breaks MSYS `bash`; `claude.exe`
//! spins forever in libuv's `uv__create_stdio_pipe_pair` retry loop the moment it
//! spawns a child (the #528 root cause). A **restricted token** confines via the
//! access-check layer instead — no private namespace, no NPFS denial — exactly
//! the model Chromium's renderer sandbox uses for processes that must spawn/IPC.
//!
//! ## How a restricted token denies host reads by default
//!
//! A token built by `CreateRestrictedToken` carries a list of *restricting SIDs*.
//! Every access check then runs **twice** — once against the token's normal
//! (enabled) SIDs, once against the restricting SIDs — and access is granted only
//! if **both** checks allow it. A user-private file (its DACL grants only the
//! interactive user's SID) fails the restricting check, so it is denied even
//! though the normal check would pass. To *open* a specific object (the agent's
//! worktree) we grant it to a SID that is in the restricting set
//! ([`RestrictedToken::grant_sid`] — `RESTRICTED`, `S-1-5-12`), so both checks
//! pass for that object only. This re-establishes ADR-0012's deny-by-default read
//! protection without an object namespace.
//!
//! ## Restricting SID set
//!
//! `Everyone` + `Users` + `RESTRICTED` + the session **logon SID**. The first
//! three let the process reach the world-readable system objects a child spawn
//! legitimately needs (the NPFS root, CSRSS's `\Windows\ApiPort`, loopback);
//! the logon SID grants the interactive window station / desktop the process is
//! launched onto. None of them appear on a user-private file's DACL, so home-dir
//! reads stay denied. Tuning this set against criteria (a)–(g) is the whole point
//! of the §4 spike.
//!
//! FFI is inline `extern "system"` with no `windows-sys`/`winapi` dependency,
//! mirroring [`super::appcontainer`] and [`super::conpty`].

#![cfg(target_os = "windows")]

use std::os::raw::c_void;

/// A Windows `PSID`.
pub type Psid = *mut c_void;

/// The SID a confined object (the worktree) is granted to so it passes the
/// restricting-SID check. `RESTRICTED` (`S-1-5-12`) is in our restricting set and
/// is never present on a user-private file's DACL, so granting it to the worktree
/// opens *only* the worktree.
const GRANT_SID: &str = "S-1-5-12";

/// Restricting SIDs that always go in the set, in string form. The session logon
/// SID is added dynamically (it is per-logon, not well-known).
const RESTRICTING_SIDS: &[&str] = &[
    "S-1-1-0",      // Everyone
    "S-1-5-32-545", // BUILTIN\Users
    GRANT_SID,      // RESTRICTED — also the worktree grant target
];

#[allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]
mod ffi {
    use super::Psid;
    use std::os::raw::c_void;

    pub type HANDLE = *mut c_void;
    pub type BOOL = i32;
    pub type DWORD = u32;

    // Token access rights (winnt.h). The result must be usable by
    // CreateProcessAsUserW (ASSIGN_PRIMARY + DUPLICATE) and readable by
    // CreateRestrictedToken / GetTokenInformation (QUERY).
    pub const TOKEN_ASSIGN_PRIMARY: DWORD = 0x0001;
    pub const TOKEN_DUPLICATE: DWORD = 0x0002;
    pub const TOKEN_QUERY: DWORD = 0x0008;

    // TOKEN_INFORMATION_CLASS values we use.
    pub const TOKEN_LOGON_SID: i32 = 28;
    /// Read-only, asserted by the integrity unit test (the token is left at the
    /// parent's Medium IL — the spike found Low IL breaks msys/Bun).
    #[cfg(test)]
    pub const TOKEN_INTEGRITY_LEVEL: i32 = 25;

    #[repr(C)]
    pub struct SID_AND_ATTRIBUTES {
        pub sid: Psid,
        pub attributes: DWORD,
    }

    /// `TOKEN_GROUPS` header — `GetTokenInformation(TokenLogonSid)` writes one of
    /// these followed by the variable-length group array; we only read `groups[0]`.
    #[repr(C)]
    pub struct TOKEN_GROUPS_HEADER {
        pub group_count: DWORD,
        pub groups: [SID_AND_ATTRIBUTES; 1],
    }

    /// Read back by the integrity unit test only.
    #[cfg(test)]
    #[repr(C)]
    pub struct TOKEN_MANDATORY_LABEL {
        pub label: SID_AND_ATTRIBUTES,
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn OpenProcessToken(
            process: HANDLE,
            desired_access: DWORD,
            token_handle: *mut HANDLE,
        ) -> BOOL;
        pub fn CreateRestrictedToken(
            existing_token: HANDLE,
            flags: DWORD,
            disable_sid_count: DWORD,
            sids_to_disable: *const SID_AND_ATTRIBUTES,
            delete_privilege_count: DWORD,
            privileges_to_delete: *const c_void,
            restricted_sid_count: DWORD,
            sids_to_restrict: *const SID_AND_ATTRIBUTES,
            new_token: *mut HANDLE,
        ) -> BOOL;
        pub fn GetTokenInformation(
            token: HANDLE,
            token_information_class: i32,
            token_information: *mut c_void,
            token_information_length: DWORD,
            return_length: *mut DWORD,
        ) -> BOOL;
        pub fn ConvertStringSidToSidW(string_sid: *const u16, sid: *mut Psid) -> BOOL;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetCurrentProcess() -> HANDLE;
        pub fn CloseHandle(object: HANDLE) -> BOOL;
        pub fn LocalFree(mem: *mut c_void) -> *mut c_void;
        pub fn GetLastError() -> DWORD;
    }
}

use ffi::HANDLE;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A token handle owned by us, closed on drop.
struct OwnedToken(HANDLE);
// SAFETY: the handle is only used on the spawning thread under the caller's
// serialisation; we never share the inner pointer concurrently.
unsafe impl Send for OwnedToken {}

impl Drop for OwnedToken {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle came from Open/CreateRestrictedToken, closed once.
            unsafe {
                ffi::CloseHandle(self.0);
            }
        }
    }
}

/// A SID allocated by `ConvertStringSidToSidW`, freed with `LocalFree`.
struct LocalSid(Psid);

impl Drop for LocalSid {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: came from ConvertStringSidToSidW, freed once with LocalFree.
            unsafe {
                ffi::LocalFree(self.0 as *mut c_void);
            }
        }
    }
}

fn convert_string_sid(s: &str) -> Result<LocalSid, String> {
    let wide = to_wide(s);
    // SAFETY: `wide` is null-terminated; `sid` is written only on success.
    unsafe {
        let mut sid: Psid = std::ptr::null_mut();
        if ffi::ConvertStringSidToSidW(wide.as_ptr(), &mut sid) == 0 {
            return Err(format!("ConvertStringSidToSidW({}) failed: {}", s, ffi::GetLastError()));
        }
        Ok(LocalSid(sid))
    }
}

/// A restricted, low-integrity primary token suitable for `CreateProcessAsUserW`.
///
/// Owns the token handle (closed on drop). The handle has `TOKEN_ASSIGN_PRIMARY`
/// / `TOKEN_DUPLICATE` / `TOKEN_QUERY` access so `CreateProcessAsUserW` can use it.
pub struct RestrictedToken {
    token: OwnedToken,
    /// Logon-SID buffer kept alive only while building (its `Psid` is read by
    /// `CreateRestrictedToken`); `None` once construction returns.
    _logon_buf: Vec<u8>,
}

impl RestrictedToken {
    /// Build the restricted token from the current process token.
    ///
    /// `include_user_sid` controls the central §4-spike trade-off (GH #528):
    ///   * `false` — the user SID is **excluded** from the restricting set, so
    ///     user-private home files (whose DACL grants the user SID) fail the
    ///     restricting check → home reads/writes denied (criteria d/e). BUT
    ///     msys/cygwin keys its shared sections on the user SID, so `bash` dies
    ///     at `CreateFileMapping` (criterion c fails).
    ///   * `true` — the user SID is in the restricting set, so `bash` inits
    ///     (criterion c) BUT home reads/writes are re-opened (d/e leak).
    ///
    /// The two cannot co-hold; see [`super::spawn`] spike tests.
    pub fn new(include_user_sid: bool) -> Result<Self, String> {
        // SAFETY: each Win32 call's pointers are valid for its duration; handles
        // and SIDs are owned by RAII guards (OwnedToken / LocalSid) or the
        // logon-SID byte buffer, all of which outlive their uses below.
        unsafe {
            // 1. Open the current process's primary token with the rights
            //    CreateRestrictedToken needs to read it and CreateProcessAsUserW
            //    needs to assign the result.
            let mut proc_token: HANDLE = std::ptr::null_mut();
            if ffi::OpenProcessToken(
                ffi::GetCurrentProcess(),
                ffi::TOKEN_DUPLICATE | ffi::TOKEN_QUERY | ffi::TOKEN_ASSIGN_PRIMARY,
                &mut proc_token,
            ) == 0
            {
                return Err(format!("OpenProcessToken failed: {}", ffi::GetLastError()));
            }
            let proc_token = OwnedToken(proc_token);

            // 2. The session logon SID (per-logon, not well-known) — needed so the
            //    confined process can attach to the interactive window station /
            //    desktop it is launched onto.
            let logon_buf = get_token_logon_sid(proc_token.0)?;
            // Guard on group_count > 0: an interactive token always carries a
            // logon SID, but if TokenLogonSid ever reports zero groups, groups[0]
            // would be an uninitialised read whose garbage Psid CreateRestrictedToken
            // would then dereference. Null → the logon SID is simply omitted below.
            let logon_sid = (logon_buf.as_ptr() as *const ffi::TOKEN_GROUPS_HEADER)
                .as_ref()
                .filter(|g| g.group_count > 0)
                .map(|g| g.groups[0].sid)
                .unwrap_or(std::ptr::null_mut());

            // 3. Build the restricting SID_AND_ATTRIBUTES array. The well-known
            //    SIDs must outlive CreateRestrictedToken, so hold their guards.
            let mut sid_guards: Vec<LocalSid> = Vec::new();
            for s in RESTRICTING_SIDS {
                sid_guards.push(convert_string_sid(s)?);
            }
            let mut restricting: Vec<ffi::SID_AND_ATTRIBUTES> = sid_guards
                .iter()
                .map(|g| ffi::SID_AND_ATTRIBUTES { sid: g.0, attributes: 0 })
                .collect();
            if !logon_sid.is_null() {
                restricting.push(ffi::SID_AND_ATTRIBUTES { sid: logon_sid, attributes: 0 });
            }
            // The user SID: including it lets msys `bash` create its user-keyed
            // shared sections, at the cost of re-opening user-private home reads
            // (the §4 trade-off). The buffer must outlive CreateRestrictedToken.
            let _user_buf;
            if include_user_sid {
                _user_buf = get_token_user_sid(proc_token.0)?;
                let user_sid = (_user_buf.as_ptr() as *const ffi::SID_AND_ATTRIBUTES)
                    .as_ref()
                    .map(|u| u.sid)
                    .unwrap_or(std::ptr::null_mut());
                if !user_sid.is_null() {
                    restricting.push(ffi::SID_AND_ATTRIBUTES { sid: user_sid, attributes: 0 });
                }
            }

            // 4. Create the restricted token. No SIDs disabled, no privileges
            //    deleted here — the restricting-SID intersection is what confines.
            let mut restricted: HANDLE = std::ptr::null_mut();
            if ffi::CreateRestrictedToken(
                proc_token.0,
                0,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                restricting.len() as u32,
                restricting.as_ptr(),
                &mut restricted,
            ) == 0
            {
                return Err(format!("CreateRestrictedToken failed: {}", ffi::GetLastError()));
            }
            let restricted = OwnedToken(restricted);

            // NB: the token is left at the parent's (Medium) integrity. An earlier
            // spike revision lowered it to Low for write-up defence-in-depth, but
            // a Low-IL process cannot create its named-object directory under the
            // Medium-IL `\BaseNamedObjects` — MSYS `bash` dies with
            // `NtCreateDirectoryObject(...): 0xC0000022` and `claude` (Bun) hits
            // the same wall. The restricting-SID intersection already denies
            // home-dir reads *and* writes (it's a DACL check, independent of IL),
            // so Low IL added breakage without adding read/write protection.

            Ok(Self {
                token: restricted,
                _logon_buf: logon_buf,
            })
        }
    }

    /// The restricted primary token handle, for `CreateProcessAsUserW`.
    pub fn raw(&self) -> HANDLE {
        self.token.0
    }

    /// The SID (string form) a confined object must be granted to so the token's
    /// restricting-SID check passes for it. Grant the worktree to this.
    pub fn grant_sid(&self) -> &'static str {
        GRANT_SID
    }
}

/// `GetTokenInformation(TokenLogonSid)` into a right-sized byte buffer.
fn get_token_logon_sid(token: HANDLE) -> Result<Vec<u8>, String> {
    // SAFETY: two-call pattern — first sizes the buffer, then fills it.
    unsafe {
        let mut needed: u32 = 0;
        ffi::GetTokenInformation(token, ffi::TOKEN_LOGON_SID, std::ptr::null_mut(), 0, &mut needed);
        let err = ffi::GetLastError();
        if needed == 0 {
            return Err(format!("GetTokenInformation(TokenLogonSid) sizing failed: {}", err));
        }
        let mut buf = vec![0u8; needed as usize];
        if ffi::GetTokenInformation(
            token,
            ffi::TOKEN_LOGON_SID,
            buf.as_mut_ptr() as *mut c_void,
            needed,
            &mut needed,
        ) == 0
        {
            return Err(format!("GetTokenInformation(TokenLogonSid) failed: {}", ffi::GetLastError()));
        }
        Ok(buf)
    }
}

/// `GetTokenInformation(TokenUser)` into a byte buffer; `buf[0..]` is a
/// `SID_AND_ATTRIBUTES` whose `.sid` points into the buffer. SPIKE-only.
fn get_token_user_sid(token: HANDLE) -> Result<Vec<u8>, String> {
    const TOKEN_USER: i32 = 1;
    // SAFETY: two-call sizing then fill.
    unsafe {
        let mut needed: u32 = 0;
        ffi::GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return Err(format!("GetTokenInformation(TokenUser) sizing failed: {}", ffi::GetLastError()));
        }
        let mut buf = vec![0u8; needed as usize];
        if ffi::GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr() as *mut c_void, needed, &mut needed) == 0 {
            return Err(format!("GetTokenInformation(TokenUser) failed: {}", ffi::GetLastError()));
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a restricted token against the real Win32 APIs (no elevation
    /// needed), pinning the inline FFI signatures against the OS.
    #[test]
    fn builds_restricted_token() {
        let tok = RestrictedToken::new(false).expect("build restricted token");
        assert!(!tok.raw().is_null(), "token handle must be non-null");
        assert_eq!(tok.grant_sid(), "S-1-5-12");
    }

    /// The token is genuinely restricted: `GetTokenInformation(TokenHasRestrictions)`
    /// reports non-zero, distinguishing it from a plain process-token duplicate.
    #[test]
    fn token_reports_restrictions() {
        const TOKEN_HAS_RESTRICTIONS: i32 = 21;
        let tok = RestrictedToken::new(false).expect("build restricted token");
        // SAFETY: fixed-size DWORD out-param for a known information class.
        let has_restrictions = unsafe {
            let mut val: u32 = 0;
            let mut ret: u32 = 0;
            let ok = ffi::GetTokenInformation(
                tok.raw(),
                TOKEN_HAS_RESTRICTIONS,
                &mut val as *mut _ as *mut std::os::raw::c_void,
                std::mem::size_of::<u32>() as u32,
                &mut ret,
            );
            assert_ne!(ok, 0, "GetTokenInformation(TokenHasRestrictions) failed: {}", ffi::GetLastError());
            val
        };
        assert_ne!(has_restrictions, 0, "token must carry restricting SIDs");
    }

    /// The token stays at **Medium** integrity (`S-1-16-8192`) — the §4 spike
    /// found Low IL blocks msys `bash` / Bun from creating their named objects
    /// under the Medium `\BaseNamedObjects`, so the low-IL layer was dropped.
    #[test]
    fn token_integrity_is_medium() {
        let tok = RestrictedToken::new(false).expect("build restricted token");
        // SAFETY: two-call sizing then fill, reading the mandatory-label SID.
        let il_sid = unsafe {
            let mut needed: u32 = 0;
            ffi::GetTokenInformation(tok.raw(), ffi::TOKEN_INTEGRITY_LEVEL, std::ptr::null_mut(), 0, &mut needed);
            assert!(needed > 0, "integrity-level sizing failed: {}", ffi::GetLastError());
            let mut buf = vec![0u8; needed as usize];
            let ok = ffi::GetTokenInformation(
                tok.raw(),
                ffi::TOKEN_INTEGRITY_LEVEL,
                buf.as_mut_ptr() as *mut std::os::raw::c_void,
                needed,
                &mut needed,
            );
            assert_ne!(ok, 0, "integrity-level read failed: {}", ffi::GetLastError());
            let label = &*(buf.as_ptr() as *const ffi::TOKEN_MANDATORY_LABEL);
            // Render the SID back to a string to compare.
            sid_to_string(label.label.sid)
        };
        assert_eq!(il_sid, "S-1-16-8192", "integrity level must be Medium (Low breaks msys/Bun named objects)");
    }

    /// Helper: render a SID to its string form for assertions.
    fn sid_to_string(sid: Psid) -> String {
        #[link(name = "advapi32")]
        extern "system" {
            fn ConvertSidToStringSidW(sid: Psid, out: *mut *mut u16) -> i32;
        }
        // SAFETY: `sid` is a live SID; the wide output is freed with LocalFree.
        unsafe {
            let mut out: *mut u16 = std::ptr::null_mut();
            assert_ne!(ConvertSidToStringSidW(sid, &mut out), 0, "ConvertSidToStringSidW failed");
            let mut len = 0usize;
            while *out.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(out, len));
            ffi::LocalFree(out as *mut std::os::raw::c_void);
            s
        }
    }
}
