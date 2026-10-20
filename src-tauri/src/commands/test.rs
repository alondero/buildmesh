//! Development HTTP bridge for Playwright and screenshot fixtures.
//!
//! Playwright reaches the Vite server, where Tauri `invoke()` is absent, so
//! this module can expose the same command bodies over a loopback socket.
//! It is compiled only for debug and `dev-tools` builds, and it listens only
//! when the process sets `BUILDMESH_TEST_BRIDGE=1` plus a per-launch token.
//! A release build does not include the listener.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use subtle::ConstantTimeEq;
use tauri::AppHandle;
use tauri::Emitter;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufStream};
use tokio::net::{TcpListener, TcpStream};

use crate::commands::node_activated::NodeActivatedPayload;
use crate::http::request::{bearer_token, extract_header_value, read_body_with_cap, ReadBodyError};
use crate::http::router::content_length;
use crate::http::server::read_request_head;

const TEST_SERVER_PORT: u16 = 1991;
const BRIDGE_ENABLE_ENV: &str = "BUILDMESH_TEST_BRIDGE";
const BRIDGE_TOKEN_ENV: &str = "BUILDMESH_TEST_BRIDGE_TOKEN";
const BRIDGE_ORIGINS_ENV: &str = "BUILDMESH_TEST_BRIDGE_ORIGINS";
const MIN_BRIDGE_TOKEN_LEN: usize = 32;
const BRIDGE_MAX_BODY: usize = 64 * 1024;

/// Per-launch admission policy. No `Debug`: the token must not land in a log.
#[derive(Clone)]
struct BridgeConfig {
    token: String,
    port: u16,
    origins: Vec<String>,
}

impl BridgeConfig {
    /// `None` means "do not bind". The enable flag without a usable token is a
    /// misconfiguration and is logged; the token value itself is never logged.
    fn from_env(port: u16) -> Option<Self> {
        let enabled = std::env::var(BRIDGE_ENABLE_ENV).ok().as_deref() == Some("1");
        if !enabled {
            tracing::info!(
                "[test_server] HTTP test bridge is off. Set {BRIDGE_ENABLE_ENV}=1 and {BRIDGE_TOKEN_ENV} to listen"
            );
            return None;
        }
        let token = std::env::var(BRIDGE_TOKEN_ENV).unwrap_or_default();
        if !token_is_acceptable(&token) {
            tracing::error!(
                "[test_server] {BRIDGE_ENABLE_ENV}=1 but {BRIDGE_TOKEN_ENV} must be at least {MIN_BRIDGE_TOKEN_LEN} non-whitespace characters; not binding"
            );
            return None;
        }
        let origins = std::env::var(BRIDGE_ORIGINS_ENV)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_owned)
            .collect();
        Some(Self {
            token,
            port,
            origins,
        })
    }

    #[cfg(test)]
    fn for_test(token: &str, port: u16, origins: &[&str]) -> Self {
        Self {
            token: token.to_string(),
            port,
            origins: origins.iter().map(|origin| (*origin).to_string()).collect(),
        }
    }
}

fn token_is_acceptable(token: &str) -> bool {
    token.len() >= MIN_BRIDGE_TOKEN_LEN && !token.chars().any(char::is_whitespace)
}

#[derive(Debug)]
enum Admission {
    Unauthorized,
    Forbidden,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcRequest {
    cmd: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    ok: bool,
    data: Option<serde_json::Value>,
    error: Option<String>,
}

impl JsonRpcResponse {
    fn success<T: Serialize>(data: &T) -> String {
        let data = serde_json::to_value(data).unwrap();
        serde_json::to_string(&JsonRpcResponse {
            ok: true,
            data: Some(data),
            error: None,
        })
        .unwrap()
    }

    fn error(msg: &str) -> String {
        serde_json::to_string(&JsonRpcResponse {
            ok: false,
            data: None,
            error: Some(msg.to_string()),
        })
        .unwrap()
    }
}

struct HttpReply {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: String,
}

impl HttpReply {
    fn new(
        status: u16,
        reason: &'static str,
        content_type: &'static str,
        body: impl Into<String>,
    ) -> Self {
        Self {
            status,
            reason,
            content_type,
            body: body.into(),
        }
    }

    fn json(status: u16, body: &str) -> Self {
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            _ => "OK",
        };
        Self::new(status, reason, "application/json", body)
    }

    fn empty(status: u16, reason: &'static str) -> Self {
        Self::new(status, reason, "text/plain", "")
    }

    fn encode(&self) -> String {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            status = self.status,
            reason = self.reason,
            content_type = self.content_type,
            len = self.body.len(),
            body = self.body,
        )
    }
}

