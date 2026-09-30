//! Integration tests for mesh creation edge cases.
//!
//! Tests that verify create_mesh handles duplicate paths gracefully,
//! returning the existing mesh instead of crashing with UNIQUE constraint.
//!
//! Each test acquires [`MESH_TESTS_LOCK`] at the top of its body to
//! serialise against the other tests in this module. The process-wide
//! `db::DB` is a `OnceCell<Mutex<Connection>>` — once any test in the
//! binary calls `db::init`, every later `init` silently no-ops and
//! writes go to the winner's connection (issue #1334). Tests outside
//! this module are unaffected.

#[cfg(test)]
mod tests {
    static MESH_TESTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        MESH_TESTS_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The GitHub clone flow creates its mesh with the cloned repo's resolved
    /// default branch rather than the `origin/main` literal, so the new mesh
    /// isn't a **drifted root**. Pin both halves: the default still writes
    /// `origin/main`, and the explicit overload persists what it's given.
    #[test]
    fn create_mesh_with_base_ref_persists_supplied_ref() {
        let _serial = serial();
        let temp = tempfile::tempdir().unwrap();
        crate::db::init(&temp.path().join("db.sqlite")).unwrap();

        let default_path = format!("C:/buildmesh-base-default-{}", uuid::Uuid::new_v4());
        let default_mesh = crate::db::create_mesh("Default", &default_path).unwrap();
        assert_eq!(default_mesh.base_ref, "origin/main");

        let clone_path = format!("C:/buildmesh-base-clone-{}", uuid::Uuid::new_v4());
        let cloned =
            crate::db::create_mesh_with_base_ref("Cloned", &clone_path, "origin/master").unwrap();
        assert_eq!(cloned.base_ref, "origin/master");
        assert_eq!(
            crate::db::get_mesh_by_id(cloned.id).unwrap().base_ref,
            "origin/master"
        );
    }

    #[test]
    fn harness_runtime_persists_and_legacy_switch_restores_mesh_runtime() {
        let _serial = serial();
        let temp = tempfile::tempdir().unwrap();
        crate::db::init(&temp.path().join("db.sqlite")).unwrap();
        crate::preferences::init_for_tests(temp.path().to_path_buf());
        crate::preferences::merge_detected_profiles(vec![crate::preferences::HarnessProfile {
            id: "muse-wsl-test".into(), name: "Muse (WSL)".into(), harness: "muse".into(),
            runtime: Some(crate::models::EnvType::Wsl), wsl_distro: Some("Ubuntu".into()), executable: None,
        }]).unwrap();
        let path = format!("C:/buildmesh-runtime-{}", uuid::Uuid::new_v4());
        let mesh = crate::db::create_mesh("Runtime test", &path).unwrap();
        let node = crate::db::create_agent_node(mesh.id, "Muse", &path, "main", crate::models::EnvType::Windows,
            "muse-wsl-test", None, None, None, None, false, None, None, None).unwrap();
        assert_eq!(node.env, crate::models::EnvType::Wsl);
        assert_eq!(crate::db::get_agent_node_by_id(node.id).unwrap().env, crate::models::EnvType::Wsl);
        crate::db::set_agent_node_provider(node.id, "terminal").unwrap();
        let node = crate::db::get_agent_node_by_id(node.id).unwrap();
        assert_eq!(node.provider, "terminal");
        assert_eq!(node.env, crate::models::EnvType::Windows);
        crate::db::set_agent_node_provider(node.id, "muse-wsl-test").unwrap();
        assert_eq!(crate::db::get_agent_node_by_id(node.id).unwrap().env, crate::models::EnvType::Wsl);
        crate::preferences::reset_for_tests();
    }

