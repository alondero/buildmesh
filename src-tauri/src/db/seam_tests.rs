//! Pins the db/mod.rs seam (issue #1655): connection + init only.

use rusqlite::Connection;
use std::collections::BTreeMap;

/// Connection-pool + init surface — the public API `mod.rs` owns.
const ALLOWED_PUB_FNS: &[&str] = &[
    "init",
    "read_conn",
    "try_read_conn",
    "write_conn",
    "is_initialized",
];

/// Baseline-DDL helpers — the `pub(crate)` API `mod.rs` owns so
/// `db::migrations` can drive the schema-evolution entry point without
/// reaching back through domain modules. The plan says "baseline CREATE
/// TABLE + init()"; these three functions ARE that baseline (one DDL
/// source, called from `migrations::evolve_to` / `init`).
const ALLOWED_PUB_CRATE_FNS: &[&str] = &[
    "init_schema",
    "ensure_baseline_tables",
    "create_canonical_indexes_after_evolution",
    // Issue #1793 — a test-only, non-query lock probe: it runs no SQL and
    // exists solely so the reaper's no-I/O-under-lock assertion can observe
    // whether the writer mutex is held. Not a query function, so it does not
    // breach the seam this file guards.
    "try_write_conn",
];

#[test]
fn db_mod_owns_connection_and_init_only() {
    let src = include_str!("mod.rs");
    let mut unexpected = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        // Match BOTH `pub fn` and `pub(crate) fn` — `pub(crate)` was the
        // gap that let two migration UPDATE helpers and the token
        // helpers hide behind the previous seam. The two allowlists
        // distinguish connection/init (public) from baseline DDL
        // (crate-private, driven by migrations).
        if let Some(rest) = trimmed.strip_prefix("pub fn ") {
            let name = rest.split('(').next().unwrap();
            if !ALLOWED_PUB_FNS.contains(&name) {
                unexpected.push(format!("{}: {name}", i + 1));
            }
        } else if let Some(rest) = trimmed.strip_prefix("pub(crate) fn ") {
            let name = rest.split('(').next().unwrap();
            if !ALLOWED_PUB_CRATE_FNS.contains(&name) {
                unexpected.push(format!("{}: {name}", i + 1));
            }
        }
    }
    assert!(
        unexpected.is_empty(),
        "db/mod.rs must not grow query functions; add them to a domain module instead: {unexpected:?}"
    );
}

fn schema_dump(conn: &Connection) -> String {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_master \
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' \
             ORDER BY type, name",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap();
    let mut map = BTreeMap::new();
    for row in rows {
        let (kind, name, sql) = row.unwrap();
        map.insert(format!("{kind}:{name}"), sql);
    }
    let mut out = String::new();
    for (key, sql) in map {
        out.push_str(&key);
        out.push('\n');
        out.push_str(sql.trim());
        out.push_str("\n\n");
    }
    // Keep the committed snapshot canonical: blank lines separate entries,
    // while the file has one final newline rather than an accidental extra.
    out.trim_end_matches('\n').to_owned() + "\n"
}

#[test]
fn init_schema_dump_matches_committed_snapshot() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let dump = schema_dump(&conn);
    let expected = include_str!("schema_dump.txt");
    if std::env::var_os("BUILDMESH_WRITE_SCHEMA_DUMP").is_some() {
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/schema_dump.txt"),
            dump.as_bytes(),
        )
        .unwrap();
        return;
    }
    assert_eq!(
        dump, expected,
        "fresh init_schema() dump drifted from src-tauri/src/db/schema_dump.txt"
    );
}

/// Schema-churn locality (issue #1655): a meshes/agent_nodes/warm_worktrees
/// query — whether `SELECT … FROM <table>` OR `UPDATE <table>` — belongs
/// in its domain file, not back in `db/mod.rs`. Adding a column still
/// goes through `migrations.rs`; the projection/query change should land
/// in exactly one domain module. The two `UPDATE` needles below closed
/// the gap that let the `ensure_mesh_default_provider_normalized` and
/// `migrate_agent_node_provider_id_custom_accounts` helpers hide behind
/// the previous `FROM`-only seam.
#[test]
fn query_sql_lives_in_domain_modules() {
    let db_mod = include_str!("mod.rs");
    for needle in [
        // SELECT … FROM — read paths.
        "FROM meshes ",
        "FROM agent_nodes ",
        "FROM warm_worktrees ",
        "FROM device_sessions ",
        "FROM coordinator_drive_prompts ",
        "FROM autopilot_circuits ",
        // UPDATE/DELETE/INSERT — mutation paths. The seam freeze is
        // bidirectional: domain modules own BOTH reads and writes for
        // their tables. The mod.rs may re-export them via `pub use`,
        // but the SQL itself lives next to the table's other queries.
        "UPDATE meshes ",
        "UPDATE agent_nodes ",
        "UPDATE warm_worktrees ",
        "UPDATE device_sessions ",
        "UPDATE coordinator_drive_prompts ",
        "UPDATE autopilot_circuits ",
    ] {
        assert!(
            !db_mod.contains(needle),
            "db/mod.rs must not contain SQL for {needle:?}; put it in the domain module"
        );
    }
    assert!(include_str!("mesh.rs").contains("FROM meshes"));
    assert!(include_str!("agent_node.rs").contains("FROM agent_nodes"));
    assert!(include_str!("warm_pool.rs").contains("FROM warm_worktrees"));
    assert!(include_str!("auth.rs").contains("FROM device_sessions"));
    assert!(include_str!("drive.rs").contains("FROM coordinator_drive_prompts"));
    assert!(include_str!("circuit/ledger.rs").contains("FROM autopilot_circuits"));
    assert!(include_str!("circuit/queue.rs").contains("queue_position"));
    assert!(include_str!("circuit/leases.rs").contains("autopilot_circuit_run_agent_leases"));
    // Domain modules own their tables' mutation SQL too.
    assert!(include_str!("mesh.rs").contains("UPDATE meshes"));
    assert!(include_str!("agent_node.rs").contains("UPDATE agent_nodes"));
}