fn rejection(reason: Admission) -> HttpReply {
    match reason {
        Admission::Unauthorized => HttpReply::json(401, r#"{"ok":false,"error":"unauthorized"}"#),
        Admission::Forbidden => HttpReply::json(403, r#"{"ok":false,"error":"forbidden"}"#),
    }
}

fn body_failure(error: ReadBodyError) -> HttpReply {
    match error {
        ReadBodyError::TooLarge => HttpReply::empty(413, "Content Too Large"),
        ReadBodyError::TimedOut => HttpReply::empty(408, "Request Timeout"),
        ReadBodyError::ReadFailed => HttpReply::empty(400, "Bad Request"),
    }
}

/// Listen addresses for the test bridge: one IPv4, one IPv6 (Windows requires
/// an explicit bind per address family). Loopback only — a wildcard bind would
/// expose the bridge beyond this machine and trigger a Windows Firewall
/// consent prompt for every freshly built executable path.
fn listen_addrs(port: u16) -> (SocketAddr, SocketAddr) {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    (
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
    )
}

/// Host must name this loopback listener, including the bound port.
/// `host_is_allowed` is the wrong helper: it accepts the machine's LAN
/// addresses and does not check the port.
fn host_is_this_listener(host_header: &str, port: u16) -> bool {
    let host = host_header.trim();
    let (name, seen_port) = if let Some(rest) = host.strip_prefix('[') {
        let Some((name, after)) = rest.split_once(']') else {
            return false;
        };
        let seen = after
            .strip_prefix(':')
            .and_then(|value| value.parse::<u16>().ok());
        (name, seen)
    } else if let Some((name, port_text)) = host.rsplit_once(':') {
        (name, port_text.parse::<u16>().ok())
    } else {
        return false;
    };
    if seen_port != Some(port) {
        return false;
    }
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    matches!(name.parse::<std::net::IpAddr>(), Ok(ip) if ip.is_loopback())
}

/// A missing `Origin` is the Node harness. Any present `Origin` must be on
/// the explicit allow-list; an empty list rejects every browser caller.
fn origin_is_allowed(headers: &str, allowed: &[String]) -> bool {
    match extract_header_value(headers, "Origin") {
        None => true,
        Some(origin) => allowed.iter().any(|candidate| candidate == origin),
    }
}

fn bearer_matches(expected: &str, headers: &str) -> bool {
    let Some(presented) = bearer_token(headers) else {
        return false;
    };
    bool::from(expected.as_bytes().ct_eq(presented.as_bytes()))
}

/// Host, then origin, then bearer. A failure here must not read the body or
/// dispatch a command. Query-string credentials are ignored.
fn admit(config: &BridgeConfig, headers: &str) -> Result<(), Admission> {
    let host = extract_header_value(headers, "Host").unwrap_or("");
    if !host_is_this_listener(host, config.port) {
        return Err(Admission::Forbidden);
    }
    if !origin_is_allowed(headers, &config.origins) {
        return Err(Admission::Forbidden);
    }
    if !bearer_matches(&config.token, headers) {
        return Err(Admission::Unauthorized);
    }
    Ok(())
}

fn is_health(request_line: &str) -> bool {
    request_line == "GET /health HTTP/1.1"
}

fn is_invoke(request_line: &str) -> bool {
    request_line == "POST /invoke HTTP/1.1"
}

fn act(request_line: &str, body: &[u8], app: Option<&AppHandle>) -> HttpReply {
    if is_health(request_line) {
        return HttpReply::new(200, "OK", "text/plain", "OK");
    }
    if is_invoke(request_line) {
        let text = String::from_utf8_lossy(body);
        let json = dispatch_invoke(text.trim(), app);
        return HttpReply::json(200, &json);
    }
    HttpReply::empty(404, "Not Found")
}

/// Admit, then dispatch. Tests use this so a rejected caller never reaches a
/// command. The socket path reads the body only after `admit` succeeds.
#[cfg(test)]
fn answer(config: &BridgeConfig, request_line: &str, headers: &str, body: &str) -> HttpReply {
    if let Err(reason) = admit(config, headers) {
        return rejection(reason);
    }
    act(request_line, body.as_bytes(), None)
}

fn dispatch_invoke(body: &str, app: Option<&AppHandle>) -> String {
    let rpc_req: JsonRpcRequest = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(error) => return JsonRpcResponse::error(&format!("Invalid JSON: {error}")),
    };

    tracing::debug!("[test_server] Handling: cmd={}", rpc_req.cmd);

    let owned_app = |app: Option<&AppHandle>| -> Result<AppHandle, String> {
        app.cloned()
            .ok_or_else(|| "test bridge has no application handle".to_string())
    };

    match rpc_req.cmd.as_str() {
        "create_test_mesh" => handle_create_test_mesh(&rpc_req.args),
        "create_test_review_fixture" => handle_create_test_review_fixture(&rpc_req.args),
        "create_mobile_review_approval_fixture" => {
            let name = rpc_req
                .args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Phone review approval");
            match crate::services::test_fixtures::create_mobile_review_approval_fixture(name) {
                Ok(data) => JsonRpcResponse::success(&data),
                Err(error) => JsonRpcResponse::error(&error),
            }
        }
        "create_attention_backlog_fixture" => {
            handle_create_attention_backlog_fixture(&rpc_req.args)
        }
        "delete_test_review_fixture" => handle_delete_test_review_fixture(&rpc_req.args),
        "create_test_review_continuation_fixture" => {
            handle_create_test_review_continuation_fixture(&rpc_req.args)
        }
        "concurrent_continue_review" => match owned_app(app) {
            Ok(app) => handle_concurrent_continue_review(&rpc_req.args, app),
            Err(error) => JsonRpcResponse::error(&error),
        },
        "create_agent_node" => match owned_app(app) {
            Ok(app) => handle_create_agent_node(&rpc_req.args, app),
            Err(error) => JsonRpcResponse::error(&error),
        },
        "list_meshes" => handle_list_meshes(),
        "list_agent_nodes" => handle_list_agent_nodes(),
        "get_agent_node" => handle_get_agent_node(&rpc_req.args),
        "spawn_agent" => match app {
            Some(app) => handle_spawn_agent(&rpc_req.args, app),
            None => JsonRpcResponse::error("test bridge has no application handle"),
        },
        "write_to_agent" => match owned_app(app) {
            Ok(app) => handle_write_to_agent(&rpc_req.args, app),
            Err(error) => JsonRpcResponse::error(&error),
        },
        "kill_agent" => handle_kill_agent(&rpc_req.args),
        "delete_mesh" => handle_delete_mesh(&rpc_req.args),
        "delete_agent_node" => handle_delete_agent_node(&rpc_req.args),
        "get_worktree_close_safety" => handle_get_worktree_close_safety(&rpc_req.args),
        "archive_agent_node" => handle_archive_agent_node(&rpc_req.args),
        "inject_test_output" => match owned_app(app) {
            Ok(app) => handle_inject_test_output(&rpc_req.args, app),
            Err(error) => JsonRpcResponse::error(&error),
        },
        "set_active_node" => match owned_app(app) {
            Ok(app) => handle_set_active_node(&rpc_req.args, app),
            Err(error) => JsonRpcResponse::error(&error),
        },
        "get_mesh_properties" => handle_get_mesh_properties(&rpc_req.args),
        "update_mesh_column" => handle_update_mesh_column(&rpc_req.args),
        "update_worktree_base_ref" => handle_update_worktree_base_ref(&rpc_req.args),
        "remove_worktree_base_ref" => handle_remove_worktree_base_ref(&rpc_req.args),
        "update_mesh_use_worktree" => handle_update_mesh_use_worktree(&rpc_req.args),
        "get_root_token" => handle_get_root_token(),
        "create_pairing_ticket" => {
            JsonRpcResponse::success(&serde_json::json!({ "ticket": crate::http::pairing::mint() }))
        }
        "spawn_handover_agent" => match owned_app(app) {
            Ok(app) => handle_spawn_handover_agent(&rpc_req.args, app),
            Err(error) => JsonRpcResponse::error(&error),
        },
        _ => JsonRpcResponse::error(&format!("Unknown command: {}", rpc_req.cmd)),
    }
}

/// Start the HTTP test bridge on the Tauri runtime. Returns without binding
/// when the process has not opted in.
pub fn start_test_server(app_handle: AppHandle, port_offset: u16) {
    let port = TEST_SERVER_PORT + port_offset;
    let Some(config) = BridgeConfig::from_env(port) else {
        return;
    };
    let (addr_ipv4, addr_ipv6) = listen_addrs(port);

    tauri::async_runtime::spawn(async move {
        let listener_ipv4 = match TcpListener::bind(addr_ipv4).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::error!("[test_server] Failed to bind IPv4 on port {port}: {error}");
                return;
            }
        };

        tracing::info!(
            "[test_server] HTTP test bridge listening on http://127.0.0.1:{port} (IPv4)"
        );

        let app_ipv6 = app_handle.clone();
        let config_ipv6 = config.clone();
        let _ = TcpListener::bind(addr_ipv6).await.map(|listener_ipv6| {
            tracing::info!(
                "[test_server] HTTP test bridge also listening on http://[::1]:{port} (IPv6)"
            );
            tauri::async_runtime::spawn(async move {
                loop {
                    match listener_ipv6.accept().await {
                        Ok((stream, _client_addr)) => {
                            let app = app_ipv6.clone();
                            let config = config_ipv6.clone();
                            tauri::async_runtime::spawn(async move {
                                handle_connection(stream, config, app).await;
                            });
                        }
                        Err(error) => tracing::warn!("[test_server] IPv6 accept error: {error}"),
                    }
                }
            });
        });

        loop {
            match listener_ipv4.accept().await {
                Ok((stream, _client_addr)) => {
                    let app = app_handle.clone();
                    let config = config.clone();
                    tauri::async_runtime::spawn(async move {
                        handle_connection(stream, config, app).await;
                    });
                }
                Err(error) => tracing::warn!("[test_server] IPv4 accept error: {error}"),
            }
        }
    });
}

