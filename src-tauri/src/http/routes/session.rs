//! Pairing (`POST /api/pair`), session refresh (`POST /api/session`), and WebSocket ticket mint
//! (`POST /api/ws-ticket`).

use crate::http::auth;
use crate::http::rate_limit;
use crate::http::request;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::ws_ticket;

/// `/api/pair` consumes a desktop invitation; `/api/session` refreshes only an
/// existing device, including one-time migration of a legacy bearer credential.
/// Neither route returns the device secret to JavaScript or accepts the root.
pub async fn login(req: &ParsedRequest) -> Response {
    let fingerprint = crate::db::hash_token(&format!("session:{}", req.peer.ip()));
    if let rate_limit::Outcome::Deny { retry_after } = rate_limit::check_and_record(
        &fingerprint,
        std::time::Instant::now(),
        rate_limit::DEFAULT_MAX_PER_WINDOW,
    ) {
        return Response::rate_limited(retry_after);
    }
    let presented = if req.path == "/api/pair" {
        // Pairing is intentionally a separate boundary: only a desktop-issued
        // invitation in Authorization may create a device. A cookie can refresh
        // an existing device, but must never be accepted as a new invitation.
        request::bearer_token(&req.headers)
    } else {
        request::bearer_token(&req.headers)
            .or_else(|| request::extract_token_from_cookies(&req.headers))
    };
    match presented {
        Some(t) => {
            let label = crate::http::routes::admin::device_label_from_user_agent(
                request::extract_header_value(&req.headers, "User-Agent"),
            );
            let peer_ip = req.peer.ip().to_string();
            let pairing = req.path == "/api/pair";
            let result = crate::commands::run_blocking("mobile_session", move || {
                if pairing {
                    crate::http::pairing::exchange(&t, label.as_deref(), &peer_ip)
                        .map_err(|e| e.to_string())
                } else {
                    let conn = crate::db::write_conn();
                    let id = crate::db::validate_device_token_inner(&conn, &t)
                        .map_err(|e| e.to_string())?;
                    if let Some(id) = id {
                        crate::db::refresh_device_session_activity_inner(&conn, id, Some(&peer_ip))
                            .map_err(|e| e.to_string())?;
                        Ok(Some(t))
                    } else {
                        Ok(None)
                    }
                }
            })
            .await;
            match result {
                Ok(Some(device_token)) => {
                    let cookie_value = request::session_cookie_value(&device_token, req.secure);
                    Response::empty("204 No Content")
                        .with_header("Cache-Control", "no-store")
                        .with_header("Set-Cookie", cookie_value)
                }
                Ok(None) => Response::empty("401 Unauthorized"),
                Err(_) => Response::empty("503 Service Unavailable"),
            }
        }
        None => Response::empty("401 Unauthorized"),
    }
}

/// `POST /api/ws-ticket` — mint a single-use WebSocket handshake ticket.
/// Rate-limit runs BEFORE auth so a stolen-token flooder cannot distinguish
/// 429 from 401 by reading the body (issue #552).
pub async fn mint_ws_ticket(req: &ParsedRequest) -> Response {
    let credential_for_rate_limit = request::bearer_token(&req.headers)
        .or_else(|| request::extract_token_from_cookies(&req.headers));
    if let Some(presented) = credential_for_rate_limit {
        let fingerprint = crate::db::hash_token(&presented);
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
    }
    // ONE credential walk (issue #589): the role the request proves and the
    // device it belongs to come from the same resolution. The mint used to
    // authorize the request and then walk the credentials again for the device
    // id — two DB checkouts per mint, and a window in which the two passes
    // could disagree (a revoke landing between them) and bind the ticket to a
    // device the request was not authorized as.
    let resolved = match auth::resolve_auth(&req.headers, Some(req.peer.ip())) {
        Ok(resolved) => resolved,
        Err(()) => return Response::empty("503 Service Unavailable"),
    };
    if let Some(denied) = auth::deny_for_role(resolved.role, auth::RequiredScope::Admin) {
        return denied;
    }
    let target = match ws_ticket::parse_mint_target(&req.body) {
        Ok(t) => t,
        Err(()) => return Response::empty("400 Bad Request"),
    };
    let body = serde_json::to_string(&ws_ticket::WsTicket {
        ticket: ws_ticket::mint(resolved.device_id, target),
    })
    .unwrap_or_else(|_| r#"{"ticket":""}"#.to_string());
    Response::json("200 OK", body)
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
