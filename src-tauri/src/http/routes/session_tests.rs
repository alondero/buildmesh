//! Unit tests for `session`.
//!
//! The suite lives in this sibling file (not inline in the owner) so the
//! owner carries no test bodies — see `docs/development/rust-conventions.md`.

use super::*;
use crate::http::router::{dispatch, DispatchResult};

/// Serialises this module's tests against the process-global database.
///
/// These handlers reach the database from a Tauri-managed thread
/// (`commands::run_blocking` / `tauri::async_runtime::spawn`), where the
/// `thread_local!` install behind `test_support::isolated` is invisible and
/// `adopt` cannot bridge a runtime the test does not own. The lock itself
/// lives in `db::test_support` so this module and `http::server` cannot
/// drift into two locks guarding one shared database.
fn shared_db() -> crate::db::test_support::SharedGlobalDb {
    crate::db::test_support::shared_global_db()
}

async fn send(path: &str, headers: &str) -> Response {
    send_with_body(path, headers, b"").await
}

async fn send_with_body(path: &str, headers: &str, body: &[u8]) -> Response {
    let mut req = ParsedRequest::test_post(path, body);
    req.secure = true;
    req.headers = format!("Host: localhost\r\n{headers}\r\n");
    match dispatch(req).await {
        DispatchResult::Http(response) => response,
        _ => panic!("unexpected upgrade"),
    }
}

#[tokio::test]
// The guard must span the whole test: it is what keeps these tests from
// sharing the process-global database concurrently, and the handlers
// below await while it is held. Holding a lock across an await is the
// intent here, not an oversight.
#[allow(clippy::await_holding_lock)]
async fn pairing_refresh_and_revocation_cross_the_real_router() {
    let _serial = shared_db();
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
        let conn = crate::db::read_conn().unwrap();
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
        let conn = crate::db::read_conn().unwrap();
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
// The guard must span the whole test: it is what keeps these tests from
// sharing the process-global database concurrently, and the handlers
// below await while it is held. Holding a lock across an await is the
// intent here, not an oversight.
#[allow(clippy::await_holding_lock)]
async fn an_expired_device_session_can_no_longer_refresh() {
    // Issue #589: the expiry sweep deletes a device session that has not
    // authenticated for the idle window, and the swept token stops
    // working — the orphan must not stay a live Admin credential.
    let _serial = shared_db();
    let invitation = crate::http::pairing::mint();
    let paired = send("/api/pair", &format!("Authorization: Bearer {invitation}")).await;
    assert_eq!(paired.status_code(), 204);
    let wire = String::from_utf8(paired.encode()).unwrap();
    let token = wire
        .split("bm_session=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let id = {
        let conn = crate::db::read_conn().unwrap();
        crate::db::validate_device_token_inner(&conn, &token)
            .unwrap()
            .unwrap()
    };
    {
        let conn = crate::db::write_conn();
        conn.execute(
            "UPDATE device_sessions SET last_active_at = datetime('now', '-400 days') WHERE id = ?1",
            [id],
        )
        .unwrap();
    }
    crate::services::device_session_expiry::sweep_once().unwrap();

    assert_eq!(
        send("/api/session", &format!("Authorization: Bearer {token}"))
            .await
            .status_code(),
        401,
        "a swept device session must not refresh itself"
    );
}

#[tokio::test]
// The guard must span the whole test: it is what keeps these tests from
// sharing the process-global database concurrently, and the handlers
// below await while it is held. Holding a lock across an await is the
// intent here, not an oversight.
#[allow(clippy::await_holding_lock)]
async fn a_minted_ticket_is_bound_to_the_device_that_presented_the_credential() {
    // Issue #589: the mint resolves the role and the device id in ONE
    // credential walk, so the ticket it binds is the device the request
    // was authorized as — never a different one.
    let _serial = shared_db();
    let invitation = crate::http::pairing::mint();
    let paired = send("/api/pair", &format!("Authorization: Bearer {invitation}")).await;
    assert_eq!(paired.status_code(), 204);
    let wire = String::from_utf8(paired.encode()).unwrap();
    let token = wire
        .split("bm_session=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let id = {
        let conn = crate::db::read_conn().unwrap();
        crate::db::validate_device_token_inner(&conn, &token)
            .unwrap()
            .unwrap()
    };

    let minted = send_with_body(
        "/api/ws-ticket",
        &format!("Cookie: bm_session={token}"),
        br#"{"surface":"events","node_id":null}"#,
    )
    .await;
    assert_eq!(minted.status_code(), 200);
    let body = String::from_utf8(minted.body().to_vec()).unwrap();
    let ticket: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ticket = ticket["ticket"].as_str().unwrap().to_string();
    assert_eq!(
        crate::http::ws_ticket::consume(
            &ticket,
            &crate::http::ws_ticket::WsTarget {
                surface: "events".into(),
                node_id: None,
            }
        ),
        crate::http::ws_ticket::ConsumeOutcome::Ok(Some(id)),
        "the ticket must be bound to the device that presented the credential"
    );

    crate::db::revoke_device_session(id).unwrap();
}

#[tokio::test]
// The guard must span the whole test: it is what keeps these tests from
// sharing the process-global database concurrently, and the handlers
// below await while it is held. Holding a lock across an await is the
// intent here, not an oversight.
#[allow(clippy::await_holding_lock)]
async fn cross_origin_pairing_is_rejected_before_consuming_the_invitation() {
    let _serial = shared_db();
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
        let conn = crate::db::read_conn().unwrap();
        crate::db::validate_device_token_inner(&conn, token)
            .unwrap()
            .unwrap()
    };
    crate::db::revoke_device_session(id).unwrap();
}
