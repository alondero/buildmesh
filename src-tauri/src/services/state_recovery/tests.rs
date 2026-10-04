//! `StateRecoveryService` contract tests (issue #1537).
//!
//! These pin the five acceptance behaviours the issue names:
//!
//! 1. Upgrading from an old schema leaves a restorable pre-migration snapshot.
//! 2. A snapshot survives a failure after the migration phases (the bytes
//!    that existed before the upgrade are still there, intact).
//! 3. Export/import round-trips non-secret Mesh/Node/Circuit settings.
//! 4. A default export contains no provider token, root token, device token,
//!    CA key, or terminal transcript.
//! 5. A corrupt or unsupported bundle is rejected without altering current
//!    data.
//!
//! Every test uses a private temp directory and its own database file — the
//! suite is parallel-safe and never touches the process-global `DB` singleton.

use super::*;
use rusqlite::params;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A profile directory holding a database at the current schema, with one
/// mesh, one agent node, one circuit, and the credentials a real install has.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    db_path: PathBuf,
    prefs_path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let db_path = root.join("buildmesh.db");
        let prefs_path = root.join("preferences.json");
        let fixture = Self {
            _dir: dir,
            root,
            db_path,
            prefs_path,
        };
        fixture.seed_current_schema();
        fixture
    }

    /// Create a database that already carries the current schema.
    fn seed_current_schema(&self) {
        let conn = Connection::open(&self.db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO meshes (name, path, color) VALUES ('alpha', 'C:/src/alpha', '#ff0000')",
            [],
        )
        .unwrap();
        let mesh_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, status) \
             VALUES (?1, 'worker-1', 'C:/src/alpha/.claude/worktrees/worker-1', 'idle')",
            params![mesh_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO autopilot_circuits (mesh_id, name, enabled) VALUES (?1, 'triage', 0)",
            params![mesh_id],
        )
        .unwrap();
        for (key, value) in [
            ("remote_access_token", "root-token-SECRET"),
            ("coordinator_read_token", "read-hash-SECRET"),
            ("coordinator_drive_token", "drive-hash-SECRET"),
            ("lan_exposure_enabled", "0"),
        ] {
            conn.execute(
                "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO device_sessions (label, token_hash, created_at) \
             VALUES ('Pixel', 'device-hash-SECRET', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        drop(conn);
    }

    fn write_preferences(&self, body: &str) {
        std::fs::write(&self.prefs_path, body).unwrap();
    }

    fn standard_preferences(&self) {
        self.write_preferences(
            r#"{
              "default_provider": "anthropic",
              "minimax_api_key": "minimax-SECRET",
              "worktree_directory": "C:/src/wt",
              "provider_accounts": [
                {"id": "anthropic", "name": "Anthropic", "enabled": true,
                 "billing_mode": "subscription", "claude_compatible": true,
                 "api_key": "sk-ant-SECRET"},
                {"id": "openai", "name": "OpenAI", "enabled": true,
                 "billing_mode": "api_key", "claude_compatible": false,
                 "api_key": "sk-oai-SECRET"}
              ],
              "spawn_configurations": [
                {"id": "cfg-1", "name": "Fast", "spawn_option_id": "claude:anthropic",
                 "model": "haiku", "effort": "low"}
              ]
            }"#,
        );
    }

    fn scalar(&self, sql: &str) -> i64 {
        Connection::open(&self.db_path)
            .unwrap()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    fn setting(&self, key: &str) -> Option<String> {
        Connection::open(&self.db_path)
            .unwrap()
            .query_row(
                "SELECT value FROM app_settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok()
    }
}

fn read_string(path: &Path) -> String {
    String::from_utf8(std::fs::read(path).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------
// 1. Pre-migration snapshot
// ---------------------------------------------------------------------------

/// A database at an older schema must be snapshotted before `db::init`
/// evolves it — and the snapshot must be restorable.
#[test]
fn old_schema_is_snapshotted_before_migration() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let db_path = root.join("buildmesh.db");

    // Build a genuinely old database: the tables, but no `schema_version`
    // bump, so `probe_schema_version` reports 0 and the runner would treat it
    // as "upgrade everything".
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE meshes (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL,
                 path TEXT NOT NULL UNIQUE,
                 layout TEXT NOT NULL DEFAULT 'grid',
                 position INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 color TEXT
             );
             INSERT INTO meshes (name, path) VALUES ('legacy-mesh', 'C:/src/legacy');
             INSERT INTO app_settings (key, value) VALUES ('schema_version', '12');",
        )
        .unwrap();
    }
    std::fs::write(
        root.join("preferences.json"),
        r#"{"default_provider":"anthropic"}"#,
    )
    .unwrap();

    let snapshot =
        snapshot_before_migration(root, &db_path).unwrap().expect("a snapshot is expected");

    assert_eq!(snapshot.kind, "pre-migration");
    assert_eq!(snapshot.schema_version, 12, "records the pre-upgrade version");
    assert!(Path::new(&snapshot.path).exists());
    assert!(!snapshot.redacted, "a rollback snapshot is full fidelity");

    // The snapshot is a real container carrying the pre-upgrade data.
    let header = bundle::verify_bundle(Path::new(&snapshot.path)).unwrap();
    assert_eq!(header.schema_version, 12);
    assert!(header.section(SECTION_DB).is_some());
    assert!(header.section(SECTION_PREFS).is_some());

    // The bundled database is a standalone SQLite file, not a fragment: the
    // pre-upgrade rows read back out of it.
    let extracted = extract_db_to_temp(Path::new(&snapshot.path));
    let restored = Connection::open(&extracted).unwrap();
    let mesh_name: String = restored
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mesh_name, "legacy-mesh");
    let version: String = restored
        .query_row("SELECT value FROM app_settings WHERE key = 'schema_version'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(version, "12", "the snapshot records the pre-upgrade version");
}

