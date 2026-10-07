//! Corruption classification, the last-known-good backup, and the explicit
//! recovery actions for `preferences.json` (issue #1523).
//!
//! Before this module existed, a truncated / malformed / forward-incompatible
//! `preferences.json` deserialized into `AppPreferences::default()`, that
//! default was published as the *authoritative* in-process cache, and the
//! next ordinary settings change atomically replaced the original file —
//! permanently losing provider accounts, API keys, pairings, harness
//! defaults, and Autopilot settings that a human could not recreate.
//!
//! The split of responsibility:
//!
//!   * [`classify`] is **pure** — bytes in, `AppPreferences` or a content-free
//!     [`CorruptPayload`] out. No filesystem, no globals, no logging. This is
//!     the seam the classification tests live on.
//!   * The file-level recovery actions take an explicit `&Path`, so they are
//!     testable against a `tempfile::TempDir` without touching the
//!     process-global cache that [`super::storage`] owns.
//!   * The decision of *what a caller is allowed to do* with a classification
//!     (read-only defaults vs. a refused write) lives in
//!     [`super::storage`], which owns the cache.
//!
//! **File contents never leave this module in a log line, an error message, or
//! the wire type.** A `preferences.json` written by a build older than issue
//! #830 contains plaintext API keys (see
//! [`crate::preferences::ProviderAccount::api_key`]), so every diagnostic here
//! carries at most a serde *category* plus a line/column position — never the
//! offending value, never the source text.

use super::model::AppPreferences;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ts_rs::TS;

/// Suffix of the last-known-good backup, written next to `preferences.json`.
///
/// Every successful preference write refreshes it with the exact bytes that
/// write persisted, so it is by construction the newest state Buildmesh
/// itself accepted — never a user hand-edit, and never a payload that failed
/// to deserialize. Restoring it is the non-destructive recovery path.
pub const BACKUP_SUFFIX: &str = "preferences.json.bak";

/// Prefix of an archived corrupt original. The full name appends a UTC
/// timestamp (`preferences.json.corrupt-20261004T185500Z`) so successive
/// resets never clobber an earlier archive.
pub const CORRUPT_ARCHIVE_PREFIX: &str = "preferences.json.corrupt-";

/// Why an on-disk `preferences.json` could not be turned into
/// [`AppPreferences`].
///
/// Generated to `src/types/generated/CorruptionReason.ts` so the Settings
/// pane can pick an actionable message without string-matching the detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "CorruptionReason.ts")]
pub enum CorruptionReason {
    /// The bytes are not valid JSON (truncated write, hand-edit, a binary
    /// blob), or not valid UTF-8 text. An **empty** file lands here too —
    /// a crash between `create` and the first atomic replace is the classic
    /// producer.
    InvalidJson,
    /// Valid JSON, but the top level is an array/string/number rather than the
    /// preferences object. Nothing in the schema can be read from it.
    NotAnObject,
    /// The payload is a JSON object but a field's type does not match the
    /// current schema (e.g. `"spawn_configurations": "not-an-array"`). This is
    /// also what a payload written by a *newer* Buildmesh with a
    /// type-changed field produces.
    SchemaMismatch,
}

impl CorruptionReason {
    /// Stable snake_case key, for logs and for the wire type. Kept as an
    /// explicit match rather than a `serde` round-trip so a rename of the
    /// variant can't silently change what a log line says.
    pub fn as_str(self) -> &'static str {
        match self {
            CorruptionReason::InvalidJson => "invalid_json",
            CorruptionReason::NotAnObject => "not_an_object",
            CorruptionReason::SchemaMismatch => "schema_mismatch",
        }
    }
}

/// A classified-but-unlocated corruption: the reason plus a message that is
/// safe to show a user and safe to log.
///
/// Returned by the pure [`classify`] seam. [`super::storage`] pairs it with
/// the file location to build the [`CorruptionInfo`] that reaches the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorruptPayload {
    pub reason: CorruptionReason,
    /// Human-readable and **content-free** — a serde category and a
    /// line/column, never the offending value.
    pub detail: String,
}

