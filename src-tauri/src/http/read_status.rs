//! The single HTTP transport policy for a failed authoritative read.
//!
//! Issue #2330 exists because each route invented its own answer for "the read
//! did not produce a value", and the cheapest invention was the same one
//! everywhere: return the empty success shape. A phone polling a populated
//! Work list while a query failed was handed `200 []` and presented an empty
//! workspace; a Coordinator scanning the same moment believed there were no
//! agents.
//!
//! This module owns the status for a failed read, so a route does not re-derive
//! it per handler:
//!
//! | read outcome | status | why |
//! |---|---|---|
//! | `Absent` | `404 Not Found` | the resource genuinely does not exist |
//! | `Unavailable` | `503 Service Unavailable` | transient; a retry may succeed |
//! | serialization failure | `500 Internal Server Error` | a bug in this build |
//!
//! `503` for an unavailable read is what lets a client tell "come back in a
//! moment" from "this will never work", and — more importantly — from
//! *success*. A client that receives `503` must keep its last-known data; one
//! that receives `200 []` has been told, falsely, that nothing exists.
//!
//! Resource unavailability must never be reported as `401`/`403`: the mobile
//! client treats those as a revoked credential and bounces the user to the
//! pairing screen (see `isAuthError` in `src/mobile/api.ts`).

use crate::http::response::Response;
use crate::read_outcome::Unavailable;

/// The status line for a read that did not yield a value.
pub fn read_status(failure: &Unavailable) -> &'static str {
    match failure {
        Unavailable::Absent => "404 Not Found",
        Unavailable::Read(_) => "503 Service Unavailable",
    }
}

/// Map a failed read onto its response.
///
/// `absent_message` is the caller's own wording for a missing resource
/// (`"Mesh not found"`, `"Node not found"`), because absence is a normal
/// answer that routes have always phrased in their own vocabulary. An
/// unavailable read carries the concrete cause instead: it is a fault, and
/// hiding it behind a generic message would cost the diagnostics this type
/// exists to preserve.
pub fn read_response(failure: &Unavailable, absent_message: &str) -> Response {
    match failure {
        Unavailable::Absent => Response::json_error(read_status(failure), absent_message),
        Unavailable::Read(cause) => Response::json_error(
            read_status(failure),
            &format!("Failed to read authoritative state: {cause}"),
        ),
    }
}

/// Serialize a successful body, or report the failure.
///
/// This replaces the `serde_json::to_string(x).unwrap_or_else(|_| "[]")`
/// pattern, which manufactured a *successful* empty body from a serialization
/// fault — a second way for a client to read "the desktop has no nodes" out of
/// an answer that never carried any. There is no fallback value: a body that
/// cannot be encoded is a `500`.
pub fn serialized<T: serde::Serialize>(
    status: &'static str,
    value: &T,
) -> Result<Response, String> {
    match serde_json::to_string(value) {
        Ok(body) => Ok(Response::json(status, body)),
        Err(error) => Err(format!("Failed to encode response body: {error}")),
    }
}

/// [`serialized`] for a handler that has no other failure to report.
///
/// The encoding failure *is* the whole answer here, so it becomes the response
/// instead of a value for the caller to unwrap. Every route that serializes a
/// successful body uses this, so the "500 on an unencodable body" rule is
/// stated once rather than repeated at each call site.
pub fn serialized_or_500<T: serde::Serialize>(status: &'static str, value: &T) -> Response {
    serialized(status, value)
        .unwrap_or_else(|error| Response::json_error("500 Internal Server Error", &error))
}

/// The response for a database failure that reached an HTTP route.
///
/// A busy reader pool is **transient** (issue #1533): the request could not get
/// a connection, which says nothing about the data. The answer is therefore
/// `503`, not the `500` a generic internal failure gets — and never a `401`,
/// which the mobile client reads as a revoked credential and answers by
/// bouncing the user to the pairing screen.
pub fn db_failure_response(error: &rusqlite::Error) -> Response {
    if crate::db::error::is_busy_error(error) {
        Response::json_error(
            read_status(&Unavailable::Read(error.to_string())),
            &format!("Failed to read authoritative state: {error}"),
        )
    } else {
        Response::json_error("500 Internal Server Error", &error.to_string())
    }
}

/// The response for a database failure that reached an HTTP route as a
/// [`crate::db::DbError`].
///
/// The same policy as [`db_failure_response`], for the callers that carry the
/// failure in the database layer's own error type.
pub fn db_error_response(error: &crate::db::DbError) -> Response {
    if error.is_busy() {
        Response::json_error(
            read_status(&Unavailable::Read(error.to_string())),
            &format!("Failed to read authoritative state: {error}"),
        )
    } else {
        Response::json_error("500 Internal Server Error", &error.to_string())
    }
}

#[cfg(test)]
mod tests {
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
}
