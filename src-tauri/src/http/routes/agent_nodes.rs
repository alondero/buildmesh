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

use crate::db;
use crate::http::request;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::state;

pub async fn discover(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
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
            Response::json("200 OK", body)
        }
        Ok(None) => Response::json_error("404 Not Found", "Mesh not found"),
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

#[derive(serde::Deserialize)]
struct ImportAndResumeRequest {
    cli_session_id: String,
    branch: String,
    worktree_name: Option<String>,
    /// The archive entry's originating harness (issue #2332). Required, and
    /// resolved before the session-id gate so that gate can use the harness's
    /// own id shape. See [`LEGACY_CLIENT_NO_PROVENANCE`].
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

/// What a Buildmesh mobile client from before issue #2332 gets when it resumes
/// with no provenance. The old contract made omission mean "Anthropic", which
/// is how a Cursor, Command Code or Antigravity conversation ended up launched
/// through Claude Code. There is no server-side guess that fixes it — only the
/// scanning client knows which harness it read — so omission is a client error
/// with an actionable message, not a silent default.
const LEGACY_CLIENT_NO_PROVENANCE: &str =
    "This Buildmesh mobile client is out of date: resuming an archived session needs the \
harness that created it. Update the app to resume archived conversations.";

pub async fn import_and_resume(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();

    let parsed: ImportAndResumeRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    // The originating harness decides which id shape is legal, so it is
    // resolved first: `resolve_resumable_harness` is the single place that
    // knows whether this build can resume through that harness at all.
    let Some(provider) = parsed
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    else {
        return Response::json_error("400 Bad Request", LEGACY_CLIENT_NO_PROVENANCE);
    };
    let executor = match crate::services::agent_node::resolve_resumable_harness(provider) {
        Ok(executor) => executor,
        Err(error) => {
            return Response::json_error("400 Bad Request", &error.to_string());
        }
    };

    // Gate the `cli_session_id` at the boundary (issue #1237): resume
    // appends the value raw to a harness CLI as `--resume <id>`, so a
    // string beginning with `-` lands in flag position and the harness
    // interprets it as an additional flag. The shared validator parses
    // AND canonicalises — the stored value is the same lowercase form
    // the spawn pipeline writes, so downstream resume lookups don't need
    // a separate case-fold step. Which validator applies is the harness's
    // own business (issue #2332): a `ses_…` OpenCode id or an `mvs_…`
    // mcode id must not be refused as "not a UUID", and a harness that
    // uses UUIDs must still be refused on anything else.
    let cli_session_id = match request::parse_session_id_for_provider(
        executor.to_string().as_str(),
        &parsed.cli_session_id,
    ) {
        Some(canonical) => canonical,
        None => {
            let harness = executor.adapter().id();
            let label = crate::agent::harness_catalog::inspector_label_for_adapter(harness)
                .unwrap_or(harness);
            return Response::json_error(
                "400 Bad Request",
                &format!("cli_session_id is not a valid {label} session id"),
            );
        }
    };

    let branch_owned = parsed.branch.clone();
    let worktree_owned = parsed.worktree_name.clone();
    let provider_owned = provider.to_string();
    let cli_session_id_for_closure = cli_session_id.clone();

    // One offload for the shared import runner (mesh lookup, node insert,
    // session id, reload). MeshNotFound is 404; an unusable harness is a 400
    // (no row is written); every other failure is 500.
    // Spawn stays here: a spawn failure is 207 with the row still returned.
    let node = match crate::commands::run_blocking("http_import_resume_prepare", move || {
        let result = crate::services::agent_node::import_discovered_session(
            mesh_id,
            &cli_session_id_for_closure,
            &branch_owned,
            worktree_owned.as_deref(),
            &provider_owned,
        );
        Ok::<_, String>(result)
    })
    .await
    {
        Ok(Ok(node)) => node,
        Ok(Err(crate::services::agent_node::AgentNodeError::MeshNotFound(_))) => {
            return Response::json_error("404 Not Found", "Mesh not found");
        }
        Ok(Err(error @ crate::services::agent_node::AgentNodeError::UnresumableHarness(_))) => {
            return Response::json_error("400 Bad Request", &error.to_string());
        }
        Ok(Err(error)) => {
            return Response::json_error("500 Internal Server Error", &error.to_string());
        }
        Err(msg) => {
            return Response::json_error("500 Internal Server Error", &msg);
        }
    };

    let Some(app) = state::app_handle() else {
        return Response::json_error("503 Service Unavailable", "App not ready");
    };

    if let Err(e) = crate::agent::spawn::spawn_with_intent(
        app,
        crate::agent::spawn::SpawnRequest::new(
            node.id,
            crate::agent::spawn::SpawnIntent::Resume {
                cause: crate::agent::spawn::ResumeCause::Explicit,
            },
            crate::agent::spawn::TerminalSize {
                rows: parsed.rows,
                cols: parsed.cols,
            },
        )
        .with_lifecycle_lease(),
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
        return Response::json("207 Multi-Status", body);
    }

    let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());
    Response::json("200 OK", body)
}

