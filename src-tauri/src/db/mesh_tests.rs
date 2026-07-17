//! Integration tests for mesh creation edge cases.
//!
//! Tests that verify create_mesh handles duplicate paths gracefully,
//! returning the existing mesh instead of crashing with UNIQUE constraint.
//!
//! Run with: cargo test --package buildmesh --lib db::mesh_tests -- --test-threads=1

#[cfg(test)]
mod tests {
    /// Test: creating a project with a duplicate path should NOT crash.
    /// Expected behavior: return the existing project (idempotent upsert).
    #[test]
    fn test_create_project_with_duplicate_path_returns_existing() {
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
        drop(crate::db::get().lock().unwrap());
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

    /// Issue #481 — the Autopilot Policy columns default off/2/None on a
    /// fresh mesh, persist through `set_mesh_autopilot`, and read back via
    /// `get_mesh_by_id` (i.e. survive an app reload). Clearing the optional
    /// strings with `None` returns them to `None` (poller defaults apply).
    #[test]
    fn test_mesh_autopilot_policy_round_trips() {
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("buildmesh_autopilot_test_{}.db", test_id));
        crate::db::init(&temp_path).unwrap();

        let path = format!("/tmp/autopilot-test-{}", test_id);
        let mesh = crate::db::create_mesh("Autopilot Mesh", &path).unwrap();
        assert!(!mesh.autopilot_enabled, "autopilot starts disabled");
        assert_eq!(mesh.autopilot_concurrency_limit, 2, "default limit is 2");
        assert_eq!(mesh.autopilot_trigger_label, None);
        assert_eq!(mesh.autopilot_provider, None);
        assert_eq!(mesh.autopilot_action_on_success, None);

        let rows = crate::db::set_mesh_autopilot(
            mesh.id,
            true,
            Some("buildmesh:run"),
            3,
            Some("minimax"),
            Some("draft_pr"),
        )
        .unwrap();
        assert_eq!(rows, 1, "one row updated");
        let saved = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert!(saved.autopilot_enabled);
        assert_eq!(saved.autopilot_trigger_label.as_deref(), Some("buildmesh:run"));
        assert_eq!(saved.autopilot_concurrency_limit, 3);
        assert_eq!(saved.autopilot_provider.as_deref(), Some("minimax"));
        assert_eq!(saved.autopilot_action_on_success.as_deref(), Some("draft_pr"));

        // The enabled mesh appears on the poller's work list.
        let enabled = crate::db::list_autopilot_enabled_meshes().unwrap();
        assert!(enabled.iter().any(|m| m.id == mesh.id));

        // Disable + clear optionals → back to defaults.
        crate::db::set_mesh_autopilot(mesh.id, false, None, 2, None, None).unwrap();
        let cleared = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert!(!cleared.autopilot_enabled);
        assert_eq!(cleared.autopilot_trigger_label, None);
        let enabled = crate::db::list_autopilot_enabled_meshes().unwrap();
        assert!(!enabled.iter().any(|m| m.id == mesh.id));

        std::fs::remove_file(&temp_path).ok();
    }

    /// Issue #482 — the `autopilot_runs` ledger: create → active count,
    /// state transitions, dedupe list, and slot release on completion.
    #[test]
    fn test_autopilot_runs_ledger_counts_and_dedupes() {
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("buildmesh_autopilot_runs_{}.db", test_id));
        crate::db::init(&temp_path).unwrap();

        let path = format!("/tmp/autopilot-runs-{}", test_id);
        let mesh = crate::db::create_mesh("Runs Mesh", &path).unwrap();
        let node = crate::db::create_agent_node(
            mesh.id,
            "gh42-node",
            &mesh.path,
            "origin/main",
            crate::models::EnvType::Windows,
            "anthropic",
            None,
            Some(42),
            None,
            None,
            true,
            None,
            None,
        )
        .unwrap();

        crate::db::create_autopilot_run(node.id, mesh.id, 42).unwrap();
        assert_eq!(crate::db::count_active_autopilot_nodes(mesh.id).unwrap(), 1);
        let (issue, state, attempts) =
            crate::db::get_autopilot_run(node.id).unwrap().expect("run row exists");
        assert_eq!((issue, state.as_str(), attempts), (42, "implementing", 0));

        // The issue number is known → the poller must not respawn it.
        let known = crate::db::list_known_autopilot_issue_numbers(mesh.id).unwrap();
        assert!(known.contains(&42));

        // finishing still occupies the slot; completed frees it.
        crate::db::set_autopilot_run_state(node.id, "finishing", Some(1)).unwrap();
        assert_eq!(crate::db::count_active_autopilot_nodes(mesh.id).unwrap(), 1);
        crate::db::set_autopilot_run_state(node.id, "completed", None).unwrap();
        assert_eq!(crate::db::count_active_autopilot_nodes(mesh.id).unwrap(), 0);
        // ...but the issue stays deduped even after completion.
        let known = crate::db::list_known_autopilot_issue_numbers(mesh.id).unwrap();
        assert!(known.contains(&42));

        // A hand-spawned issue node (no run row) also dedupes.
        crate::db::create_agent_node(
            mesh.id,
            "gh43-node",
            &mesh.path,
            "origin/main",
            crate::models::EnvType::Windows,
            "anthropic",
            None,
            Some(43),
            None,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        let known = crate::db::list_known_autopilot_issue_numbers(mesh.id).unwrap();
        assert!(known.contains(&43));

        std::fs::remove_file(&temp_path).ok();
    }
}
