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
        drop(crate::db::lock_db());
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

    /// Ticket #994 — the narrow `set_mesh_autopilot_enabled` Start/Stop write
    /// flips ONLY `autopilot_enabled` and leaves the issue-driven policy
    /// columns untouched (the reason it exists instead of reusing
    /// `set_mesh_autopilot`). A missing mesh id updates zero rows so the
    /// command layer can surface "mesh not found".
    #[test]
    fn test_set_mesh_autopilot_enabled_is_narrow() {
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("buildmesh_ap_enabled_test_{}.db", test_id));
        crate::db::init(&temp_path).unwrap();

        let path = format!("/tmp/ap-enabled-test-{}", test_id);
        let mesh = crate::db::create_mesh("Loop Mesh", &path).unwrap();

        // Seed a full issue-driven policy so we can prove the narrow toggle
        // preserves it.
        crate::db::set_mesh_autopilot(
            mesh.id,
            false,
            Some("buildmesh:run"),
            3,
            Some("minimax"),
            Some("draft_pr"),
        )
        .unwrap();

        // Start: flip enabled on WITHOUT clobbering the policy columns.
        let rows = crate::db::set_mesh_autopilot_enabled(mesh.id, true).unwrap();
        assert_eq!(rows, 1, "one row updated");
        let on = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert!(on.autopilot_enabled, "Start enables the mesh");
        assert_eq!(
            on.autopilot_trigger_label.as_deref(),
            Some("buildmesh:run"),
            "narrow toggle preserves the trigger label"
        );
        assert_eq!(on.autopilot_concurrency_limit, 3, "preserves concurrency");
        assert_eq!(on.autopilot_provider.as_deref(), Some("minimax"));

        // Stop: flip enabled off, policy still intact.
        crate::db::set_mesh_autopilot_enabled(mesh.id, false).unwrap();
        let off = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert!(!off.autopilot_enabled, "Stop disables the mesh");
        assert_eq!(off.autopilot_concurrency_limit, 3, "still preserved on Stop");

        // Missing mesh → zero rows (drives the command's not-found guard).
        let missing = crate::db::set_mesh_autopilot_enabled(999_999, true).unwrap();
        assert_eq!(missing, 0, "no rows updated for a nonexistent mesh");

        // Command layer maps the zero-rows case to the "mesh not found" error
        // contract (ticket #994) — the guard the plan calls for, exercised
        // through the async command via the repo's `block_on` idiom.
        let err = tauri::async_runtime::block_on(
            crate::commands::mesh_properties::set_mesh_autopilot_enabled(999_999, true),
        )
        .expect_err("a missing mesh must surface an error, not a silent success");
        assert!(
            err.contains("not found"),
            "command surfaces the not-found contract, got: {err}"
        );

        std::fs::remove_file(&temp_path).ok();
    }

    /// Wayfinder #990 / ticket #991 — Looping Autopilot config columns
    /// (v30) default to `IssueDriven` mode + no prompts + 0/None caps on
    /// a fresh mesh, persist through `set_mesh_loop_config`, and read back
    /// via `get_mesh_by_id` (i.e. survive an app reload). Clearing the
    /// optional strings with `None` returns them to `None` (poller reads
    /// these as "loop not configured"). Mirrors the autopilot-policy
    /// round-trip pattern just above.
    #[test]
    fn test_mesh_loop_config_round_trips() {
        use crate::db::AutopilotMode;

        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("buildmesh_loop_test_{}.db", test_id));
        crate::db::init(&temp_path).unwrap();

        let path = format!("/tmp/loop-test-{}", test_id);
        let mesh = crate::db::create_mesh("Loop Mesh", &path).unwrap();
        // Defaults — every column at its schema default (mode = issue_driven,
        // prompts = None, caps = 0 / None).
        assert_eq!(mesh.autopilot_mode, AutopilotMode::IssueDriven);
        assert_eq!(mesh.loop_initial_prompt, None);
        assert_eq!(mesh.loop_suffix_prompt, None);
        assert_eq!(mesh.loop_max_iterations, None);
        assert_eq!(mesh.loop_interval_seconds, 0);
        assert_eq!(mesh.loop_consecutive_failures, 0);

        // Write all six columns in one atomic UPDATE, then read back.
        let rows = crate::db::set_mesh_loop_config(
            mesh.id,
            AutopilotMode::Looping,
            Some("ship the next iteration of X"),
            Some("now run the verify suite and report"),
            Some(5),
            30,
            3,
        )
        .unwrap();
        assert_eq!(rows, 1, "one row updated");
        let saved = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert_eq!(saved.autopilot_mode, AutopilotMode::Looping);
        assert_eq!(saved.loop_initial_prompt.as_deref(), Some("ship the next iteration of X"));
        assert_eq!(saved.loop_suffix_prompt.as_deref(), Some("now run the verify suite and report"));
        assert_eq!(saved.loop_max_iterations, Some(5));
        assert_eq!(saved.loop_interval_seconds, 30);
        assert_eq!(saved.loop_consecutive_failures, 3);

        // Unknown persisted value (e.g. a row written by a future build with
        // a renamed mode) degrades to IssueDriven — same fail-open semantics
        // as SessionStatus::from_db_str. Defensive: the poller refuses to
        // crash on a malformed row.
        {
            let db = crate::db::lock_db();
            db.execute(
                "UPDATE meshes SET autopilot_mode = 'tomorrow' WHERE id = ?1",
                rusqlite::params![mesh.id],
            )
            .unwrap();
        }
        let degraded = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert_eq!(
            degraded.autopilot_mode,
            AutopilotMode::IssueDriven,
            "unknown autopilot_mode strings must degrade to IssueDriven (fail-open for the poller)"
        );

        // Back to defaults via the same helper.
        crate::db::set_mesh_loop_config(
            mesh.id,
            AutopilotMode::IssueDriven,
            None,
            None,
            None,
            0,
            0,
        )
        .unwrap();
        let cleared = crate::db::get_mesh_by_id(mesh.id).unwrap();
        assert_eq!(cleared.autopilot_mode, AutopilotMode::IssueDriven);
        assert_eq!(cleared.loop_initial_prompt, None);
        assert_eq!(cleared.loop_suffix_prompt, None);
        assert_eq!(cleared.loop_max_iterations, None);
        assert_eq!(cleared.loop_interval_seconds, 0);
        assert_eq!(cleared.loop_consecutive_failures, 0);

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

        use crate::db::AutopilotRunState as S;
        crate::db::create_autopilot_run(node.id, mesh.id, 42).unwrap();
        assert_eq!(crate::db::count_active_autopilot_nodes(mesh.id).unwrap(), 1);
        let (issue, state, attempts, loop_iteration, pr_url) =
            crate::db::get_autopilot_run(node.id).unwrap().expect("run row exists");
        assert_eq!((issue, state, attempts), (42, S::Implementing, 0));
        assert_eq!(loop_iteration, None, "issue-driven rows are not loop iterations");
        assert_eq!(pr_url, None);

        // The issue number is known → the poller must not respawn it.
        let known = crate::db::list_known_autopilot_issue_numbers(mesh.id).unwrap();
        assert!(known.contains(&42));

        // finishing still occupies the slot; completed frees it.
        crate::db::set_autopilot_run_state(node.id, S::Finishing, Some(1)).unwrap();
        assert_eq!(crate::db::count_active_autopilot_nodes(mesh.id).unwrap(), 1);
        crate::db::set_autopilot_run_state(node.id, S::Completed, None).unwrap();
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

    /// Poller re-drive work list (node 2328, 2026-07-17): a `finishing` run
    /// becomes a candidate only once its ledger row has been quiet for the
    /// stale window; fresh activity and state advances take it back off.
    #[test]
    fn test_stalled_finishing_runs_listing() {
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("buildmesh_autopilot_stalled_{}.db", test_id));
        crate::db::init(&temp_path).unwrap();

        let path = format!("/tmp/autopilot-stalled-{}", test_id);
        let mesh = crate::db::create_mesh("Stalled Mesh", &path).unwrap();
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

        use crate::db::AutopilotRunState as S;
        crate::db::create_autopilot_run(node.id, mesh.id, 42).unwrap();
        crate::db::set_autopilot_run_state(node.id, S::Finishing, Some(2)).unwrap();

        // The state write just bumped updated_at → not stalled yet.
        assert!(
            !crate::db::list_stalled_finishing_autopilot_runs(5)
                .unwrap()
                .contains(&node.id),
            "a run with fresh pipeline activity must not be re-driven"
        );

        // Backdate the row as if no pipeline activity happened for 10 minutes.
        {
            let db = crate::db::lock_db();
            db.execute(
                "UPDATE autopilot_runs SET updated_at = datetime('now', '-10 minutes') \
                 WHERE node_id = ?1",
                rusqlite::params![node.id],
            )
            .unwrap();
        }
        assert!(
            crate::db::list_stalled_finishing_autopilot_runs(5)
                .unwrap()
                .contains(&node.id),
            "a quiet finishing run must surface as a re-drive candidate"
        );

        // Any state advance takes it off the work list.
        crate::db::set_autopilot_run_state(node.id, S::Completed, None).unwrap();
        assert!(!crate::db::list_stalled_finishing_autopilot_runs(5)
            .unwrap()
            .contains(&node.id));

        std::fs::remove_file(&temp_path).ok();
    }

    /// Merged-PR auto-close plumbing: the recorded wrap-up PR makes a
    /// completed run sweepable, the pill listing reports every live run,
    /// and archiving the node removes it from both.
    #[test]
    fn test_autopilot_run_pr_recording_and_sweep_listing() {
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("buildmesh_autopilot_pr_{}.db", test_id));
        crate::db::init(&temp_path).unwrap();

        let path = format!("/tmp/autopilot-pr-{}", test_id);
        let mesh = crate::db::create_mesh("PR Mesh", &path).unwrap();
        let node = crate::db::create_agent_node(
            mesh.id,
            "gh99-node",
            &mesh.path,
            "origin/main",
            crate::models::EnvType::Windows,
            "anthropic",
            None,
            Some(99),
            None,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        crate::db::create_autopilot_run(node.id, mesh.id, 99).unwrap();

        // Pill listing sees the run while the node is live.
        let states = crate::db::list_autopilot_run_states().unwrap();
        assert!(states.contains(&(node.id, crate::db::AutopilotRunState::Implementing)));

        // Not sweepable yet: completed but no PR recorded.
        crate::db::set_autopilot_run_state(node.id, crate::db::AutopilotRunState::Completed, None)
            .unwrap();
        assert!(crate::db::list_completed_autopilot_runs_with_pr(mesh.id)
            .unwrap()
            .is_empty());

        // Recording the wrap-up PR makes it sweepable.
        crate::db::set_autopilot_run_pr(node.id, 512, "https://github.com/x/y/pull/512")
            .unwrap();
        assert_eq!(
            crate::db::list_completed_autopilot_runs_with_pr(mesh.id).unwrap(),
            vec![(node.id, 512)]
        );

        // The sweep archives the node + moves the run to `merged`: it must
        // drop out of the sweep list AND the pill listing, but stay deduped.
        crate::db::archive_agent_node(node.id).unwrap();
        crate::db::set_autopilot_run_state(node.id, crate::db::AutopilotRunState::Merged, None)
            .unwrap();
        assert!(crate::db::list_completed_autopilot_runs_with_pr(mesh.id)
            .unwrap()
            .is_empty());
        let states = crate::db::list_autopilot_run_states().unwrap();
        assert!(states.iter().all(|(id, _)| *id != node.id));
        let known = crate::db::list_known_autopilot_issue_numbers(mesh.id).unwrap();
        assert!(known.contains(&99), "archived run still dedupes its issue");

        std::fs::remove_file(&temp_path).ok();
    }

    /// Issue #993 — a loop iteration that has passed deterministic wrap-up
    /// remains active while its optional suffix turn runs. The same ledger row
    /// must retain its iteration/PR context, then release capacity only when
    /// that suffix turn reaches `Completed`.
    #[test]
    fn test_loop_suffix_pending_preserves_context_and_capacity() {
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("buildmesh_loop_suffix_{}.db", test_id));
        crate::db::init(&temp_path).unwrap();

        let path = format!("/tmp/loop-suffix-{}", test_id);
        let mesh = crate::db::create_mesh("Loop Suffix Mesh", &path).unwrap();
        let total_before = crate::db::count_active_autopilot_nodes_total().unwrap();
        let node = crate::db::create_agent_node(
            mesh.id,
            "loop-iter-4",
            &mesh.path,
            "origin/main",
            crate::models::EnvType::Windows,
            "anthropic",
            None,
            None,
            None,
            None,
            true,
            None,
            None,
        )
        .unwrap();

        use crate::db::AutopilotRunState as S;
        crate::db::create_autopilot_loop_run(node.id, mesh.id, 4).unwrap();
        crate::db::set_autopilot_run_pr(node.id, 993, "https://github.com/x/y/pull/993")
            .unwrap();
        crate::db::set_autopilot_run_state(node.id, S::SuffixPending, Some(2)).unwrap();

        assert_eq!(S::SuffixPending.as_db_str(), "suffix_pending");
        assert_eq!(S::from_db_str("suffix_pending"), S::SuffixPending);
        let (issue, state, attempts, iteration, pr_url) =
            crate::db::get_autopilot_run(node.id).unwrap().expect("run row exists");
        assert_eq!(issue, 0);
        assert_eq!(state, S::SuffixPending);
        assert_eq!(attempts, 2, "suffix turn does not consume a wrap-up attempt");
        assert_eq!(iteration, Some(4));
        assert_eq!(pr_url.as_deref(), Some("https://github.com/x/y/pull/993"));
        assert_eq!(crate::db::count_active_autopilot_nodes(mesh.id).unwrap(), 1);
        assert_eq!(
            crate::db::count_active_autopilot_nodes_total().unwrap(),
            total_before + 1
        );
        assert!(crate::db::list_active_autopilot_node_ids()
            .unwrap()
            .contains(&node.id));

        // `suffix_pending` is active but is not another stale wrap-up
        // verification candidate, even when its timestamp is old.
        {
            let db = crate::db::lock_db();
            db.execute(
                "UPDATE autopilot_runs SET updated_at = datetime('now', '-10 minutes') \
                 WHERE node_id = ?1",
                rusqlite::params![node.id],
            )
            .unwrap();
        }
        assert!(!crate::db::list_stalled_finishing_autopilot_runs(5)
            .unwrap()
            .contains(&node.id));

        crate::db::set_autopilot_run_state(node.id, S::Completed, None).unwrap();
        assert_eq!(crate::db::count_active_autopilot_nodes(mesh.id).unwrap(), 0);
        assert_eq!(
            crate::db::count_active_autopilot_nodes_total().unwrap(),
            total_before
        );
        let rows = crate::db::list_loop_iterations(mesh.id).unwrap();
        assert!(rows.iter().any(|(iteration, state, _)| {
            *iteration == 4 && *state == S::Completed
        }));

        std::fs::remove_file(&temp_path).ok();
    }
}
