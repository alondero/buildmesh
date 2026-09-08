//! Backend-owned fixtures for the real-runtime test bridge.
//!
//! The HTTP bridge should only decode requests and encode responses. Fixture
//! assembly belongs here so it can use the production persistence seams while
//! keeping test setup out of command adapters.

use crate::autopilot::circuit::context::CircuitContext;
use crate::autopilot::circuit::model::StepOutcome;
use crate::db::CircuitStepOp;
use crate::models::{EnvType, SessionStatus};
use serde_json::Value;
use std::path::Path;

/// Create the completed node-review rows used by the real-runtime activity
/// screenshot. No agent process or worktree is created.
pub(crate) fn create_review_activity_fixture(name: &str) -> Result<Value, String> {
    let mesh = crate::services::mesh::create_test(name).map_err(|error| error.to_string())?;
    let mesh_path = mesh.path.clone();
    let result = create_review_activity_rows(mesh.id, &mesh);
    match result {
        Ok(data) => Ok(data),
        Err(error) => {
            match teardown_fixture(mesh.id, &mesh_path) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(format!(
                    "{}; fixture cleanup failed: {}",
                    error, cleanup_error
                )),
            }
        }
    }
}

fn create_review_activity_rows(mesh_id: i64, mesh: &crate::models::Mesh) -> Result<Value, String> {
    let source = crate::db::create_agent_node(
        mesh_id,
        "Review implementation",
        &mesh.path,
        "main",
        EnvType::Windows,
        "codex",
        Some("Review implementation"),
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .map_err(|error| error.to_string())?;
    crate::db::update_agent_node_status(source.id, SessionStatus::Completed)
        .map_err(|error| error.to_string())?;

    let reviewer = crate::db::create_agent_node(
        mesh_id,
        "Code reviewer",
        &mesh.path,
        "main",
        EnvType::Windows,
        "codex",
        Some("Code reviewer"),
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .map_err(|error| error.to_string())?;

    // Use the same production circuit creation path as the node-review
    // command so source.* context and the canonical preset stay realistic.
    let run_id = crate::db::create_node_circuit_run(source.id, None, 3)?;
    let run = crate::db::get_circuit_run(run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("review fixture run {} disappeared", run_id))?;
    let context = CircuitContext::from_json(&run.context_json)?;
    let parent_id = context
        .source_agent_id()
        .ok_or_else(|| "review fixture run has no source agent".to_string())?;
    if parent_id != source.id {
        return Err(format!(
            "review fixture source mismatch: expected {}, got {}",
            source.id, parent_id
        ));
    }

    // Terminalise before attaching the reviewer so the worker cannot observe
    // a pending/running fixture and launch a provider process.
    crate::db::commit_circuit_advance(
        run_id,
        Some("completed"),
        None,
        &[CircuitStepOp {
            node_id: "reviewer".into(),
            status: "completed".into(),
            outcome: Some(Some(StepOutcome::Completed.as_db_str().to_string())),
            error: None,
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .map_err(|error| error.to_string())?;

    let attached = crate::db::set_circuit_step_agent_node_with_parent(
        run_id,
        "reviewer",
        reviewer.id,
        Some(parent_id),
    )
    .map_err(|error| error.to_string())?;
    if !attached {
        return Err("review fixture reviewer step disappeared".to_string());
    }
    crate::db::update_agent_node_status(reviewer.id, SessionStatus::Completed)
        .map_err(|error| error.to_string())?;

    Ok(serde_json::json!({
        "mesh": mesh,
        "source": source.id,
        "reviewer": reviewer.id,
        "run": run_id,
    }))
}

/// Delete the fixture rows and then remove the temporary mesh root. The
/// fixture owns this directory and never creates a worktree inside it.
pub(crate) fn delete_review_activity_fixture(mesh_id: i64) -> Result<(), String> {
    let mesh_path = crate::db::get_mesh_by_id(mesh_id)
        .ok()
        .map(|mesh| mesh.path);
    if let Some(path) = mesh_path {
        teardown_fixture(mesh_id, &path)
    } else {
        crate::db::delete_mesh(mesh_id).map_err(|error| error.to_string())
    }
}

fn teardown_fixture(mesh_id: i64, mesh_path: &str) -> Result<(), String> {
    let db_error = crate::db::delete_mesh(mesh_id).err();
    let fs_error = remove_fixture_root(mesh_path).err();
    match (db_error, fs_error) {
        (None, None) => Ok(()),
        (Some(error), None) => Err(error.to_string()),
        (None, Some(error)) => Err(error),
        (Some(db_error), Some(fs_error)) => Err(format!(
            "database teardown failed: {}; root cleanup failed: {}",
            db_error, fs_error
        )),
    }
}

fn remove_fixture_root(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(|error| {
            format!("could not remove review fixture root {}: {}", path.display(), error)
        })?;
    }
    Ok(())
}