/// The upgrade itself: after `db::init`, the snapshot still restores the
/// original rows, and the live database has moved on. This is the issue's
/// "original or snapshot can restore without partial state" case, with the
/// migration phases run for real.
#[test]
fn snapshot_survives_a_real_migration_and_restores_pre_upgrade_rows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let db_path = root.join("buildmesh.db");

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE meshes (id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
                 layout TEXT NOT NULL DEFAULT 'grid',
                 position INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 color TEXT);
             INSERT INTO meshes (name, path) VALUES ('pre-upgrade-mesh', 'C:/src/pre');
             INSERT INTO app_settings (key, value) VALUES ('schema_version', '3');",
        )
        .unwrap();
    }

    let snapshot = snapshot_before_migration(root, &db_path)
        .unwrap()
        .expect("a snapshot is expected");
    let snapshot_db = extract_db_to_temp(Path::new(&snapshot.path));

    // Now run the real migration phases.
    {
        let conn = Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
    }
    let live: Connection = Connection::open(&db_path).unwrap();
    let live_version: i32 = live
        .query_row("SELECT value FROM app_settings WHERE key = 'schema_version'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        live_version, SCHEMA_VERSION as i32,
        "the live database advanced to the current schema"
    );
    let name: String = live
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "pre-upgrade-mesh", "the row survived the migration");
    drop(live);

    // The snapshot restores the *pre-upgrade* state: the old columns are
    // exactly the ones it recorded, with no partial new-schema state mixed in.
    let restored = Connection::open(&snapshot_db).unwrap();
    let columns: Vec<String> = restored
        .prepare("SELECT name FROM pragma_table_info('meshes')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        !columns.contains(&"worktree_directory".to_string()),
        "the snapshot is the old schema, not a half-migrated one; got {columns:?}"
    );
    let version: String = restored
        .query_row("SELECT value FROM app_settings WHERE key = 'schema_version'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(version, "3");
}

#[test]
fn no_snapshot_when_the_database_is_already_current() {
    let fixture = Fixture::new();
    let snapshot = snapshot_before_migration(&fixture.root, &fixture.db_path).unwrap();
    assert!(
        snapshot.is_none(),
        "a current database is not snapshotted on every launch"
    );
    assert!(
        !fixture.root.join(SNAPSHOT_DIR).exists(),
        "and no snapshot directory is created for it"
    );
}

#[test]
fn a_fresh_install_is_not_snapshotted() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("buildmesh.db");
    assert!(snapshot_before_migration(dir.path(), &missing).unwrap().is_none());
    assert!(!dir.path().join(SNAPSHOT_DIR).exists());
}

// ---------------------------------------------------------------------------
// 2. Snapshot fidelity
// ---------------------------------------------------------------------------

/// `VACUUM INTO` is WAL-safe: a snapshot taken while committed transactions
/// are still only in the `-wal` file contains them. A plain file copy would
/// not, which is why this is a distinct guarantee.
#[test]
fn snapshot_includes_committed_wal_content() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let db_path = root.join("buildmesh.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn.execute("INSERT INTO meshes (name, path) VALUES ('wal-mesh', 'C:/src/wal')", [])
            .unwrap();
        // Leave the WAL un-checkpointed: the writer connection is still open
        // and the frames have not been folded into the main file.
        assert!(Path::new(&format!("{}-wal", db_path.to_string_lossy())).exists());
    }

    let snapshot = write_snapshot(root, &db_path, "manual", false).unwrap();
    let extracted = extract_db_to_temp(Path::new(&snapshot.path));
    let restored = Connection::open(&extracted).unwrap();
    let name: String = restored
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        name, "wal-mesh",
        "committed WAL-only content must be in the snapshot"
    );
}

