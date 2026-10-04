//! `StateRecoveryService` — consistent snapshots, integrity validation, a
//! user-facing export, and a staged restore (issue #1537).
//!
//! ## What this owns
//!
//! Buildmesh's durable state is a profile-scoped `buildmesh.db` (SQLite), a
//! `preferences.json` beside it, and a `tls/` directory holding the LAN root
//! CA **private key**. This module owns the first two end to end — snapshot,
//! validate, export, restore, retain — and deliberately does *not* own the
//! third. See "What an export never carries" below.
//!
//! ## The four operations, and why they are shaped this way
//!
//! 1. **Snapshot** (`create_snapshot`, [`snapshot_before_migration`]) —
//!    `VACUUM INTO` produces a consistent copy that includes the WAL's
//!    committed content. A plain file copy of a WAL-mode database is *not*
//!    consistent: it misses everything still in `-wal`.
//! 2. **Validate** ([`check_integrity`]) — `PRAGMA quick_check` is the
//!    pre-migration gate; `PRAGMA integrity_check` is the explicit-diagnostics
//!    gate. Both report rather than repair. Nothing here ever rewrites a
//!    database to make a check pass.
//! 3. **Export** ([`export_to`]) — a user-chosen `.bmsnap` with credentials
//!    stripped by default. Not a zip of the app-data folder: that would
//!    sweep in `ca.key.der` and the cleartext remote-access token without the
//!    user ever seeing them.
//! 4. **Restore** ([`stage_restore`] + [`apply_pending_restore`]) — verify
//!    first, snapshot the current state as a rollback, *stage* the payload,
//!    and apply it on the next launch.
//!
//! ## Why restore is staged rather than immediate
//!
//! Applying a restored database underneath a running app means a live
//! writer, a reader pool, circuit workers, and session pollers all holding
//! handles to the file being replaced. Instead `stage_restore` only verifies
//! and writes a payload plus a marker; `apply_pending_restore` runs from
//! `run_profile_startup` **before `db::init`**, where no connection, no
//! worker, and no PTY exists yet. That is the "close workers/connections
//! safely" requirement satisfied structurally rather than by a shutdown race.
//! The user's restart is the confirmation step.
//!
//! ## Never silently reset
//!
//! Corrupt state is preserved, never discarded:
//!
//! - A pre-migration [`quick_check`] failure still writes a snapshot — via a
//!   raw byte copy when `VACUUM INTO` cannot run on a damaged file — and
//!   records a [`RecoveryNotice`] the UI surfaces. The original bytes are on
//!   disk before anything else happens.
//! - A bundle that fails structural validation, or carries a format version
//!   from a newer build, is rejected **before** the pending directory is
//!   touched. Rejection costs nothing and changes no state.
//!
//! ## What an export never carries
//!
//! Default exports omit, and the restore path reports as omitted:
//!
//! - `tls/ca.key.der` and the leaf key — exporting the LAN root CA private
//!   key would let anyone who has the file impersonate the HTTPS identity
//!   every paired device trusts. There is deliberately no "include secrets"
//!   toggle for it; a user who needs a full profile copy should copy their
//!   own app-data directory.
//! - Provider API keys and the root/coordinator/device tokens (see
//!   [`redact`]).
//! - Terminal transcripts, because durable state never held any: scrollback
//!   is xterm.js state and agent transcripts live in harness session
//!   directories outside the profile. [`export_to`] asserts the profile
//!   contains no transcript section rather than assuming it.
//! - Windows Credential Manager blobs (the OpenCode OAuth token and
//!   Antigravity's `gemini:antigravity` credential). These live outside any
//!   file Buildmesh can copy; credential-storage remediation is tracked
//!   separately in issue #830.
//!
//! A redacted export therefore restores **structure without credentials**:
//! Meshes, Agent Nodes, Circuits, and preferences come back, and the
//! user re-enters keys afterwards. Restoring mints a fresh root token, so
//! previously paired devices must re-authenticate.

mod bundle;
mod redact;

#[cfg(test)]
mod tests;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use ts_rs::TS;

pub(crate) use bundle::{
    BundleHeader, BundlePrologue, BundleReader, PendingRestore, SECTION_DB, SECTION_PREFS,
};
pub(crate) use redact::RedactionReport;

use crate::db::migrations::SCHEMA_VERSION;

/// Directory (under the app-data profile) holding automatic snapshots.
pub(crate) const SNAPSHOT_DIR: &str = "snapshots";
/// File extension for every container this module writes.
pub(crate) const BUNDLE_EXTENSION: &str = "bmsnap";
/// Marker file recording the outcome of the last startup recovery pass.
pub(crate) const NOTICE_FILE: &str = "recovery-notice.json";

/// How many automatic snapshots to keep.
///
/// Bounded because a pre-migration snapshot is written on **every** version
/// bump, and a user who stays on a rolling build would otherwise accumulate
/// one full database copy per release forever. Three covers "the upgrade
/// before this one" and "the one that broke" with a spare, which is the
/// rollback depth the pre-migration gate actually needs. Manual snapshots the
/// user creates are pruned by the same policy — a bound that the user cannot
/// exceed is a bound they cannot be surprised by.
pub(crate) const SNAPSHOT_RETENTION: usize = 3;