/// The error type as written in a `-> Result<…, E>` signature, or `None` when
/// the line is not one or the signature cannot say which type is the error.
///
/// Nesting is counted so `Result<Option<String>, DbError>` closes at the right
/// angle bracket and its payload is never mistaken for its error type. A
/// single-argument `Result<String>` is genuinely ambiguous — one type, no
/// position — so it is left alone rather than guessed at.
fn signature_error_type(line: &str) -> Option<&str> {
    let start = line.find("-> Result<")? + "-> Result<".len();
    let rest = &line[start..];
    let mut depth = 1usize;
    let mut end = rest.len();
    let mut commas = 0usize;
    for (i, c) in rest.char_indices() {
        match c {
            '<' => depth += 1,
            ',' if depth == 1 => commas += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    // No top-level comma: `Result<T>` cannot be read as `Result<T, E>`.
    if commas == 0 {
        return None;
    }
    rest[..end]
        .rsplit(',')
        .next()
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// The check itself, on the shapes that made the original grep unusable.
#[test]
fn signature_error_type_separates_payload_from_error() {
    // A String *payload* is fine and is not an error position.
    assert_eq!(
        signature_error_type("fn a() -> SqlResult<Option<String>> {"),
        None
    );
    assert_eq!(
        signature_error_type("fn a() -> SqlResult<HashMap<String, String>> {"),
        None
    );
    // One argument and no position: ambiguous, so not claimed.
    assert_eq!(signature_error_type("fn a() -> Result<String> {"), None);
    // Nested generics: the error is still the last argument.
    assert_eq!(
        signature_error_type("fn a() -> Result<Option<crate::models::X>, DbError> {"),
        Some("DbError")
    );
    // The case the original `Result<[^,(]*String` pattern missed.
    assert_eq!(
        signature_error_type("fn a() -> Result<usize, String> {"),
        Some("String")
    );
    assert_eq!(
        signature_error_type("pub fn a() -> Result<(), String>"),
        Some("String")
    );
    // A payload String alongside a String error: only the error position counts.
    assert_eq!(
        signature_error_type("fn a() -> Result<Option<String>, String> {"),
        Some("String")
    );
    assert_eq!(signature_error_type("fn a() {}"), None);
}

/// Persistence failures are typed (issue #2448): the db layer reports
/// `DbError` or `SqlResult`, never a hand-written `String`.
///
/// The obvious grep for this — `Result<[^,(]*String` — is wrong in both
/// directions, which is how the slice that introduced `DbError` shipped a file
/// list that named four files with no String errors and missed five that had
/// them: the pattern matches `SqlResult<String>` (a String *payload*), and it
/// misses `Result<usize, String>` (a comma before the `String`). So the check
/// walks each signature's generic arguments instead and looks at the last one,
/// which is the error type.
///
/// The file list is walked at runtime rather than `include_str!`-ed, so a new
/// module under `db/` is covered the day it lands instead of the day someone
/// remembers to add it here.
#[test]
fn db_reports_no_stringly_typed_errors() {
    fn db_sources() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/db");
        let mut found = Vec::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("db/ source tree must be readable") {
                let path = entry.expect("db/ entry must be readable").path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                // Test modules may keep asserting on legacy String helpers, and
                // `test_support.rs` is fixtures.
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if path.extension().and_then(|e| e.to_str()) != Some("rs")
                    || name.ends_with("_tests.rs")
                    || name == "test_support.rs"
                {
                    continue;
                }
                let label = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push((
                    label,
                    std::fs::read_to_string(&path).expect("db/ source readable"),
                ));
            }
        }
        found.sort();
        assert!(
            found.len() >= 20,
            "the db/ walk found only {} files - it is not scanning the layer",
            found.len()
        );
        found
    }

    let mut offenders = Vec::new();
    for (name, src) in db_sources() {
        for (i, line) in src.lines().enumerate() {
            // A `String` payload is fine — only the error position is gated.
            if let Some(error) = signature_error_type(line) {
                if error == "String" {
                    offenders.push(format!("{name}:{}: {error}", i + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "db/ must report DbError or SqlResult, not Result<_, String>: {offenders:?}"
    );
}

#[test]
fn terminal_run_state_defers_to_stepper() {
    use crate::circuit::stepper::RunState;
    for state in [
        "completed",
        "failed",
        "cancelled",
        "paused",
        "running",
        "pending",
        "nope",
    ] {
        assert_eq!(
            crate::db::is_terminal_run_state(state),
            RunState::from_db_str(state).is_terminal(),
            "{state}"
        );
    }
}