#[test]
fn manual_snapshot_is_full_fidelity_and_keeps_credentials() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let snapshot = create_snapshot_in(&fixture.root, "manual", false).unwrap();
    assert!(!snapshot.redacted);
    let extracted = extract_db_to_temp(Path::new(&snapshot.path));
    let restored = Connection::open(&extracted).unwrap();
    let token: String = restored
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'remote_access_token'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        token, "root-token-SECRET",
        "a rollback snapshot must be able to restore the user's own credentials"
    );
    assert!(read_string(&fixture.prefs_path).contains("sk-ant-SECRET"));
}

/// Retention: automatic snapshots are bounded, oldest first.
#[test]
fn retention_keeps_the_newest_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let snap_dir = dir.path().join(SNAPSHOT_DIR);
    std::fs::create_dir_all(&snap_dir).unwrap();
    for slug in ["20260101T000000Z", "20260102T000000Z", "20260103T000000Z", "20260104T000000Z"] {
        let (header, _) = build_bundle(
            &fixture_db(snap_dir.parent().unwrap()),
            &nonexistent(snap_dir.parent().unwrap()),
            &snap_dir.join(format!("{slug}-manual.{BUNDLE_EXTENSION}")),
            "manual",
            false,
            "2026-01-01T00:00:00Z",
            "test",
        )
        .unwrap();
        assert_eq!(header.format_version, bundle::FORMAT_VERSION);
    }
    assert_eq!(std::fs::read_dir(&snap_dir).unwrap().count(), 4);

    let removed = prune_snapshots(&snap_dir, 3).unwrap();
    assert_eq!(removed.len(), 1);
    assert!(
        removed[0].to_string_lossy().contains("20260101T000000Z"),
        "the oldest is pruned first; removed {:?}",
        removed[0]
    );
    let remaining: Vec<String> = std::fs::read_dir(&snap_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(remaining.len(), 3);
    assert!(!remaining.iter().any(|n| n.contains("20260101")));
}

/// A corrupt database must never be silently reset. When `VACUUM INTO`
/// cannot run, the raw bytes are preserved and the user is told.
#[test]
fn a_corrupt_database_is_preserved_and_reported_never_reset() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let db_path = root.join("buildmesh.db");
    // Valid SQLite header, then garbage: `VACUUM INTO` fails on this, which is
    // exactly the fallback path the issue's "never silently reset" rule needs.
    let mut bytes = b"SQLite format 3\0".to_vec();
    bytes.extend_from_slice(&[0xAB; 4096]);
    std::fs::write(&db_path, &bytes).unwrap();
    let original = std::fs::read(&db_path).unwrap();

    let snapshot = write_snapshot(root, &db_path, "manual", false).unwrap();

    assert_eq!(snapshot.kind, "manual-raw", "fell back to a byte copy");
    let preserved = std::fs::read(&snapshot.path).unwrap();
    assert_eq!(
        preserved, original,
        "the damaged original is preserved byte for byte"
    );
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        original,
        "and the live file is untouched — nothing was reset"
    );
    let notice = read_notice(root).expect("the user is told what happened");
    assert_eq!(notice.severity, "warning");
    assert!(notice.message.contains("raw copy"));
    assert_eq!(notice.snapshot_path, Some(snapshot.path.clone()));
}

// ---------------------------------------------------------------------------
// 3. Export / import round-trip
// ---------------------------------------------------------------------------

#[test]
fn export_import_round_trips_non_secret_state() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("exported.bmsnap");

    let result = export_to_path(&fixture.root, &dest, true).unwrap();
    assert!(result.redacted);
    assert!(result.sections.contains(&SECTION_DB.to_string()));
    assert!(result.sections.contains(&SECTION_PREFS.to_string()));
    assert!(Path::new(&result.path).exists());

    // Drift the live profile away from the export, so the round-trip cannot
    // pass by accident. The database stays in place deliberately: that is the
    // realistic restore case, and it is the only way a rollback snapshot has
    // something to preserve.
    {
        let conn = Connection::open(&fixture.db_path).unwrap();
        conn.execute("UPDATE meshes SET name = 'drifted-away'", [])
            .unwrap();
        conn.execute("DELETE FROM agent_nodes", []).unwrap();
    }
    fixture.write_preferences(r#"{"default_provider": "drifted"}"#);

    let plan = stage_restore(&fixture.root, &dest).unwrap();
    assert!(plan.requires_restart);
    assert!(plan.redacted);
    assert!(
        Path::new(&plan.rollback_snapshot).exists(),
        "a redacted restore is still reversible — the current state is kept first"
    );

    let applied = apply_pending_restore(&fixture.root).unwrap().unwrap();
    assert_eq!(applied.applied, vec!["buildmesh.db", "preferences.json"]);

    // Non-secret database state came back.
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM meshes"), 1);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM agent_nodes"), 1);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM autopilot_circuits"), 1);
    let mesh_name: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mesh_name, "alpha");
    let node_status: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row("SELECT status FROM agent_nodes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(node_status, "idle");

    // The rollback snapshot captured the *drifted* state, so a restore really
    // is undoable rather than merely advertised as such.
    let rollback = extract_db_to_temp(Path::new(&plan.rollback_snapshot));
    let rolled_back = Connection::open(&rollback).unwrap();
    let rollback_name: String = rolled_back
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        rollback_name, "drifted-away",
        "the pre-restore snapshot holds the state that was replaced"
    );

    // Non-secret preferences came back, byte-identical in the fields that matter.
    let prefs = read_string(&fixture.prefs_path);
    assert!(!prefs.contains("drifted"), "the drifted preferences were replaced");
    assert!(prefs.contains("\"default_provider\": \"anthropic\""));
    assert!(prefs.contains("\"worktree_directory\": \"C:/src/wt\""));
    assert!(prefs.contains("\"spawn_configurations\""));
    assert!(prefs.contains("\"haiku\""), "circuit/spawn config survived");
}