    /// Test: creating a project with a duplicate path should NOT crash.
    /// Expected behavior: return the existing project (idempotent upsert).
    #[test]
    fn test_create_project_with_duplicate_path_returns_existing() {
        let _serial = serial();
        // Use a unique temp file per test so each test is fully isolated
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let temp_path = std::env::temp_dir().join(format!("buildmesh_dup_test_{}.db", test_id));

        crate::db::init(&temp_path).unwrap();

        // Create first mesh
        let first = crate::db::create_mesh("First Project", "/tmp/dup-test").unwrap();
        assert_eq!(first.name, "First Project");
        assert_eq!(first.layout, "grid");

        // Act: create another mesh with the same path but different name
        let second_result = crate::db::create_mesh("Second Project", "/tmp/dup-test");

        // Cleanup
        drop(crate::db::write_conn());
        std::fs::remove_file(&temp_path).ok();

        // Assert: should return Ok(existing_mesh), NOT Err(UNIQUE constraint)
        match second_result {
            Ok(mesh) => {
                assert_eq!(mesh.name, "First Project", "should return the FIRST (existing) mesh");
                assert_eq!(mesh.layout, "grid", "should preserve original layout");
            }
            Err(e) => {
                panic!("create_mesh with duplicate path should NOT error, but got: {}", e);
            }
        }
    }

    /// A freshly created mesh has no colour; `set_mesh_color` persists a hex
    /// and reads back through `get_mesh_by_id`, and clearing with `None`
    /// returns to the palette-fallback (`None`).
    #[test]
    fn test_mesh_color_round_trips() {
        let _serial = serial();
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_path = std::env::temp_dir().join(format!("buildmesh_color_test_{}.db", test_id));
        // First-init-wins: a no-op if another test file already set the global DB.
        crate::db::init(&temp_path).unwrap();

        let path = format!("/tmp/color-test-{}", test_id);
        let mesh = crate::db::create_mesh("Color Mesh", &path).unwrap();
        assert_eq!(mesh.color, None, "new meshes start with no colour");

        let rows = crate::db::set_mesh_color(mesh.id, Some("#38bdf8")).unwrap();
        assert_eq!(rows, 1, "one row updated");
        let recolored = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert_eq!(recolored.color.as_deref(), Some("#38bdf8"));

        crate::db::set_mesh_color(mesh.id, None).unwrap();
        let cleared = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert_eq!(cleared.color, None, "clearing returns to palette fallback");

        std::fs::remove_file(&temp_path).ok();
    }

