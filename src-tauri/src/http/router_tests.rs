use super::*;

fn status_of(result: DispatchResult) -> u16 {
    match result {
        DispatchResult::Http(r) => r.status_code(),
        DispatchResult::Upgrade(_) => 101,
    }
}

fn sample_path(m: &RouteMatch) -> String {
    match m {
        RouteMatch::Exact(p) => p.to_string(),
        RouteMatch::OneId { prefix, suffix } => format!("{}1{}", prefix, suffix),
        RouteMatch::TwoId {
            prefix,
            mid,
            suffix,
        } => format!("{}1{}2{}", prefix, mid, suffix),
        RouteMatch::Under(base) => format!("{base}/x"),
        RouteMatch::Prefix(p) => format!("{p}x"),
        RouteMatch::Any => "/".to_string(),
    }
}

fn pattern_str(m: &RouteMatch) -> String {
    match m {
        RouteMatch::Exact(p) => p.to_string(),
        RouteMatch::OneId { prefix, suffix } => format!("{}{{id}}{}", prefix, suffix),
        RouteMatch::TwoId {
            prefix,
            mid,
            suffix,
        } => {
            let (left, right) = if *mid == "/pulls/" {
                ("{mesh_id}", "{pr_number}")
            } else {
                ("{mesh_id}", "{issue_number}")
            };
            format!("{}{}{}{}{}", prefix, left, mid, right, suffix)
        }
        RouteMatch::Under(base) => format!("{base}/*"),
        RouteMatch::Prefix(p) => format!("{p}*"),
        RouteMatch::Any => "*".to_string(),
    }
}

#[tokio::test]
async fn every_guarded_table_route_requires_credentials() {
    for route in ROUTES {
        if route.scope.required().is_none() {
            continue;
        }
        let path = sample_path(&route.m);
        let req = ParsedRequest::test(route.method, &path, b"");
        let status = status_of(dispatch(req).await);
        assert_eq!(
            status, 401,
            "{} {} must reject an uncredentialed request with 401",
            route.method, path
        );
    }
}

#[test]
fn nodes_input_body_cap_is_1kib() {
    let route = ROUTES
        .iter()
        .find(|r| matches!(r.handler, Handler::NodesInput))
        .unwrap();
    assert!(
        matches!(route.body, BodyPolicy::Cap(n) if n == 1024),
        "POST /api/nodes/{{id}}/input must cap bodies at 1024 bytes"
    );
}

#[test]
fn router_tests_do_not_bind_sockets() {
    // Structural pin: this module never binds sockets. The transport
    // contract lives in `http::server` tests.
    let src = include_str!("router.rs");
    let needle = ["tokio", "::net::", "TcpListener"].concat();
    assert!(!src.contains(&needle), "router tests must not open sockets");
}

