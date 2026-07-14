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
}
