//! Integration tests for project creation edge cases.
//!
//! Tests that verify create_project handles duplicate paths gracefully,
//! returning the existing project instead of crashing with UNIQUE constraint.
//!
//! Run with: cargo test --package buildmesh --lib db::project_tests -- --test-threads=1

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

        // Create first project
        let first = crate::db::create_project("First Project", "/tmp/dup-test").unwrap();
        assert_eq!(first.name, "First Project");
        assert_eq!(first.layout, "grid");

        // Act: create another project with the same path but different name
        let second_result = crate::db::create_project("Second Project", "/tmp/dup-test");

        // Cleanup
        drop(crate::db::get().lock().unwrap());
        std::fs::remove_file(&temp_path).ok();

        // Assert: should return Ok(existing_project), NOT Err(UNIQUE constraint)
        match second_result {
            Ok(project) => {
                assert_eq!(project.name, "First Project", "should return the FIRST (existing) project");
                assert_eq!(project.layout, "grid", "should preserve original layout");
            }
            Err(e) => {
                panic!("create_project with duplicate path should NOT error, but got: {}", e);
            }
        }
    }
}