async fn handle_connection(stream: TcpStream, config: BridgeConfig, app: AppHandle) {
    let mut lines = BufStream::new(stream);
    serve_connection(&mut lines, &config, Some(app)).await;
}

async fn serve_connection<R>(
    lines: &mut BufStream<R>,
    config: &BridgeConfig,
    app: Option<AppHandle>,
) where
    R: AsyncRead + AsyncWrite + Unpin,
{
    let Some((request_line, headers, overflow)) = read_request_head(lines).await else {
        return;
    };
    if overflow {
        write_reply(
            lines,
            &HttpReply::empty(431, "Request Header Fields Too Large"),
        )
        .await;
        return;
    }
    if let Err(reason) = admit(config, &headers) {
        tracing::debug!("[test_server] rejected request: {reason:?}");
        write_reply(lines, &rejection(reason)).await;
        return;
    }

    let request_line = request_line.trim().to_string();
    let reply = if is_invoke(&request_line) {
        let claimed = content_length(&headers);
        let body = match read_body_with_cap(lines, claimed, BRIDGE_MAX_BODY).await {
            Ok(body) => body,
            Err(error) => {
                write_reply(lines, &body_failure(error)).await;
                return;
            }
        };
        let app = app.clone();
        match crate::commands::run_blocking("test_server_request", move || {
            Ok(act(&request_line, &body, app.as_ref()))
        })
        .await
        {
            Ok(reply) => reply,
            Err(error) => HttpReply::json(200, &JsonRpcResponse::error(&error)),
        }
    } else {
        act(&request_line, &[], app.as_ref())
    };
    write_reply(lines, &reply).await;
}

