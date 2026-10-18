//! File delivery for long terminal handovers to Codex.

use crate::models::{AgentNode, EnvType, Provider};
use std::path::{Path, PathBuf};

fn node_dir(root: &Path, node_id: i64) -> PathBuf {
    root.join("handovers").join(format!("node-{node_id}"))
}

#[derive(Debug)]
pub(crate) struct PreparedHandover {
    prompt: String,
    staged_file: Option<PathBuf>,
}

impl PreparedHandover {
    pub(crate) fn prompt(&self) -> &str {
        &self.prompt
    }

    pub(crate) fn into_prompt(self) -> String {
        self.prompt
    }

    /// Remove this attempt's file only when the PTY rejected the pointer.
    pub(crate) fn discard(self) {
        if let Some(path) = self.staged_file {
            remove_staged_file(&path);
        }
    }
}

pub(crate) fn prepare(
    node: &AgentNode,
    provider: Provider,
    text: &str,
    app_data_dir: Option<&Path>,
) -> Result<PreparedHandover, String> {
    if provider != Provider::Codex
        || text.chars().count() <= crate::circuit::delivery::VISIBLE_PASTE_TEXT_LIMIT
    {
        return Ok(PreparedHandover {
            prompt: text.to_string(),
            staged_file: None,
        });
    }
    let root = app_data_dir.ok_or_else(|| {
        "Could not save the handover: app data directory is unavailable".to_string()
    })?;
    // A busy node may read the brief after another handover has arrived.
    let path = node_dir(root, node.id).join(format!("{}.md", uuid::Uuid::new_v4()));
    let prompt = pointer_line_for_path(&path, node.env);
    if prompt.contains(['\r', '\n']) {
        return Err("Could not save the handover: the file path contains a newline".into());
    }
    crate::circuit::handoff::write_prompt(&path, text)
        .map_err(|error| format!("Could not save the handover: {error}"))?;
    // Deletion can commit and clean up after the caller took its node snapshot.
    // Checking after the write covers deletion that finished before staging.
    // A delete can still commit after this check and remove the file before
    // the caller submits its pointer; that pointer then targets a deleted node.
    match crate::db::agent_node_exists(node.id) {
        Ok(true) => {}
        Ok(false) => {
            remove_node_files(node.id, app_data_dir);
            return Err("Could not save the handover: the target node was deleted".into());
        }
        Err(error) => {
            remove_staged_file(&path);
            return Err(format!("Could not save the handover: {error}"));
        }
    }
    Ok(PreparedHandover {
        prompt,
        staged_file: Some(path),
    })
}

fn pointer_line_for_path(path: &Path, env: EnvType) -> String {
    let visible = crate::circuit::handoff::agent_visible_path(path, env);
    crate::circuit::handoff::pointer_line(&format!("\"{visible}\""))
}

fn remove_staged_file(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                "Could not remove rejected handover file {}: {error}",
                path.display()
            );
        }
    }
}

