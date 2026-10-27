use super::*;
use crate::agent::process::{AgentProcess, AgentProcessParams, InputQueueGauge};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct RunningAgent(i64);

impl Drop for RunningAgent {
    fn drop(&mut self) {
        PROCESS_REGISTRY.kill_session(self.0);
    }
}

fn register_running_agent(node_id: i64) -> RunningAgent {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command =
        portable_pty::CommandBuilder::new(if cfg!(windows) { "cmd.exe" } else { "sh" });
    if cfg!(windows) {
        command.args(["/d", "/q"]);
    }
    let child = pair.slave.spawn_command(command).unwrap();
    let (writer_tx, _writer_rx) = std::sync::mpsc::sync_channel(1);
    PROCESS_REGISTRY.insert(
        node_id,
        AgentProcess::new(AgentProcessParams {
            child,
            writer_tx,
            queue: Arc::new(InputQueueGauge::default()),
            writer_handle: None,
            master: pair.master,
            reader_alive: Arc::new(AtomicBool::new(true)),
            deliberate_kill: Arc::new(AtomicBool::new(false)),
            job: None,
            reader_handle: None,
            spawn_start: std::time::Instant::now(),
            mesh_id: 0,
        }),
    );
    RunningAgent(node_id)
}

fn close_request(id: i64) -> ParsedRequest {
    ParsedRequest::test_post(&format!("/api/nodes/{id}/close"), b"{}").with_ids(Some(id), None)
}

#[tokio::test]
async fn close_removes_only_its_node_preserves_files_and_can_be_retried() {
    let _db = db::test_support::isolated();
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("worktree");
    std::fs::create_dir(&checkout).unwrap();
    let file = checkout.join("unsaved.txt");
    std::fs::write(&file, "keep this work").unwrap();
    let mesh = db::create_mesh("mobile-close", &root.path().to_string_lossy()).unwrap();
    let make_node = |name| {
        db::create_agent_node(db::NewAgentNode {
            mesh_id: mesh.id,
            name,
            path: &checkout.to_string_lossy(),
            branch: "main",
            env: crate::models::EnvType::Windows,
            provider: "terminal",
            worktree_name: Some("worktree"),
            source_issue: None,
            source_pr: None,
            source_pr_pinned_sha: None,
            use_worktree: true,
            head_repo_owner: None,
            head_repo_clone_url: None,
            worktree_path: Some(&checkout.to_string_lossy()),
            spawn_configuration: None,
        })
        .unwrap()
    };
    let mut node = make_node("close-me");
    // Process registries are global even though each test's database is private.
    // Give this process its own id instead of registering the usual first row.
    let process_id = 8_400_001;
    db::write_conn()
        .execute(
            "UPDATE agent_nodes SET id = ?1 WHERE id = ?2",
            rusqlite::params![process_id, node.id],
        )
        .unwrap();
    node.id = process_id;
    let sibling = make_node("keep-me");
    let _running = register_running_agent(node.id);
    let process = PROCESS_REGISTRY.get(&node.id).unwrap();
    assert!(process.child.lock().unwrap().try_wait().unwrap().is_none());
    let mut request = close_request(node.id);
    let unauthenticated = crate::http::router::dispatch(request.clone()).await;
    assert!(
        matches!(unauthenticated, crate::http::router::DispatchResult::Http(response) if response.status_code() == 401)
    );
    assert!(db::get_agent_node_by_id(node.id).is_ok());
    let token = db::get_or_create_root_token().unwrap();
    request
        .headers
        .push_str(&format!("Cookie: bm_session={token}\r\n"));
    let result = crate::http::router::dispatch(request.clone()).await;
    assert!(
        matches!(result, crate::http::router::DispatchResult::Http(response) if response.status_code() == 204)
    );
    assert!(matches!(
        db::get_agent_node_by_id(node.id),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    assert!(db::get_agent_node_by_id(sibling.id).is_ok());
    assert!(!PROCESS_REGISTRY.contains(&node.id));
    assert!(process.deliberate_kill.load(Ordering::SeqCst));
    assert!(process.master.lock().unwrap().is_none());
    assert!(db::list_pending_worktree_removals().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep this work");
    assert_eq!(close(&request).await.status_code(), 204);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep this work");
}

#[tokio::test]
async fn close_rejects_invalid_ids_and_does_not_turn_storage_failure_into_success() {
    let _db = db::test_support::isolated();
    assert_eq!(close(&close_request(0)).await.status_code(), 400);
    db::write_conn()
        .execute_batch("DROP TABLE agent_nodes")
        .unwrap();
    assert_eq!(close(&close_request(1)).await.status_code(), 500);
}