#[test]
fn the_restored_database_is_evolved_forward_to_the_current_schema() {
    let fixture = Fixture::new();
    let dest = fixture.root.join("old.bmsnap");
    build_bundle(
        &fixture.db_path,
        &fixture.prefs_path,
        &dest,
        "export",
        true,
        "2026-01-01T00:00:00Z",
        "test",
    )
    .unwrap();

    stage_restore(&fixture.root, &dest).unwrap();
    apply_pending_restore(&fixture.root).unwrap().unwrap();

    // A bundle from an older build is legal to restore; the migration runner
    // then evolves it, because `db::init` runs after this hook.
    let conn = Connection::open(&fixture.db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let version: i32 = conn
        .query_row("SELECT value FROM app_settings WHERE key = 'schema_version'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION as i32);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM meshes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

// ---------------------------------------------------------------------------
// 4. Redaction — the leak gate
// ---------------------------------------------------------------------------

/// The issue's acceptance criterion, asserted on the bytes of a real default
/// export: no provider token, root token, device token, or CA key.
#[test]
fn a_default_export_contains_no_credentials() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    // A CA private key in the profile, as a real LAN-exposure install has.
    let tls_dir = fixture.root.join("tls");
    std::fs::create_dir_all(&tls_dir).unwrap();
    std::fs::write(tls_dir.join("ca.key.der"), b"CA-PRIVATE-KEY-SECRET").unwrap();

    let dest = fixture.root.join("safe.bmsnap");
    let result = export_to_path(&fixture.root, &dest, true).unwrap();
    let bytes = std::fs::read(&dest).unwrap();
    // A raw byte scan is the strongest form of this assertion: it catches a
    // secret in a section name, a header field, or a padding region, not just
    // in the places we thought to look. The failure message includes the
    // surrounding context and the section name, so a leak identifies itself.
    //
    // The credential needles are the JSON *key* forms (`"api_key":`), never the
    // bare substring — `ProviderAccount.billing_mode` legitimately has the
    // enum value `"api_key"` beside `"subscription"`, which is not a secret
    // and must survive the round-trip. The secret *values* below are what a
    // real profile would hold; none of them may appear anywhere in the file.
    for secret in [
        "sk-ant-SECRET".as_bytes(),
        "sk-oai-SECRET".as_bytes(),
        "minimax-SECRET".as_bytes(),
        "root-token-SECRET".as_bytes(),
        "read-hash-SECRET".as_bytes(),
        "drive-hash-SECRET".as_bytes(),
        "device-hash-SECRET".as_bytes(),
        b"\"api_key\":",
        b"\"minimax_api_key\"",
        b"remote_access_token",
        b"CA-PRIVATE-KEY-SECRET",
    ] {
        if let Some(at) = find(&bytes, secret) {
            let start = at.saturating_sub(90);
            let end = (at + secret.len() + 90).min(bytes.len());
            let context = String::from_utf8_lossy(&bytes[start..end]).replace('\n', "\\n");
            // Report which section the match landed in, not just the bytes.
            let section = bundle::BundleReader::open(&dest)
                .ok()
                .and_then(|reader| {
                    reader
                        .header()
                        .sections
                        .iter()
                        .find(|s| {
                            let payload_start =
                                12 + u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize
                                    + s.offset as usize;
                            at >= payload_start && at < payload_start + s.length as usize
                        })
                        .map(|s| s.name.clone())
                })
                .unwrap_or_else(|| "header or unknown".to_string());
            panic!(
                "the default export leaked {:?} in section `{section}` at byte {at}\n  context: {context}",
                String::from_utf8_lossy(secret)
            );
        }
    }
    assert!(result.omitted.len() >= 4, "the omissions are stated, not implied");
    assert!(result
        .omitted
        .iter()
        .any(|o| o.contains("private key")));

    // ...and the useful data is still there.
    let mut reader = bundle::BundleReader::open(&dest).unwrap();
    reader.verify().unwrap();
    let prefs = String::from_utf8(reader.read_section(SECTION_PREFS).unwrap()).unwrap();
    assert!(prefs.contains("\"default_provider\""));
    assert!(prefs.contains("anthropic"), "account identity is not a secret");
    assert!(prefs.contains("\"billing_mode\""));
}

#[test]
fn an_unredacted_export_is_only_produced_on_explicit_request() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("full.bmsnap");
    let result = export_to_path(&fixture.root, &dest, false).unwrap();
    assert!(!result.redacted);
    assert!(contains(
        &std::fs::read(&dest).unwrap(),
        b"sk-ant-SECRET"
    ));
}

/// Redaction must not mutate the live state — the whole point is that it runs
/// against the copy.
#[test]
fn redaction_leaves_live_state_untouched() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    export_to_path(&fixture.root, &fixture.root.join("e.bmsnap"), true).unwrap();
    assert_eq!(
        fixture.setting("remote_access_token").as_deref(),
        Some("root-token-SECRET")
    );
    assert!(read_string(&fixture.prefs_path).contains("sk-ant-SECRET"));
}