#[cfg(test)]
mod tests {
    //! Security boundary tests for issue #1237: a `cli_session_id` beginning
    //! with `-` lands in argv flag position on the harness CLI
    //! (`--resume <id>`), letting a malicious mobile client smuggle extra
    //! flags. The validator must reject at the route boundary before any
    //! DB work. Issue #2332 widened that gate to the conversation's own
    //! harness, so each case names the harness it is impersonating.
    //!
    //! These tests call `import_and_resume` through [`ParsedRequest`] so they
    //! do not open sockets. A request that fails a gate returns 400 without
    //! touching the DB; the two that pass one fall through to
    //! `db::get_mesh_by_id(0)`, which returns 404 in the per-test DB. Both are
    //! non-400, which is all these tests assert.
    use super::*;

    async fn drive(body: &[u8]) -> Response {
        import_and_resume(
            &ParsedRequest::test_post("/api/meshes/0/agent-nodes/import-and-resume", body)
                .with_ids(Some(0), None),
        )
        .await
    }

    fn status_line(resp: &Response) -> String {
        format!("HTTP/1.1 {}", resp.status)
    }

    /// A well-formed mixed-case UUID for a UUID harness must NOT be rejected
    /// with 400 — the validator accepts it and canonicalises to lowercase. The
    /// route then reaches `db::get_mesh_by_id(0)` and returns 404 in the
    /// per-test DB. Any non-400 response confirms the validator did its job
    /// and the canonicalisation didn't reject a valid input.
    #[tokio::test]
    async fn accepts_valid_mixed_case_uuid() {
        let body = br#"{"cli_session_id":"C1234567-89AB-CDEF-0123-456789ABCDEF","branch":"main","provider":"cursor"}"#;
        let resp = drive(body).await;
        let s = status_line(&resp);
        assert!(
            !s.starts_with("HTTP/1.1 400"),
            "valid mixed-case UUID must not be rejected with 400; got: {s:?}"
        );
    }

    /// The pre-#2332 mobile client sent no provider at all, and the old import
    /// read that as "Anthropic". A Cursor conversation resumed that way ran
    /// through Claude Code, so omission must now be refused with an actionable
    /// message rather than defaulted.
    #[tokio::test]
    async fn rejects_absent_provenance_instead_of_defaulting_to_anthropic() {
        let body = br#"{"cli_session_id":"C1234567-89AB-CDEF-0123-456789ABCDEF","branch":"main"}"#;
        let resp = drive(body).await;
        let s = status_line(&resp);
        assert!(s.starts_with("HTTP/1.1 400"), "expected 400; got: {s:?}");
        let body_str = String::from_utf8_lossy(resp.body());
        assert!(
            body_str.contains("out of date"),
            "expected an actionable upgrade message; got: {body_str:?}"
        );
    }

