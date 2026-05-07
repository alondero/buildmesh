//! Checkpoint service — git snapshot creation, revert, and diff operations

use crate::db;
use crate::models::Checkpoint;
use git2::Repository;

/// Error type for checkpoint service operations
#[derive(Debug)]
pub enum CheckpointError {
    Db(rusqlite::Error),
    Git(git2::Error),
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckpointError::Db(e) => write!(f, "{}", e),
            CheckpointError::Git(e) => write!(f, "{}", e),
        }
    }
}

impl From<rusqlite::Error> for CheckpointError {
    fn from(e: rusqlite::Error) -> Self {
        CheckpointError::Db(e)
    }
}

impl From<git2::Error> for CheckpointError {
    fn from(e: git2::Error) -> Self {
        CheckpointError::Git(e)
    }
}

/// Create a checkpoint by committing current working state to a private git ref,
/// then recording the checkpoint in the database.
///
/// Returns the checkpoint record and the node path (for event emission by the caller).
pub fn create(
    session_id: i64,
    turn_index: i32,
    message: Option<&str>,
) -> Result<(Checkpoint, String), CheckpointError> {
    let node = db::get_agent_node_by_id(session_id)?;

    let repo = Repository::open(&node.path)?;

    // Stage all files and write the tree
    let mut index = repo.index()?;
    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    let ref_name = format!("refs/heads/conductor/checkpoints/c{}", turn_index);
    let sig = repo.signature()?;
    let commit_msg = message.unwrap_or_else(|| {
        Box::leak(format!("Checkpoint {}", turn_index).into_boxed_str()) as &str
    });

    // Check if ref already exists (for parent commit chain)
    let parent_commit = match repo.find_reference(&ref_name) {
        Ok(ref_ref) => Some(ref_ref.peel_to_commit()?),
        Err(_) => None,
    };

    if let Some(ref old_commit) = parent_commit {
        repo.reference(&ref_name, old_commit.id(), true, "update checkpoint ref")?;
    }

    let parent_refs: Vec<&git2::Commit> = parent_commit.iter().collect();
    repo.commit(Some(&ref_name), &sig, &sig, commit_msg, &tree, &parent_refs)?;

    // Record in database
    let git_ref = format!("conductor/checkpoints/c{}", turn_index);
    let checkpoint = db::create_checkpoint(session_id, &git_ref, turn_index, message)?;

    Ok((checkpoint, node.path))
}

/// Revert a session's working directory to a previously created checkpoint.
///
/// Returns the node path (for event emission by the caller).
pub fn revert(checkpoint_id: i64) -> Result<String, CheckpointError> {
    let checkpoint = db::get_checkpoint_by_id(checkpoint_id)?;
    let node = db::get_agent_node_by_id(checkpoint.node_id)?;

    let repo = Repository::open(&node.path)?;

    let ref_name = format!("refs/heads/{}", checkpoint.git_ref);
    let reference = repo.find_reference(&ref_name)?;
    let commit = reference.peel_to_commit()?;

    repo.checkout_tree(commit.as_object(), None)?;
    repo.set_head(&ref_name)?;

    Ok(node.path)
}

/// Compute a patch-format diff between two checkpoints.
pub fn diff(checkpoint_a_id: i64, checkpoint_b_id: i64) -> Result<String, CheckpointError> {
    let cp_a = db::get_checkpoint_by_id(checkpoint_a_id)?;
    let cp_b = db::get_checkpoint_by_id(checkpoint_b_id)?;

    let node = db::get_agent_node_by_id(cp_a.node_id)?;
    let repo = Repository::open(&node.path)?;

    let ref_a = format!("refs/heads/conductor/checkpoints/c{}", cp_a.turn_index);
    let ref_b = format!("refs/heads/conductor/checkpoints/c{}", cp_b.turn_index);

    let commit_a = repo.find_reference(&ref_a)?.peel_to_commit()?;
    let commit_b = repo.find_reference(&ref_b)?.peel_to_commit()?;

    let diff = repo.diff_tree_to_tree(
        commit_a.as_object().peel_to_tree().ok().as_ref(),
        commit_b.as_object().peel_to_tree().ok().as_ref(),
        None,
    )?;

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
    })?;

    Ok(diff_text)
}