/// An export of a preferences file written by a *newer* build keeps the
/// fields this build does not know about. Round-tripping through today's
/// `AppPreferences` struct would silently drop them.
#[test]
fn redaction_preserves_unknown_preference_fields() {
    let fixture = Fixture::new();
    fixture.write_preferences(
        r#"{
          "default_provider": "anthropic",
          "minimax_api_key": "SECRET",
          "some_future_setting": {"nested": [1, 2, 3]},
          "provider_accounts": [{"id": "x", "api_key": "SECRET", "future_flag": true}]
        }"#,
    );
    let dest = fixture.root.join("future.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();
    let mut reader = bundle::BundleReader::open(&dest).unwrap();
    let prefs = String::from_utf8(reader.read_section(SECTION_PREFS).unwrap()).unwrap();
    assert!(prefs.contains("some_future_setting"));
    assert!(prefs.contains("future_flag"));
    assert!(!prefs.contains("SECRET"));
}

#[test]
fn a_profile_without_preferences_still_exports_and_restores() {
    let fixture = Fixture::new();
    // No preferences.json at all.
    let dest = fixture.root.join("noprefs.bmsnap");
    let result = export_to_path(&fixture.root, &dest, true).unwrap();
    assert_eq!(result.sections, vec![SECTION_DB.to_string()]);

    let plan = stage_restore(&fixture.root, &dest).unwrap();
    assert!(plan.requires_restart);
    let applied = apply_pending_restore(&fixture.root).unwrap().unwrap();
    assert_eq!(applied.applied, vec!["buildmesh.db"]);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM meshes"), 1);
}

// ---------------------------------------------------------------------------
// 5. Rejecting a bad bundle without touching current data
// ---------------------------------------------------------------------------

/// Every rejection case, asserted against the same invariant: the live
/// database and preferences are byte-identical afterwards, and nothing was
/// staged.
fn assert_rejected_without_side_effects(fixture: &Fixture, bundle_bytes: &[u8], needle: &str) {
    let dest = fixture.root.join("bad.bmsnap");
    std::fs::write(&dest, bundle_bytes).unwrap();
    let db_before = std::fs::read(&fixture.db_path).unwrap();
    let prefs_before = std::fs::read(&fixture.prefs_path).unwrap();

    let error = stage_restore(&fixture.root, &dest)
        .expect_err("a bad bundle must be rejected");
    assert!(
        error.contains(needle),
        "error should explain the rejection (wanted {needle:?}), got: {error}"
    );

    assert_eq!(std::fs::read(&fixture.db_path).unwrap(), db_before);
    assert_eq!(std::fs::read(&fixture.prefs_path).unwrap(), prefs_before);
    assert!(
        !bundle::pending_dir(&fixture.root).exists(),
        "a rejected bundle must not stage anything"
    );
    assert!(
        !fixture.root.join(SNAPSHOT_DIR).exists(),
        "a rejected bundle must not even take a rollback snapshot"
    );
}

