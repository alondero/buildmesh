//! Minimal FFI to the Windows Credential Manager (`advapi32!CredReadW` /
//! `CredWriteW` / `CredDeleteW`). Just enough surface for Buildmesh to read
//! provider OAuth blobs (Antigravity's `gemini:antigravity`, Buildmesh's own
//! OpenCode OAuth dance — issue #956) and to write/delete the latter.
//!
//! `cfg(windows)` only — non-Windows callers see "not available" via the
//! `NoCredential` return path rather than via a conditional sibling module, so
//! every call site stays one-statement-uniform.
use crate::services::usage::UsageError;
use std::os::windows::ffi::OsStrExt;

#[repr(C)]
struct Filetime {
    _low: u32,
    _high: u32,
}

#[repr(C)]
struct CredentialW {
    _flags: u32,
    _typ: u32,
    _target_name: *mut u16,
    _comment: *mut u16,
    _last_written: Filetime,
    credential_blob_size: u32,
    credential_blob: *mut u8,
    _persist: u32,
    _attribute_count: u32,
    _attributes: *mut core::ffi::c_void,
    _target_alias: *mut u16,
    _user_name: *mut u16,
}

#[link(name = "advapi32")]
extern "system" {
    fn CredReadW(
        target: *const u16,
        typ: u32,
        flags: u32,
        cred: *mut *mut CredentialW,
    ) -> i32;
    fn CredWriteW(cred: *const CredentialW, flags: u32) -> i32;
    fn CredDeleteW(target: *const u16, typ: u32, flags: u32) -> i32;
    fn CredFree(buf: *mut core::ffi::c_void);
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

/// Windows Credential Manager credential type we use for every Buildmesh-owned
/// blob (Antigravity's, OpenCode's). `CRED_TYPE_GENERIC` is the catch-all "store
/// my own bytes here" type — domain credentials get mapped to Windows logon
/// secrets, which we never want.
const CRED_TYPE_GENERIC: u32 = 1;
/// Persist flag for `CredWriteW`: persists across reboots, only the local user
/// sees the credential. The other two values are `CRED_PERSIST_SESSION` (lost
/// at logoff, never what an OAuth token wants) and `CRED_PERSIST_ENTERPRISE`
/// (roams across domain-joined machines, requires policy we don't ship).
const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
/// `CredDeleteW` follows up a FALSE return with this LAST_ERROR when the
/// target wasn't present. We collapse it to `Ok(())` inside `delete` so
/// revoke is idempotent and the Settings "Sign out" affordance never errors
/// on a no-op.
const ERROR_NOT_FOUND: u32 = 1168;

/// Reads a generic credential's blob bytes from the credential manager.
/// Returns `NoCredential(target)` when the credential is absent or any other
/// Windows API error fires; missing or empty blob bytes deserialize to an
/// empty `Vec` (the parser decides whether an empty blob is "no credential"
/// or "credential whose payload is JSON `null`").
pub(crate) fn read(target: &str) -> Result<Vec<u8>, UsageError> {
    let wide: Vec<u16> = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated UTF-16; `CredReadW` writes a single
    // owned pointer we free with `CredFree` after copying its blob.
    unsafe {
        let mut ptr: *mut CredentialW = std::ptr::null_mut();
        if CredReadW(wide.as_ptr(), CRED_TYPE_GENERIC, 0, &mut ptr) == 0 || ptr.is_null() {
            return Err(UsageError::NoCredential(target.to_string()));
        }
        let cred = &*ptr;
        // `from_raw_parts` requires non-null even for length 0, so guard
        // the empty-blob case rather than risk UB.
        let blob = if cred.credential_blob.is_null() || cred.credential_blob_size == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(cred.credential_blob, cred.credential_blob_size as usize)
                .to_vec()
        };
        CredFree(ptr as *mut core::ffi::c_void);
        Ok(blob)
    }
}

/// Upserts a generic credential at `target` with the given blob bytes.
/// `CredWriteW` always overwrites when the target exists, so this is the
/// "refresh" seam too — the OAuth refresher writes the freshly-issued
/// (token, refresh_token, expires_at) here over the previous bundle.
///
/// `UserName` is set to the target name (`Self` pattern) so Credential Manager's
/// detail view shows the same string as the target column; clearing it leaves
/// the row visually anonymous and breaks management in `control /name
/// Microsoft.CredentialManager`.
pub(crate) fn write(target: &str, blob: &[u8]) -> Result<(), UsageError> {
    let mut target_wide: Vec<u16> = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut user_wide: Vec<u16> = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `target_wide` and `user_wide` are NUL-terminated UTF-16; the blob
    // pointer is only read for `credential_blob_size` bytes; CredWriteW copies
    // them out before returning. The temp UTF-16 buffers stay valid for the
    // call's duration because we hold them in this scope.
    unsafe {
        let cred = CredentialW {
            _flags: 0,
            _typ: CRED_TYPE_GENERIC,
            _target_name: target_wide.as_mut_ptr(),
            _comment: std::ptr::null_mut(),
            _last_written: Filetime { _low: 0, _high: 0 },
            credential_blob_size: blob.len() as u32,
            credential_blob: blob.as_ptr() as *mut u8,
            _persist: CRED_PERSIST_LOCAL_MACHINE,
            _attribute_count: 0,
            _attributes: std::ptr::null_mut(),
            _target_alias: std::ptr::null_mut(),
            _user_name: user_wide.as_mut_ptr(),
        };
        // `Flags` is reserved per Microsoft docs and must be 0.
        let ok = CredWriteW(&cred, 0);
        if ok == 0 {
            return Err(UsageError::Shape(format!(
                "CredWriteW failed for target {target}"
            )));
        }
        Ok(())
    }
}