// ---------------------------------------------------------------------------
// Wire types — derived with ts-rs, never hand-declared in TS (issue #359).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "StateRecoveryInfo.ts")]
pub struct StateRecoveryInfo {
    /// The profile's app-data directory. This is what "Open data folder"
    /// reveals, so it is also the anchor for every other path below.
    pub app_data_dir: String,
    pub snapshot_dir: String,
    /// The schema version the live database is currently at.
    pub schema_version: u32,
    pub snapshot_count: u32,
    /// The retention cap, so the UI can say "keeping the newest N" honestly.
    pub retention: u32,
    /// True when a verified restore payload is staged and will apply on the
    /// next launch.
    pub pending_restore: bool,
    /// The last startup recovery finding, if there was one.
    pub notice: Option<RecoveryNotice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "StateSnapshot.ts")]
pub struct StateSnapshot {
    pub path: String,
    pub file_name: String,
    /// `pre-migration`, `manual`, `pre-restore`, or `pre-restore-raw` (the
    /// byte-copy fallback taken when a corrupt database cannot be `VACUUM`ed).
    pub kind: String,
    pub created_at: String,
    pub schema_version: u32,
    /// `#[ts(as = "i32")]` per the project convention for 64-bit wire ints —
    /// ts-rs would otherwise emit `bigint`, which breaks the frontend's
    /// arithmetic. Display-only field, so the implied 2 GiB ceiling is
    /// harmless; a real state database is orders of magnitude below it.
    #[ts(as = "i32")]
    pub size_bytes: u64,
    /// Snapshots are always full fidelity; only exports default to redacted.
    pub redacted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "StateIntegrityReport.ts")]
pub struct StateIntegrityReport {
    pub ok: bool,
    /// `quick` or `full`.
    pub scope: String,
    pub checked_at: String,
    /// Human-readable summary. On failure this is SQLite's own finding.
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "StateExportResult.ts")]
pub struct StateExportResult {
    pub path: String,
    /// See `StateSnapshot::size_bytes` for why this carries the 64-bit
    /// annotation.
    #[ts(as = "i32")]
    pub size_bytes: u64,
    pub schema_version: u32,
    pub created_at: String,
    pub redacted: bool,
    /// The sections actually written, so the UI can list them.
    pub sections: Vec<String>,
    /// Categories deliberately left out, stated plainly rather than implied.
    pub omitted: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "StateRestorePlan.ts")]
pub struct StateRestorePlan {
    pub bundle_path: String,
    pub format_version: u32,
    pub schema_version: u32,
    pub created_at: String,
    pub redacted: bool,
    /// Where the pre-restore rollback snapshot was written. A restore is
    /// always reversible.
    pub rollback_snapshot: String,
    pub requires_restart: bool,
    /// What applying this will change — credentials re-minted, devices
    /// re-authenticating, and so on.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "RecoveryNotice.ts")]
pub struct RecoveryNotice {
    /// `info` or `warning`.
    pub severity: String,
    pub message: String,
    /// Set when the notice concerns a preserved snapshot.
    pub snapshot_path: Option<String>,
    pub recorded_at: String,
}

// ---------------------------------------------------------------------------
// Timestamps — injected so tests pin them.
// ---------------------------------------------------------------------------

/// RFC 3339 UTC, second resolution. Filenames use the same shape so a sorted
/// listing is a chronological one.
pub(crate) fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A filename-safe rendering of [`timestamp`].
pub(crate) fn timestamp_slug() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

// ---------------------------------------------------------------------------
// Database capture.
// ---------------------------------------------------------------------------

/// Open a **private** connection to the database for snapshotting.
///
/// Deliberately not `db::read_conn()`: this connection performs its own I/O
/// (`VACUUM INTO` writes the destination file), and the project rule is that
/// filesystem I/O must never happen while a pooled/shared DB handle is held.
/// A private connection also means a snapshot cannot deadlock against the
/// writer mutex it is capturing.
fn open_capture_connection(db_path: &Path) -> io::Result<Connection> {
    let conn = Connection::open(db_path)
        .map_err(|e| io::Error::other(format!("could not open the database: {e}")))?;
    // Matches `db::apply_connection_pragmas`' busy timeout so a concurrent
    // write retries instead of surfacing `SQLITE_BUSY` to the user.
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(conn)
}

/// Write a consistent copy of the live database to `dest` via `VACUUM INTO`.
///
/// `VACUUM INTO` is the WAL-safe mechanism: it reads through the WAL and
/// emits a single self-contained file with no sidecars. It also runs as one
/// statement inside SQLite's own read transaction, so the result is a
/// transactionally consistent image.
pub(crate) fn capture_database(db_path: &Path, dest: &Path) -> io::Result<u64> {
    let conn = open_capture_connection(db_path)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // `VACUUM INTO` refuses to overwrite.
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    conn.execute("VACUUM INTO ?1", [path_string(dest)])
        .map_err(|e| io::Error::other(format!("VACUUM INTO failed: {e}")))?;
    std::fs::metadata(dest).map(|m| m.len())
}

/// The schema version recorded in a database file, or `None` when the file is
/// unreadable or has no `app_settings` table.
fn probe_schema_version(db_path: &Path) -> Option<u32> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()?
    .parse()
    .ok()
}