/// Everything the Settings pane needs to explain a corrupt
/// `preferences.json` and offer recovery. Generated to
/// `src/types/generated/CorruptionInfo.ts`.
///
/// `path` / `backup_path` are display-only conveniences (the same path the
/// `Open file location` action hands to the OS). No field carries file
/// content — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "CorruptionInfo.ts")]
pub struct CorruptionInfo {
    pub reason: CorruptionReason,
    /// Content-free explanation, safe to render verbatim.
    pub detail: String,
    /// Absolute path of the corrupt file.
    pub path: String,
    /// Size of the corrupt file in bytes. Reported so a user can tell
    /// "empty file" from "a real file that a text editor mangled" — the
    /// bytes themselves are never read into the UI.
    // Tagged `u32` so ts-rs emits `number`: `serde_json` sends a `u64` as a
    // JS number, not a BigInt, so the TS type must agree. A preferences
    // file is orders of magnitude below 4 GiB.
    #[ts(as = "u32")]
    pub byte_len: u64,
    /// True when a last-known-good backup exists beside the file and
    /// [`restore_backup`] can be offered.
    pub backup_available: bool,
    /// Absolute path of that backup, when present.
    pub backup_path: Option<String>,
}

impl CorruptPayload {
    /// Attach the file location, producing the wire type.
    pub(crate) fn into_info(self, path: &Path, backup_path: Option<PathBuf>) -> CorruptionInfo {
        CorruptionInfo {
            reason: self.reason,
            detail: self.detail,
            path: path.display().to_string(),
            byte_len: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            backup_available: backup_path.is_some(),
            backup_path: backup_path.map(|p| p.display().to_string()),
        }
    }
}

/// Result of a successful recovery action — the preferences that are now live
/// plus where the bytes that were replaced were preserved. Generated to
/// `src/types/generated/RecoveryOutcome.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "RecoveryOutcome.ts")]
pub struct RecoveryOutcome {
    pub preferences: AppPreferences,
    /// Absolute path of the archive holding the replaced file's bytes, when
    /// there was a file to archive. `None` when the file was simply missing.
    pub archive_path: Option<String>,
    /// One-line description of what happened, suitable for a success toast.
    pub message: String,
}

/// The three states the Settings pane distinguishes. `missing` is *not* an
/// error — a fresh install has no `preferences.json` and starts from
/// defaults — it is separated from `healthy` only so the pane can say
/// "nothing saved yet" instead of implying a file was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "PreferencesStatus.ts")]
pub enum PreferencesStatus {
    /// No file on disk yet.
    Missing,
    /// The file was read, migrated, and deserialized cleanly.
    Healthy,
    /// The file exists but could not be read; the bytes are preserved and
    /// settings writes are refused until it is recovered.
    Corrupt,
}

/// What the Settings pane needs to decide whether to render the corruption
/// recovery panel. Generated to `src/types/generated/PreferencesHealth.ts`.
///
/// Deliberately a **successful** result rather than an `Err`: a corrupt file
/// is a state the user must act on, not an IPC failure, and folding it into
/// a `Result::Err` string would make the UI classify it by matching prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PreferencesHealth.ts")]
pub struct PreferencesHealth {
    pub status: PreferencesStatus,
    /// Present exactly when `status` is `corrupt`.
    pub corruption: Option<CorruptionInfo>,
    /// Directory holding `preferences.json`, for the "open file location"
    /// action and for the path shown to the user.
    pub preferences_directory: String,
}

impl PreferencesHealth {
    /// Project a read result onto the wire type.
    pub(crate) fn from_state(path: &Path, state: &super::storage::LoadState) -> PreferencesHealth {
        let (status, corruption) = match state {
            super::storage::LoadState::Missing => (PreferencesStatus::Missing, None),
            super::storage::LoadState::Healthy(_) => (PreferencesStatus::Healthy, None),
            super::storage::LoadState::Corrupt(info) => {
                (PreferencesStatus::Corrupt, Some((**info).clone()))
            }
        };
        PreferencesHealth {
            status,
            corruption,
            preferences_directory: path.parent().unwrap_or(path).display().to_string(),
        }
    }
}

