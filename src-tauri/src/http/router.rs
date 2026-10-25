//! HTTP router: path matching, auth, and handler dispatch.
//!
//! Every route — including `/ws/*`, assets, attention, certs — registers in
//! [`ROUTES`]. [`dispatch`] is the test seam: handlers take [`ParsedRequest`]
//! and return [`Response`] (or a WebSocket upgrade), with no sockets.

use std::net::SocketAddr;

use crate::http::assets;
use crate::http::auth;
use crate::http::rate_limit;
use crate::http::request;
use crate::http::response::Response;
use crate::http::routes;
use crate::http::ws_ticket;

use crate::http::auth::RequiredScope;

/// How long the server will wait for a client to finish sending its request line
/// and headers before dropping the connection.
pub(crate) const REQUEST_HEAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Upper bound on the combined size of a request's header block.
pub(crate) const MAX_HEADER_BYTES: usize = 64 * 1024;

const DEFAULT_LOG_TAIL: usize = 10;
const MAX_DEBUG_BODY_CHARS: usize = 512;

/// A fully-parsed HTTP request ready for a handler. The server owns reading
/// the head and (when the route asks) the body; handlers never see the stream.
#[derive(Debug, Clone)]
pub struct ParsedRequest {
    /// HTTP method on the request line. Read by `match_route` and again here,
    /// where the Origin check below rejects a mutating cross-origin request;
    /// kept on the public type because the test seam
    /// ([`ParsedRequest::test_get`] / [`ParsedRequest::test_post`]) needs to
    /// populate it for handler assertions.
    pub method: String,
    /// Path without the query string.
    pub path: String,
    pub path_with_query: String,
    pub headers: String,
    pub body: Vec<u8>,
    pub peer: SocketAddr,
    pub secure: bool,
    pub ids: (Option<i64>, Option<i64>),
}

impl ParsedRequest {
    pub fn id0(&self) -> i64 {
        self.ids.0.unwrap_or(0)
    }

    pub fn id1(&self) -> i64 {
        self.ids.1.unwrap_or(0)
    }

    pub fn query(&self) -> Option<&str> {
        self.path_with_query.split_once('?').map(|(_, q)| q)
    }

    pub fn query_param(&self, name: &str) -> Option<String> {
        query_param(&self.path_with_query, name)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        request::extract_header_value(&self.headers, name)
    }

    pub fn tail_param(&self) -> usize {
        self.query_param("tail")
            .and_then(|t| t.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_LOG_TAIL)
    }

    pub fn content_length(&self) -> usize {
        self.header("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn test_get(path: &str) -> Self {
        Self::test("GET", path, b"")
    }

    #[cfg(test)]
    pub fn test_post(path: &str, body: &[u8]) -> Self {
        Self::test("POST", path, body)
    }

    #[cfg(test)]
    fn test(method: &str, path: &str, body: &[u8]) -> Self {
        let path_without = path.split('?').next().unwrap_or(path).to_string();
        Self {
            method: method.to_string(),
            path: path_without,
            path_with_query: path.to_string(),
            headers: format!("Host: localhost\r\nContent-Length: {}\r\n", body.len()),
            body: body.to_vec(),
            peer: SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 1)),
            secure: false,
            ids: (None, None),
        }
    }

    #[cfg(test)]
    pub fn with_ids(mut self, a: Option<i64>, b: Option<i64>) -> Self {
        self.ids = (a, b);
        self
    }
}