    use rusqlite::Connection;
    fn conn_with_meshes() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        conn
    }

    fn insert_mesh(conn: &Connection, name: &str) -> i64 {
        let path = format!("/tmp/{}-{}", name, uuid::Uuid::new_v4());
        conn.execute(
            "INSERT INTO meshes (name, path) VALUES (?1, ?2)",
            rusqlite::params![name, path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn read_position(conn: &Connection, id: i64) -> i64 {
        conn.query_row(
            "SELECT position FROM meshes WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// Empty input must be a no-op — the public function short-circuits
    /// without ever touching the writer. Pinning that contract stops a
    /// future refactor from accidentally materialising an empty tx (which
    /// would still pay one commit under the writer mutex).
    #[test]
    fn update_mesh_positions_batch_inner_zero_rows_is_noop() {
        let conn = conn_with_meshes();
        let id = insert_mesh(&conn, "no-op-mesh");
        assert_eq!(read_position(&conn, id), 0);

        crate::db::update_mesh_positions_batch_inner(&conn, &[]).unwrap();

        assert_eq!(read_position(&conn, id), 0);
    }

    /// Single-row batch must move that one row only. Locks the contract
    /// against accidental over-write when the bulk shape is mis-built for
    /// the N=1 boundary (the IN list has one element, the CASE has one
    /// WHEN, the params vector has 3 entries).
    #[test]
    fn update_mesh_positions_batch_inner_single_row() {
        let conn = conn_with_meshes();
        let a = insert_mesh(&conn, "a");
        let b = insert_mesh(&conn, "b");
        let c = insert_mesh(&conn, "c");

        crate::db::update_mesh_positions_batch_inner(&conn, &[(b, 42)]).unwrap();

        assert_eq!(read_position(&conn, a), 0);
        assert_eq!(read_position(&conn, b), 42);
        assert_eq!(read_position(&conn, c), 0);
    }

    /// 600 rows → spans three chunks of 300 (the bulk form's internal
    /// chunk size). Verifies final positions and that the chunked commit
    /// boundaries don't drop rows mid-batch.
    #[test]
    fn update_mesh_positions_batch_inner_six_hundred_rows_spans_chunks() {
        let conn = conn_with_meshes();
        let mut ids = Vec::with_capacity(600);
        for i in 0..600 {
            ids.push(insert_mesh(&conn, &format!("m-{i}")));
        }
        let updates: Vec<(i64, i64)> =
            ids.iter().enumerate().map(|(i, id)| (*id, (i + 1) as i64)).collect();

        crate::db::update_mesh_positions_batch_inner(&conn, &updates).unwrap();

        for (i, id) in ids.iter().enumerate() {
            assert_eq!(read_position(&conn, *id), (i + 1) as i64);
        }
    }

    /// The bulk form's final state must match the per-row baseline the old
    /// code path produced. We don't bench here (see the `#[ignore]`-gated
    /// test below) — we just confirm that semantics didn't shift while the
    /// SQL was rewritten.
    #[test]
    fn update_mesh_positions_batch_inner_matches_per_row_baseline() {
        // New bulk form
        let conn_new = conn_with_meshes();
        let mut ids_new = Vec::with_capacity(50);
        for i in 0..50 {
            ids_new.push(insert_mesh(&conn_new, &format!("new-{i}")));
        }
        let updates: Vec<(i64, i64)> =
            ids_new.iter().enumerate().map(|(i, id)| (*id, ((i + 7) * 3) as i64)).collect();
        crate::db::update_mesh_positions_batch_inner(&conn_new, &updates).unwrap();

        // Per-row baseline (re-implemented locally so the test pins the
        // contract, not just itself).
        let conn_old = conn_with_meshes();
        let mut ids_old = Vec::with_capacity(50);
        for i in 0..50 {
            ids_old.push(insert_mesh(&conn_old, &format!("old-{i}")));
        }
        let tx = conn_old.unchecked_transaction().unwrap();
        for (id, pos) in &updates {
            tx.execute(
                "UPDATE meshes SET position = ?1 WHERE id = ?2",
                rusqlite::params![pos, id],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let positions_new: Vec<i64> = ids_new.iter().map(|id| read_position(&conn_new, *id)).collect();
        let positions_old: Vec<i64> = ids_old.iter().map(|id| read_position(&conn_old, *id)).collect();
        assert_eq!(positions_new, positions_old);
    }

    /// The new bulk UPDATE must drive an `INTEGER PRIMARY KEY` lookup, not
    /// a full scan — the per-row baseline was explicitly preserved on
    /// that point when #1746 was authored (issue §"Solution").
    #[test]
    fn update_mesh_positions_batch_inner_query_plan_uses_primary_key() {
        let conn = conn_with_meshes();
        for i in 0..10 {
            insert_mesh(&conn, &format!("plan-{i}"));
        }
        let mut stmt = conn
            .prepare("EXPLAIN QUERY PLAN UPDATE meshes SET position = CASE id WHEN ?1 THEN ?2 WHEN ?3 THEN ?4 END WHERE id IN (?5, ?6)")
            .unwrap();
        let plan = stmt
            .query_map(
                rusqlite::params![1_i64, 10_i64, 2_i64, 20_i64, 1_i64, 2_i64],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            plan.iter().any(|line| line.contains("PRIMARY KEY") || line.contains("rowid")),
            "bulk UPDATE must use the INTEGER PRIMARY KEY index, got plan: {plan:?}"
        );
    }

    /// Issue #1746 bench: time 500 rows for the new bulk form and the
    /// per-row baseline against the global WAL-enabled DB so the commit
    /// cost (the real bottleneck on production) is captured.
    /// Marked `#[ignore]` so it does not run in normal CI; opt in with
    /// `cargo test --lib db::mesh_tests::tests::update_mesh_positions_batch_bench_500_rows -- --ignored --nocapture`.
    /// The numbers are reported via `eprintln!` and pasted into the PR
    /// body. The loose 1× assertion catches a catastrophic regression
    /// without flaking on slow Windows runners — the genuine production
    /// benefit is writer-mutex lock-hold time (the per-row path holds the
    /// mutex for N × fsync, the bulk path for one), which is captured by
    /// the N→1 commit collapse regardless of in-memory vs WAL benches.
    #[test]
    #[ignore = "issue #1746 bench; run with --ignored --nocapture to print before/after numbers"]
    fn update_mesh_positions_batch_bench_500_rows() {
        // Use the global DB so commits pay real WAL fsyncs (the cost the
        // issue targets) — an in-memory conn would skip fsyncs and make
        // the two paths look equivalent.
        crate::db::test_support::ensure_db_for_tests();
        let _serial = serial();

        // Unique paths per run — multiple benches in the same DB never
        // collide on the `meshes.path` UNIQUE constraint.
        let run_id = uuid::Uuid::new_v4();
        let mut ids = Vec::with_capacity(500);
        for i in 0..500 {
            let mesh = crate::db::create_mesh(
                &format!("bench-{run_id}-{i}"),
                &format!("/tmp/bench-{run_id}-{i}"),
            )
            .unwrap();
            ids.push(mesh.id);
        }
        let updates: Vec<(i64, i64)> =
            ids.iter().enumerate().map(|(i, id)| (*id, (i + 1) as i64)).collect();

        // Warm up the writer mutex + prepare cache.
        crate::db::update_mesh_positions_batch(&updates[..10]).unwrap();

        // New bulk path (issue #1746).
        let started = std::time::Instant::now();
        crate::db::update_mesh_positions_batch(&updates).unwrap();
        let bulk_elapsed = started.elapsed();

        // Per-row baseline — same shape the old code path had, re-implemented
        // locally so the bench numbers are self-contained (no git stashing).
        let started = std::time::Instant::now();
        {
            let db = crate::db::write_conn();
            let tx = db.unchecked_transaction().unwrap();
            for (id, pos) in &updates {
                tx.execute(
                    "UPDATE meshes SET position = ?1 WHERE id = ?2",
                    rusqlite::params![pos, id],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let per_row_elapsed = started.elapsed();

        eprintln!(
            "[bench #1746] update_mesh_positions_batch x500 rows (WAL, global DB): \
             bulk={bulk_elapsed:?}, per_row={per_row_elapsed:?}, \
             speedup={speedup:.1}x",
            speedup = per_row_elapsed.as_secs_f64() / bulk_elapsed.as_secs_f64(),
        );
        // Loose floor: catch a catastrophic regression without flaking on
        // the inherent noise of microsecond-scale wall-clock measurements.
        assert!(
            per_row_elapsed.as_secs_f64() / bulk_elapsed.as_secs_f64() >= 1.0,
            "bulk form regressed below 1x: bulk={bulk_elapsed:?}, per_row={per_row_elapsed:?}",
        );
    }

    /// All-or-nothing: an error mid-batch must roll back every row that
    /// the same `tx` already updated. Pre-#1746 paid N commits, so a
    /// mid-batch failure left earlier rows committed; the post-#1746
    /// single-transaction path keeps the "one batch, one commit"
    /// guarantee the issue acceptance criteria call for. The trigger
    /// fires on the sentinel id; the earlier-in-the-IN-list row must
    /// still be at its original position.
    #[test]
    fn update_mesh_positions_batch_inner_rolls_back_on_trigger_error() {
        let conn = conn_with_meshes();
        let keep = insert_mesh(&conn, "rollback-keep");
        let sentinel = insert_mesh(&conn, "rollback-sentinel");
        assert_eq!(read_position(&conn, keep), 0);
        assert_eq!(read_position(&conn, sentinel), 0);

        // BEFORE UPDATE trigger fires per affected row; rows whose id
        // matches the sentinel fail the whole statement. SQLite still
        // processes earlier rows in the same CASE-WHEN before the trigger
        // raises, so the tx must roll back all of them on the way out.
        conn.execute_batch(&format!(
            "CREATE TRIGGER abort_on_sentinel BEFORE UPDATE ON meshes \
             WHEN NEW.id = OLD.id AND OLD.id = {sentinel} \
             BEGIN SELECT RAISE(ABORT, 'sentinel'); END;"
        )).unwrap();

        let err = crate::db::update_mesh_positions_batch_inner(
            &conn,
            &[(keep, 100), (sentinel, 200)],
        )
        .expect_err("trigger must abort the update");
        assert!(
            err.to_string().contains("sentinel"),
            "error must surface the trigger's message, got {err}"
        );

        assert_eq!(
            read_position(&conn, keep),
            0,
            "the earlier-in-the-IN-list row must survive the rollback"
        );
        assert_eq!(
            read_position(&conn, sentinel),
            0,
            "the sentinel row must survive the rollback"
        );
    }
}