/// `PRAGMA quick_check` — the fast structural gate.
///
/// Returns `Ok(true)` only when SQLite reports the single row `ok`. A database
/// that cannot even run the pragma yields `Ok(false)` rather than propagating,
/// because "we could not check" and "the check failed" must both stop a
/// migration, and only the second is worth alarming the user about.
pub(crate) fn quick_check(conn: &Connection) -> rusqlite::Result<bool> {
    match conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)) {
        Ok(verdict) => Ok(verdict.eq_ignore_ascii_case("ok")),
        // A pragma that cannot even be prepared means the file is not a
        // database SQLite will touch.
        Err(rusqlite::Error::SqliteFailure(_, _)) => Ok(false),
        Err(other) => Err(other),
    }
}

/// `PRAGMA integrity_check` — the thorough gate, for explicit diagnostics.
pub(crate) fn full_check(conn: &Connection) -> rusqlite::Result<bool> {
    match conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
        Ok(verdict) => Ok(verdict.eq_ignore_ascii_case("ok")),
        Err(rusqlite::Error::SqliteFailure(_, _)) => Ok(false),
        Err(other) => Err(other),
    }
}

/// The full `integrity_check` output, for the message field. Only called when
/// the check has already failed.
fn describe_corruption(conn: &Connection) -> String {
    let mut statement = match conn.prepare("PRAGMA integrity_check(20)") {
        Ok(statement) => statement,
        Err(error) => return format!("integrity check could not run: {error}"),
    };
    let rows = statement.query_map([], |row| row.get::<_, String>(0));
    match rows {
        Ok(rows) => {
            let findings: Vec<String> = rows.filter_map(Result::ok).collect();
            if findings.is_empty() {
                "integrity check reported corruption but produced no detail".to_string()
            } else {
                findings.join("; ")
            }
        }
        Err(error) => format!("integrity check output could not be read: {error}"),
    }
}

// ---------------------------------------------------------------------------
// Bundle construction.
// ---------------------------------------------------------------------------

/// Build a container from the live state at `db_path` / `prefs_path`.
///
/// `redact` strips credentials from the **copy** (see [`redact`]); snapshots
/// pass `false` and exports default to `true`.
pub(crate) fn build_bundle(
    db_path: &Path,
    prefs_path: &Path,
    dest: &Path,
    kind: &str,
    redact: bool,
    created_at: &str,
    app_version: &str,
) -> io::Result<(BundleHeader, Option<RedactionReport>)> {
    // Stage beside the destination: `VACUUM INTO` needs a concrete path, and
    // a sibling keeps the later rename on one volume.
    let staging = dest.with_extension("bmsnap-db");
    capture_database(db_path, &staging)?;

    let redaction = if redact {
        let rows = redact::redact_database_copy(&staging)?;
        Some(RedactionReport {
            db_rows_removed: rows,
            preference_fields_removed: 0,
        })
    } else {
        None
    };

    let mut sections: Vec<(&str, Vec<u8>)> = vec![(SECTION_DB, std::fs::read(&staging)?)];

    // Preferences are optional on purpose: a profile whose first launch
    // crashed before preferences were written still has a restorable database,
    // and refusing to snapshot it would be a worse failure than a one-section
    // bundle. An absent section is recorded by its absence, and restore skips
    // what is not there.
    let mut redaction = redaction;
    match std::fs::read(prefs_path) {
        Ok(raw) => {
            let bytes = if redact {
                let cleaned = redact::redact_preferences(&raw)?;
                if let Some(report) = redaction.as_mut() {
                    report.preference_fields_removed = count_secret_preference_fields(&raw);
                }
                cleaned
            } else {
                raw
            };
            sections.push((SECTION_PREFS, bytes));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(?prefs_path, "no preferences.json to include in the bundle");
        }
        Err(error) => {
            // A preferences file we cannot read is not a reason to throw away a
            // good database snapshot, but it must not be silent either.
            tracing::warn!(?prefs_path, %error, "skipping unreadable preferences.json in bundle");
        }
    }

    let header = bundle::write_bundle(
        dest,
        BundlePrologue {
            kind: kind.to_string(),
            created_at: created_at.to_string(),
            app_version: app_version.to_string(),
            schema_version: probe_schema_version(db_path).unwrap_or(0),
            redacted: redact,
        },
        &sections,
    )?;

    // The staging database is a temp artifact either way; removing it after
    // the container is durable keeps a failed bundle from leaving a
    // credential-bearing database copy in the destination directory.
    let _ = std::fs::remove_file(&staging);
    Ok((header, redaction))
}

