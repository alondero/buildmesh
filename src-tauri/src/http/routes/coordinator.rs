//! Coordinator read API routes (ADR-0008). Thin skins over the
//! `coordinator::node_digest` module — all the read-model logic lives there so
//! these handlers stay a transport adapter.
//!
//! This module exposes the read routes — `GET /nodes` (layered digests: spine +
//! transcript enrichment) and `GET /nodes/{id}/log?tail=N` (raw recent
//! transcript turns) — plus the drive route `POST /nodes/{id}/prompt` (issue
//! #319), which writes a prompt to a live node's PTY and returns an honest
//! verdict. Auth (the off-by-default master switch + the read- or drive-scoped
//! token) is enforced by the dispatcher in `http::router` via
//! `auth::authorize(.., CoordinatorRead | CoordinatorWrite)` (issue #500) before any
//! handler is reached.

use crate::coordinator::{drive, enrichment, node_digest};
use crate::db;
use crate::http::read_status::{read_response, serialized};
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::read_outcome::{ReadOutcome, Unavailable};

/// `GET /nodes` → JSON array of layered Node Digests across every Mesh. Each
/// digest is the always-available spine plus a transcript-derived rich layer;
/// for a provider with no readable transcript the enrichment is explicitly
/// flagged `unsupported` (degrade-and-flag, ADR-0008 §3).
///
/// Issue #2330: this used to answer `200 []` when the read failed, so a
/// Coordinator scanning during a storage fault believed the fleet was empty
/// and could schedule against that. A failed read is now a `503`: an
/// authoritative inventory may not be invented. Empty stays reserved for a
/// successful query that matched zero rows.
pub async fn list(_req: &ParsedRequest) -> Response {
    match list_nodes_json() {
        Ok(response) => response,
        // A list read is never "absent" — this message only names the
        // resource the list is *of*, for the (unreachable) absence branch.
        Err(failure) => read_response(&failure, "No nodes found"),
    }
}

pub fn list_nodes_json() -> Result<Response, Unavailable> {
    let rows = ReadOutcome::ok_or_unavailable(db::list_coordinator_node_rows()).into_result()?;
    let digests: Vec<_> = rows
        .iter()
        .map(|(node, mesh, changed)| {
            let tail = enrichment::digest_enrichment(node);
            let mut digest = node_digest::layered(node, mesh, *changed, tail.as_ref());
            digest.observed_session_telemetry = enrichment::observed_session_telemetry(node);
            digest
        })
        .collect();
    serialized("200 OK", &digests).map_err(Unavailable::Read)
}

/// `GET /nodes/{id}/log?tail=N` → the raw recent transcript turns for one node.
/// Only a node id that no row matches is a `404`; a read that *could not be
/// performed* is a `503`. The content degrade paths (unsupported provider, no
/// session, missing/unreadable/shape-changed transcript) are a `200` carrying a
/// typed `{"status":"unavailable",...}` envelope, so the Coordinator gets a
/// structured answer — and that envelope stays distinct from a node whose
/// persistence was unavailable.
pub async fn log(req: &ParsedRequest) -> Response {
    let tail = req.tail_param();
    match log_json(req.id0(), tail) {
        Ok(response) => response,
        Err(failure) => read_response(&failure, "Unknown node"),
    }
}

/// Read one node and render its transcript tail.
///
/// Issue #2330: `db::get_agent_node_by_id(node_id).ok()?` turned *every* lookup
/// failure into `None`, and the dispatcher mapped `None` to 404. So a broken
/// query was indistinguishable from a deleted node — and a Coordinator acting
/// on that 404 would drop a live node from its model.
pub fn log_json(node_id: i64, tail: usize) -> Result<Response, Unavailable> {
    let node: crate::models::AgentNode =
        ReadOutcome::absent_or_unavailable(db::get_agent_node_by_id(node_id)).into_result()?;
    let result = enrichment::transcript_tail(&node, tail);
    serialized("200 OK", &result).map_err(Unavailable::Read)
}

/// The drive request body: `{"prompt": "...", "idempotency_key": "..."}`. Strict
/// (no serde default) so a malformed body — or one missing the key — is a 400,
/// not a silent no-op. The mandatory `idempotency_key` is #178's cardinal rule
/// ("never let a prompt land twice") enforced at the contract: a Coordinator
/// cannot drive without declaring a retry-dedup key (ADR-0008 §6, issue #320).
#[derive(serde::Deserialize)]
struct PromptRequest {
    prompt: String,
    idempotency_key: String,
}

