//! Agent-node discovery routes for the mobile SPA.
//!
//! "Discovered nodes" are external CLI runs (Claude Code, Gemini, ...)
//! that buildmesh found on disk but doesn't yet track. The mobile flow:
//!   1. GET /api/meshes/{id}/agent-nodes/discover → list them
//!   2. POST /api/meshes/{id}/agent-nodes/import-and-resume → create the
//!      agent node, store the cli_session_id, and immediately spawn the
//!      agent with `--resume` so the user lands on a live terminal.
//!
//! Renamed from `sessions.rs` in issue #490: the public HTTP surface uses
//! "Agent Node" vocabulary. The old `/sessions/*` paths are kept alive in
//! the dispatcher (src/http/mod.rs) for one release as a deprecation shim
//! with a `Deprecation: true` response header.

use crate::http::MaybeTls;

use crate::db;
use crate::http::request;

pub async fn discover(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    mesh_id: i64,
) {
    // Issue #1389 — `services::agent_node_discovery::discover` walks every
    // session directory under `~/.claude/projects/`, `~/.cursor/projects/`,
    // and `~/.gemini/antigravity-cli/brain/` and reads each JSONL line by
    // line. That's tens of milliseconds of `std::fs` + `serde_json` work
    // that would otherwise park the tokio worker that's polling the
    // WebSocket and PTY streams. Offload the whole sequence (mesh lookup
    // + disk walk) in one `run_blocking` so the worker only hops once.
    //
    // `run_blocking` is generic over `T: Send + 'static`, so the closure
    // returns `Result<Option<Vec<ArchivedAgentNode>>, String>` — `None`
    // means "mesh not found" (route → 404), `Some(_)` is the discovery
    // payload (route → 200). The route distinguishes 404 vs 500 without
    // string sentinels or a second offload.
    match crate::commands::run_blocking("http_discover_agent_nodes", move || {
        let mesh = match db::get_mesh_by_id(mesh_id) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };
        crate::services::agent_node_discovery::discover(mesh_id, &mesh.path)
            .map(Some)
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(Some(nodes)) => {
            let body = serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Ok(None) => {
            request::send_json_error(lines, "404 Not Found", "Mesh not found").await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}

#[derive(serde::Deserialize)]
struct ImportAndResumeRequest {
    cli_session_id: String,
    branch: String,
    worktree_name: Option<String>,
    provider: Option<String>,
    #[serde(default = "default_rows")]
    rows: u16,
    #[serde(default = "default_cols")]
    cols: u16,
}
fn default_rows() -> u16 {
    24
}
fn default_cols() -> u16 {
    80
}

pub async fn import_and_resume(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    mesh_id: i64,
    content_length: usize,
) {
    let Some(body_bytes) =
        request::read_body_or_send_error(lines, content_length, 64 * 1024).await
    else {
        return;
    };

    let req: ImportAndResumeRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            request::send_json_error(lines, "400 Bad Request", &format!("Invalid JSON: {}", e))
                .await;
            return;
        }
    };

    // Gate the `cli_session_id` at the boundary (issue #1237): resume
    // appends the value raw to a harness CLI as `--resume <id>`, so a
    // string beginning with `-` lands in flag position and the harness
    // interprets it as an additional flag. The shared validator parses
    // AND canonicalises — the stored value is the same lowercase form
    // the spawn pipeline writes, so downstream resume lookups don't need
    // a separate case-fold step.
    let cli_session_id = match request::parse_cli_session_id(&req.cli_session_id) {
        Some(canonical) => canonical,
        None => {
            request::send_json_error(
                lines,
                "400 Bad Request",
                "cli_session_id must be a valid UUID",
            )
            .await;
            return;
        }
    };

    let mesh_id = mesh_id;
    let session_name = crate::session_naming::on_spawn();
    // Store the harness/profile id verbatim (issue #535); resolve to a
    // concrete executor only at the spawn seam. Absent → "anthropic".
    let provider_id = req.provider.as_deref().unwrap_or("anthropic").to_string();
    let branch_owned = req.branch.clone();
    let worktree_owned = req.worktree_name.clone();
    let cli_session_id_for_closure = cli_session_id.clone();

    // Issue #1389 / PR #1429 review feedback: bundle the entire pre-spawn
    // SQLite sequence (mesh lookup + create_agent_node + cli_session_id
    // update + reload) into a single `run_blocking` call so the tokio
    // worker only hops once. The closure returns
    // `Result<Option<AgentNode>, String>` — three outcomes:
    //   * `Ok(Some(node))` — pre-spawn setup succeeded, ready to spawn.
    //   * `Ok(None)`       — mesh lookup failed → route returns 404.
    //   * `Err(msg)`       — any SQLite failure → route returns 500.
    // The reload is folded into the same closure (rather than a fourth
    // offload) so the returned row already carries the just-written
    // `cli_session_id`.
    let node = match crate::commands::run_blocking(
        "http_import_resume_prepare",
        move || -> Result<Option<crate::models::AgentNode>, String> {
            let mesh = match db::get_mesh_by_id(mesh_id) {
                Ok(m) => m,
                Err(_) => return Ok(None),
            };
            // Issue #1519: persist the effective `worktree_path` so HTTP
            // imports agree with spawns on the configured directory.
            let app_dir = crate::preferences::worktree_directory();
            let effective_dir = crate::env::effective_worktree_dir_raw(
                &mesh.path,
                mesh.worktree_directory.as_deref(),
                app_dir.as_deref(),
            );
            let worktree_path_owned: Option<String> = worktree_owned
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|n| crate::env::resolve_worktree_node_raw(&effective_dir, n));
            let resolved =
                crate::env::resolve_agent_path_in_dir(&mesh.path, &effective_dir, worktree_owned.as_deref());
            let use_worktree = worktree_owned.is_some();
            let mut node = match db::create_agent_node(
                mesh_id,
                &session_name,
                &mesh.path,
                &branch_owned,
                resolved.env_type,
                &provider_id,
                worktree_owned.as_deref(),
                None,
                None,
                None, // source_pr_pinned_sha — HTTP route doesn't accept a pinned SHA
                use_worktree,
                None,
                None,
                worktree_path_owned.as_deref(),
            ) {
                Ok(n) => n,
                Err(e) => return Err(format!("create_agent_node failed: {e}")),
            };
            if let Err(e) = db::update_cli_session_id(node.id, &cli_session_id_for_closure) {
                return Err(format!("update_cli_session_id failed: {e}"));
            }
            node = match db::get_agent_node_by_id(node.id) {
                Ok(n) => n,
                Err(e) => return Err(format!("Failed to reload node: {e}")),
            };
            Ok(Some(node))
        },
    )
    .await
    {
        Ok(Some(node)) => node,
        Ok(None) => {
            request::send_json_error(lines, "404 Not Found", "Mesh not found").await;
            return;
        }
        Err(msg) => {
            request::send_json_error(lines, "500 Internal Server Error", &msg).await;
            return;
        }
    };

    let Some(app) = crate::http::app_handle() else {
        request::send_json_error(lines, "503 Service Unavailable", "App not ready").await;
        return;
    };

    if let Err(e) = crate::agent::spawn::spawn_with_intent(
        app,
        crate::agent::spawn::SpawnRequest::new(
            node.id,
            crate::agent::spawn::SpawnIntent::Resume {
                cause: crate::agent::spawn::ResumeCause::Explicit,
            },
            crate::agent::spawn::TerminalSize {
                rows: req.rows,
                cols: req.cols,
            },
        ),
    )
    .await
    {
        // Don't surface as 500 — the node row exists and could still be
        // used; instead return the node + the spawn error in one payload
        // so the mobile UI can decide.
        let body = serde_json::to_string(&serde_json::json!({
            "node": node,
            "spawn_error": e,
        }))
        .unwrap_or_else(|_| "{}".to_string());
        let _ = request::write_json(lines, "207 Multi-Status", &body).await;
        return;
    }

    let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());
    let _ = request::write_json(lines, "200 OK", &body).await;
}

