//! Checkpoint management via Git refs for Buildmesh

use crate::db;
use crate::models::Checkpoint;
use git2::Repository;
use tauri::command;

/// Create a checkpoint — commits current state to a private git ref
#[command]
pub async fn create_checkpoint(
    workspace_id: i64,
    turn_index: i32,
    message: Option<String>,
) -> Result<Checkpoint, String> {
    let workspace = db::get_workspace_by_id(workspace_id)
        .map_err(|e| e.to_string())?;

    let repo = Repository::open(&workspace.path)
        .map_err(|e| e.to_string())?;

    let mut index = repo.index()
        .map_err(|e| e.to_string())?;
    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| e.to_string())?;
    index.write().map_err(|e| e.to_string())?;
    let tree_id = index.write_tree().map_err(|e| e.to_string())?;
    let tree = repo.find_tree(tree_id)
        .map_err(|e| e.to_string())?;

    let ref_name = format!("refs/heads/conductor/checkpoints/c{}", turn_index);
    let sig = repo.signature()
        .map_err(|e| e.to_string())?;
    let commit_msg = message.as_ref()
        .map(|s| s.as_str())
        .unwrap_or_else(|| Box::leak(format!("Checkpoint {}", turn_index).into_boxed_str()) as &str);

    // Check if ref already exists
    let parent_commit = match repo.find_reference(&ref_name) {
        Ok(ref_ref) => {
            Some(ref_ref.peel_to_commit().map_err(|e| e.to_string())?)
        }
        Err(_) => None,
    };

    if let Some(ref old_commit) = parent_commit {
        repo.reference(
            &ref_name,
            old_commit.id(),
            true,
            "update checkpoint ref",
        ).map_err(|e| e.to_string())?;
    }

    let parent_refs: Vec<&git2::Commit> = parent_commit.iter().collect();
    repo.commit(
        Some(&ref_name),
        &sig,
        &sig,
        commit_msg,
        &tree,
        &parent_refs,
    ).map_err(|e| e.to_string())?;

    let git_ref = format!("conductor/checkpoints/c{}", turn_index);
    db::create_checkpoint(workspace_id, &git_ref, turn_index, message.as_deref())
        .map_err(|e| e.to_string())
}

/// List checkpoints for a workspace
#[command]
pub async fn list_checkpoints(workspace_id: i64) -> Result<Vec<Checkpoint>, String> {
    db::list_checkpoints(workspace_id).map_err(|e| e.to_string())
}

/// Revert workspace to a specific checkpoint
#[command]
pub async fn revert_to_checkpoint(checkpoint_id: i64) -> Result<(), String> {
    let checkpoint = db::get_checkpoint_by_id(checkpoint_id)
        .map_err(|e| e.to_string())?;
    let workspace = db::get_workspace_by_id(checkpoint.workspace_id)
        .map_err(|e| e.to_string())?;

    let repo = Repository::open(&workspace.path)
        .map_err(|e| e.to_string())?;

    let ref_name = format!("refs/heads/{}", checkpoint.git_ref);
    let reference = repo.find_reference(&ref_name)
        .map_err(|e| e.to_string())?;
    let commit = reference.peel_to_commit()
        .map_err(|e| e.to_string())?;

    repo.checkout_tree(commit.as_object(), None)
        .map_err(|e| e.to_string())?;
    repo.set_head(&ref_name)
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Get the diff between two checkpoints
#[command]
pub async fn diff_checkpoints(
    checkpoint_a_id: i64,
    checkpoint_b_id: i64,
) -> Result<String, String> {
    let cp_a = db::get_checkpoint_by_id(checkpoint_a_id)
        .map_err(|e| e.to_string())?;
    let cp_b = db::get_checkpoint_by_id(checkpoint_b_id)
        .map_err(|e| e.to_string())?;

    let workspace = db::get_workspace_by_id(cp_a.workspace_id)
        .map_err(|e| e.to_string())?;
    let repo = Repository::open(&workspace.path)
        .map_err(|e| e.to_string())?;

    let ref_a = format!("refs/heads/conductor/checkpoints/c{}", cp_a.turn_index);
    let ref_b = format!("refs/heads/conductor/checkpoints/c{}", cp_b.turn_index);

    let commit_a = repo.find_reference(&ref_a)
        .and_then(|r| r.peel_to_commit())
        .map_err(|e| e.to_string())?;
    let commit_b = repo.find_reference(&ref_b)
        .and_then(|r| r.peel_to_commit())
        .map_err(|e| e.to_string())?;

    let diff = repo.diff_tree_to_tree(
        commit_a.as_object().peel_to_tree().ok().as_ref(),
        commit_b.as_object().peel_to_tree().ok().as_ref(),
        None,
    ).map_err(|e| e.to_string())?;

    let mut diff_text = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let prefix = match line.origin() {
            '+' => "+",
            '-' => "-",
            ' ' => " ",
            _ => "",
        };
        diff_text.push_str(prefix);
        diff_text.push_str(std::str::from_utf8(line.content()).unwrap_or(""));
        diff_text.push('\n');
        true
    }).map_err(|e| e.to_string())?;

    Ok(diff_text)
}
