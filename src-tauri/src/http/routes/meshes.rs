//! `GET /api/meshes` — list all meshes.

use crate::db;
use crate::http::read_status::{read_response, serialized};
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::read_outcome::ReadOutcome;

pub async fn list(_req: &ParsedRequest) -> Response {
    match list_json().await {
        Ok(response) => response,
        // A list read is never "absent" — this message only names the
        // resource the list is *of*, for the (unreachable) absence branch.
        Err(failure) => read_response(&failure, "No meshes found"),
    }
}

/// The mesh inventory as a JSON body.
///
/// Issue #2330: this used to end in `Err(_) => "[]"`, so a failed query and a
/// genuinely empty `meshes` table were the same `200 []` on the wire. The
/// inventory is authoritative — a phone reads it to decide what exists — so an
/// empty array now means "a successful query found zero rows", and a failed
/// read is a `503`.
pub async fn list_json() -> Result<Response, crate::read_outcome::Unavailable> {
    let meshes: Vec<crate::models::Mesh> = crate::commands::run_blocking_typed::<
        Vec<crate::models::Mesh>,
        crate::read_outcome::Unavailable,
        _,
    >("http_list_meshes", || {
        ReadOutcome::ok_or_unavailable(db::list_meshes()).into_result()
    })
    .await?;

    serialized("200 OK", &meshes).map_err(crate::read_outcome::Unavailable::Read)
}

#[cfg(test)]
mod tests {
    //! Issue #2330 — the route's whole contract is which *shape* each outcome
    //! takes. These drive the real handler against a private database so a
    //! broken query is a real SQLite failure, not a mocked one.
    use super::*;
    use crate::http::router::ParsedRequest;

    async fn drive() -> Response {
        list(&ParsedRequest::test_get("/api/meshes")).await
    }

    fn body(resp: &Response) -> String {
        String::from_utf8_lossy(resp.body()).to_string()
    }

    /// The headline acceptance check: a successful query against an empty
    /// database is still a `200 []`. If this ever fails, "no meshes" has
    /// stopped being a thing the client can observe.
    #[tokio::test]
    async fn an_empty_database_is_still_a_successful_empty_array() {
        let _db = crate::db::test_support::isolated();
        let resp = drive().await;
        assert_eq!(resp.status, "200 OK");
        assert_eq!(body(&resp), "[]");
    }

    /// Seeded rows come back as themselves — the success path is not broken by
    /// the new failure handling.
    #[tokio::test]
    async fn a_populated_database_returns_its_meshes() {
        let _db = crate::db::test_support::isolated();
        crate::db::create_mesh("alpha", "/tmp/bm-2330-alpha").unwrap();
        let resp = drive().await;
        assert_eq!(resp.status, "200 OK");
        let parsed: serde_json::Value = serde_json::from_str(&body(&resp)).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    /// A broken query must not answer `200 []`. The route reads on the
    /// blocking pool, so the fault is injected into *this test's* isolated
    /// database — `blocking::adopt_test_db` carries the install onto the pool
    /// thread. The process-global schema is never touched.
    #[tokio::test]
    async fn a_failed_query_is_503_and_never_an_empty_success() {
        let _db = crate::db::test_support::isolated();
        // A populated table first, so a passing test cannot be explained by
        // "there was nothing to lose".
        crate::db::create_mesh("alpha", "/tmp/bm-2330-fault").unwrap();
        // Now break the query this route depends on.
        crate::db::write_conn()
            .execute_batch("DROP TABLE meshes;")
            .unwrap();

        let resp = drive().await;
        assert_ne!(
            resp.status,
            "200 OK",
            "a failed query must never answer a successful empty array: {:?}",
            body(&resp)
        );
        assert_eq!(
            resp.status, "503 Service Unavailable",
            "unavailable persistence is retryable, not a missing mesh"
        );
        assert!(
            !body(&resp).contains("[]"),
            "the failure body must not read as an empty inventory: {:?}",
            body(&resp)
        );
    }
}
