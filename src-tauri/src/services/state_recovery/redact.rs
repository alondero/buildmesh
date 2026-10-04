//! Credential redaction for exported state (issue #1537).
//!
//! A default export must be safe to email to yourself or drop in a bug
//! report. Buildmesh stores credentials in three separate places, and each
//! needs a different treatment:
//!
//! | Location | Form | Treatment |
//! |---|---|---|
//! | `app_settings` in `buildmesh.db` | cleartext / SHA-256 | delete the row |
//! | `preferences.json` | cleartext JSON field | drop the field |
//! | `tls/ca.key.der` | PKCS#8 private key | never in the bundle at all |
//! | Windows Credential Manager | outside both files | not exportable — see the module note in `mod.rs` |
//!
//! The rule throughout: **redact the copy, never the original.** The
//! redacted database is a separate file produced by `VACUUM INTO`, so a bug
//! in the redaction path cannot reach live state. That is also why redaction
//! runs after the snapshot rather than before it.
//!
//! Terminal transcripts are *not* here because Buildmesh never writes them to
//! durable state: scrollback lives in the frontend's xterm.js instances, and
//! agent transcripts live in each harness's own session directory outside the
//! app-data profile. `mod.rs` asserts that on the export path rather than
//! assuming it.

use rusqlite::Connection;
use serde_json::Value;
use std::io;
use std::path::Path;

/// `app_settings` keys whose value is a credential.
///
/// `remote_access_token` is the Admin-role bearer token and is stored
/// **cleartext** (`db/auth.rs` — hashing is deferred to the Keychain slice,
/// issue #495), so it is the one that must never leave in an export. The two
/// coordinator tokens are already SHA-256 digests, but a digest of a bearer
/// token is still a credential-adjacent artifact and there is no reason to
/// carry it, so they go too.
///
/// Not in this list, deliberately: `schema_version`,
/// `coordinator_api_enabled`, `coordinator_drive_enabled`,
/// `lan_exposure_enabled`, and the two `*_upgrade_v1/v2` prompt-bump markers.
/// Those are behaviour flags — losing them is harmless and keeping them is
/// what makes an export a faithful round-trip.
pub(crate) const SECRET_DB_SETTINGS: &[&str] = &[
    "remote_access_token",
    "coordinator_read_token",
    "coordinator_drive_token",
];

/// `preferences.json` top-level fields that are credentials.
const SECRET_PREF_FIELDS: &[&str] = &["minimax_api_key"];

/// `provider_accounts[]` member that is a credential.
const SECRET_ACCOUNT_FIELD: &str = "api_key";

/// Strip credential rows from a **copy** of the database at `path`.
///
/// The caller must pass a file it just created; this opens it read-write and
/// commits the deletions, so the live database is never a participant.
pub(crate) fn redact_database_copy(path: &Path) -> io::Result<usize> {
    let conn = Connection::open(path)
        .map_err(|e| io::Error::other(e.to_string()))?;
    // The copy came out of `VACUUM INTO`, which produces a rollback-journal
    // database. `quick_check` here is the belt to the container checksum's
    // braces: it proves the redaction is about to run against a sound file,
    // so a later "checksum mismatch" can only mean the bytes on disk changed.
    if !crate::services::state_recovery::quick_check(&conn).map_err(|e| {
        io::Error::other(format!("integrity check failed: {e}"))
    })? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "refusing to redact a database copy that fails its integrity check",
        ));
    }

    let mut removed = 0;
    for key in SECRET_DB_SETTINGS {
        removed += conn
            .execute("DELETE FROM app_settings WHERE key = ?1", [key])
            .map_err(|e| io::Error::other(e.to_string()))?;
    }
    // `device_sessions.token_hash` is already a hash and is the whole point of
    // the table — but a restore into a different install should not inherit
    // another machine's logged-in devices, so the rows go too. The table is
    // best-effort: a redacted export of a schema old enough not to have it
    // must still export.
    removed += conn
        .execute("DELETE FROM device_sessions", [])
        .unwrap_or(0);
    conn.execute_batch("VACUUM")
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(removed)
}

/// Strip credential fields from serialized `preferences.json` bytes.
///
/// Works on `serde_json::Value` rather than the `AppPreferences` struct on
/// purpose: an export must be able to round-trip a preferences file written
/// by a **newer** build (or one that has since dropped a field), and
/// round-tripping through today's struct would silently discard whatever it
/// does not know about. Unknown fields are preserved verbatim.
pub(crate) fn redact_preferences(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("preferences.json is not valid JSON: {e}"),
        )
    })?;
    let object = match value.as_object_mut() {
        Some(object) => object,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "preferences.json is not a JSON object",
            ))
        }
    };
    for field in SECRET_PREF_FIELDS {
        object.remove(*field);
    }
    if let Some(accounts) = object.get_mut("provider_accounts").and_then(Value::as_array_mut) {
        for account in accounts.iter_mut() {
            if let Some(entry) = account.as_object_mut() {
                entry.remove(SECRET_ACCOUNT_FIELD);
            }
        }
    }
    serde_json::to_vec_pretty(&value)
        .map_err(|e| io::Error::other(e.to_string()))
}

/// What a redaction pass removed, for the caller to log and the UI to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedactionReport {
    pub db_rows_removed: usize,
    pub preference_fields_removed: usize,
}
