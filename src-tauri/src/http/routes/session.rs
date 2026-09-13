//! Pairing (`POST /api/pair`), session refresh (`POST /api/session`), and WebSocket ticket mint
//! (`POST /api/ws-ticket`).

use crate::http::auth;
use crate::http::rate_limit;
use crate::http::request;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::ws_ticket;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::router::{dispatch, DispatchResult};

    async fn send(path: &str, headers: &str) -> Response {
        let mut req = ParsedRequest::test_post(path, b"");
        req.secure = true;
        req.headers = format!("Host: localhost\r\n{headers}\r\n");
        match dispatch(req).await {
            DispatchResult::Http(response) => response,
            _ => panic!("unexpected upgrade"),
        }
    }

    #[tokio::test]
    async fn pairing_refresh_and_revocation_cross_the_real_router() {
        crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let root = {
            let conn = crate::db::write_conn();
            crate::db::get_or_create_root_token_inner(&conn).unwrap()
        };
        assert_eq!(
            send("/api/session", &format!("Authorization: Bearer {root}"))
                .await
                .status_code(),
            401
        );
        let invitation = crate::http::pairing::mint();
        let paired = send("/api/pair", &format!("Authorization: Bearer {invitation}")).await;
        assert_eq!(paired.status_code(), 204);
        assert!(
            paired.body().is_empty(),
            "device secrets never appear in JSON"
        );
        let wire = String::from_utf8(paired.encode()).unwrap();
        let cookie = wire
            .lines()
            .find_map(|line| line.strip_prefix("Set-Cookie: "))
            .unwrap();
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Max-Age=34560000"));
        let credential = cookie.split(';').next().unwrap();
        let token = credential.strip_prefix("bm_session=").unwrap();
        let id = {
            let conn = crate::db::read_conn();
            crate::db::validate_device_token_inner(&conn, token)
                .unwrap()
                .unwrap()
        };
        assert_eq!(
            send("/api/pair", &format!("Authorization: Bearer {invitation}"))
                .await
                .status_code(),
            401
        );
        let second_invitation = crate::http::pairing::mint();
        assert_eq!(
            send("/api/pair", &format!("Cookie: {credential}"))
                .await
                .status_code(),
            401,
            "an existing device cookie cannot cross the pairing boundary"
        );
        let second = send(
            "/api/pair",
            &format!("Authorization: Bearer {second_invitation}"),
        )
        .await;
        assert_eq!(second.status_code(), 204);
        let second_token = String::from_utf8(second.encode())
            .unwrap()
            .split("bm_session=")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let second_id = {
            let conn = crate::db::read_conn();
            crate::db::validate_device_token_inner(&conn, &second_token)
                .unwrap()
                .unwrap()
        };
        crate::db::revoke_device_session(second_id).unwrap();
        assert_eq!(
            send("/api/session", &format!("Cookie: {credential}"))
                .await
                .status_code(),
            204
        );
        // Old localStorage device credentials migrate through the same refresh.
        assert_eq!(
            send("/api/session", &format!("Authorization: Bearer {token}"))
                .await
                .status_code(),
            204
        );
        crate::db::revoke_device_session(id).unwrap();
        assert_eq!(
            send("/api/session", &format!("Cookie: {credential}"))
                .await
                .status_code(),
            401
        );
    }

    #[tokio::test]
    async fn cross_origin_pairing_is_rejected_before_consuming_the_invitation() {
        crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let ticket = crate::http::pairing::mint();
        assert_eq!(
            send(
                "/api/pair",
                &format!("Origin: https://evil.example\r\nAuthorization: Bearer {ticket}")
            )
            .await
            .status_code(),
            403
        );
        // Check it is still usable, then remove the test-created identity.
        let paired = send(
            "/api/pair",
            &format!("Origin: https://localhost\r\nAuthorization: Bearer {ticket}"),
        )
        .await;
        assert_eq!(paired.status_code(), 204);
        let wire = String::from_utf8(paired.encode()).unwrap();
        let token = wire
            .split("bm_session=")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let id = {
            let conn = crate::db::read_conn();
            crate::db::validate_device_token_inner(&conn, token)
                .unwrap()
                .unwrap()
        };
        crate::db::revoke_device_session(id).unwrap();
    }
}

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
                        crate::db::touch_device_session_inner(&conn, id, Some(&peer_ip))
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
    if let Some(denied) =
        auth::deny_response(auth::authorize(&req.headers, auth::RequiredScope::Admin))
    {
        return denied;
    }
    let target = match ws_ticket::parse_mint_target(&req.body) {
        Ok(t) => t,
        Err(()) => return Response::empty("400 Bad Request"),
    };
    let device_id = match auth::resolve_device_session_result(&req.headers) {
        Ok(id) => id,
        Err(()) => return Response::empty("503 Service Unavailable"),
    };
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