fn count_secret_preference_fields(raw: &[u8]) -> usize {
    let value: serde_json::Value = match serde_json::from_slice(raw) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let mut count = 0;
    if let Some(object) = value.as_object() {
        for field in ["minimax_api_key", "provider_accounts"] {
            if !object.contains_key(field) {
                continue;
            }
            if field == "minimax_api_key" {
                count += 1;
                continue;
            }
            count += object
                .get("provider_accounts")
                .and_then(serde_json::Value::as_array)
                .map(|accounts| {
                    accounts
                        .iter()
                        .filter(|a| {
                            a.get("api_key")
                                .map(|v| !v.is_null())
                                .unwrap_or(false)
                        })
                        .count()
                })
                .unwrap_or(0);
        }
    }
    count
}

// ---------------------------------------------------------------------------
// Retention.
// ---------------------------------------------------------------------------

/// The one ordering rule for snapshots: oldest first, so `prune_snapshots`
/// can drop from the front and the UI can render the reverse.
///
/// By the timestamp slug embedded in the filename first — a copy restored from
/// disk can carry an mtime older than its own label — then mtime, then the
/// full name as a deterministic final tiebreak. The mtime step matters
/// because two snapshots can share a second-resolution slug (see
/// `unique_snapshot_path`) and the later one must be the one that survives.
fn snapshot_sort_key(path: &Path) -> (String, std::time::SystemTime, String) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = name.split('-').next().unwrap_or("").to_string();
    let modified = path
        .metadata()
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    (slug, modified, name)
}

