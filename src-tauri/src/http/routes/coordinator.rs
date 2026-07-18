//! Coordinator read API routes (ADR-0008). Thin skins over the
//! `coordinator::node_digest` module — all the read-model logic lives there so
//! these handlers stay a transport adapter.
//!
//! This module exposes the read routes — `GET /nodes` (layered digests: spine +
//! transcript enrichment) and `GET /nodes/{id}/log?tail=N` (raw recent
//! transcript turns) — plus the drive route `POST /nodes/{id}/prompt` (issue
//! #319), which writes a prompt to a live node's PTY and returns an honest
//! verdict. Auth (the off-by-default master switch + the read- or drive-scoped
//! token) is enforced by the dispatcher in `http::mod` via
//! `auth::guard(.., CoordinatorRead | CoordinatorWrite)` (issue #500) before any
//! handler is reached.

use crate::coordinator::{drive, enrichment, node_digest};
use crate::db;
use crate::http::request;
use tokio::io::{AsyncReadExt, BufStream};
use crate::http::MaybeTls;

/// `GET /nodes` → JSON array of layered Node Digests across every Mesh. Each
/// digest is the always-available spine plus a transcript-derived rich layer;
/// for a provider with no readable transcript the enrichment is explicitly
/// flagged `unsupported` (degrade-and-flag, ADR-0008 §3). Returns `[]` rather
/// than erroring if the DB read fails, so a Coordinator scan degrades to "no
/// nodes" instead of a 500.
pub fn list_nodes_json() -> String {
    match db::list_coordinator_node_rows() {
        Ok(rows) => {
            let digests: Vec<_> = rows
                .iter()
                .map(|(node, mesh, changed)| {
                    let tail = enrichment::digest_enrichment(node);
                    node_digest::layered(node, mesh, *changed, tail.as_ref())
                })
                .collect();
            serde_json::to_string(&digests).unwrap_or_else(|_| "[]".to_string())
        }
        Err(_) => "[]".to_string(),
    }
}

/// `GET /nodes/{id}/log?tail=N` → the raw recent transcript turns for one node.
/// Returns `None` (→ 404 in the dispatcher) only when the node id is unknown;
/// every other degrade path (unsupported provider, no session,
/// missing/unreadable/shape-changed transcript) is a `200` carrying a typed
/// `{"status":"unavailable",...}` envelope, so the Coordinator always gets a
/// structured answer.
pub fn log_json(node_id: i64, tail: usize) -> Option<String> {
    let node = db::get_agent_node_by_id(node_id).ok()?;
    let result = enrichment::transcript_tail(&node, tail);
    Some(serde_json::to_string(&result).unwrap_or_else(|_| {
        // A serialization failure still answers structurally rather than 500ing.
        "{\"status\":\"unavailable\",\"reason\":\"unreadable\"}".to_string()
    }))
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
/// is enforced by the dispatcher via `auth::guard(.., CoordinatorWrite)` (issue
/// #500) before this is reached.
///
/// Outcomes:
/// - unknown node id → `404`
/// - empty prompt, empty/oversized idempotency key, or malformed body → `400`
/// - node not live (no agent process to write to) → `409` with a clear error
/// - idempotency ledger unreadable (fail-safe, no send) → `503`, retryable
/// - written → `200 {"verdict":..,"idempotency_key":..,"replayed":..}`
/// - duplicate key → `200` replaying the original verdict, no second write
pub async fn prompt(
    lines: &mut BufStream<MaybeTls>,
    node_id: i64,
    content_length: usize,
) {
    let Some(body_bytes) =
        request::read_body_or_send_error(lines, content_length, 256 * 1024).await
    else {
        return;
    };

    let req: PromptRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            request::send_json_error(lines, "400 Bad Request", &format!("Invalid JSON: {}", e))
                .await;
            return;
        }
    };
    let idempotency_key = match validate_drive_request(&req.prompt, &req.idempotency_key) {
        Ok(key) => key,
        Err((status, message)) => {
            request::send_json_error(lines, status, message).await;
            return;
        }
    };

    // The idempotency replay lives *inside* `drive_node_with_key` and
    // short-circuits before any liveness check, so a duplicate key is a clean
    // no-op even for a since-vanished node. Only a genuinely fresh key that then
    // fails to find a node surfaces as `NotLive`, which we split into 404
    // (unknown) vs 409 (known but not drivable).
    match drive::drive_node_with_key(node_id, idempotency_key, &req.prompt) {
        Ok(outcome) => {
            let body = serde_json::json!({
                "verdict": outcome.verdict,
                // Echo the *normalized* key — the value actually recorded, so the
                // caller learns exactly what a retry must send to dedup.
                "idempotency_key": idempotency_key,
                "replayed": outcome.replayed,
            })
            .to_string();
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(drive::DriveError::NotLive) => {
            if db::get_agent_node_by_id(node_id).is_err() {
                request::send_json_error(lines, "404 Not Found", "Unknown node").await;
            } else {
                request::send_json_error(
                    lines,
                    "409 Conflict",
                    "Node is not live — only a node with a running agent can be driven",
                )
                .await;
            }
        }
        Err(drive::DriveError::WriteFailed(e)) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
        // The ledger couldn't be consulted, so we refused to risk a double-send.
        // 503 tells the Coordinator this is transient and safe to retry.
        Err(drive::DriveError::LedgerUnavailable(e)) => {
            request::send_json_error(lines, "503 Service Unavailable", &e).await;
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
}