/// Upper bound on an idempotency key, in bytes (storage is bytes). A key is a
/// caller-chosen opaque token (a UUID is ~36 chars); anything past this is a
/// malformed request, capped so a hostile caller can't bloat the ledger row.
const MAX_IDEMPOTENCY_KEY_LEN: usize = 256;

/// Validate and normalize the drive body — a pure function so the 400 branches are
/// unit-tested without standing up the socket (issue #320 review). On success it
/// returns the *trimmed* idempotency key: the empty-check, the length cap, storage
/// and lookup must all see the same bytes, or `" abc"` and `"abc"` would be
/// distinct ledger keys and a retry could double-send. Returns `(status, message)`
/// on rejection.
fn validate_drive_request<'a>(
    prompt: &str,
    idempotency_key: &'a str,
) -> Result<&'a str, (&'static str, &'static str)> {
    if prompt.trim().is_empty() {
        return Err(("400 Bad Request", "Prompt must not be empty"));
    }
    let key = idempotency_key.trim();
    if key.is_empty() {
        return Err(("400 Bad Request", "idempotency_key must not be empty"));
    }
    if key.len() > MAX_IDEMPOTENCY_KEY_LEN {
        return Err(("400 Bad Request", "idempotency_key is too long"));
    }
    Ok(key)
}

