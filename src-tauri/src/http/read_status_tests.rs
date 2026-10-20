use super::*;
use crate::read_outcome::ReadOutcome;

fn body(resp: &Response) -> String {
    String::from_utf8_lossy(resp.body()).to_string()
}

#[test]
fn an_absent_resource_is_404_with_the_callers_wording() {
    let resp = read_response(&Unavailable::Absent, "Mesh not found");
    assert_eq!(resp.status, "404 Not Found");
    assert!(body(&resp).contains("Mesh not found"));
}

#[test]
fn an_unavailable_read_is_503_and_keeps_the_cause() {
    let failure = Unavailable::Read("no such table: agent_nodes".into());
    let resp = read_response(&failure, "Node not found");
    assert_eq!(
        resp.status, "503 Service Unavailable",
        "unavailability must be retryable, never a success or a 404"
    );
    // The cause survives to the wire for diagnostics.
    assert!(
        body(&resp).contains("no such table: agent_nodes"),
        "{:?}",
        body(&resp)
    );
    assert!(
        !body(&resp).contains("Node not found"),
        "an unavailable read must not borrow the absent-resource wording"
    );
}

#[test]
fn an_unavailable_read_is_never_reported_as_an_auth_failure() {
    // The mobile client clears its token on 401/403 and bounces the user
    // to the pairing screen. A broken query must never revoke pairing.
    for failure in [
        Unavailable::Absent,
        Unavailable::Read("disk I/O error".into()),
    ] {
        let status = read_status(&failure);
        assert!(
            !status.starts_with("401") && !status.starts_with("403"),
            "{status} would log the user out"
        );
    }
}

#[test]
fn an_empty_successful_query_still_encodes_an_empty_array() {
    let resp = serialized("200 OK", &Vec::<i64>::new()).expect("empty array encodes");
    assert_eq!(resp.status, "200 OK");
    assert_eq!(body(&resp), "[]");
}

/// Issue #1533: a busy reader pool is a *transient* failure, so the HTTP
/// answer is `503` — not the `500` a genuinely broken query gets, and
/// never a `401`, which the mobile client would answer by revoking the
/// user's pairing.
#[test]
fn a_busy_database_is_503_and_a_broken_query_is_500() {
    let busy = crate::db::error::busy_error();
    assert!(
        crate::db::error::is_busy_error(&busy),
        "the pool's busy error must be recognisable as busy"
    );
    let response = db_failure_response(&busy);
    assert_eq!(
        response.status, "503 Service Unavailable",
        "a busy database is retryable, not a broken build"
    );
    assert!(
        body(&response).contains(crate::db::READER_POOL_BUSY_MESSAGE),
        "the busy copy must reach the client: {:?}",
        body(&response)
    );

    let broken = rusqlite::Error::InvalidQuery;
    assert_eq!(
        db_failure_response(&broken).status,
        "500 Internal Server Error",
        "a broken query is a fault in this build, not a transient shortage"
    );
}

/// The specific false-success this replaces: a body that could not be
/// encoded used to become `[]` under a `200`.
#[test]
fn a_body_that_cannot_encode_is_an_error_not_an_empty_success() {
    // A map keyed by a tuple is not a JSON object: serde_json rejects the
    // key type outright. (A bare `f64::NAN` would *not* do — serde_json
    // encodes it as `null`, so the value serializes fine.)
    #[derive(serde::Serialize)]
    struct Unrepresentable {
        keys: std::collections::HashMap<(u8, u8), u8>,
    }
    let value = Unrepresentable {
        keys: std::collections::HashMap::from([((1, 2), 3)]),
    };

    let err = serialized("200 OK", &value).expect_err("this cannot be encoded");
    assert!(err.contains("Failed to encode response body"), "{err}");
}

#[test]
fn the_status_is_the_only_thing_a_client_needs_to_branch_on() {
    // A caller that reads nothing but the status still cannot confuse the
    // three outcomes — the vision for completion in issue #2330.
    assert_eq!(read_status(&Unavailable::Absent), "404 Not Found");
    assert_eq!(
        read_status(&Unavailable::Read("boom".into())),
        "503 Service Unavailable"
    );
    let present: ReadOutcome<Vec<i64>> = ReadOutcome::Present(vec![]);
    assert!(present.as_present().is_some_and(|v| v.is_empty()));
}
