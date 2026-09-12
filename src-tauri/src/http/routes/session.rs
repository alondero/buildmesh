//! Pairing login (`POST /api/session`) and WebSocket ticket mint
//! (`POST /api/ws-ticket`).

use crate::http::auth;
use crate::http::rate_limit;
use crate::http::request;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::ws_ticket;

/// `POST /api/session` — pairing/login handoff (issue #500, extended by #502).
/// The client POSTs a token as `Authorization: Bearer <token>`:
///   - the **root token** mints a new persistent device session;
///   - an existing **device token** refreshes that device and returns the same token.
pub async fn login(req: &ParsedRequest) -> Response {
    match request::bearer_token(&req.headers) {
        Some(t) => {
            let label = crate::http::routes::admin::device_label_from_user_agent(
                request::extract_header_value(&req.headers, "User-Agent"),
            );
            let peer_ip = req.peer.ip().to_string();
            match crate::db::login_device_session(&t, label.as_deref(), Some(&peer_ip)) {
                Ok(Some((_, device_token))) => {
                    let cookie = request::session_cookie_header(&device_token, req.secure);
                    let body = serde_json::json!({ "token": device_token }).to_string();
                    // Cookie is `Set-Cookie: ...` already; strip the header name
                    // so Response can emit it as a normal header.
                    let cookie_value = cookie
                        .strip_prefix("Set-Cookie: ")
                        .unwrap_or(&cookie)
                        .to_string();
                    Response::json("200 OK", body)
                        .with_header("Cache-Control", "no-store")
                        .with_header("Set-Cookie", cookie_value)
                }
                _ => Response::empty("401 Unauthorized"),
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
    if let Some(denied) = auth::deny_response(auth::authorize(&req.headers, auth::RequiredScope::Admin))
    {
        return denied;
    }
    let target = match ws_ticket::parse_mint_target(&req.body) {
        Ok(t) => t,
        Err(()) => return Response::empty("400 Bad Request"),
    };
    let device_id = auth::resolve_device_session(&req.headers);
    if let Some(id) = device_id {
        let peer_ip = req.peer.ip().to_string();
        let _ = crate::db::touch_device_session(id, Some(&peer_ip));
    }
    let body = serde_json::to_string(&ws_ticket::WsTicket {
        ticket: ws_ticket::mint(device_id, target),
    })
    .unwrap_or_else(|_| r#"{"ticket":""}"#.to_string());
    Response::json("200 OK", body)
}