#[test]
fn redacting_preferences_alone_strips_every_credential_field() {
    // Isolates the preferences half of the leak gate, so a failure here names
    // the cause instead of surfacing as an opaque "the bundle leaked api_key".
    let raw = br#"{
      "minimax_api_key": "minimax-SECRET",
      "provider_accounts": [
        {"id": "a", "api_key": "sk-ant-SECRET"},
        {"id": "b", "api_key": "sk-oai-SECRET"},
        {"id": "c"}
      ]
    }"#;
    let cleaned = redact::redact_preferences(raw).unwrap();
    let text = String::from_utf8(cleaned).unwrap();
    assert!(!contains(text.as_bytes(), b"api_key"), "got: {text}");
    assert!(!contains(text.as_bytes(), b"minimax-SECRET"), "got: {text}");
    assert!(!contains(text.as_bytes(), b"sk-ant-SECRET"), "got: {text}");
    assert!(text.contains("\"id\": \"a\""), "account identity survives: {text}");
}

#[test]
fn redacting_a_database_copy_strips_every_credential_row() {
    // The other half of the leak gate, isolated the same way.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("copy.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        crate::db::init_schema(&conn).unwrap();
        for (key, value) in [
            ("remote_access_token", "root-token-SECRET"),
            ("coordinator_read_token", "read-hash-SECRET"),
            ("coordinator_drive_token", "drive-hash-SECRET"),
            ("lan_exposure_enabled", "1"),
        ] {
            conn.execute(
                "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO device_sessions (label, token_hash) VALUES ('Pixel', 'device-hash-SECRET')",
            [],
        )
        .unwrap();
    }
    let removed = redact::redact_database_copy(&path).unwrap();
    assert_eq!(removed, 4, "three tokens plus the one device session");

    let bytes = std::fs::read(&path).unwrap();
    for secret in [
        "root-token-SECRET".as_bytes(),
        "read-hash-SECRET".as_bytes(),
        "drive-hash-SECRET".as_bytes(),
        "device-hash-SECRET".as_bytes(),
    ] {
        assert!(
            !contains(&bytes, secret),
            "the redacted database copy still contains {:?}",
            String::from_utf8_lossy(secret)
        );
    }
    let conn = Connection::open(&path).unwrap();
    // A behaviour flag is not a credential and must survive, or a redacted
    // export would silently re-enable LAN exposure on restore.
    let lan: String = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'lan_exposure_enabled'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lan, "1");
}

#[test]
fn a_file_that_is_not_a_bundle_is_rejected() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    assert_rejected_without_side_effects(&fixture, b"not a bundle at all", "signature");
}

#[test]
fn a_truncated_bundle_is_rejected() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("good.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();
    let bytes = std::fs::read(&dest).unwrap();
    assert_rejected_without_side_effects(&fixture, &bytes[..bytes.len() / 2], "invalid");
}

#[test]
fn a_tampered_section_is_rejected_by_its_checksum() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("good.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();
    let mut bytes = std::fs::read(&dest).unwrap();
    // Flip a byte deep in the payload — past the header, inside `state.db`.
    let target = bytes.len() - 64;
    bytes[target] ^= 0xFF;
    assert_rejected_without_side_effects(&fixture, &bytes, "checksum");
}

#[test]
fn a_tampered_header_is_rejected() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("good.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();
    let mut bytes = std::fs::read(&dest).unwrap();
    // The header starts at byte 12; corrupt its JSON without touching magic.
    let header_start = 12;
    bytes[header_start] = b'#';
    assert_rejected_without_side_effects(&fixture, &bytes, "header");
}

#[test]
fn a_bundle_from_a_newer_build_is_rejected() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("future.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();

    // Rewrite the header with a future format version, keeping the file
    // otherwise intact — exactly what the next Buildmesh release would emit.
    let reader = bundle::BundleReader::open(&dest).unwrap();
    let mut header = reader.header().clone();
    header.format_version = bundle::FORMAT_VERSION + 1;
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(&bundle::MAGIC);
    rebuilt.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    rebuilt.extend_from_slice(&header_bytes);
    // Re-append the original payload so only the version differs.
    let original = std::fs::read(&dest).unwrap();
    let data_start = 12 + u32::from_le_bytes(original[8..12].try_into().unwrap()) as usize;
    rebuilt.extend_from_slice(&original[data_start..]);

    assert_rejected_without_side_effects(&fixture, &rebuilt, "newer than this build");
}

#[test]
fn a_bundle_without_a_database_section_is_rejected() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("prefs-only.bmsnap");
    bundle::write_bundle(
        &dest,
        BundlePrologue {
            kind: "export".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            app_version: "test".to_string(),
            schema_version: SCHEMA_VERSION,
            redacted: true,
        },
        &[(SECTION_PREFS, br#"{"default_provider":"anthropic"}"#.to_vec())],
    )
    .unwrap();
    assert_rejected_without_side_effects(&fixture, &read(&dest), "no `state.db` section");
}

#[test]
fn a_missing_bundle_is_rejected() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let error = stage_restore(&fixture.root, &fixture.root.join("nope.bmsnap"))
        .expect_err("a missing file must be an error, not a silent no-op");
    assert!(!error.is_empty());
}