    /// An unknown harness name is the same refusal with the discovery-facing
    /// wording: the entry exists, Buildmesh just cannot say who wrote it.
    #[tokio::test]
    async fn rejects_unknown_provenance() {
        let body = br#"{"cli_session_id":"C1234567-89AB-CDEF-0123-456789ABCDEF","branch":"main","provider":"totally-not-a-harness"}"#;
        let resp = drive(body).await;
        assert!(
            status_line(&resp).starts_with("HTTP/1.1 400"),
            "expected 400 for unknown harness; got: {:?}",
            status_line(&resp)
        );
        let body_str = String::from_utf8_lossy(resp.body());
        assert!(
            body_str.contains("does not know which agent harness"),
            "expected the unknown-harness sentence; got: {body_str:?}"
        );
    }

    /// A plain Terminal row carries `provider: "terminal"` and is honest about
    /// not being resumable. Import must refuse it by name instead of spawning
    /// a shell and calling it a resume.
    #[tokio::test]
    async fn rejects_a_harness_that_cannot_resume() {
        let body = br#"{"cli_session_id":"C1234567-89AB-CDEF-0123-456789ABCDEF","branch":"main","provider":"terminal"}"#;
        let resp = drive(body).await;
        assert!(
            status_line(&resp).starts_with("HTTP/1.1 400"),
            "expected 400 for a non-resumable harness; got: {:?}",
            status_line(&resp)
        );
        let body_str = String::from_utf8_lossy(resp.body());
        assert!(
            body_str.contains("cannot resume"),
            "expected the non-resumable sentence; got: {body_str:?}"
        );
    }

    /// A Proxied Provider id (`claude:minimax`) is not an adapter id, but it
    /// does execute Claude Code — the id gate must follow it to that executor's
    /// UUID shape instead of refusing the composite id as unknown.
    #[tokio::test]
    async fn proxied_provider_id_resolves_to_the_claude_executor() {
        let body = br#"{"cli_session_id":"C1234567-89AB-CDEF-0123-456789ABCDEF","branch":"main","provider":"claude:minimax"}"#;
        let resp = drive(body).await;
        assert!(
            !status_line(&resp).starts_with("HTTP/1.1 400"),
            "a proxied Claude profile must resume; got: {:?}",
            status_line(&resp)
        );
    }

    /// The gate is the harness's own id shape: a harness that uses UUIDs must
    /// still be refused on anything else, now that the route knows which
    /// harness is asking.
    #[tokio::test]
    async fn rejects_flag_like_cli_session_id() {
        let body = br#"{"cli_session_id":"--dangerously-skip-permissions","branch":"main","provider":"anthropic"}"#;
        let resp = drive(body).await;
        let s = status_line(&resp);
        assert!(s.starts_with("HTTP/1.1 400"), "expected 400; got: {s:?}");
        // The error envelope must name the harness's session id so the mobile
        // SPA can surface something actionable.
        let body_str = String::from_utf8_lossy(resp.body());
        assert!(
            body_str.contains("cli_session_id is not a valid"),
            "expected a session-id validation message; got: {body_str:?}"
        );
    }

    /// A second flag-prefix string. Belt-and-braces the flag-prefix
    /// rejection — the validator must catch every `-`-prefixed input, not
    /// just the exact payload from the issue spec.
    #[tokio::test]
    async fn rejects_short_flag_like_cli_session_id() {
        let body = br#"{"cli_session_id":"-x","branch":"main","provider":"cursor"}"#;
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
        let body = br#"{"cli_session_id":"","branch":"main","provider":"agy"}"#;
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
        let body = br#"{"cli_session_id":"$(whoami)","branch":"main","provider":"commandcode"}"#;
        let resp = drive(body).await;
        assert!(
            status_line(&resp).starts_with("HTTP/1.1 400"),
            "expected 400 for shell metacharacters; got: {:?}",
            status_line(&resp)
        );
    }
}