/// Delete the oldest snapshots beyond `keep`.
pub(crate) fn prune_snapshots(dir: &Path, keep: usize) -> io::Result<Vec<PathBuf>> {
    let mut entries: Vec<(String, std::time::SystemTime, String, PathBuf)> = Vec::new();
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(BUNDLE_EXTENSION) {
            continue;
        }
        let (slug, modified, name) = snapshot_sort_key(&path);
        entries.push((slug, modified, name, path));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    let mut removed = Vec::new();
    let overflow = entries.len().saturating_sub(keep);
    for (_, _, _, path) in entries.into_iter().take(overflow) {
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(error) => {
                tracing::warn!(?path, %error, "could not prune an old state snapshot");
            }
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Startup hooks.
// ---------------------------------------------------------------------------

/// Take a pre-migration snapshot when the database is about to be evolved.
///
/// Called from `run_profile_startup` immediately **before** `db::init`, and
/// it opens its own read-only connection to read `schema_version` — the
/// layering stays `commands → services → db` (nothing in `db::` grows a
/// dependency on `services::`), and the check is independently testable
/// without the process-global database singleton.
///
/// Returns `Ok(None)` in the ordinary case: a fresh install, or a database
/// already at the current version.
///
/// On a corrupt database it takes a **raw byte copy** instead of a
/// `VACUUM INTO` (which may itself fail on damaged pages) and records a
/// [`RecoveryNotice`]. The original bytes are preserved before anything else
/// runs; nothing is reset.
pub(crate) fn snapshot_before_migration(
    app_data_dir: &Path,
    db_path: &Path,
) -> io::Result<Option<StateSnapshot>> {
    if !is_real_database_file(db_path) {
        return Ok(None);
    }
    let Some(current) = probe_schema_version(db_path) else {
        // An unreadable version means we cannot tell whether a migration is
        // coming. Assume it is: preserving the file is the cheap side of that
        // bet, and `db::init`'s own probe treats this as version 0.
        tracing::warn!("could not read schema_version; snapshotting before migration anyway");
        return write_snapshot(
            app_data_dir,
            db_path,
            "pre-migration",
            false,
        )
        .map(Some);
    };
    if current >= SCHEMA_VERSION {
        return Ok(None);
    }
    tracing::info!(
        "state recovery: database is at schema v{current}, snapshotting before migrating to v{SCHEMA_VERSION}"
    );
    write_snapshot(app_data_dir, db_path, "pre-migration", false).map(Some)
}

/// Is this a real on-disk database with content? Excludes the `:memory:`
/// shared-cache URI `db::init` substitutes in tests, and fresh installs.
fn is_real_database_file(db_path: &Path) -> bool {
    let rendered = db_path.to_string_lossy();
    if rendered == ":memory:" || rendered.starts_with("file:") {
        return false;
    }
    std::fs::metadata(db_path)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

fn write_snapshot(
    app_data_dir: &Path,
    db_path: &Path,
    kind: &str,
    redact: bool,
) -> io::Result<StateSnapshot> {
    let snapshot_dir = app_data_dir.join(SNAPSHOT_DIR);
    std::fs::create_dir_all(&snapshot_dir)?;
    crate::http::tls::protect_private_directory(&snapshot_dir)?;
    let dir = snapshot_dir;

    let created_at = timestamp();
    let slug = timestamp_slug();
    let dest = unique_snapshot_path(&dir, &slug, kind);

    let outcome = build_bundle(
        db_path,
        &prefs_path(app_data_dir),
        &dest,
        kind,
        redact,
        &created_at,
        env!("CARGO_PKG_VERSION"),
    );

    let snapshot = match outcome {
        Ok((header, _)) => StateSnapshot {
            path: path_string(&dest),
            file_name: file_name(&dest),
            kind: kind.to_string(),
            created_at,
            schema_version: header.schema_version,
            size_bytes: std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0),
            redacted: redact,
        },
        Err(error) => {
            // `VACUUM INTO` can fail on a database whose pages are damaged.
            // The one thing that must never happen is losing the only copy of
            // the user's state, so fall back to a byte-for-byte copy. It is
            // not a *consistent* snapshot (it may miss committed WAL frames),
            // which is why it is labelled distinctly and the user is warned.
            tracing::error!(%error, "state recovery: VACUUM INTO snapshot failed, falling back to a raw byte copy");
            let raw_dest = unique_snapshot_path(&dir, &slug, &format!("{kind}-raw"));
            std::fs::copy(db_path, &raw_dest)?;
            crate::http::tls::protect_private_file(&raw_dest)?;
            record_notice(
                app_data_dir,
                &RecoveryNotice {
                    severity: "warning".to_string(),
                    message: format!(
                        "Buildmesh could not take a consistent snapshot of its database ({error}). \
                         A raw copy was preserved instead; restoring it may not include the most \
                         recent changes."
                    ),
                    snapshot_path: Some(path_string(&raw_dest)),
                    recorded_at: timestamp(),
                },
            )?;
            StateSnapshot {
                path: path_string(&raw_dest),
                file_name: file_name(&raw_dest),
                kind: format!("{kind}-raw"),
                created_at,
                schema_version: probe_schema_version(db_path).unwrap_or(0),
                size_bytes: std::fs::metadata(&raw_dest).map(|m| m.len()).unwrap_or(0),
                redacted: false,
            }
        }
    };

    if let Err(error) = prune_snapshots(&dir, SNAPSHOT_RETENTION) {
        tracing::warn!(%error, "state recovery: snapshot retention pass failed");
    }
    Ok(snapshot)
}

/// A snapshot path that does not collide with an existing one.
///
/// The timestamp slug has second resolution, so a user who clicks "Create
/// snapshot" twice quickly — or a pre-migration snapshot taken right after a
/// pre-restore one — would otherwise silently overwrite the first. A
/// suffixed candidate is a *real* defect here, not a nicety: the overwritten
/// file is a rollback point.
fn unique_snapshot_path(dir: &Path, slug: &str, kind: &str) -> PathBuf {
    let base = dir.join(format!("{slug}-{kind}.{BUNDLE_EXTENSION}"));
    if !base.exists() {
        return base;
    }
    for attempt in 2..1000 {
        let candidate = dir.join(format!("{slug}-{kind}-{attempt}.{BUNDLE_EXTENSION}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // A thousand files inside one second is not a state this app can reach;
    // fall back to a name that cannot collide rather than overwriting.
    dir.join(format!("{slug}-{kind}-{}.{BUNDLE_EXTENSION}", std::process::id()))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn prefs_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("preferences.json")
}

// ---------------------------------------------------------------------------
// Notice marker.
// ---------------------------------------------------------------------------

fn notice_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(NOTICE_FILE)
}

fn record_notice(app_data_dir: &Path, notice: &RecoveryNotice) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(notice)
        .map_err(io::Error::other)?;
    std::fs::write(notice_path(app_data_dir), bytes)
}

pub(crate) fn read_notice(app_data_dir: &Path) -> Option<RecoveryNotice> {
    let bytes = std::fs::read(notice_path(app_data_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn clear_notice(app_data_dir: &Path) {
    let _ = std::fs::remove_file(notice_path(app_data_dir));
}

// ---------------------------------------------------------------------------
// Staged restore.
// ---------------------------------------------------------------------------

/// Verify a bundle, snapshot the current state for rollback, and stage the
/// payload for the next launch.
///
/// Order is the whole point:
///
/// 1. **Verify first.** A bundle that is not a container, is truncated, has a
///    bad checksum, or comes from a newer build is rejected here — before
///    anything on disk has been touched. Rejection leaves current data
///    exactly as it was.
/// 2. **Rollback second.** The current state is snapshotted (full fidelity,
///    never redacted) so the restore itself is reversible.
/// 3. **Stage last.** Sections are extracted into the pending directory and
///    fsynced, and only then is the marker written. A crash before the marker
///    leaves an inert directory; there is no state in which a partial restore
///    can apply.
pub(crate) fn stage_restore(app_data_dir: &Path, bundle_path: &Path) -> Result<StateRestorePlan, String> {
    let header = bundle::verify_bundle(bundle_path).map_err(|e| e.to_string())?;
    if header.section(SECTION_DB).is_none() {
        return Err(format!(
            "this bundle has no `{SECTION_DB}` section, so there is nothing to restore"
        ));
    }

    let db_path = app_data_dir.join("buildmesh.db");
    let current_version = probe_schema_version(&db_path).unwrap_or(0);

    // A pre-restore rollback snapshot, taken even when the live database has
    // never been created — an empty profile has nothing worth rolling back to,
    // and `write_snapshot` would fail on a missing file.
    let rollback = if is_real_database_file(&db_path) {
        Some(write_snapshot(app_data_dir, &db_path, "pre-restore", false).map_err(|e| {
            format!("Could not preserve your current state before restoring: {e}")
        })?)
    } else {
        None
    };

    let pending = bundle::pending_dir(app_data_dir);
    // Clear any earlier staged restore first so a superseded payload cannot be
    // half-overwritten by this one.
    if pending.exists() {
        std::fs::remove_dir_all(&pending)
            .map_err(|e| format!("could not clear the previous staged restore: {e}"))?;
    }
    std::fs::create_dir_all(&pending)
        .map_err(|e| format!("could not create the staging directory: {e}"))?;

    let mut reader = BundleReader::open(bundle_path).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    // The database streams to disk (`extract_section_to`) — it is the large
    // payload and must never be buffered twice. `preferences.json` is a few
    // kilobytes, so it is read whole and written directly; running the
    // streaming path over it would only add an fsync we do not need.
    if header.section(SECTION_DB).is_some() {
        let dest = pending.join("buildmesh.db");
        reader
            .extract_section_to(SECTION_DB, &dest)
            .map_err(|e| format!("could not stage `{SECTION_DB}`: {e}"))?;
        crate::http::tls::protect_private_file(&dest)
            .map_err(|e| format!("could not secure the staged `{SECTION_DB}`: {e}"))?;
        files.push("buildmesh.db".to_string());
    }
    if header.section(SECTION_PREFS).is_some() {
        let bytes = reader
            .read_section(SECTION_PREFS)
            .map_err(|e| format!("could not stage `{SECTION_PREFS}`: {e}"))?;
        let dest = pending.join("preferences.json");
        std::fs::write(&dest, &bytes)
            .map_err(|e| format!("could not stage `{SECTION_PREFS}`: {e}"))?;
        crate::http::tls::protect_private_file(&dest)
            .map_err(|e| format!("could not secure the staged `{SECTION_PREFS}`: {e}"))?;
        files.push("preferences.json".to_string());
    }
    crate::http::tls::protect_private_directory(&pending)
        .map_err(|e| format!("could not secure the staging directory: {e}"))?;

    let marker = PendingRestore {
        source: path_string(bundle_path),
        created_at: timestamp(),
        redacted: header.redacted,
        schema_version: header.schema_version,
        files,
    };
    std::fs::write(
        pending.join(bundle::PENDING_MARKER),
        serde_json::to_vec_pretty(&marker)
            .map_err(|e| format!("could not describe the staged restore: {e}"))?,
    )
    .map_err(|e| format!("could not write the staged restore marker: {e}"))?;

    let mut warnings = vec![
        format!(
            "Buildmesh will restart to apply this. Meshes, Agent Nodes, and Circuits in the \
             bundle (schema v{}) replace the current state (schema v{current_version}).",
            header.schema_version
        ),
    ];
    if header.redacted {
        warnings.push(
            "This export has no credentials. After restarting you will need to re-enter your \
             provider API keys, and Buildmesh will mint a new remote-access token — paired \
             devices must sign in again."
                .to_string(),
        );
    }
    if rollback.is_none() {
        warnings.push(
            "There was no existing database to roll back to, so this restore cannot be undone."
                .to_string(),
        );
    }

    Ok(StateRestorePlan {
        bundle_path: path_string(bundle_path),
        format_version: header.format_version,
        schema_version: header.schema_version,
        created_at: header.created_at,
        redacted: header.redacted,
        rollback_snapshot: rollback
            .map(|s| s.path)
            .unwrap_or_default(),
        requires_restart: true,
        warnings,
    })
}

/// What [`apply_pending_restore`] did, for the startup log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoreApplied {
    pub source: String,
    pub applied: Vec<String>,
    pub redacted: bool,
}

/// Apply a staged restore. Called from `run_profile_startup` **before**
/// `db::init`, so no connection, worker, or PTY exists yet.
///
/// The WAL sidecars are removed before the new database is moved into place.
/// This is not optional: SQLite would otherwise replay the *old* database's
/// `-wal` frames on top of the restored file and corrupt it — the exact
/// "partial state" failure the staging design exists to prevent.
pub(crate) fn apply_pending_restore(
    app_data_dir: &Path,
) -> io::Result<Option<RestoreApplied>> {
    let pending = bundle::pending_dir(app_data_dir);
    if !pending.exists() {
        return Ok(None);
    }
    let marker_path = pending.join(bundle::PENDING_MARKER);
    let marker: PendingRestore = match std::fs::read(&marker_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(marker) => marker,
        None => {
            // No marker means an interrupted stage, which is inert by
            // construction. Drop it rather than guessing what it meant.
            tracing::warn!("state recovery: discarding an incomplete staged restore");
            std::fs::remove_dir_all(&pending)?;
            return Ok(None);
        }
    };

    let staged_db = pending.join("buildmesh.db");
    if !staged_db.exists() {
        tracing::warn!("state recovery: staged restore marker has no database payload; discarding");
        std::fs::remove_dir_all(&pending)?;
        return Ok(None);
    }
    // The staged payload was a verified container section, but the bytes on
    // disk since staging are not. Re-check before they become live state.
    if !is_restorable_database(&staged_db)? {
        tracing::error!("state recovery: staged database fails its integrity check; keeping current state");
        std::fs::remove_dir_all(&pending)?;
        return Ok(None);
    }

    let db_path = app_data_dir.join("buildmesh.db");
    for sidecar in ["-wal", "-shm"] {
        let stale = PathBuf::from(format!("{}{sidecar}", db_path.to_string_lossy()));
        if stale.exists() {
            std::fs::remove_file(&stale)?;
        }
    }
    std::fs::rename(&staged_db, &db_path)?;

    let mut applied = vec!["buildmesh.db".to_string()];
    let staged_prefs = pending.join("preferences.json");
    if staged_prefs.exists() {
        std::fs::rename(&staged_prefs, prefs_path(app_data_dir))?;
        applied.push("preferences.json".to_string());
    }

    // Only now is the pending directory fully drained; removing it afterwards
    // means a crash mid-apply leaves the marker for the next launch to find,
    // and the already-applied database re-application is idempotent.
    std::fs::remove_dir_all(&pending)?;
    clear_notice(app_data_dir);

    tracing::info!(
        source = %marker.source,
        redacted = marker.redacted,
        ?applied,
        "state recovery: applied a staged restore"
    );
    Ok(Some(RestoreApplied {
        source: marker.source,
        applied,
        redacted: marker.redacted,
    }))
}

/// Would SQLite accept this file as a database, and is it sound? Used on the
/// staged payload so a file that changed after staging cannot become live
/// state.
fn is_restorable_database(path: &Path) -> io::Result<bool> {
    let conn = match Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(conn) => conn,
        Err(_) => return Ok(false),
    };
    // Both gates: `quick_check` for speed on a possibly-large file, then the
    // full pass because this is the one moment we are about to trust a file
    // with the user's entire state.
    match quick_check(&conn) {
        Ok(true) => full_check(&conn).map_err(|e| io::Error::other(e.to_string())),
        Ok(false) => Ok(false),
        Err(_) => Ok(false),
    }
}

// ---------------------------------------------------------------------------
// Command-facing surface.
// ---------------------------------------------------------------------------

/// Resolve the app-data directory, or explain why it is not available.
fn require_app_data_dir() -> Result<PathBuf, String> {
    crate::preferences::app_data_dir().ok_or_else(|| {
        "Buildmesh's data folder is not available yet. Try again once the app has finished starting."
            .to_string()
    })
}

pub fn info() -> Result<StateRecoveryInfo, String> {
    let app_data_dir = require_app_data_dir()?;
    Ok(build_info(&app_data_dir))
}

pub(crate) fn build_info(app_data_dir: &Path) -> StateRecoveryInfo {
    let db_path = app_data_dir.join("buildmesh.db");
    StateRecoveryInfo {
        app_data_dir: path_string(app_data_dir),
        snapshot_dir: path_string(&app_data_dir.join(SNAPSHOT_DIR)),
        schema_version: probe_schema_version(&db_path).unwrap_or(0),
        snapshot_count: list_snapshots_in(&app_data_dir.join(SNAPSHOT_DIR))
            .map(|s| s.len() as u32)
            .unwrap_or(0),
        retention: SNAPSHOT_RETENTION as u32,
        pending_restore: bundle::pending_dir(app_data_dir).join(bundle::PENDING_MARKER).exists(),
        notice: read_notice(app_data_dir),
    }
}

pub fn list_snapshots() -> Result<Vec<StateSnapshot>, String> {
    let app_data_dir = require_app_data_dir()?;
    list_snapshots_in(&app_data_dir.join(SNAPSHOT_DIR))
        .map_err(|e| format!("Could not read the snapshot list: {e}"))
}

/// Read each snapshot's own header rather than trusting its filename, so the
/// UI shows what the file actually is.
///
/// Returned **newest first**, using the same ordering rule as
/// [`prune_snapshots`] — one rule, so the list the user reads and the set the
/// retention policy protects can never disagree about which is oldest.
pub(crate) fn list_snapshots_in(dir: &Path) -> io::Result<Vec<StateSnapshot>> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut keyed: Vec<((String, std::time::SystemTime, String), PathBuf)> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(BUNDLE_EXTENSION))
        .map(|p| (snapshot_sort_key(&p), p))
        .collect();
    // Ascending, then reversed: the sort key reads the file's metadata, so
    // computing it once and reversing is both cheaper and clearer than a
    // descending comparator.
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let paths: Vec<PathBuf> = keyed.into_iter().map(|(_, p)| p).rev().collect();

    let mut snapshots = Vec::with_capacity(paths.len());
    for path in paths {
        let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match bundle::BundleReader::open(&path) {
            Ok(reader) => {
                let header = reader.header();
                snapshots.push(StateSnapshot {
                    path: path_string(&path),
                    file_name: file_name(&path),
                    kind: header.kind.clone(),
                    created_at: header.created_at.clone(),
                    schema_version: header.schema_version,
                    size_bytes,
                    redacted: header.redacted,
                });
            }
            Err(error) => {
                // A snapshot we cannot read is reported, not hidden — and not
                // deleted. It may be the only copy of something.
                tracing::warn!(?path, %error, "state recovery: snapshot is unreadable");
                snapshots.push(StateSnapshot {
                    path: path_string(&path),
                    file_name: file_name(&path),
                    kind: "unreadable".to_string(),
                    created_at: String::new(),
                    schema_version: 0,
                    size_bytes,
                    redacted: false,
                });
            }
        }
    }
    Ok(snapshots)
}

pub fn create_snapshot() -> Result<StateSnapshot, String> {
    let app_data_dir = require_app_data_dir()?;
    let db_path = app_data_dir.join("buildmesh.db");
    if !is_real_database_file(&db_path) {
        return Err("There is no stored state to snapshot yet.".to_string());
    }
    write_snapshot(&app_data_dir, &db_path, "manual", false)
        .map_err(|e| format!("Could not create a snapshot: {e}"))
}

pub fn check_integrity(full: bool) -> Result<StateIntegrityReport, String> {
    let app_data_dir = require_app_data_dir()?;
    let db_path = app_data_dir.join("buildmesh.db");
    let checked_at = timestamp();
    if !is_real_database_file(&db_path) {
        return Ok(StateIntegrityReport {
            ok: true,
            scope: if full { "full" } else { "quick" }.to_string(),
            checked_at,
            message: "There is no stored database yet, so there is nothing to check.".to_string(),
        });
    }
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("Could not open the database: {e}"))?;

    let (ok, message) = if full {
        let ok = full_check(&conn).map_err(|e| format!("Integrity check failed to run: {e}"))?;
        let message = if ok {
            "The database passed a full integrity check.".to_string()
        } else {
            format!("Database damage found: {}", describe_corruption(&conn))
        };
        (ok, message)
    } else {
        let ok = quick_check(&conn).map_err(|e| format!("Quick check failed to run: {e}"))?;
        let message = if ok {
            "The database passed a quick check.".to_string()
        } else {
            format!("Database damage found: {}", describe_corruption(&conn))
        };
        (ok, message)
    };

    Ok(StateIntegrityReport {
        ok,
        scope: if full { "full" } else { "quick" }.to_string(),
        checked_at,
        message,
    })
}

/// The categories a default export leaves behind. Stated in the result rather
/// than implied by the file's contents, so the UI can show them and a user
/// is never surprised by what is *not* in the file.
pub(crate) fn omitted_categories() -> Vec<String> {
    vec![
        "Provider API keys and account credentials (from preferences.json)".to_string(),
        "The remote-access root token, coordinator tokens, and paired-device sessions (from the database)"
            .to_string(),
        "The LAN HTTPS root certificate and its private key".to_string(),
        "Terminal scrollback and agent transcripts (never part of stored state)".to_string(),
        "Windows Credential Manager entries (stored outside the data folder)".to_string(),
    ]
}

pub fn export_to(dest: &Path, redact: bool) -> Result<StateExportResult, String> {
    let app_data_dir = require_app_data_dir()?;
    let db_path = app_data_dir.join("buildmesh.db");
    if !is_real_database_file(&db_path) {
        return Err("There is no stored state to export yet.".to_string());
    }
    let created_at = timestamp();
    let (header, report) = build_bundle(
        &db_path,
        &prefs_path(&app_data_dir),
        dest,
        "export",
        redact,
        &created_at,
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| format!("Export failed: {e}"))?;
    if let Some(report) = report {
        tracing::info!(
            db_rows_removed = report.db_rows_removed,
            preference_fields_removed = report.preference_fields_removed,
            "state recovery: wrote a redacted export"
        );
    }
    Ok(StateExportResult {
        path: path_string(dest),
        size_bytes: std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0),
        schema_version: header.schema_version,
        created_at: header.created_at,
        redacted: header.redacted,
        sections: header.sections.iter().map(|s| s.name.clone()).collect(),
        omitted: omitted_categories(),
    })
}

