//! File delivery for long terminal handovers to Codex.

use crate::models::{AgentNode, EnvType, Provider};
use std::path::{Path, PathBuf};

fn node_dir(root: &Path, node_id: i64) -> PathBuf {
    root.join("handovers").join(format!("node-{node_id}"))
}

pub(crate) fn prepare(node: &AgentNode, text: &str) -> Result<String, String> {
    let provider = node
        .launch_configuration
        .as_ref()
        .and_then(|configuration| configuration.resolved.as_ref())
        .map_or_else(
            || crate::preferences::resolve_harness_provider(&node.provider),
            |plan| Provider::from_db_str(&plan.harness.harness),
        );
    if provider != Provider::Codex
        || text.chars().count() <= crate::circuit::delivery::VISIBLE_PASTE_TEXT_LIMIT
    {
        return Ok(text.to_string());
    }
    let root = crate::preferences::app_data_dir().ok_or_else(|| {
        "Could not save the handover: app data directory is unavailable".to_string()
    })?;
    // A busy node may read the brief after another handover has arrived.
    let path = node_dir(&root, node.id).join(format!("{}.md", uuid::Uuid::new_v4()));
    let visible = crate::circuit::handoff::agent_visible_path(&path, node.env);
    let visible = if node.env == EnvType::WindowsInterop {
        crate::env::windows_path_from_wsl(&visible)
    } else {
        visible
    };
    let prompt = crate::circuit::handoff::pointer_line(&format!("\"{visible}\""));
    if prompt.contains(['\r', '\n']) {
        return Err("Could not save the handover: the file path contains a newline".into());
    }
    crate::circuit::handoff::write_prompt(&path, text)
        .map_err(|error| format!("Could not save the handover: {error}"))?;
    // Deletion can commit and clean up after the caller took its node snapshot.
    // Checking after the write covers deletion that finished before staging;
    // deletion that follows this check performs the normal directory cleanup.
    if !crate::db::agent_node_exists(node.id)
        .map_err(|error| format!("Could not save the handover: {error}"))?
    {
        remove_node_files(node.id);
        return Err("Could not save the handover: the target node was deleted".into());
    }
    Ok(prompt)
}

pub(crate) fn remove_node_files(node_id: i64) {
    let Some(root) = crate::preferences::app_data_dir() else {
        return;
    };
    if let Err(error) = std::fs::remove_dir_all(node_dir(&root, node_id)) {
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
            let node = crate::db::create_agent_node(
                mesh.id,
                "handover",
                &mesh.path,
                "main",
                EnvType::Windows,
                "codex",
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
            )
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
        let prompt = prepare(&fixture.node, &text).unwrap();
        let files = fixture.files();
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read(&files[0]).unwrap(), text.as_bytes());
        assert_eq!(
            prompt,
            format!(
                "Read and follow the complete task brief in this file: \"{}\"",
                crate::circuit::handoff::agent_visible_path(&files[0], fixture.node.env)
            )
        );
        assert!(!prompt.contains(['\r', '\n']));
    }

    #[test]
    fn later_handovers_keep_the_earlier_file_intact() {
        let fixture = Fixture::new();
        let first_text = "first review\n".repeat(40);
        let second_text = "second review\n".repeat(40);
        let first = prepare(&fixture.node, &first_text).unwrap();
        let second = prepare(&fixture.node, &second_text).unwrap();
        assert_ne!(first, second);
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
        let error = prepare(&fixture.node, &"review\n".repeat(100)).unwrap_err();
        assert!(error.contains("Could not save the handover"), "{error}");
    }

    #[test]
    fn short_codex_and_other_harness_handovers_keep_their_text() {
        let mut fixture = Fixture::new();
        let at_limit = "é".repeat(256);
        assert_eq!(prepare(&fixture.node, &at_limit).unwrap(), at_limit);
        fixture.node.provider = "anthropic".into();
        let long = "long review\r\n".repeat(100);
        assert_eq!(prepare(&fixture.node, &long).unwrap(), long);
        assert!(!fixture.root.path().join("handovers").exists());
    }

    #[test]
    fn the_first_character_over_the_limit_uses_file_delivery() {
        let fixture = Fixture::new();
        let text = "é".repeat(257);
        assert_ne!(prepare(&fixture.node, &text).unwrap(), text);
        assert_eq!(
            fs::read(fixture.files().pop().unwrap()).unwrap(),
            text.as_bytes()
        );
    }

    #[test]
    fn missing_app_data_prevents_delivery() {
        let fixture = Fixture::new();
        crate::preferences::reset_for_tests();
        let error = prepare(&fixture.node, &"review\n".repeat(100)).unwrap_err();
        assert_eq!(
            error,
            "Could not save the handover: app data directory is unavailable"
        );
    }

    #[test]
    fn saved_launch_harness_takes_precedence_over_the_profile_id() {
        let mut fixture = Fixture::new();
        fixture.node.provider = "launch/saved-reviewer".into();
        fixture.node.launch_configuration = Some(serde_json::from_value(serde_json::json!({
            "id": "launch/saved-reviewer", "name": "Reviewer", "spawn_option_id": "custom-codex",
            "model": null, "effort": null, "extra_args": null,
            "resolved": {
                "configuration_id": "launch/saved-reviewer", "configuration_name": "Reviewer",
                "spawn_option_id": "custom-codex",
                "harness": { "id": "custom-codex", "name": "Codex", "harness": "codex" },
                "route": null, "model": null, "effort": null, "extra_args": null
            }
        })).unwrap());
        let text = "review\n".repeat(100);
        let prompt = prepare(&fixture.node, &text).unwrap();
        assert_ne!(prompt, text);
        assert_eq!(
            fs::read(fixture.files().pop().unwrap()).unwrap(),
            text.as_bytes()
        );

        fixture.node.provider = "codex".into();
        fixture
            .node
            .launch_configuration
            .as_mut()
            .unwrap()
            .resolved
            .as_mut()
            .unwrap()
            .harness
            .harness = "anthropic".into();
        assert_eq!(prepare(&fixture.node, &text).unwrap(), text);
    }

    #[test]
    fn process_exit_keeps_files_and_permanent_deletion_removes_them() {
        let fixture = Fixture::new();
        prepare(&fixture.node, &"review\n".repeat(100)).unwrap();
        crate::agent::node_teardown::release(fixture.node.id);
        assert_eq!(fixture.files().len(), 1);
        crate::services::agent_node::delete(fixture.node.id, false).unwrap();
        assert!(!node_dir(fixture.root.path(), fixture.node.id).exists());
    }

    #[test]
    fn deletion_before_staging_rejects_the_stale_node_and_removes_recreated_files() {
        let fixture = Fixture::new();
        prepare(&fixture.node, &"first review\n".repeat(100)).unwrap();
        crate::services::agent_node::delete(fixture.node.id, false).unwrap();
        let error = prepare(&fixture.node, &"stale review\n".repeat(100)).unwrap_err();
        assert_eq!(
            error,
            "Could not save the handover: the target node was deleted"
        );
        assert!(!node_dir(fixture.root.path(), fixture.node.id).exists());
    }
}