/// Turn raw `preferences.json` bytes into [`AppPreferences`], running the
/// read-time migration ([`super::migrations::migrate_prefs_json`]) between
/// parse and deserialize.
///
/// Pure — no filesystem, no globals, no logging — so every classification
/// rule is unit-testable against an in-memory string.
///
/// The migration runs **before** deserialization and its result is returned
/// but not persisted: a payload that deserializes cleanly is handed to the
/// caller, and it is an *explicit* preference write that persists the
/// migrated shape. A payload that fails to deserialize yields
/// [`CorruptPayload`] and the migrated `Value` is dropped on the floor —
/// nothing is written back (issue #1523).
///
/// Unknown fields are *not* an error: `AppPreferences` has no
/// `deny_unknown_fields`, so a file written by a newer Buildmesh whose new
/// fields are all additive loads cleanly and keeps the fields this build
/// understands. A forward version only reaches
/// [`CorruptionReason::SchemaMismatch`] when it changed the type of a field
/// this build already knows — and even then the file is preserved.
pub fn classify(raw: &[u8]) -> Result<AppPreferences, CorruptPayload> {
    classify_with(raw, |_| ())
}

/// [`classify`] with a hook that sees the parsed JSON **before** the read-time
/// migration. Credentials live outside the file (issue #830), and a legacy
/// migration reads `api_key` out of the raw JSON (ADR-0025 turns a keyed
/// account's endpoint into a pairing), so they must be restored first or a
/// scrubbed legacy file would migrate differently from a plaintext one.
pub fn classify_with(
    raw: &[u8],
    before_migration: impl FnOnce(&mut serde_json::Value),
) -> Result<AppPreferences, CorruptPayload> {
    let text = match std::str::from_utf8(raw) {
        Ok(text) => text,
        Err(e) => {
            return Err(CorruptPayload {
                reason: CorruptionReason::InvalidJson,
                detail: format!(
                    "the file is not valid UTF-8 text (invalid byte at offset {})",
                    e.valid_up_to()
                ),
            })
        }
    };
    if text.trim().is_empty() {
        return Err(CorruptPayload {
            reason: CorruptionReason::InvalidJson,
            detail: "the file is empty".to_string(),
        });
    }
    let mut value: serde_json::Value = serde_json::from_str(text).map_err(describe_parse_error)?;
    if !value.is_object() {
        return Err(CorruptPayload {
            reason: CorruptionReason::NotAnObject,
            detail: format!(
                "the file holds a JSON {} instead of a preferences object",
                json_type_name(&value)
            ),
        });
    }
    before_migration(&mut value);
    crate::preferences::migrations::migrate_prefs_json(&mut value);
    serde_json::from_value(value).map_err(describe_schema_error)
}

/// Content-free description of a JSON parse failure.
///
/// `serde_json::Error`'s `Display` for a *syntax* error quotes the offending
/// character, so it is deliberately not used; only the category and the
/// position are reported.
fn describe_parse_error(error: serde_json::Error) -> CorruptPayload {
    CorruptPayload {
        reason: CorruptionReason::InvalidJson,
        detail: format!(
            "the file is not valid JSON ({} at line {} column {})",
            category_name(error.classify()),
            error.line(),
            error.column()
        ),
    }
}

/// Content-free description of a post-migration deserialization failure.
///
/// `serde_json::Error`'s `Display` for a *data* error echoes the offending
/// value (`invalid type: string "sk-abc123…"`), which for a preferences file
/// can be an API key fragment. Only the category and position are reported.
fn describe_schema_error(error: serde_json::Error) -> CorruptPayload {
    CorruptPayload {
        reason: CorruptionReason::SchemaMismatch,
        detail: format!(
            "a stored field has a type this Buildmesh version cannot read \
             ({} at line {} column {})",
            category_name(error.classify()),
            error.line(),
            error.column()
        ),
    }
}