#[test]
fn route_table_scope_snapshot() {
    let table: Vec<String> = ROUTES
        .iter()
        .map(|r| {
            format!(
                "{} {} -> {}",
                r.method,
                pattern_str(&r.m),
                r.scope.snapshot_label()
            )
        })
        .collect();
    let actual = table.join("\n");
    let expected = "\
GET /api/launch-configurations -> Admin
GET /api/launch-targets -> Admin
POST /api/launch-configurations/save -> Admin
POST /api/launch-configurations/verify -> Admin
POST /api/launch-configurations/delete -> Admin
GET /launch-configurations -> CoordinatorRead
POST /nodes/create -> CoordinatorWrite
GET /admin/devices -> Admin
POST /admin/devices/{id}/revoke -> Admin
GET /nodes -> CoordinatorRead
GET /nodes/{id}/log -> CoordinatorRead
POST /nodes/{id}/prompt -> CoordinatorWrite
POST /api/nodes/create -> Admin
POST /api/nodes/{id}/input -> Admin
POST /api/nodes/{id}/close -> Admin
POST /api/meshes/{id}/pr -> Admin
GET /api/meshes/{id}/pr/source -> Admin
POST /api/meshes/{mesh_id}/pulls/{pr_number}/merge -> Admin
POST /api/meshes/{mesh_id}/pulls/{pr_number}/spawn -> Admin
POST /api/meshes/{id}/agent-nodes/import-and-resume -> Admin
POST /api/meshes/{mesh_id}/issues/{issue_number}/spawn -> Admin
GET /api/meshes/{id}/issue-labels -> Admin
POST /api/meshes/{mesh_id}/issues/{issue_number}/label -> Admin
GET /api/nodes/{id}/reviews -> Admin
POST /api/nodes/{id}/reviews -> Admin
GET /api/circuit-agent-ownerships -> Admin
GET /api/circuit-runs -> Admin
POST /api/circuit-runs/{id}/approve -> Admin
POST /api/circuit-runs/{id}/outcome -> Admin
POST /api/circuit-runs/{id}/recover -> Admin
POST /api/circuit-runs/{id}/cancel -> Admin
GET /api/agents/{id}/git/status -> Admin
GET /api/agents/{id}/git/summary -> Admin
GET /api/agents/{id}/git/branch -> Admin
GET /api/agents/{id}/diff -> Admin
GET /api/gh/auth -> Admin
GET /api/meshes/{id}/agent-nodes/discover -> Admin
GET /api/meshes/{id}/issues -> Admin
GET /api/meshes/{id}/pulls -> Admin
GET /api/meshes/{mesh_id}/pulls/{pr_number}/mergeability -> Admin
GET /ws/events -> WsTicket
GET /ws/terminal/* -> WsTicket
POST /__debug/log -> Public
GET /__certs/status -> Public
GET /install-cert.der -> Public
* /admin/* -> Admin (catch-all, 404 on authorized)
POST /api/session -> Public
POST /api/pair -> Public
POST /api/ws-ticket -> Public
POST /api/attention/* -> Public
GET /assets/* -> Public
GET /v2/assets/* -> Public
GET /v2 -> Public
GET /v2/ -> Public
GET /api/nodes -> Admin
GET /api/usage -> Admin
GET /api/providers -> Admin
GET /api/meshes -> Admin
GET /api/* -> Admin
* * -> Public";
    assert_eq!(actual, expected);
}

#[test]
fn path_two_segment_ids_parses_pulls_routes() {
    assert_eq!(
        path_two_segment_ids(
            "/api/meshes/7/pulls/42/mergeability",
            "/api/meshes/",
            "/pulls/",
            "/mergeability",
        ),
        Some((7, 42)),
    );
    assert_eq!(
        path_two_segment_ids(
            "/api/meshes/7/pulls/42/merge",
            "/api/meshes/",
            "/pulls/",
            "/merge",
        ),
        Some((7, 42)),
    );
}

#[test]
fn path_two_segment_ids_rejects_non_numeric_pulls_segments() {
    assert!(path_two_segment_ids(
        "/api/meshes/foo/pulls/42/mergeability",
        "/api/meshes/",
        "/pulls/",
        "/mergeability",
    )
    .is_none());
    assert!(path_two_segment_ids(
        "/api/meshes/7/pulls/bar/mergeability",
        "/api/meshes/",
        "/pulls/",
        "/mergeability",
    )
    .is_none());
}

#[tokio::test]
async fn attention_webhook_returns_503_when_app_handle_unset() {
    let req = ParsedRequest::test_post("/api/attention/42", b"");
    assert_eq!(status_of(dispatch(req).await), 503);
}

#[tokio::test]
async fn attention_webhook_returns_400_for_unparseable_session_id() {
    let req = ParsedRequest::test_post("/api/attention/not-an-int", b"");
    assert_eq!(status_of(dispatch(req).await), 400);
}

#[tokio::test]
async fn certs_status_returns_503_when_app_handle_unset() {
    let req = ParsedRequest::test_get("/__certs/status");
    assert_eq!(status_of(dispatch(req).await), 503);
}

#[tokio::test]
async fn install_cert_der_returns_503_when_app_handle_unset() {
    let req = ParsedRequest::test_get("/install-cert.der");
    assert_eq!(status_of(dispatch(req).await), 503);
}

#[tokio::test]
async fn root_serves_shell_without_credentials() {
    let req = ParsedRequest::test_get("/");
    let status = status_of(dispatch(req).await);
    assert!(status == 200 || status == 404, "got {status}");
}

#[tokio::test]
async fn coordinator_nodes_rejects_without_token() {
    let req = ParsedRequest::test_get("/nodes");
    assert_eq!(status_of(dispatch(req).await), 401);
}

#[tokio::test]
async fn admin_namespace_rejects_without_credentials() {
    let req = ParsedRequest::test_get("/admin/nope");
    assert_eq!(status_of(dispatch(req).await), 401);
}

#[tokio::test]
async fn ws_events_rejects_without_ticket() {
    let req = ParsedRequest::test_get("/ws/events");
    assert_eq!(status_of(dispatch(req).await), 401);
}

#[tokio::test]
async fn ws_terminal_rejects_raw_url_token() {
    let req = ParsedRequest::test_get("/ws/terminal/123?token=anything");
    assert_eq!(status_of(dispatch(req).await), 401);
}

#[tokio::test]
async fn ws_terminal_upgrade_on_valid_ticket() {
    let ticket = ws_ticket::mint(
        None,
        ws_ticket::WsTarget {
            surface: ws_ticket::SURFACE_TERMINAL.to_string(),
            node_id: Some(123),
        },
    );
    let req = ParsedRequest::test_get(&format!("/ws/terminal/123?ticket={ticket}"));
    match dispatch(req).await {
        DispatchResult::Upgrade(Upgrade::Terminal { node_id, .. }) => {
            assert_eq!(node_id, 123);
        }
        other => panic!("expected upgrade, got status {}", status_of(other)),
    }
}

#[tokio::test]
async fn v2_root_redirects_to_root() {
    let req = ParsedRequest::test_get("/v2");
    assert_eq!(status_of(dispatch(req).await), 301);
}

#[tokio::test]
async fn asset_is_public_not_401() {
    let req = ParsedRequest::test_get("/assets/index.js");
    let status = status_of(dispatch(req).await);
    assert_ne!(status, 401);
    assert_eq!(status, 404);
}

#[tokio::test]
async fn ws_ticket_endpoint_requires_admin_credentials() {
    let req = ParsedRequest::test_post("/api/ws-ticket", b"{}");
    assert_eq!(status_of(dispatch(req).await), 401);
}

#[tokio::test]
async fn coordinator_node_log_rejects_without_token() {
    let req = ParsedRequest::test_get("/nodes/42/log?tail=5");
    assert_eq!(status_of(dispatch(req).await), 401);
}

#[test]
fn sanitize_debug_body_strips_newlines_and_controls() {
    assert_eq!(sanitize_debug_body("hello\nINFO: forged"), "hello");
    assert_eq!(sanitize_debug_body("a\u{001b}[31mb"), "a[31mb");
}

#[test]
fn debug_logs_redact_credentials_before_truncation() {
    assert_eq!(
        sanitize_debug_body("https://host/#pair=abc123"),
        "https://host/#[redacted]"
    );
    assert_eq!(
        sanitize_debug_body("Authorization: Bearer 0123456789abcdef0123456789abcdef"),
        "Authorization: Bearer [redacted]"
    );
    assert_eq!(sanitize_debug_body("/?token=old-secret"), "/?[redacted]");
}