// ---------------------------------------------------------------------------
// 6. Staging mechanics
// ---------------------------------------------------------------------------

/// The staged payload must not become live state until the next launch, and a
/// cancelled restore must leave nothing behind.
#[test]
fn staging_does_not_touch_live_state_until_the_next_launch() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("other.bmsnap");
    {
        // A different profile's export.
        let other = Fixture::new();
        other.standard_preferences();
        let conn = Connection::open(&other.db_path).unwrap();
        conn.execute("UPDATE meshes SET name = 'from-other-profile'", [])
            .unwrap();
        drop(conn);
        build_bundle(
            &other.db_path,
            &other.prefs_path,
            &dest,
            "export",
            true,
            "2026-01-01T00:00:00Z",
            "test",
        )
        .unwrap();
    }

    let original_name: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(original_name, "alpha");

    stage_restore(&fixture.root, &dest).unwrap();
    let staged_name: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        staged_name, "alpha",
        "staging alone must not change live state"
    );
    assert!(bundle::pending_dir(&fixture.root).exists());

    apply_pending_restore(&fixture.root).unwrap().unwrap();
    let applied_name: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(applied_name, "from-other-profile");
}

#[test]
fn a_cancelled_restore_leaves_nothing_staged() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("e.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();
    stage_restore(&fixture.root, &dest).unwrap();
    assert!(bundle::pending_dir(&fixture.root).exists());
    cancel_pending_restore_in(&fixture.root).unwrap();
    assert!(!bundle::pending_dir(&fixture.root).exists());
    assert!(apply_pending_restore(&fixture.root).unwrap().is_none());
}

/// Applying twice is a no-op the second time — the marker is consumed.
#[test]
fn apply_pending_restore_is_not_repeatable() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("e.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();
    stage_restore(&fixture.root, &dest).unwrap();
    assert!(apply_pending_restore(&fixture.root).unwrap().is_some());
    assert!(
        apply_pending_restore(&fixture.root).unwrap().is_none(),
        "a consumed restore must not re-apply on the next launch"
    );
}

/// An interrupted stage (payload written, marker not yet) is inert. This is
/// the crash window the write-marker-last ordering exists to close.
#[test]
fn an_interrupted_stage_is_discarded_rather_than_half_applied() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("e.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();

    let pending = bundle::pending_dir(&fixture.root);
    std::fs::create_dir_all(&pending).unwrap();
    std::fs::write(pending.join("buildmesh.db"), b"partial payload").unwrap();
    // Deliberately no marker.

    assert!(apply_pending_restore(&fixture.root).unwrap().is_none());
    assert!(!pending.exists(), "the inert directory is cleaned up");
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM meshes"), 1);
    let name: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "alpha", "live state is untouched");
}

/// A staged database that stopped being a valid database between staging and
/// launch must not be applied.
#[test]
fn a_corrupted_staged_payload_is_refused_at_apply_time() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let dest = fixture.root.join("e.bmsnap");
    export_to_path(&fixture.root, &dest, true).unwrap();
    stage_restore(&fixture.root, &dest).unwrap();

    // Corrupt the staged file after it passed verification at stage time.
    let staged = bundle::pending_dir(&fixture.root).join("buildmesh.db");
    let mut bytes = std::fs::read(&staged).unwrap();
    for byte in bytes.iter_mut().skip(100) {
        *byte = 0;
    }
    std::fs::write(&staged, &bytes).unwrap();

    assert!(apply_pending_restore(&fixture.root).unwrap().is_none());
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM meshes"), 1);
    let name: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row("SELECT name FROM meshes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "alpha", "the current state is kept, not replaced");
}

// ---------------------------------------------------------------------------
// 7. Integrity reporting
// ---------------------------------------------------------------------------

#[test]
fn integrity_check_passes_on_a_healthy_database() {
    let fixture = Fixture::new();
    let conn = Connection::open(&fixture.db_path).unwrap();
    assert!(quick_check(&conn).unwrap());
    assert!(full_check(&conn).unwrap());
}

#[test]
fn integrity_check_fails_loudly_on_a_damaged_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("buildmesh.db");
    let mut bytes = b"SQLite format 3\0".to_vec();
    bytes.extend_from_slice(&[0x00; 2048]);
    std::fs::write(&path, &bytes).unwrap();

    let report = check_integrity_in(dir.path(), true).unwrap();
    assert!(!report.ok, "damage must be reported, not smoothed over");
    assert_eq!(report.scope, "full");
    assert!(
        report.message.len() > "Database damage found: ".len(),
        "the report carries SQLite's own finding, not a bare boolean"
    );
}

