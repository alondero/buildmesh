//! `GET /api/nodes/{id}/glance`: present, absent and unavailable answers.
use super::*;

fn glance_request(id: i64) -> ParsedRequest {
    ParsedRequest::test_get(&format!("/api/nodes/{id}/glance")).with_ids(Some(id), None)
}

fn body(response: &Response) -> serde_json::Value {
    serde_json::from_slice(response.body()).unwrap()
}

#[tokio::test]
async fn a_node_without_a_transcript_answers_its_timings_and_the_reason() {
    let _db = db::test_support::isolated();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_string_lossy().to_string();
    let mesh = db::create_mesh("mobile-glance", &path).unwrap();
    let node = db::create_agent_node(db::NewAgentNode {
        mesh_id: mesh.id,
        name: "shell",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "terminal",
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

    let response = glance(&glance_request(node.id)).await;
    assert_eq!(response.status_code(), 200);
    let json = body(&response);
    assert_eq!(json["node_id"], node.id);
    assert_eq!(json["transcript"], serde_json::Value::Null);
    assert_eq!(json["transcript_unavailable"], "unsupported");
    assert!(json["observed_at_ms"].as_i64().unwrap() > 0);
    assert!(json["created_at_ms"].as_i64().is_some());
}

#[tokio::test]
async fn a_missing_node_is_404_and_a_failed_read_is_not_success() {
    let _db = db::test_support::isolated();
    assert_eq!(glance(&glance_request(4242)).await.status_code(), 404);
    db::write_conn()
        .execute_batch("DROP TABLE agent_nodes")
        .unwrap();
    assert_eq!(glance(&glance_request(4242)).await.status_code(), 503);
}