/// Name for a serde error category. `serde_json::error::Category` has no
/// `Display`, and the cases are spelled out because "malformed JSON" and
/// "unexpected data" are the difference between a damaged file and a
/// forward-incompatible one.
fn category_name(category: serde_json::error::Category) -> &'static str {
    use serde_json::error::Category;
    match category {
        Category::Io => "I/O error",
        Category::Syntax => "malformed JSON",
        Category::Eof => "unexpected end of file",
        Category::Data => "unexpected data",
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Absolute path of the last-known-good backup beside `path`.
pub fn backup_path(path: &Path) -> PathBuf {
    sibling(path, BACKUP_SUFFIX)
}

/// Absolute path the next archive of `path` would use — a UTC timestamp, with
/// a `-1`, `-2`, … suffix if that name is already taken so two resets in the
/// same second cannot clobber each other's archive.
pub fn corrupt_archive_path(path: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let first = sibling(path, &format!("{CORRUPT_ARCHIVE_PREFIX}{stamp}"));
    if !first.exists() {
        return first;
    }
    for attempt in 1..1000 {
        let candidate = sibling(path, &format!("{CORRUPT_ARCHIVE_PREFIX}{stamp}-{attempt}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

fn sibling(path: &Path, name: &str) -> PathBuf {
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Whether a usable last-known-good backup exists beside `path`.
///
/// Only a backup that itself classifies as healthy is "usable" — offering
/// "Restore" for a backup that would fail to deserialize is a dead end, and
/// the file it would restore from is the user's only remaining copy of their
/// real settings.
pub fn backup_available(path: &Path) -> Option<PathBuf> {
    let backup = backup_path(path);
    let raw = std::fs::read(&backup).ok()?;
    classify(&raw).ok()?;
    Some(backup)
}

/// Refresh the last-known-good backup from the exact bytes a successful write
/// just persisted.
///
/// Called **after** the atomic replacement, and only from the ordinary write
/// path — so a backup can never contain a payload that failed to
/// deserialize, and can never be refreshed by the refused writes a corrupt
/// file triggers.
pub(crate) fn write_last_known_good(path: &Path, bytes: &[u8]) -> Result<(), String> {
    persist_bytes(&backup_path(path), bytes)
}

/// Copy the file at `path` aside, byte for byte, before it is replaced. Used
/// by the two recovery actions so **no** path through the app can destroy
/// the only remaining copy of a file the user cannot regenerate.
///
/// Returns `None` when there is nothing to archive.
pub(crate) fn archive_existing(path: &Path) -> Result<Option<PathBuf>, String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("failed to read preferences before archiving: {e}")),
    };
    let archive = corrupt_archive_path(path);
    persist_bytes(&archive, &raw)?;
    Ok(Some(archive))
}

/// Replace the file at `path` with the last-known-good backup, archiving
/// whatever is there first.
///
/// The backup is classified before anything is overwritten: a backup that no
/// longer deserializes is an error, not a silent second data loss. The
/// bytes are persisted verbatim rather than re-serialized from the parsed
/// struct, so restoring cannot drop fields this build does not model.
pub fn restore_backup(path: &Path) -> Result<RecoveryOutcome, String> {
    let backup = backup_path(path);
    let raw = std::fs::read(&backup).map_err(|e| {
        format!(
            "no last-known-good backup to restore at {}: {e}",
            backup.display()
        )
    })?;
    let mut prefs = classify(&raw).map_err(|payload| {
        format!(
            "the last-known-good backup is itself unreadable ({}), so it was \
             not restored: {}",
            payload.reason.as_str(),
            payload.detail
        )
    })?;
    super::launch_configurations::reconcile(&mut prefs);
    let archive = archive_existing(path)?;
    persist_with_backup(path, &raw)?;
    Ok(RecoveryOutcome {
        message: match &archive {
            Some(archive) => format!(
                "Restored the last-known-good settings. The unreadable file was kept at {}",
                archive.display()
            ),
            None => "Restored the last-known-good settings.".to_string(),
        },
        preferences: prefs,
        archive_path: archive.map(|p| p.display().to_string()),
    })
}

/// Replace the file at `path` with defaults, archiving the existing file
/// first.
///
/// This is the **only** path through the app that writes defaults over a
/// file the app could not read, and it is never reached implicitly — the
/// caller is the explicit `reset_app_preferences` command, behind a
/// confirmation that spells out the data loss.
pub fn reset_to_defaults(path: &Path) -> Result<RecoveryOutcome, String> {
    let archive = archive_existing(path)?;
    let mut prefs = AppPreferences::default();
    super::launch_configurations::reconcile(&mut prefs);
    let bytes = serde_json::to_vec_pretty(&prefs)
        .map_err(|e| format!("failed to serialize default preferences: {e}"))?;
    persist_bytes(path, &bytes)?;
    // Deliberately *not* refreshing the last-known-good backup here: a reset
    // is the one write that knowingly discards data the user might still
    // want back, so it preserves every recovery artifact it can. The next
    // ordinary settings change re-establishes the backup.
    Ok(RecoveryOutcome {
        message: match &archive {
            Some(archive) => format!(
                "Settings reset to defaults. The unreadable file was kept at {}",
                archive.display()
            ),
            None => "Settings reset to defaults.".to_string(),
        },
        preferences: prefs,
        archive_path: archive.map(|p| p.display().to_string()),
    })
}

/// Atomically replace `preferences.json` with `bytes` and refresh the
/// last-known-good backup from the same bytes.
///
/// This is the **ordinary** write path. The backup is written second, from
/// the same buffer that just landed, so it is by construction the newest
/// state this Buildmesh accepted.
pub(crate) fn persist_with_backup(path: &Path, bytes: &[u8]) -> Result<(), String> {
    persist_bytes(path, bytes)?;
    write_last_known_good(path, bytes)
}

/// Atomically replace `path` with `bytes`: write a sibling temp file, flush
/// it, rename it over the target, then flush the parent directory.
///
/// The temp file is created by `tempfile`, which creates with mode `0600` on
/// Unix — so both `preferences.json` and its backup are owner-only, which
/// matters because on a build with no credential store both still hold
/// plaintext API keys (issue #830).
pub(crate) fn persist_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| {
        format!(
            "failed to create temporary file in {}: {e}",
            parent.display()
        )
    })?;
    temporary
        .write_all(bytes)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|e| {
            format!(
                "failed to write temporary file in {}: {e}",
                parent.display()
            )
        })?;
    temporary.persist(path).map_err(|e| {
        format!(
            "failed to atomically replace {}: {}",
            path.display(),
            e.error
        )
    })?;
    sync_parent_dir(parent);
    Ok(())
}

/// Flush the directory entry so the rename itself survives a power loss, not
/// just the file contents.
///
/// A no-op on Windows: `MoveFileExW` (behind `fs::rename`) has no equivalent
/// of `fsync` on a directory handle, and reopening a directory for
/// `GENERIC_WRITE` is denied without `FILE_FLAG_BACKUP_SEMANTICS`. The file
/// data itself is already flushed by `sync_all` above, so a crash can lose
/// the rename but not the bytes.
#[cfg(unix)]
fn sync_parent_dir(parent: &Path) {
    match std::fs::File::open(parent) {
        Ok(dir) => {
            if let Err(e) = dir.sync_all() {
                tracing::warn!(
                    "failed to fsync preferences directory {}: {e}",
                    parent.display()
                );
            }
        }
        Err(e) => tracing::warn!(
            "failed to open preferences directory {} for fsync: {e}",
            parent.display()
        ),
    }
}

#[cfg(not(unix))]
fn sync_parent_dir(_parent: &Path) {}