async fn write_reply<R>(lines: &mut BufStream<R>, reply: &HttpReply)
where
    R: AsyncRead + AsyncWrite + Unpin,
{
    let bytes = reply.encode();
    let writer = lines.get_mut();
    if let Err(error) = writer.write_all(bytes.as_bytes()).await {
        tracing::warn!("[test_server] Failed to send response: {error}");
        return;
    }
    if let Err(error) = writer.flush().await {
        tracing::warn!("[test_server] Failed to flush response: {error}");
    }
}

fn handle_create_test_mesh(args: &serde_json::Value) -> String {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Test Mesh");
    // An explicit `path` lets a check point the Mesh at a scratch repository it
    // prepared; the default keeps the historical temp-directory Mesh.
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());

    match crate::db::create_mesh(name, &path) {
        Ok(mesh) => JsonRpcResponse::success(&mesh),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_create_test_review_fixture(args: &serde_json::Value) -> String {
    let name = args
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("Review consistency verification");
    match crate::services::test_fixtures::create_review_activity_fixture(name) {
        Ok(data) => JsonRpcResponse::success(&data),
        Err(error) => JsonRpcResponse::error(&error),
    }
}

fn handle_create_attention_backlog_fixture(args: &serde_json::Value) -> String {
    let name = args
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("Attention backlog");
    let count = args
        .get("count")
        .and_then(|value| value.as_i64())
        .unwrap_or(3);
    match crate::services::test_fixtures::create_attention_backlog_fixture(name, count) {
        Ok(data) => JsonRpcResponse::success(&data),
        Err(error) => JsonRpcResponse::error(&error),
    }
}

fn handle_delete_test_review_fixture(args: &serde_json::Value) -> String {
    let mesh_id = args
        .get("meshId")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    match crate::services::test_fixtures::delete_review_activity_fixture(mesh_id) {
        Ok(()) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id })),
        Err(error) => JsonRpcResponse::error(&error),
    }
}

fn handle_create_test_review_continuation_fixture(args: &serde_json::Value) -> String {
    let source_node_id = args
        .get("sourceNodeId")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    match crate::services::test_fixtures::create_review_continuation_fixture(source_node_id) {
        Ok(data) => JsonRpcResponse::success(&data),
        Err(error) => JsonRpcResponse::error(&error),
    }
}

/// Two overlapping invocations of the production `continue_circuit_review`
/// command body. The UI serialises the button behind one busy flag, so this
/// fixture exercises the race that must reuse the same run without adding the
/// requested rounds twice.
fn handle_concurrent_continue_review(args: &serde_json::Value, app: AppHandle) -> String {
    let run_id = args
        .get("runId")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let additional_rounds = args
        .get("additionalRounds")
        .and_then(|value| value.as_i64())
        .unwrap_or(1) as i32;
    let threads: Vec<_> = (0..2)
        .map(|attempt| {
            let app = app.clone();
            std::thread::spawn(move || {
                tauri::async_runtime::block_on(crate::commands::circuit::continue_circuit_review(
                    app,
                    run_id,
                    additional_rounds,
                ))
                .map_err(|error| format!("attempt {attempt}: {error}"))
            })
        })
        .collect();
    let results: Vec<Result<i64, String>> = threads
        .into_iter()
        .map(|thread| {
            thread
                .join()
                .unwrap_or_else(|_| Err("continuation thread panicked".into()))
        })
        .collect();
    JsonRpcResponse::success(&serde_json::json!({ "results": results }))
}