#[cfg(test)]
mod tests {
    //! Security boundary tests for issue #1237: a `cli_session_id` beginning
    //! with `-` lands in argv flag position on the harness CLI
    //! (`--resume <id>`), letting a malicious mobile client smuggle extra
    //! flags. The validator must reject at the route boundary before any
    //! DB work.
    //!
    //! These tests call `import_and_resume` directly with a real TCP socket
    //! so the body-read path is exercised end-to-end. They stop at the
    //! validator — `mesh_id = 0` never reaches a mesh lookup because the
    //! validator short-circuits with 400 first, and a successful validator
    //! pass falls through to `db::get_mesh_by_id(0)` which returns 404 in
    //! the per-test DB.
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Drive `import_and_resume` over a real TCP socket and return the
    /// response bytes. `mesh_id` is irrelevant for the validator path.
    async fn drive(body: &[u8]) -> Vec<u8> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Hoist the byte count out — `usize` is `Copy`, so the spawned
        // task captures it by value rather than borrowing the slice.
        let content_length = body.len();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut lines = tokio::io::BufStream::new(crate::http::MaybeTls::Plain(stream));
            import_and_resume(&mut lines, 0, content_length).await;
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(body).await.unwrap();
        stream.shutdown().await.unwrap();

        let mut resp = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read_to_end(&mut resp),
        )
        .await
        .expect("server hung");
        let _ = server.await;
        resp
    }

    fn status_line(resp: &[u8]) -> String {
        String::from_utf8_lossy(resp)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    }

    /// Exact attack from issue #1237: `--dangerously-skip-permissions` in the
    /// `cli_session_id` field would land in argv flag position. The route
    /// must reject with 400 BEFORE any DB call.
    #[tokio::test]
    async fn rejects_flag_like_cli_session_id() {
        let body = br#"{"cli_session_id":"--dangerously-skip-permissions","branch":"main"}"#;
        let resp = drive(body).await;
        let s = status_line(&resp);
        assert!(s.starts_with("HTTP/1.1 400"), "expected 400; got: {s:?}");
        // The error envelope must carry a UUID-validation message so the
        // mobile SPA can surface something actionable.
        let body_str = String::from_utf8_lossy(&resp);
        assert!(
            body_str.contains("must be a valid UUID"),
            "expected UUID validation message; got: {body_str:?}"
        );
    }

    /// A second flag-prefix string. Belt-and-braces the flag-prefix
    /// rejection — the validator must catch every `-`-prefixed input, not
    /// just the exact payload from the issue spec.
    #[tokio::test]
    async fn rejects_short_flag_like_cli_session_id() {
        let body = br#"{"cli_session_id":"-x","branch":"main"}"#;
        let resp = drive(body).await;
        assert!(
            status_line(&resp).starts_with("HTTP/1.1 400"),
            "expected 400 for short-flag cli_session_id; got: {:?}",
            status_line(&resp)
        );
    }

    /// Empty string: the spawn pipeline treats empty as "no session" (issue
    /// #949), so an empty `cli_session_id` would silently degrade to a
    /// fresh spawn instead of a resume — and `is_empty()` only catches the
    /// empty case after the validator already accepted it. The validator
    /// must reject so a malformed mobile request never reaches the spawn
    /// path at all.
    #[tokio::test]
    async fn rejects_empty_cli_session_id() {
        let body = br#"{"cli_session_id":"","branch":"main"}"#;
        let resp = drive(body).await;
        assert!(
            status_line(&resp).starts_with("HTTP/1.1 400"),
            "expected 400 for empty cli_session_id; got: {:?}",
            status_line(&resp)
        );
    }

    /// Shell metacharacters must never parse as a UUID. Belt-and-braces
    /// against a future bypass.
    #[tokio::test]
    async fn rejects_shell_metacharacters_in_cli_session_id() {
        let body = br#"{"cli_session_id":"$(whoami)","branch":"main"}"#;
        let resp = drive(body).await;
        assert!(
            status_line(&resp).starts_with("HTTP/1.1 400"),
            "expected 400 for shell metacharacters; got: {:?}",
            status_line(&resp)
        );
    }

    /// A well-formed mixed-case UUID must NOT be rejected with 400 — the
    /// validator accepts it and canonicalises to lowercase. The route then
    /// reaches `db::get_mesh_by_id(0)` and returns 404 in the per-test DB.
    /// Any non-400 response confirms the validator did its job and the
    /// canonicalisation didn't reject a valid input.
    #[tokio::test]
    async fn accepts_valid_mixed_case_uuid() {
        let body = br#"{"cli_session_id":"C1234567-89AB-CDEF-0123-456789ABCDEF","branch":"main"}"#;
        let resp = drive(body).await;
        let s = status_line(&resp);
        assert!(
            !s.starts_with("HTTP/1.1 400"),
            "valid mixed-case UUID must not be rejected with 400; got: {s:?}"
        );
    }
}