/// Deletes a generic credential at `target`. Idempotent: a missing credential
/// (`ERROR_NOT_FOUND`) returns `Ok(())` so the Settings "Sign out" affordance
/// never errors on a no-op. Other Windows failures surface as `Shape` so the
/// caller can diagnose (e.g. permission issues, target corruption).
///
/// `CredDeleteW` returns `FALSE` for *every* failure — including
/// `ERROR_NOT_FOUND` — so a TRUE-only check would conflate "deleted" with
/// "didn't exist". We follow up with `GetLastError` and collapse the not-found
/// case to success.
pub(crate) fn delete(target: &str) -> Result<(), UsageError> {
    let wide: Vec<u16> = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated UTF-16; `CredDeleteW` only reads.
    unsafe {
        let result = CredDeleteW(wide.as_ptr(), CRED_TYPE_GENERIC, 0);
        if result != 0 {
            return Ok(());
        }
        // Failed. Was it just not present?
        let last_err = GetLastError();
        if last_err == ERROR_NOT_FOUND as u32 {
            return Ok(());
        }
        Err(UsageError::Shape(format!(
            "CredDeleteW failed for target {target} (GetLastError={last_err})"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Collision-resistant test target: every test run gets a fresh random
    /// suffix so a half-cleaned credential from a previous failure cannot
    /// shadow the result. `buildmesh-test-` prefix is registered in
    /// `docs/knowledge-primer.md` so a future operator clearing test
    /// credentials can run `cmdkey /list:buildmesh-test-*` to find leftovers.
    fn unique_target(label: &str) -> String {
        format!(
            "buildmesh-test-{label}-{}",
            Uuid::new_v4().simple().to_string()
        )
    }

    /// Drops the credential on scope exit. Test bodies must hold this as a
    /// `let _guard = ...` so cleanup runs even on assertion failure —
    /// otherwise a failing test leaves a phantom credential that can mask
    /// future run results if the random suffix collides.
    struct CleanupTarget(String);
    impl Drop for CleanupTarget {
        fn drop(&mut self) {
            let _ = delete(&self.0);
        }
    }

    #[test]
    fn write_then_read_returns_equal_blob() {
        let target = unique_target("write-read");
        let _cleanup = CleanupTarget(target.clone());
        let blob = b"hello credential bytes \x00\x01\x02\xff";

        write(&target, blob).expect("write should succeed");

        let read_back = read(&target).expect("read should succeed");
        assert_eq!(
            read_back, blob,
            "round-tripped blob must equal the input byte-for-byte"
        );
    }

    #[test]
    fn write_overwrites_existing_target() {
        // `CredWriteW` is documented as upsert; pin that behavior so a future
        // caller depending on "replace" semantics doesn't get surprised.
        let target = unique_target("overwrite");
        let _cleanup = CleanupTarget(target.clone());

        write(&target, b"first").unwrap();
        write(&target, b"second-version-much-longer").unwrap();

        let read_back = read(&target).unwrap();
        assert_eq!(read_back, b"second-version-much-longer");
    }

    #[test]
    fn read_missing_target_returns_no_credential() {
        let target = unique_target("absent");
        let result = read(&target);
        assert!(
            matches!(result, Err(UsageError::NoCredential(_))),
            "absent credential must surface as NoCredential, got {result:?}"
        );
    }

    #[test]
    fn delete_then_read_returns_no_credential() {
        let target = unique_target("delete-read");
        let _cleanup = CleanupTarget(target.clone());

        write(&target, b"about to be deleted").unwrap();
        assert!(read(&target).is_ok());

        delete(&target).expect("delete of existing credential should succeed");
        let result = read(&target);
        assert!(
            matches!(result, Err(UsageError::NoCredential(_))),
            "read-after-delete must surface as NoCredential, got {result:?}"
        );
    }

    #[test]
    fn delete_idempotent_when_missing() {
        // The Settings "Sign out" affordance must not error when the user
        // was already signed out — pin the idempotency contract so a future
        // tightening of `delete` to surface missing-credential as an error
        // is caught at code review.
        let target = unique_target("delete-missing");
        let _cleanup = CleanupTarget(target.clone());

        let result = delete(&target);
        assert!(
            result.is_ok(),
            "deleting a missing credential must be a no-op success, got {result:?}"
        );
    }

    #[test]
    fn empty_blob_round_trips_as_empty_vec() {
        // Mirrors the agy / opencode read paths, where an existing credential
        // with a null/empty blob body parses to `Ok(Vec::new())` so the
        // higher-level parser can decide what "empty" means.
        let target = unique_target("empty");
        let _cleanup = CleanupTarget(target.clone());

        write(&target, b"").expect("write of empty blob should succeed");

        let read_back = read(&target).expect("read should succeed");
        assert!(
            read_back.is_empty(),
            "empty write must round-trip as empty Vec"
        );
    }

    #[test]
    fn ascii_blob_round_trips() {
        // Pin the JSON-blob shape that the OAuth refresher + reader will
        // hand around. Without this, a future migration to UTF-16 blob
        // encoding would silently break the parser.
        let target = unique_target("ascii");
        let _cleanup = CleanupTarget(target.clone());
        let blob = b"{\"access_token\":\"oc_sk_test\",\"workspace_id\":\"wrk_a\"}";

        write(&target, blob).unwrap();
        let read_back = read(&target).unwrap();
        assert_eq!(read_back, blob);
    }
}