fn handle_create_agent_node(args: &serde_json::Value, app: AppHandle) -> String {
    let mesh_id = args.get("meshId").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("Node");
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("/tmp");
    let branch = args
        .get("branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    // A check that only needs a live process can keep the agent in the Mesh
    // directory instead of provisioning a worktree for it.
    let use_worktree = args
        .get("useWorktree")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let provider = args
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("anthropic");

    use crate::models::EnvType;

    match crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id,
        name,
        path,
        branch,
        env: EnvType::Windows,
        provider,
        worktree_name: Some(name),
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None, // source_pr_pinned_sha — test fixture; no PR SHA
        use_worktree,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    }) {
        Ok(node) => {
            // Emit event so frontend agent-node store can refetch via invoke()
            let _ = app.emit(
                "node-created",
                crate::commands::agent::NodeCreatedPayload { id: node.id },
            );
            JsonRpcResponse::success(&node)
        }
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_list_meshes() -> String {
    match crate::db::list_meshes() {
        Ok(meshes) => JsonRpcResponse::success(&meshes),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_list_agent_nodes() -> String {
    match crate::db::list_agent_nodes() {
        Ok(nodes) => JsonRpcResponse::success(&nodes),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_get_agent_node(args: &serde_json::Value) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::db::get_agent_node_by_id(node_id) {
        Ok(node) => JsonRpcResponse::success(&node),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_spawn_agent(args: &serde_json::Value, app: &AppHandle) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);
    let provider = args
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("anthropic")
        .to_string();
    let resume = args
        .get("resume")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let intent = if resume.is_some() {
        crate::commands::agent::SpawnAgentIntent::Resume
    } else {
        crate::commands::agent::SpawnAgentIntent::Fresh
    };

    tracing::info!(
        "[test_server] spawn_agent: spawning thread for node_id={}",
        node_id
    );

    // Spawn a dedicated thread to avoid panics corrupting the async runtime
    let app_clone = app.clone();
    let result = std::thread::spawn(move || {
        tauri::async_runtime::block_on(crate::commands::agent::spawn_agent(
            app_clone,
            crate::commands::agent::SpawnAgentRequest {
                session_id: node_id,
                provider,
                intent,
                rows: None,
                cols: None,
            },
        ))
    })
    .join();

    tracing::info!("[test_server] spawn_agent result: {:?}", result);

    match result {
        Ok(Ok(_)) => {
            tracing::info!("[test_server] spawn_agent returning success");
            JsonRpcResponse::success(&serde_json::json!({ "node_id": node_id }))
        }
        Ok(Err(e)) => {
            tracing::error!("[test_server] spawn_agent error: {}", e);
            JsonRpcResponse::error(&e)
        }
        Err(e) => {
            tracing::error!("[test_server] spawn_agent panicked: {:?}", e);
            JsonRpcResponse::error(&format!("spawn_agent panicked: {:?}", e))
        }
    }
}

fn handle_spawn_handover_agent(args: &serde_json::Value, app: AppHandle) -> String {
    let mesh_id = args.get("meshId").and_then(|v| v.as_i64()).unwrap_or(0);
    let prefill = args
        .get("prefill")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let provider = args
        .get("provider")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    tracing::info!(
        "[test_server] spawn_handover_agent: mesh_id={}, prefill_len={}",
        mesh_id,
        prefill.len()
    );

    let app_clone = app.clone();
    let result = std::thread::spawn(move || {
        tauri::async_runtime::block_on(crate::commands::agent::spawn_handover_agent(
            app_clone, mesh_id, prefill, provider,
        ))
    })
    .join();

    match result {
        Ok(Ok(node)) => {
            let _ = app.emit(
                "node-created",
                crate::commands::agent::NodeCreatedPayload { id: node.id },
            );
            JsonRpcResponse::success(&node)
        }
        Ok(Err(e)) => {
            tracing::error!("[test_server] spawn_handover_agent error: {}", e);
            JsonRpcResponse::error(&e)
        }
        Err(e) => {
            tracing::error!("[test_server] spawn_handover_agent panicked: {:?}", e);
            JsonRpcResponse::error(&format!("spawn_handover_agent panicked: {:?}", e))
        }
    }
}

fn handle_kill_agent(args: &serde_json::Value) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);

    let result = tauri::async_runtime::block_on(crate::agent::process::kill_agent(node_id));

    match result {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "node_id": node_id })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

/// Deliver a prompt through the production `write_to_agent` command body so a
/// real-runtime check can start a turn without synthesising keystrokes in the
/// terminal widget.
fn handle_write_to_agent(args: &serde_json::Value, app: AppHandle) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);
    let data = args
        .get("data")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();

    let result =
        tauri::async_runtime::block_on(crate::agent::process::write_to_agent(app, node_id, data));

    match result {
        // The disposition is echoed so a harness can assert the write was
        // genuinely accepted rather than refused behind a 200 (issue #1530).
        Ok(outcome) => JsonRpcResponse::success(
            &serde_json::json!({ "node_id": node_id, "disposition": outcome.disposition }),
        ),
        Err(error) => JsonRpcResponse::error(&error),
    }
}

/// Inject fake terminal output into a session for testing purposes.
/// This bypasses the PTY and directly emits 'agent-output' events.
fn handle_inject_test_output(args: &serde_json::Value, app: AppHandle) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);
    let lines = args
        .get("lines")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec!["Hello from test output!\n".to_string()]);

    tracing::info!(
        "[test_server] inject_test_output: node_id={} lines={}",
        node_id,
        lines.len()
    );

    for line in &lines {
        // Use the same wire key as the production `agent-output` emit
        // (`session_id`, NOT `node_id` — see `crate::agent::spawn` and
        // `src/components/Terminal/TerminalRegistry.ts`'s `AgentOutputPayload`).
        // Pre-#161 this used `node_id` and the TS listener silently dropped the
        // payload — the test injection was broken in production.
        let _ = app.emit(
            "agent-output",
            crate::agent::spawn::AgentOutputPayload {
                session_id: node_id,
                line: Some(line.clone()),
                data: None,
            },
        );
    }

    JsonRpcResponse::success(&serde_json::json!({
        "node_id": node_id,
        "lines_injected": lines.len()
    }))
}