#[test]
fn integrity_check_on_an_empty_profile_is_a_pass_with_an_explanation() {
    let dir = tempfile::tempdir().unwrap();
    let report = check_integrity_in(dir.path(), false).unwrap();
    assert!(report.ok);
    assert!(report.message.contains("nothing to check"));
}

// ---------------------------------------------------------------------------
// 8. Listing and reporting
// ---------------------------------------------------------------------------

#[test]
fn two_rapid_snapshots_do_not_overwrite_each_other() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    // The timestamp slug has second resolution, so two snapshots taken in the
    // same second used to collide on one filename — silently destroying a
    // rollback point, which is the one thing a snapshot must never be.
    let first = create_snapshot_in(&fixture.root, "manual", false).unwrap();
    let second = create_snapshot_in(&fixture.root, "manual", false).unwrap();
    assert_ne!(
        first.path, second.path,
        "a second snapshot in the same second must not overwrite the first"
    );
    assert!(Path::new(&first.path).exists());
    assert!(Path::new(&second.path).exists());

    let listed = list_snapshots_in(&fixture.root.join(SNAPSHOT_DIR)).unwrap();
    assert_eq!(listed.len(), 2, "both snapshots survive");
    // Newest first, per the shared ordering rule.
    let names: Vec<&str> = listed.iter().map(|s| s.file_name.as_str()).collect();
    assert_eq!(names, vec![second.file_name.as_str(), first.file_name.as_str()]);
}

#[test]
fn snapshots_are_listed_newest_first_with_real_metadata() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    create_snapshot_in(&fixture.root, "manual", false).unwrap();
    create_snapshot_in(&fixture.root, "manual", false).unwrap();

    let listed = list_snapshots_in(&fixture.root.join(SNAPSHOT_DIR)).unwrap();
    assert_eq!(listed.len(), 2);
    for snapshot in &listed {
        assert_eq!(snapshot.kind, "manual");
        assert!(!snapshot.redacted);
        assert!(snapshot.size_bytes > 0);
        assert!(snapshot.file_name.ends_with(BUNDLE_EXTENSION));
    }
}

#[test]
fn info_reports_the_profile_shape() {
    let fixture = Fixture::new();
    fixture.standard_preferences();
    let info = build_info(&fixture.root);
    assert_eq!(info.schema_version, SCHEMA_VERSION);
    assert_eq!(info.snapshot_count, 0);
    assert_eq!(info.retention as usize, SNAPSHOT_RETENTION);
    assert!(!info.pending_restore);
    assert!(info.notice.is_none());
    assert!(info.snapshot_dir.ends_with(SNAPSHOT_DIR));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Path-explicit wrappers over the command-facing functions, so the tests
/// never depend on the process-global app-data directory.
fn export_to_path(root: &Path, dest: &Path, redact: bool) -> io::Result<StateExportResult> {
    let created_at = timestamp();
    let (header, _) = build_bundle(
        &root.join("buildmesh.db"),
        &root.join("preferences.json"),
        dest,
        "export",
        redact,
        &created_at,
        env!("CARGO_PKG_VERSION"),
    )?;
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

fn create_snapshot_in(root: &Path, kind: &str, redact: bool) -> io::Result<StateSnapshot> {
    write_snapshot(root, &root.join("buildmesh.db"), kind, redact)
}

fn check_integrity_in(root: &Path, full: bool) -> Result<StateIntegrityReport, String> {
    let db_path = root.join("buildmesh.db");
    if !is_real_database_file(&db_path) {
        return Ok(StateIntegrityReport {
            ok: true,
            scope: if full { "full" } else { "quick" }.to_string(),
            checked_at: timestamp(),
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
        checked_at: timestamp(),
        message,
    })
}

fn cancel_pending_restore_in(root: &Path) -> io::Result<()> {
    let pending = bundle::pending_dir(root);
    if pending.exists() {
        std::fs::remove_dir_all(pending)?;
    }
    Ok(())
}

/// Extract the `state.db` section of a bundle to a fresh temp file, so the
/// tests can open the payload as a standalone database.
fn extract_db_to_temp(bundle_path: &Path) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.keep().join("state.db");
    let mut reader = bundle::BundleReader::open(bundle_path).unwrap();
    reader.verify().unwrap();
    reader.extract_section_to(SECTION_DB, &dest).unwrap();
    dest
}

/// A minimal throwaway database in a temp dir, for the retention test's
/// bundle-building loop.
fn fixture_db(root: &Path) -> PathBuf {
    let path = root.join("retention-fixture.db");
    if !path.exists() {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        crate::db::init_schema(&conn).unwrap();
    }
    path
}

fn nonexistent(root: &Path) -> PathBuf {
    root.join("no-such-preferences.json")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}