pub fn inspect_bundle(path: &Path) -> Result<StateRestorePlan, String> {
    let header = bundle::verify_bundle(path).map_err(|e| e.to_string())?;
    let mut warnings = Vec::new();
    if header.redacted {
        warnings.push(
            "This export has no credentials: provider keys must be re-entered, and \
             Buildmesh will mint a new remote-access token."
                .to_string(),
        );
    }
    if !header.sections.iter().any(|s| s.name == SECTION_DB) {
        warnings.push("This bundle contains no database, so restoring it would not change any state.".to_string());
    }
    Ok(StateRestorePlan {
        bundle_path: path_string(path),
        format_version: header.format_version,
        schema_version: header.schema_version,
        created_at: header.created_at,
        redacted: header.redacted,
        rollback_snapshot: String::new(),
        requires_restart: true,
        warnings,
    })
}

pub fn stage_restore_from(path: &Path) -> Result<StateRestorePlan, String> {
    let app_data_dir = require_app_data_dir()?;
    stage_restore(&app_data_dir, path)
}

/// Discard a staged restore without applying it.
pub fn cancel_pending_restore() -> Result<(), String> {
    let app_data_dir = require_app_data_dir()?;
    let pending = bundle::pending_dir(&app_data_dir);
    if pending.exists() {
        std::fs::remove_dir_all(&pending)
            .map_err(|e| format!("Could not cancel the staged restore: {e}"))?;
    }
    Ok(())
}

/// Default export filename offered in the save dialog.
pub fn default_export_name() -> String {
    format!("buildmesh-state-{}.{BUNDLE_EXTENSION}", timestamp_slug())
}