fn handle_delete_mesh(args: &serde_json::Value) -> String {
    let mesh_id = args.get("meshId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::commands::mesh::delete_mesh_inner(mesh_id) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_delete_agent_node(args: &serde_json::Value) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);
    let remove_worktree = args
        .get("removeWorktree")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match crate::services::agent_node::delete(node_id, remove_worktree) {
        Ok(_) => {
            // The real app drains worktree removals in the background; the test
            // server flushes synchronously so E2E sees the directory gone when
            // the call returns.
            crate::services::agent_node::process_pending_removals();
            JsonRpcResponse::success(&serde_json::json!({ "node_id": node_id }))
        }
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_get_worktree_close_safety(args: &serde_json::Value) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::services::agent_node::get_worktree_close_safety(node_id) {
        Ok(safety) => JsonRpcResponse::success(&safety),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_archive_agent_node(args: &serde_json::Value) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::db::archive_agent_node(node_id) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "node_id": node_id })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_set_active_node(args: &serde_json::Value, app: AppHandle) -> String {
    let node_id = args.get("nodeId").and_then(|v| v.as_i64()).unwrap_or(0);

    tracing::info!("[test_server] set_active_node: node_id={}", node_id);

    // Emit a frontend event that the agent-node store listens to
    let _ = app.emit("node-activated", NodeActivatedPayload { node_id });

    JsonRpcResponse::success(&serde_json::json!({ "node_id": node_id }))
}

fn handle_get_mesh_properties(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::commands::mesh_properties::get_mesh_properties(mesh_id) {
        Ok(config) => JsonRpcResponse::success(&config),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

fn handle_update_mesh_column(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let column = args.get("column").and_then(|v| v.as_str()).unwrap_or("");
    let value = args.get("value").and_then(|v| v.as_str()).unwrap_or("");

    match crate::commands::mesh_properties::update_mesh_column(
        mesh_id,
        column.to_string(),
        value.to_string(),
    ) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

fn handle_update_worktree_base_ref(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let base_ref = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("fresh")
        .to_string();

    match crate::commands::mesh_properties::update_worktree_base_ref(mesh_id, base_ref) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

fn handle_remove_worktree_base_ref(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::commands::mesh_properties::remove_worktree_base_ref(mesh_id) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

fn handle_update_mesh_use_worktree(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let use_worktree = args
        .get("use_worktree")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    match crate::commands::mesh_properties::update_mesh_use_worktree(mesh_id, use_worktree) {
        Ok(_) => JsonRpcResponse::success(
            &serde_json::json!({ "mesh_id": mesh_id, "use_worktree": use_worktree }),
        ),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

/// Expose the remote-access root token to Playwright mobile e2e specs so
/// they can construct `/v2?token=...` URLs without scraping the SQLite DB
/// from outside. The Tauri command of the same name does the same thing
/// for the desktop QR-code modal; we go through the test server here only
/// because Playwright runs outside the Tauri webview.
fn handle_get_root_token() -> String {
    match crate::db::get_or_create_root_token() {
        Ok(token) => JsonRpcResponse::success(&serde_json::json!({ "token": token })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn sample() -> BridgeConfig {
        BridgeConfig::for_test(TOKEN, 1991, &[])
    }

    fn headers(token: Option<&str>, host: &str, origin: Option<&str>) -> String {
        let mut out = format!("Host: {host}\r\n");
        if let Some(origin) = origin {
            out.push_str(&format!("Origin: {origin}\r\n"));
        }
        if let Some(token) = token {
            out.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        out
    }

    fn invoke(cmd: &str, args: &str) -> String {
        format!(r#"{{"cmd":"{cmd}","args":{args}}}"#)
    }

    fn with_bridge_env(pairs: &[(&str, Option<&str>)], check: impl FnOnce()) {
        let owned: Vec<(&str, Option<std::ffi::OsString>)> = pairs
            .iter()
            .map(|(key, value)| (*key, value.map(std::ffi::OsString::from)))
            .collect();
        let view: Vec<(&str, Option<&std::ffi::OsStr>)> = owned
            .iter()
            .map(|(key, value)| (*key, value.as_deref()))
            .collect();
        let _guard = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        crate::env::with_env_vars(&view, check);
    }

    fn stored_root_token() -> Option<String> {
        let conn = crate::db::read_conn().unwrap();
        conn.query_row(
            "SELECT value FROM app_settings WHERE key = 'remote_access_token'",
            [],
            |row| row.get(0),
        )
        .ok()
    }

    /// The bridge executes real backend commands. It must never listen on a
    /// routable interface: a wildcard bind lets anyone on the LAN invoke
    /// commands, and on Windows it triggers a firewall consent prompt for
    /// every freshly built executable path.
    #[test]
    fn test_server_binds_loopback_only() {
        let (v4, v6) = listen_addrs(1991);
        assert!(
            v4.ip().is_loopback(),
            "IPv4 bind must be loopback, got {}",
            v4.ip()
        );
        assert!(
            v6.ip().is_loopback(),
            "IPv6 bind must be loopback, got {}",
            v6.ip()
        );
        assert_eq!(v4.port(), 1991);
        assert_eq!(v6.port(), 1991);
    }

    #[test]
    fn bridge_stays_closed_without_the_enable_flag() {
        with_bridge_env(
            &[(BRIDGE_ENABLE_ENV, None), (BRIDGE_TOKEN_ENV, Some(TOKEN))],
            || assert!(BridgeConfig::from_env(1991).is_none()),
        );
    }

    #[test]
    fn bridge_stays_closed_when_the_token_is_too_short() {
        with_bridge_env(
            &[
                (BRIDGE_ENABLE_ENV, Some("1")),
                (BRIDGE_TOKEN_ENV, Some("short-token")),
            ],
            || assert!(BridgeConfig::from_env(1991).is_none()),
        );
    }

    #[test]
    fn bridge_stays_closed_when_the_token_contains_whitespace() {
        with_bridge_env(
            &[
                (BRIDGE_ENABLE_ENV, Some("1")),
                (BRIDGE_TOKEN_ENV, Some("0123456789abcdef 0123456789abcde")),
            ],
            || assert!(BridgeConfig::from_env(1991).is_none()),
        );
    }

    #[test]
    fn bridge_opens_only_with_the_flag_and_a_long_token() {
        with_bridge_env(
            &[
                (BRIDGE_ENABLE_ENV, Some("1")),
                (BRIDGE_TOKEN_ENV, Some(TOKEN)),
                (
                    BRIDGE_ORIGINS_ENV,
                    Some("http://localhost:1420, http://127.0.0.1:1420"),
                ),
            ],
            || {
                let config = BridgeConfig::from_env(2991).expect("bridge should be configured");
                assert_eq!(config.token, TOKEN);
                assert_eq!(config.port, 2991);
                assert_eq!(
                    config.origins,
                    vec![
                        "http://localhost:1420".to_string(),
                        "http://127.0.0.1:1420".to_string()
                    ]
                );
            },
        );
    }

    #[test]
    fn body_read_errors_map_to_client_statuses_without_cors() {
        for (error, status) in [
            (ReadBodyError::TooLarge, 413),
            (ReadBodyError::TimedOut, 408),
            (ReadBodyError::ReadFailed, 400),
        ] {
            let reply = body_failure(error);
            assert_eq!(reply.status, status);
            assert!(!reply.encode().contains("Access-Control-Allow-Origin"));
        }
    }

    #[test]
    fn missing_token_does_not_create_a_mesh() {
        let _db = crate::db::test_support::isolated();
        let reply = answer(
            &sample(),
            "POST /invoke HTTP/1.1",
            &headers(None, "127.0.0.1:1991", None),
            &invoke("create_test_mesh", r#"{"name":"should-not-exist"}"#),
        );
        assert_eq!(reply.status, 401);
        assert_eq!(reply.body, r#"{"ok":false,"error":"unauthorized"}"#);
        assert!(!reply.encode().contains("Access-Control-Allow-Origin"));
        assert!(crate::db::list_meshes().unwrap().is_empty());
    }

    #[test]
    fn wrong_token_does_not_create_a_mesh() {
        let _db = crate::db::test_support::isolated();
        let reply = answer(
            &sample(),
            "POST /invoke HTTP/1.1",
            &headers(Some(&"f".repeat(32)), "127.0.0.1:1991", None),
            &invoke("create_test_mesh", r#"{"name":"should-not-exist"}"#),
        );
        assert_eq!(reply.status, 401);
        assert!(crate::db::list_meshes().unwrap().is_empty());
    }

    #[test]
    fn query_token_is_not_a_credential() {
        let _db = crate::db::test_support::isolated();
        let reply = answer(
            &sample(),
            &format!("POST /invoke?token={TOKEN} HTTP/1.1"),
            &headers(None, "127.0.0.1:1991", None),
            &invoke("create_test_mesh", r#"{"name":"should-not-exist"}"#),
        );
        assert_eq!(reply.status, 401);
        assert!(crate::db::list_meshes().unwrap().is_empty());
    }

    #[test]
    fn rejected_host_does_not_create_a_mesh() {
        let _db = crate::db::test_support::isolated();
        for host in [
            "evil.com:1991",
            "127.0.0.1:1992",
            "192.168.1.20:1991",
            "127.0.0.1",
        ] {
            let reply = answer(
                &sample(),
                "POST /invoke HTTP/1.1",
                &headers(Some(TOKEN), host, None),
                &invoke("create_test_mesh", r#"{"name":"should-not-exist"}"#),
            );
            assert_eq!(reply.status, 403, "host {host}");
            assert_eq!(reply.body, r#"{"ok":false,"error":"forbidden"}"#);
        }
        assert!(crate::db::list_meshes().unwrap().is_empty());
    }

    #[test]
    fn rejected_origin_does_not_create_a_mesh() {
        let _db = crate::db::test_support::isolated();
        let config = BridgeConfig::for_test(TOKEN, 1991, &["http://localhost:1420"]);
        let reply = answer(
            &config,
            "POST /invoke HTTP/1.1",
            &headers(Some(TOKEN), "127.0.0.1:1991", Some("http://evil.example")),
            &invoke("create_test_mesh", r#"{"name":"should-not-exist"}"#),
        );
        assert_eq!(reply.status, 403);
        assert!(crate::db::list_meshes().unwrap().is_empty());
    }

    #[test]
    fn rejected_get_root_token_does_not_store_one() {
        let _db = crate::db::test_support::isolated();
        let reply = answer(
            &sample(),
            "POST /invoke HTTP/1.1",
            &headers(None, "127.0.0.1:1991", None),
            &invoke("get_root_token", "{}"),
        );
        assert_eq!(reply.status, 401);
        assert!(stored_root_token().is_none());
    }

    #[test]
    fn rejected_pairing_ticket_is_unauthorized() {
        let reply = answer(
            &sample(),
            "POST /invoke HTTP/1.1",
            &headers(None, "localhost:1991", None),
            &invoke("create_pairing_ticket", "{}"),
        );
        assert_eq!(reply.status, 401);
        assert!(!reply.body.contains("ticket"));
        assert!(!reply.encode().contains("Access-Control-Allow-Origin"));
    }

    #[test]
    fn health_requires_the_same_bearer() {
        let denied = answer(
            &sample(),
            "GET /health HTTP/1.1",
            &headers(None, "127.0.0.1:1991", None),
            "",
        );
        assert_eq!(denied.status, 401);

        let allowed = answer(
            &sample(),
            "GET /health HTTP/1.1",
            &headers(Some(TOKEN), "[::1]:1991", None),
            "",
        );
        assert_eq!(allowed.status, 200);
        assert_eq!(allowed.body, "OK");
        let encoded = allowed.encode();
        assert_eq!(encoded.matches("HTTP/1.1").count(), 1);
        assert!(!encoded.contains("Access-Control-Allow-Origin"));
    }

    #[test]
    fn prefixed_route_does_not_dispatch() {
        let _db = crate::db::test_support::isolated();
        for line in [
            "POST /invokeX HTTP/1.1",
            "GET /healthcheck HTTP/1.1",
            "POST /invoke HTTP/1.0",
        ] {
            let reply = answer(
                &sample(),
                line,
                &headers(Some(TOKEN), "127.0.0.1:1991", None),
                &invoke("create_test_mesh", r#"{"name":"should-not-exist"}"#),
            );
            assert_eq!(reply.status, 404, "{line}");
        }
        assert!(crate::db::list_meshes().unwrap().is_empty());
    }

    #[test]
    fn authenticated_request_creates_a_mesh() {
        let _db = crate::db::test_support::isolated();
        let reply = answer(
            &sample(),
            "POST /invoke HTTP/1.1",
            &headers(Some(TOKEN), "127.0.0.1:1991", None),
            &invoke("create_test_mesh", r#"{"name":"bridge-auth-mesh"}"#),
        );
        assert_eq!(reply.status, 200);
        let parsed: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["data"]["name"], "bridge-auth-mesh");
        assert!(!reply.encode().contains("Access-Control-Allow-Origin"));
        let meshes = crate::db::list_meshes().unwrap();
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].name, "bridge-auth-mesh");
    }

    #[test]
    fn allowlisted_origin_can_create_a_mesh() {
        let _db = crate::db::test_support::isolated();
        let config = BridgeConfig::for_test(TOKEN, 1991, &["http://localhost:1420"]);
        let reply = answer(
            &config,
            "POST /invoke HTTP/1.1",
            &headers(Some(TOKEN), "localhost:1991", Some("http://localhost:1420")),
            &invoke("create_test_mesh", r#"{"name":"origin-mesh"}"#),
        );
        assert_eq!(reply.status, 200);
        assert_eq!(crate::db::list_meshes().unwrap().len(), 1);
    }

    #[test]
    fn authenticated_introspection_still_dispatches() {
        let _db = crate::db::test_support::isolated();
        let token_reply = answer(
            &sample(),
            "POST /invoke HTTP/1.1",
            &headers(Some(TOKEN), "127.0.0.1:1991", None),
            &invoke("get_root_token", "{}"),
        );
        assert_eq!(token_reply.status, 200);
        let parsed: serde_json::Value = serde_json::from_str(&token_reply.body).unwrap();
        assert_eq!(parsed["ok"], true);
        assert!(parsed["data"]["token"].as_str().unwrap().len() >= 32);
        assert!(stored_root_token().is_some());

        let ticket_reply = answer(
            &sample(),
            "POST /invoke HTTP/1.1",
            &headers(Some(TOKEN), "127.0.0.1:1991", None),
            &invoke("create_pairing_ticket", "{}"),
        );
        assert_eq!(ticket_reply.status, 200);
        let ticket: serde_json::Value = serde_json::from_str(&ticket_reply.body).unwrap();
        assert_eq!(ticket["ok"], true);
        assert!(!ticket["data"]["ticket"].as_str().unwrap().is_empty());
    }

    async fn exchange(raw: &[u8], config: BridgeConfig) -> String {
        let (client, server) = tokio::io::duplex(256 * 1024);
        let task = tokio::spawn(async move {
            let mut lines = BufStream::new(server);
            serve_connection(&mut lines, &config, None).await;
        });
        let (mut reader, mut writer) = tokio::io::split(client);
        writer.write_all(raw).await.unwrap();
        // Dropping the write half does not close it. Shutdown delivers EOF so
        // a short body is a failed read rather than a 60s wait.
        writer.shutdown().await.unwrap();
        drop(writer);
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buf),
        )
        .await;
        task.abort();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test]
    async fn rejected_caller_is_answered_before_the_body_is_read() {
        let raw = b"POST /invoke HTTP/1.1\r\nHost: 127.0.0.1:1991\r\nContent-Length: 100\r\n\r\n";
        let response = exchange(raw, sample()).await;
        assert!(
            response.starts_with("HTTP/1.1 401"),
            "expected 401 before the body, got {response:?}"
        );
    }

    #[tokio::test]
    async fn oversized_header_is_rejected_before_admission() {
        let pad = "a".repeat(70 * 1024);
        let raw = format!("POST /invoke HTTP/1.1\r\nHost: 127.0.0.1:1991\r\nX-Pad: {pad}\r\n\r\n");
        let response = exchange(raw.as_bytes(), sample()).await;
        assert!(
            response.starts_with("HTTP/1.1 431"),
            "expected 431, got {response:?}"
        );
    }

    #[tokio::test]
    async fn oversized_body_is_rejected_after_admission_and_before_dispatch() {
        let raw = format!(
            "POST /invoke HTTP/1.1\r\nHost: 127.0.0.1:1991\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 70000\r\n\r\n"
        );
        let response = exchange(raw.as_bytes(), sample()).await;
        assert!(
            response.starts_with("HTTP/1.1 413"),
            "expected 413, got {response:?}"
        );
        assert!(!response.contains("Access-Control-Allow-Origin"));
    }

    #[tokio::test]
    async fn truncated_body_is_a_bad_request_after_admission() {
        let raw = format!(
            "POST /invoke HTTP/1.1\r\nHost: 127.0.0.1:1991\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 8\r\n\r\n"
        );
        let response = exchange(raw.as_bytes(), sample()).await;
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "expected 400, got {response:?}"
        );
    }
}
