//! `POST /api/attention/{session_id}` — webhook from Claude Code's Stop hook.
//!
//! No token required: the hook is configured locally and runs over localhost.

use tokio::net::TcpStream;

use crate::http::request;

pub async fn handle_post(
    lines: &mut tokio::io::BufStream<TcpStream>,
    path_without_query: &str,
) {
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

    crate::commands::attention::mark_attention(session_id, app);
    crate::http::events::emit(crate::http::events::EventMsg::AttentionNeeded { session_id });

    let _ = request::write_status_only(lines, "200 OK").await;
}
