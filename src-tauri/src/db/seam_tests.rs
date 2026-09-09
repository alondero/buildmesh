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

#[test]
fn terminal_run_state_defers_to_stepper() {
    use crate::autopilot::circuit::stepper::RunState;
    for state in ["completed", "failed", "cancelled", "paused", "running", "pending", "nope"] {
        assert_eq!(
            crate::db::is_terminal_run_state(state),
            RunState::from_db_str(state).is_terminal(),
            "{state}"
        );
    }
}
