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
#[path = "read_status_tests.rs"]
mod tests;