pub(crate) fn remove_node_files(node_id: i64, app_data_dir: Option<&Path>) {
    let Some(root) = app_data_dir else {
        return;
    };
    if let Err(error) = std::fs::remove_dir_all(node_dir(root, node_id)) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("Could not remove handover files for node {node_id}: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::EnvType;
    use std::fs;
    use std::path::PathBuf;

    struct Fixture {
        root: tempfile::TempDir,
        node: AgentNode,
        _db: crate::db::test_support::IsolatedDbGuard,
    }

    impl Fixture {
        fn new() -> Self {
            let db = crate::db::test_support::isolated();
            let root = tempfile::tempdir().unwrap();
            crate::preferences::init_for_tests(root.path().to_path_buf());
            let mesh = crate::db::create_mesh("handover", "C:/handover-fixture").unwrap();
            let node = crate::db::create_agent_node(crate::db::NewAgentNode {
                mesh_id: mesh.id,
                name: "handover",
                path: &mesh.path,
                branch: "main",
                env: EnvType::Windows,
                provider: "codex",
                worktree_name: None,
                source_issue: None,
                source_pr: None,
                source_pr_pinned_sha: None,
                use_worktree: false,
                head_repo_owner: None,
                head_repo_clone_url: None,
                worktree_path: None,
                spawn_configuration: None,
            })
            .unwrap();
            Self {
                root,
                node,
                _db: db,
            }
        }

        fn files(&self) -> Vec<PathBuf> {
            fs::read_dir(node_dir(self.root.path(), self.node.id))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            crate::preferences::reset_for_tests();
        }
    }

    #[test]
    fn long_codex_handover_preserves_the_complete_utf8_text_in_a_file() {
        let fixture = Fixture::new();
        let text = "REQUEST CHANGES.\r\nReview all findings — 🚀\r\n".repeat(40);
        let prompt = prepare(
            &fixture.node,
            Provider::Codex,
            &text,
            Some(fixture.root.path()),
        )
        .unwrap();
        let files = fixture.files();
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read(&files[0]).unwrap(), text.as_bytes());
        assert_eq!(
            prompt.prompt(),
            format!(
                "Read and follow the complete task brief in this file: \"{}\"",
                crate::circuit::handoff::agent_visible_path(&files[0], fixture.node.env)
            )
        );
        assert!(!prompt.prompt().contains(['\r', '\n']));
    }

    #[test]
    fn later_handovers_keep_the_earlier_file_intact() {
        let fixture = Fixture::new();
        let first_text = "first review\n".repeat(40);
        let second_text = "second review\n".repeat(40);
        let first = prepare(
            &fixture.node,
            Provider::Codex,
            &first_text,
            Some(fixture.root.path()),
        )
        .unwrap();
        let second = prepare(
            &fixture.node,
            Provider::Codex,
            &second_text,
            Some(fixture.root.path()),
        )
        .unwrap();
        assert_ne!(first.prompt(), second.prompt());
        let mut contents: Vec<_> = fixture
            .files()
            .iter()
            .map(|path| fs::read_to_string(path).unwrap())
            .collect();
        contents.sort();
        assert_eq!(contents, vec![first_text, second_text]);
    }

    #[test]
    fn a_staging_failure_does_not_fall_back_to_a_long_paste() {
        let fixture = Fixture::new();
        fs::write(fixture.root.path().join("handovers"), "blocked").unwrap();
        let error = prepare(
            &fixture.node,
            Provider::Codex,
            &"review\n".repeat(100),
            Some(fixture.root.path()),
        )
        .unwrap_err();
        assert!(error.contains("Could not save the handover"), "{error}");
    }

    #[test]
    fn short_codex_and_other_harness_handovers_keep_their_text() {
        let mut fixture = Fixture::new();
        let at_limit = "é".repeat(256);
        assert_eq!(
            prepare(
                &fixture.node,
                Provider::Codex,
                &at_limit,
                Some(fixture.root.path())
            )
            .unwrap()
            .prompt(),
            at_limit
        );
        fixture.node.provider = "anthropic".into();
        let long = "long review\r\n".repeat(100);
        assert_eq!(
            prepare(
                &fixture.node,
                Provider::Anthropic,
                &long,
                Some(fixture.root.path())
            )
            .unwrap()
            .prompt(),
            long
        );
        assert!(!fixture.root.path().join("handovers").exists());
    }

    #[test]
    fn the_first_character_over_the_limit_uses_file_delivery() {
        let fixture = Fixture::new();
        let text = "é".repeat(257);
        assert_ne!(
            prepare(
                &fixture.node,
                Provider::Codex,
                &text,
                Some(fixture.root.path())
            )
            .unwrap()
            .prompt(),
            text
        );
        assert_eq!(
            fs::read(fixture.files().pop().unwrap()).unwrap(),
            text.as_bytes()
        );
    }

    #[test]
    fn missing_app_data_prevents_delivery() {
        let fixture = Fixture::new();
        crate::preferences::reset_for_tests();
        let error = prepare(
            &fixture.node,
            Provider::Codex,
            &"review\n".repeat(100),
            None,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Could not save the handover: app data directory is unavailable"
        );
    }

    #[test]
    fn handover_staging_uses_the_resolved_provider_argument() {
        let fixture = Fixture::new();
        let text = "review\n".repeat(100);
        let prompt = prepare(
            &fixture.node,
            Provider::Codex,
            &text,
            Some(fixture.root.path()),
        )
        .unwrap();
        assert_ne!(prompt.prompt(), text);
        assert_eq!(
            fs::read(fixture.files().pop().unwrap()).unwrap(),
            text.as_bytes()
        );

        assert_eq!(
            prepare(
                &fixture.node,
                Provider::Anthropic,
                &text,
                Some(fixture.root.path())
            )
            .unwrap()
            .prompt(),
            text
        );
    }

    #[test]
    fn node_file_cleanup_uses_the_supplied_app_data_root() {
        let root = tempfile::tempdir().unwrap();
        let dir = node_dir(root.path(), 42);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("saved.md"), "handover").unwrap();

        remove_node_files(42, Some(root.path()));

        assert!(!dir.exists());
    }

    #[test]
    fn process_exit_keeps_files_and_permanent_deletion_removes_them() {
        let fixture = Fixture::new();
        prepare(
            &fixture.node,
            Provider::Codex,
            &"review\n".repeat(100),
            Some(fixture.root.path()),
        )
        .unwrap();
        crate::agent::node_teardown::release(fixture.node.id);
        assert_eq!(fixture.files().len(), 1);
        crate::services::agent_node::delete(fixture.node.id, false).unwrap();
        assert!(!node_dir(fixture.root.path(), fixture.node.id).exists());
    }

    #[test]
    fn deletion_before_staging_rejects_the_stale_node_and_removes_recreated_files() {
        let fixture = Fixture::new();
        prepare(
            &fixture.node,
            Provider::Codex,
            &"first review\n".repeat(100),
            Some(fixture.root.path()),
        )
        .unwrap();
        crate::services::agent_node::delete(fixture.node.id, false).unwrap();
        let error = prepare(
            &fixture.node,
            Provider::Codex,
            &"stale review\n".repeat(100),
            Some(fixture.root.path()),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Could not save the handover: the target node was deleted"
        );
        assert!(!node_dir(fixture.root.path(), fixture.node.id).exists());
    }

    #[test]
    fn wsl_handover_pointer_uses_the_agent_visible_mnt_path() {
        let path = Path::new(r"C:\Buildmesh\handovers\node-7\brief.md");
        assert_eq!(
            pointer_line_for_path(path, EnvType::Wsl),
            "Read and follow the complete task brief in this file: \"/mnt/c/Buildmesh/handovers/node-7/brief.md\""
        );
    }
}
