//! `POST /api/attention/{session_id}` — webhook for a Node Turn (see CONTEXT.md
//! and `crate::node_turn`). Both Claude Code hooks point here: the Stop hook
//! (turn finished, awaiting input) and the catch-all Notification hook (idle or
//! permission prompt). All are yields, so the payload carries no discriminator;
//! publishing the Node Turn fans out to attention-marking and session naming.
//!
//! No token required: the hook is configured locally and runs over localhost.
//! Because it is unauthenticated, the handler verifies the client peer address
//! is loopback (issue #496) — an external machine cannot spoof attention events
//! even if it can reach the port.

use std::net::SocketAddr;

use tokio::net::TcpStream;

use crate::http::request;

pub async fn handle_post(
    lines: &mut tokio::io::BufStream<TcpStream>,
    path_without_query: &str,
    peer: SocketAddr,
) {
    // Loopback-only: the Claude Code hook always posts from 127.0.0.1/::1. A
    // non-loopback peer is an external spoof attempt — refuse before doing work.
    if !peer.ip().is_loopback() {
        let _ = request::write_status_only(lines, "403 Forbidden").await;
        return;
    }

    let session_id: Option<i64> = path_without_query
        .strip_prefix("/api/attention/")
        .and_then(|s| s.parse().ok());

    let Some(session_id) = session_id else {
        let _ = request::write_status_only(lines, "400 Bad Request").await;
        return;
    };

    let Some(app) = crate::http::app_handle() else {
        let _ = request::write_status_only(lines, "503 Service Unavailable").await;
        return;
    };

    crate::node_turn::publish(session_id, app);
    crate::http::events::emit(crate::http::events::EventMsg::AttentionNeeded { session_id });

    let _ = request::write_status_only(lines, "200 OK").await;
}