/// `POST /nodes/{id}/prompt` — drive a live node by writing `prompt` to its PTY
/// (ADR-0008 §5, issue #319). Auth (the drive-scoped token + drive kill-switch)
/// is enforced by the dispatcher via `auth::authorize(.., CoordinatorWrite)` (issue
/// #500) before this is reached.
///
/// Outcomes:
/// - unknown node id → `404`
/// - empty prompt, empty/oversized idempotency key, or malformed body → `400`
/// - node not live (no agent process to write to) → `409` with a clear error
/// - idempotency ledger unreadable (fail-safe, no send) → `503`, retryable
/// - same key + different prompt → `409 key_payload_mismatch` (issue #750,
///   item 2 — Stripe-style reject, prevents a silent 200 replay)
/// - another caller is currently driving this key → `409 in_progress` with
///   `Retry-After: 1` (issue #750, item 1 — atomic claim-before-send)
/// - written → `200 {"verdict":..,"idempotency_key":..,"replayed":..}`
/// - duplicate key → `200` replaying the original verdict, no second write
pub async fn prompt(req: &ParsedRequest) -> Response {
    let node_id = req.id0();

    let parsed: PromptRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };
    let idempotency_key = match validate_drive_request(&parsed.prompt, &parsed.idempotency_key) {
        Ok(key) => key.to_string(),
        Err((status, message)) => {
            return Response::json_error(status, message);
        }
    };

    // The idempotency replay lives *inside* `drive_node_with_key_async` and
    // short-circuits before any liveness check, so a duplicate key is a clean
    // no-op even for a since-vanished node. Only a genuinely fresh key that then
    // fails to find a node surfaces as `NotLive`, which we split into 404
    // (unknown) vs 409 (known but not drivable).
    //
    // `_async` runs that fully-sync path on the blocking pool (issue #1234): the
    // `InProgress` wait loop sleeps for up to 5s, which would otherwise pin a
    // tokio worker and stall every other route sharing the pool.
    match drive::drive_node_with_key_async(
        node_id,
        idempotency_key.clone(),
        parsed.prompt.clone(),
    )
    .await
    {
        Ok(outcome) => {
            let body = serde_json::json!({
                "verdict": outcome.verdict,
                // Echo the *normalized* key — the value actually recorded, so the
                // caller learns exactly what a retry must send to dedup.
                "idempotency_key": idempotency_key,
                "replayed": outcome.replayed,
            })
            .to_string();
            Response::json("200 OK", body)
        }
        Err(drive::DriveError::NotLive) => {
            // Issue #2330: `is_err()` answered 404 for every failure. A node
            // that exists but whose row could not be read must not be reported
            // as deleted — the Coordinator would drop it from its model.
            let lookup =
                ReadOutcome::<crate::models::AgentNode>::absent_or_unavailable(db::get_agent_node_by_id(node_id));
            match lookup {
                ReadOutcome::Present(_) => Response::json_error(
                    "409 Conflict",
                    "Node is not live — only a node with a running agent can be driven",
                ),
                _ => read_response(
                    &lookup
                        .failure()
                        .expect("a non-Present outcome has a failure"),
                    "Unknown node",
                ),
            }
        }
        Err(drive::DriveError::WriteFailed(e)) => {
            Response::json_error("500 Internal Server Error", &e)
        }
        // Issue #1530: the node's PTY input queue refused the prompt. The
        // prompt was never queued, so the ledger claim was released and a
        // retry with the same key is safe. 503 + `Retry-After` matches the
        // "transient and safe to retry" precedent below.
        Err(drive::DriveError::Backpressured) => {
            Response::json(
                "503 Service Unavailable",
                r#"{"error":"input_backpressured: the agent is not reading its input; retry with the same idempotency_key"}"#,
            )
            .with_header("Retry-After", "1")
        }
        // The ledger couldn't be consulted, so we refused to risk a double-send.
        // 503 tells the Coordinator this is transient and safe to retry.
        Err(drive::DriveError::LedgerUnavailable(e)) => {
            Response::json_error("503 Service Unavailable", &e)
        }
        // Same idempotency key + *different* prompt payload — Stripe-style
        // reject (issue #750, item 2). 409 is the right shape: the request
        // is well-formed and authenticated, but the resource state
        // (existing row with a different payload) forbids the action. The
        // Coordinator must mint a fresh key rather than silently drop the
        // new prompt.
        Err(drive::DriveError::KeyPayloadMismatch) => Response::json_error(
            "409 Conflict",
            "key_payload_mismatch: same idempotency_key with a different prompt — mint a fresh key",
        ),
        // A peer holds the claim in `pending` and the orchestrator's brief
        // wait window expired (issue #750, item 1). 409 + `Retry-After: 1`
        // tells the Coordinator to back off briefly and try again — the
        // peer should have finalized by then (a successful drive is
        // sub-millisecond; a stuck peer hits the orphan-recovery pass within
        // `PENDING_CLAIM_TIMEOUT_SECS` = 30s).
        Err(drive::DriveError::InProgress) => {
            // Body uniformity with the other 4xx shapes — `{"error":"..."}`.
            // The status line + `Retry-After` header is the load-bearing
            // signal; the body is purely informational.
            Response::json(
                "409 Conflict",
                r#"{"error":"in_progress: another caller is currently driving this key"}"#,
            )
            .with_header("Retry-After", "1")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_drive_request, MAX_IDEMPOTENCY_KEY_LEN};

    #[test]
    fn rejects_empty_or_whitespace_prompt() {
        assert_eq!(
            validate_drive_request("", "k").unwrap_err().0,
            "400 Bad Request"
        );
        assert_eq!(
            validate_drive_request("   ", "k").unwrap_err().1,
            "Prompt must not be empty"
        );
    }

    #[test]
    fn rejects_empty_or_whitespace_key() {
        assert_eq!(
            validate_drive_request("hi", "").unwrap_err().1,
            "idempotency_key must not be empty"
        );
        // A whitespace-only key trims to empty and is rejected — not stored as a
        // blank key that every future blank-key retry would collide on.
        assert_eq!(
            validate_drive_request("hi", "  \t ").unwrap_err().1,
            "idempotency_key must not be empty"
        );
    }

    #[test]
    fn rejects_oversized_key() {
        let big = "x".repeat(MAX_IDEMPOTENCY_KEY_LEN + 1);
        assert_eq!(
            validate_drive_request("hi", &big).unwrap_err().1,
            "idempotency_key is too long"
        );
    }

    #[test]
    fn normalizes_by_trimming_surrounding_whitespace() {
        // The headline reason to normalize: `" abc "` and `"abc"` must resolve to
        // the SAME stored key, or a retry whose key gained/lost whitespace would
        // dedup-miss and double-send.
        assert_eq!(validate_drive_request("hi", " abc ").unwrap(), "abc");
        assert_eq!(validate_drive_request("hi", "abc").unwrap(), "abc");
    }

    #[test]
    fn accepts_a_normal_key_at_the_length_boundary() {
        let max = "x".repeat(MAX_IDEMPOTENCY_KEY_LEN);
        assert_eq!(validate_drive_request("hi", &max).unwrap(), max);
    }

    // --- Issue #2330: a Coordinator must not schedule against an invented fleet ---

    /// A successful query against an empty database stays a successful empty
    /// array: "no agents" is a legitimate answer a Coordinator may act on.
    #[test]
    fn an_empty_database_is_still_a_successful_empty_array() {
        let _db = crate::db::test_support::isolated();
        let resp = super::list_nodes_json().expect("an empty read is a success");
        assert_eq!(resp.status, "200 OK");
        assert_eq!(String::from_utf8_lossy(resp.body()), "[]");
    }

    /// The pre-fix handler documented itself as degrading a failed read to "no
    /// nodes" so a Coordinator scan would not 500. The consequence was worse
    /// than the 500 it avoided: the Coordinator believed the fleet was empty
    /// and could make scheduling decisions from that.
    #[test]
    fn a_failed_query_is_unavailable_not_an_empty_fleet() {
        let _db = crate::db::test_support::isolated();
        crate::db::write_conn()
            .execute_batch("DROP TABLE agent_nodes;")
            .unwrap();

        match super::list_nodes_json() {
            Ok(resp) => panic!(
                "a failed inventory read must not answer a successful empty fleet: {} {:?}",
                resp.status,
                String::from_utf8_lossy(resp.body())
            ),
            Err(failure) => assert_eq!(
                crate::http::read_status::read_status(&failure),
                "503 Service Unavailable"
            ),
        }
    }

    /// `log_json` was `.ok()?`, so *every* lookup failure became a 404 and the
    /// dispatcher answered "not found". A Coordinator acting on that would drop
    /// a live node from its model.
    #[test]
    fn an_unreadable_node_is_not_reported_as_a_deleted_one() {
        let _db = crate::db::test_support::isolated();
        crate::db::write_conn()
            .execute_batch("DROP TABLE agent_nodes;")
            .unwrap();

        match super::log_json(1, 5) {
            Ok(resp) => panic!(
                "an unreadable node must not answer as a transcript: {} {:?}",
                resp.status,
                String::from_utf8_lossy(resp.body())
            ),
            Err(failure) => assert_eq!(
                crate::http::read_status::read_status(&failure),
                "503 Service Unavailable"
            ),
        }
    }

    /// A genuinely unknown node id *is* absence — the one case that is a 404.
    #[test]
    fn an_unknown_node_is_still_a_404() {
        let _db = crate::db::test_support::isolated();
        match super::log_json(987_654_321, 5) {
            Ok(_) => panic!("an unknown node id must not resolve to a transcript"),
            Err(failure) => assert_eq!(
                crate::http::read_status::read_status(&failure),
                "404 Not Found"
            ),
        }
    }

    /// The degrade-and-flag contract of ADR-0008 §3 survives: an unsupported
    /// transcript is a `200` carrying a typed envelope, and must stay distinct
    /// from a node whose persistence was unavailable. A Terminal node has no
    /// readable transcript at all, so it exercises exactly that path.
    #[test]
    fn an_unreadable_transcript_stays_a_200_unavailable_envelope() {
        let _db = crate::db::test_support::isolated();
        let mesh = crate::db::create_mesh("demo", "/tmp/bm-2330-coord").unwrap();
        let node = crate::db::create_agent_node(crate::db::NewAgentNode {
            mesh_id: mesh.id,
            name: "agent-1",
            path: "/tmp/bm-2330-coord",
            branch: "main",
            env: crate::models::EnvType::Windows,
            provider: "terminal",
            worktree_name: None,
            source_issue: None,
            source_pr: None,
            source_pr_pinned_sha: None,
            use_worktree: false,
            head_repo_owner: None,
            head_repo_clone_url: None,
            worktree_path: None,
            spawn_configuration: None,
        })
        .unwrap();

        let resp = super::log_json(node.id, 5).expect("the node resolved, so this is not absence");
        assert_eq!(
            resp.status, "200 OK",
            "a missing transcript is content degradation, not a storage fault"
        );
        let body = String::from_utf8_lossy(resp.body());
        assert!(
            body.contains("unavailable"),
            "expected the typed unavailable envelope; got: {body:?}"
        );
    }
}