/// How a [`Route`] matches a request path and captures its integer id(s).
#[derive(Clone, Copy, Debug)]
pub(crate) enum RouteMatch {
    Exact(&'static str),
    OneId {
        prefix: &'static str,
        suffix: &'static str,
    },
    TwoId {
        prefix: &'static str,
        mid: &'static str,
        suffix: &'static str,
    },
    /// Path equals `base` or starts with `base/`.
    Under(&'static str),
    Prefix(&'static str),
    /// Always matches; must be last.
    Any,
}

impl RouteMatch {
    fn captures(&self, path: &str) -> Option<(Option<i64>, Option<i64>)> {
        match self {
            RouteMatch::Exact(p) => (path == *p).then_some((None, None)),
            RouteMatch::OneId { prefix, suffix } => {
                path_segment_id(path, prefix, suffix).map(|id| (Some(id), None))
            }
            RouteMatch::TwoId {
                prefix,
                mid,
                suffix,
            } => path_two_segment_ids(path, prefix, mid, suffix).map(|(a, b)| (Some(a), Some(b))),
            RouteMatch::Under(base) => {
                // Avoid the `format!("{base}/")` allocation per request — slice
                // checks are sufficient: `path.starts_with(base)` plus the next
                // byte being `/` matches the same set without heap.
                (path == *base || (path.starts_with(base) && path[base.len()..].starts_with('/')))
                    .then_some((None, None))
            }
            RouteMatch::Prefix(p) => path.starts_with(p).then_some((None, None)),
            RouteMatch::Any => Some((None, None)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteScope {
    Public,
    Admin,
    CoordinatorRead,
    CoordinatorWrite,
    /// Ticket consume happens in the handler; unauthenticated probe → 401.
    WsTicket,
    /// `/admin` catch-all: 401 / 403 / 404.
    AdminCatchAll,
}

impl RouteScope {
    fn required(self) -> Option<RequiredScope> {
        match self {
            RouteScope::Admin => Some(RequiredScope::Admin),
            RouteScope::CoordinatorRead => Some(RequiredScope::CoordinatorRead),
            RouteScope::CoordinatorWrite => Some(RequiredScope::CoordinatorWrite),
            RouteScope::Public | RouteScope::WsTicket | RouteScope::AdminCatchAll => None,
        }
    }

    #[cfg(test)]
    fn snapshot_label(self) -> &'static str {
        match self {
            RouteScope::Public => "Public",
            RouteScope::Admin => "Admin",
            RouteScope::CoordinatorRead => "CoordinatorRead",
            RouteScope::CoordinatorWrite => "CoordinatorWrite",
            RouteScope::WsTicket => "WsTicket",
            RouteScope::AdminCatchAll => "Admin (catch-all, 404 on authorized)",
        }
    }
}

/// Per-route body-read policy applied by the server *before* dispatch.
#[derive(Clone, Copy, Debug)]
pub(crate) enum BodyPolicy {
    None,
    Cap(usize),
    /// Oversize still dispatches with an empty body (debug log: always 204).
    CapOrSkip(usize),
}

#[derive(Clone, Copy)]
enum Handler {
    AdminDevices,
    AdminRevoke,
    CoordinatorNodes,
    CoordinatorLog,
    CoordinatorPrompt,
    NodesCreate,
    NodesInput,
    PrCreate,
    PrSource,
    PrMerge,
    ImportResume,
    IssuesSpawn,
    PullsSpawn,
    IssueLabels,
    IssueSetLabel,
    CircuitRuns,
    CircuitAgentOwnerships,
    NodeReviewOptions,
    NodeStartReview,
    CircuitApprove,
    CircuitOutcome,
    CircuitRecover,
    CircuitCancel,
    GitStatus,
    GitSummary,
    GitBranch,
    GitDiff,
    GhAuth,
    AgentNodesDiscover,
    IssuesList,
    PullsList,
    PrMergeability,
    WsEvents,
    WsTerminal,
    DebugLog,
    CertsStatus,
    InstallCert,
    AdminCatchAll,
    Session,
    WsTicket,
    Attention,
    Assets,
    V2Redirect,
    ApiCatchAll,
    SpaShell,
    ApiNodes,
    ApiProviders,
    ApiUsage,
    LaunchConfigurations,
    LaunchTargets,
    LaunchConfigurationSave,
    LaunchConfigurationVerify,
    LaunchConfigurationDelete,
    ApiMeshes,
}

struct Route {
    method: &'static str,
    m: RouteMatch,
    scope: RouteScope,
    body: BodyPolicy,
    handler: Handler,
}

impl Route {
    fn method_matches(&self, method: &str) -> bool {
        self.method == "*" || self.method == method
    }
}

/// The full HTTP surface. Order is load-bearing: more specific paths precede
/// prefix/catch-all routes (`/admin/devices*` before `/admin/*`, `/api/meshes/…`
/// before `GET /api/*`, assets before the SPA fallback).
const ROUTES: &[Route] = &[
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/launch-configurations"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::LaunchConfigurations,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/launch-targets"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::LaunchTargets,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/api/launch-configurations/save"),
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(64 * 1024),
        handler: Handler::LaunchConfigurationSave,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/api/launch-configurations/verify"),
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(64 * 1024),
        handler: Handler::LaunchConfigurationVerify,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/api/launch-configurations/delete"),
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(8 * 1024),
        handler: Handler::LaunchConfigurationDelete,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/launch-configurations"),
        scope: RouteScope::CoordinatorRead,
        body: BodyPolicy::None,
        handler: Handler::ApiProviders,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/nodes/create"),
        scope: RouteScope::CoordinatorWrite,
        body: BodyPolicy::Cap(64 * 1024),
        handler: Handler::NodesCreate,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/admin/devices"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::AdminDevices,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/admin/devices/",
            suffix: "/revoke",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::AdminRevoke,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/nodes"),
        scope: RouteScope::CoordinatorRead,
        body: BodyPolicy::None,
        handler: Handler::CoordinatorNodes,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/nodes/",
            suffix: "/log",
        },
        scope: RouteScope::CoordinatorRead,
        body: BodyPolicy::None,
        handler: Handler::CoordinatorLog,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/nodes/",
            suffix: "/prompt",
        },
        scope: RouteScope::CoordinatorWrite,
        body: BodyPolicy::Cap(256 * 1024),
        handler: Handler::CoordinatorPrompt,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/api/nodes/create"),
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(64 * 1024),
        handler: Handler::NodesCreate,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/api/nodes/",
            suffix: "/input",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(routes::nodes::INPUT_BODY_MAX_BYTES),
        handler: Handler::NodesInput,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/api/meshes/",
            suffix: "/pr",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(64 * 1024),
        handler: Handler::PrCreate,
    },
    // Preview the source/base pair a create-PR would use, resolved from the
    // agent node's worktree (issue #2024 rank 4 / #1567). Registered before
    // the GitBranch route so the more specific `/pr/source` suffix wins.
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/meshes/",
            suffix: "/pr/source",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::PrSource,
    },
    Route {
        method: "POST",
        m: RouteMatch::TwoId {
            prefix: "/api/meshes/",
            mid: "/pulls/",
            suffix: "/merge",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(8 * 1024),
        handler: Handler::PrMerge,
    },
    Route {
        method: "POST",
        m: RouteMatch::TwoId {
            prefix: "/api/meshes/",
            mid: "/pulls/",
            suffix: "/spawn",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(16 * 1024),
        handler: Handler::PullsSpawn,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/api/meshes/",
            suffix: "/agent-nodes/import-and-resume",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(64 * 1024),
        handler: Handler::ImportResume,
    },
    Route {
        method: "POST",
        m: RouteMatch::TwoId {
            prefix: "/api/meshes/",
            mid: "/issues/",
            suffix: "/spawn",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(256 * 1024),
        handler: Handler::IssuesSpawn,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/meshes/",
            suffix: "/issue-labels",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::IssueLabels,
    },
    Route {
        method: "POST",
        m: RouteMatch::TwoId {
            prefix: "/api/meshes/",
            mid: "/issues/",
            suffix: "/label",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(8 * 1024),
        handler: Handler::IssueSetLabel,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/nodes/",
            suffix: "/reviews",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::NodeReviewOptions,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/api/nodes/",
            suffix: "/reviews",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(1024),
        handler: Handler::NodeStartReview,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/circuit-agent-ownerships"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::CircuitAgentOwnerships,
    },
    // Circuit Runs on Agent Nodes (issue #2314). The run id is in the path;
    // the act routes reuse the desktop commands, so eligibility is theirs.
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/circuit-runs"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::CircuitRuns,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/api/circuit-runs/",
            suffix: "/approve",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(8 * 1024),
        handler: Handler::CircuitApprove,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/api/circuit-runs/",
            suffix: "/outcome",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(64 * 1024),
        handler: Handler::CircuitOutcome,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/api/circuit-runs/",
            suffix: "/recover",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::Cap(64 * 1024),
        handler: Handler::CircuitRecover,
    },
    Route {
        method: "POST",
        m: RouteMatch::OneId {
            prefix: "/api/circuit-runs/",
            suffix: "/cancel",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::CircuitCancel,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/agents/",
            suffix: "/git/status",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::GitStatus,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/agents/",
            suffix: "/git/summary",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::GitSummary,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/agents/",
            suffix: "/git/branch",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::GitBranch,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/agents/",
            suffix: "/diff",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::GitDiff,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/gh/auth"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::GhAuth,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/meshes/",
            suffix: "/agent-nodes/discover",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::AgentNodesDiscover,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/meshes/",
            suffix: "/issues",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::IssuesList,
    },
    Route {
        method: "GET",
        m: RouteMatch::OneId {
            prefix: "/api/meshes/",
            suffix: "/pulls",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::PullsList,
    },
    Route {
        method: "GET",
        m: RouteMatch::TwoId {
            prefix: "/api/meshes/",
            mid: "/pulls/",
            suffix: "/mergeability",
        },
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::PrMergeability,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/ws/events"),
        scope: RouteScope::WsTicket,
        body: BodyPolicy::None,
        handler: Handler::WsEvents,
    },
    Route {
        method: "GET",
        m: RouteMatch::Prefix("/ws/terminal/"),
        scope: RouteScope::WsTicket,
        body: BodyPolicy::None,
        handler: Handler::WsTerminal,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/__debug/log"),
        scope: RouteScope::Public,
        body: BodyPolicy::CapOrSkip(64 * 1024),
        handler: Handler::DebugLog,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/__certs/status"),
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::CertsStatus,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/install-cert.der"),
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::InstallCert,
    },
    Route {
        method: "*",
        m: RouteMatch::Under("/admin"),
        scope: RouteScope::AdminCatchAll,
        body: BodyPolicy::None,
        handler: Handler::AdminCatchAll,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/api/session"),
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::Session,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/api/pair"),
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::Session,
    },
    Route {
        method: "POST",
        m: RouteMatch::Exact("/api/ws-ticket"),
        scope: RouteScope::Public,
        body: BodyPolicy::Cap(8 * 1024),
        handler: Handler::WsTicket,
    },
    Route {
        method: "POST",
        m: RouteMatch::Prefix("/api/attention/"),
        scope: RouteScope::Public,
        body: BodyPolicy::Cap(routes::attention::MAX_HOOK_BODY),
        handler: Handler::Attention,
    },
    Route {
        method: "GET",
        m: RouteMatch::Prefix("/assets/"),
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::Assets,
    },
    Route {
        method: "GET",
        m: RouteMatch::Prefix("/v2/assets/"),
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::Assets,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/v2"),
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::V2Redirect,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/v2/"),
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::V2Redirect,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/nodes"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::ApiNodes,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/usage"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::ApiUsage,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/providers"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::ApiProviders,
    },
    Route {
        method: "GET",
        m: RouteMatch::Exact("/api/meshes"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::ApiMeshes,
    },
    Route {
        method: "GET",
        m: RouteMatch::Prefix("/api/"),
        scope: RouteScope::Admin,
        body: BodyPolicy::None,
        handler: Handler::ApiCatchAll,
    },
    Route {
        method: "*",
        m: RouteMatch::Any,
        scope: RouteScope::Public,
        body: BodyPolicy::None,
        handler: Handler::SpaShell,
    },
];

/// Result of routing a parsed request.
pub enum DispatchResult {
    Http(Response),
    Upgrade(Upgrade),
}

pub enum Upgrade {
    Events {
        device_id: Option<i64>,
    },
    Terminal {
        node_id: i64,
        device_id: Option<i64>,
    },
}

pub(crate) struct MatchedRoute {
    pub body: BodyPolicy,
    handler: Handler,
    scope: RouteScope,
    ids: (Option<i64>, Option<i64>),
}

pub(crate) fn match_route(method: &str, path: &str) -> MatchedRoute {
    for route in ROUTES {
        if route.method_matches(method) {
            if let Some(ids) = route.m.captures(path) {
                return MatchedRoute {
                    body: route.body,
                    handler: route.handler,
                    scope: route.scope,
                    ids,
                };
            }
        }
    }
    MatchedRoute {
        body: BodyPolicy::None,
        handler: Handler::SpaShell,
        scope: RouteScope::Public,
        ids: (None, None),
    }
}

/// Dispatch a parsed request. Tests call this directly — no sockets.
/// The server uses [`dispatch_matched`] so it can reuse the
/// [`BodyPolicy`] it already resolved to size the body read; this entry exists
/// for the test seam (tests construct a `ParsedRequest` directly and need to
/// round-trip without first calling `match_route`), so it is compiled for tests
/// alone (issue #2450).
#[cfg(test)]
pub async fn dispatch(req: ParsedRequest) -> DispatchResult {
    let matched = match_route(&req.method, &req.path);
    dispatch_matched(req, matched).await
}

/// Dispatch using a route the caller already matched. The server uses this
/// after `match_route` resolves the [`BodyPolicy`] it needs to size the
/// request-body read; re-matching inside dispatch would scan the 38-route
/// table a second time per connection.
pub(crate) async fn dispatch_matched(
    mut req: ParsedRequest,
    matched: MatchedRoute,
) -> DispatchResult {
    req.ids = matched.ids;

    // Browsers attach Origin to mutating requests. Compare the full origin,
    // including scheme and port; SameSite alone permits hostile sibling ports.
    if req.method != "GET" && req.method != "HEAD" {
        if let Some(origin) = request::extract_header_value(&req.headers, "Origin") {
            let host = request::extract_header_value(&req.headers, "Host").unwrap_or("");
            let scheme = if req.secure { "https" } else { "http" };
            if origin != format!("{scheme}://{host}") {
                return DispatchResult::Http(Response::empty("403 Forbidden"));
            }
        }
    }

    if let Some(required) = matched.scope.required() {
        if let Some(denied) = auth::deny_response(auth::authorize(&req.headers, required)) {
            return DispatchResult::Http(denied);
        }
    }

    run_handler(matched.handler, &req).await
}

async fn run_handler(handler: Handler, req: &ParsedRequest) -> DispatchResult {
    use DispatchResult::Http;
    match handler {
        Handler::AdminDevices => Http(routes::admin::list(req).await),
        Handler::AdminRevoke => Http(routes::admin::revoke(req).await),
        Handler::CoordinatorNodes => Http(routes::coordinator::list(req).await),
        Handler::CoordinatorLog => Http(routes::coordinator::log(req).await),
        Handler::CoordinatorPrompt => Http(routes::coordinator::prompt(req).await),
        Handler::NodesCreate => Http(routes::nodes::create(req).await),
        Handler::NodesInput => Http(routes::nodes::post_input(req).await),
        Handler::PrCreate => Http(routes::pr::create(req).await),
        Handler::PrSource => Http(routes::pr::pr_source(req).await),
        Handler::PrMerge => Http(routes::pr::merge(req).await),
        Handler::ImportResume => Http(routes::agent_nodes::import_and_resume(req).await),
        Handler::IssuesSpawn => Http(routes::issues::spawn(req).await),
        Handler::PullsSpawn => Http(routes::pr::spawn(req).await),
        Handler::IssueLabels => Http(routes::issues::labels(req).await),
        Handler::IssueSetLabel => Http(routes::issues::set_label(req).await),
        Handler::CircuitRuns => Http(routes::circuit::list(req).await),
        Handler::CircuitAgentOwnerships => Http(routes::circuit::ownerships(req).await),
        Handler::NodeReviewOptions => Http(routes::circuit::review_options(req).await),
        Handler::NodeStartReview => Http(routes::circuit::start_review(req).await),
        Handler::CircuitApprove => Http(routes::circuit::approve(req).await),
        Handler::CircuitOutcome => Http(routes::circuit::outcome(req).await),
        Handler::CircuitRecover => Http(routes::circuit::recover(req).await),
        Handler::CircuitCancel => Http(routes::circuit::cancel(req).await),
        Handler::GitStatus => Http(routes::git::status(req).await),
        Handler::GitSummary => Http(routes::git::summary(req).await),
        Handler::GitBranch => Http(routes::git::branch(req).await),
        Handler::GitDiff => Http(routes::git::diff(req).await),
        Handler::GhAuth => Http(routes::git::gh_auth(req).await),
        Handler::AgentNodesDiscover => Http(routes::agent_nodes::discover(req).await),
        Handler::IssuesList => Http(routes::issues::list(req).await),
        Handler::PullsList => Http(routes::pr::list_pulls(req).await),
        Handler::PrMergeability => Http(routes::pr::get_mergeability(req).await),
        Handler::WsEvents => ws_events(req),
        Handler::WsTerminal => ws_terminal(req),
        Handler::DebugLog => Http(debug_log(req)),
        Handler::CertsStatus => Http(routes::certs::status(req).await),
        Handler::InstallCert => Http(routes::certs::install(req).await),
        Handler::AdminCatchAll => Http(admin_catchall(req)),
        Handler::Session => Http(routes::session::login(req).await),
        Handler::WsTicket => Http(routes::session::mint_ws_ticket(req).await),
        Handler::Attention => Http(routes::attention::handle_post(req).await),
        Handler::Assets => Http(assets::asset(req)),
        Handler::V2Redirect => Http(v2_redirect(req)),
        Handler::ApiCatchAll => Http(Response::json("200 OK", r#"{"error":"not found"}"#)),
        Handler::SpaShell => Http(assets::spa_shell(req)),
        Handler::ApiNodes => Http(routes::nodes::list(req).await),
        Handler::ApiProviders => Http(routes::providers::list(req).await),
        Handler::ApiUsage => Http(routes::providers::usage(req).await),
        Handler::LaunchConfigurations => Http(routes::launch_configurations::list(req).await),
        Handler::LaunchTargets => Http(routes::launch_configurations::targets(req).await),
        Handler::LaunchConfigurationSave => Http(routes::launch_configurations::save(req).await),
        Handler::LaunchConfigurationVerify => {
            Http(routes::launch_configurations::verify(req).await)
        }
        Handler::LaunchConfigurationDelete => {
            Http(routes::launch_configurations::delete(req).await)
        }
        Handler::ApiMeshes => Http(routes::meshes::list(req).await),
    }
}

fn admin_catchall(req: &ParsedRequest) -> Response {
    match auth::authorize(&req.headers, RequiredScope::Admin) {
        auth::AuthOutcome::Ok(_) => Response::empty("404 Not Found"),
        auth::AuthOutcome::Unauthorized => Response::empty("401 Unauthorized"),
        auth::AuthOutcome::Forbidden => Response::empty("403 Forbidden"),
        auth::AuthOutcome::Unavailable => Response::empty("503 Service Unavailable"),
    }
}

fn ws_events(req: &ParsedRequest) -> DispatchResult {
    let ticket = req.query_param("ticket").unwrap_or_default();
    let requested = ws_ticket::WsTarget {
        surface: ws_ticket::SURFACE_EVENTS.to_string(),
        node_id: None,
    };
    match ws_ticket::consume(&ticket, &requested) {
        ws_ticket::ConsumeOutcome::Ok(device_id) => {
            DispatchResult::Upgrade(Upgrade::Events { device_id })
        }
        ws_ticket::ConsumeOutcome::TargetMismatch => {
            DispatchResult::Http(Response::empty("403 Forbidden"))
        }
        ws_ticket::ConsumeOutcome::Invalid => {
            DispatchResult::Http(Response::empty("401 Unauthorized"))
        }
    }
}

fn ws_terminal(req: &ParsedRequest) -> DispatchResult {
    let node_id: Option<i64> = req
        .path_with_query
        .split('/')
        .nth(3)
        .and_then(|s| s.split('?').next().unwrap_or(s).parse().ok());
    let Some(node_id) = node_id else {
        return DispatchResult::Http(Response::empty("400 Bad Request"));
    };
    let ticket = req.query_param("ticket").unwrap_or_default();
    let requested = ws_ticket::WsTarget {
        surface: ws_ticket::SURFACE_TERMINAL.to_string(),
        node_id: Some(node_id),
    };
    match ws_ticket::consume(&ticket, &requested) {
        ws_ticket::ConsumeOutcome::Ok(device_id) => {
            DispatchResult::Upgrade(Upgrade::Terminal { node_id, device_id })
        }
        ws_ticket::ConsumeOutcome::TargetMismatch => {
            DispatchResult::Http(Response::empty("403 Forbidden"))
        }
        ws_ticket::ConsumeOutcome::Invalid => {
            DispatchResult::Http(Response::empty("401 Unauthorized"))
        }
    }
}

fn debug_log(req: &ParsedRequest) -> Response {
    let fingerprint = crate::db::hash_token(&req.peer.ip().to_string());
    match rate_limit::check_and_record(
        &fingerprint,
        std::time::Instant::now(),
        rate_limit::DEFAULT_MAX_PER_WINDOW,
    ) {
        rate_limit::Outcome::Allow => {}
        rate_limit::Outcome::Deny { retry_after } => {
            return Response::rate_limited(retry_after);
        }
    }
    let content_length = req.content_length();
    if content_length <= 64 * 1024 {
        let body = String::from_utf8_lossy(&req.body);
        let sanitized = sanitize_debug_body(&body);
        tracing::info!(target: "buildmesh_lib::diagnostics", "SPA debug event: {sanitized}");
    } else {
        tracing::warn!(target: "buildmesh_lib::diagnostics",
            "SPA debug log payload too large: {} bytes", content_length);
    }
    Response::empty("204 No Content")
}

fn v2_redirect(req: &ParsedRequest) -> Response {
    let preserve_query = req
        .path_with_query
        .split_once('?')
        .map(|(_, q)| format!("?{}", q))
        .unwrap_or_default();
    Response::empty("301 Moved Permanently").with_header("Location", format!("/{preserve_query}"))
}

fn sanitize_debug_body(input: &str) -> String {
    static SECRETS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = SECRETS.get_or_init(|| {
        regex::Regex::new(r"(?i)[0-9a-f]{32,}|(?:token|ticket|pair)(?:=|%3d)[^\s\x22&#\\]*")
            .unwrap()
    });
    let redacted = pattern.replace_all(input, "[redacted]");
    let first_line = redacted.lines().next().unwrap_or("").trim_end();
    first_line
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DEBUG_BODY_CHARS)
        .collect()
}

pub(crate) fn path_segment_id(path: &str, prefix: &str, suffix: &str) -> Option<i64> {
    let rest = path.strip_prefix(prefix)?;
    let id_str = rest.strip_suffix(suffix)?;
    id_str.parse().ok()
}

pub(crate) fn path_two_segment_ids(
    path: &str,
    prefix: &str,
    middle: &str,
    suffix: &str,
) -> Option<(i64, i64)> {
    let rest = path.strip_prefix(prefix)?;
    let (id1_str, rest) = rest.split_once(middle)?;
    let id2_str = rest.strip_suffix(suffix)?;
    Some((id1_str.parse().ok()?, id2_str.parse().ok()?))
}

pub(crate) fn query_param(path_with_query: &str, name: &str) -> Option<String> {
    let query = path_with_query.split('?').nth(1)?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == name {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.replace('+', " ");
    let mut out = Vec::with_capacity(bytes.len());
    let raw = bytes.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            let hi = (raw[i + 1] as char).to_digit(16);
            let lo = (raw[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn content_length(headers: &str) -> usize {
    request::extract_header_value(headers, "Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
