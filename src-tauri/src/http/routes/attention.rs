//! `POST /api/attention/{session_id}` — webhook for a Node Turn (see CONTEXT.md
//! and `crate::node_turn`). Both Claude Code hooks point here — the Stop hook
//! (turn finished) and the catch-all Notification hook (idle or permission
//! prompt) — as do Codex's SessionStart, Stop, and PermissionRequest hooks
//! (issue #884). SessionStart is capture-only (no Ready/AwaitingInput).
//! The hook command forwards the hook's stdin JSON as the POST body
//! (issue #878), which is what lets this handler tell a genuine yield from a
//! turn that only ended because the harness is waiting on background tasks and
//! will re-invoke itself — those must NOT mark the node as awaiting input.
//!
//! No token required: the hook is configured locally and runs over localhost.
//! Because it is unauthenticated, the handler verifies the client peer address
//! is loopback (issue #496 / ADR-0012) — an external machine cannot spoof
//! attention events even if it can reach the port.

use std::net::SocketAddr;
use std::path::Path;

use crate::http::MaybeTls;
use crate::agent::session_lifecycle::{SemanticTurnKind, SemanticTurnPayload};

use crate::http::request;

/// Hook payloads are small (a handful of metadata fields); 64 KB is far above
/// any real payload while still bounding what a local process can make us
/// buffer.
const MAX_HOOK_BODY: usize = 64 * 1024;

/// The fields of agent harness hook stdin JSON this route cares about. Unknown
/// fields are ignored; every field is optional so an empty or legacy body
/// (`{}` or nothing at all) degrades to the pre-#878 behaviour of always
/// marking attention.
///
/// Grok Code's HTTP hook (`~/.grok/docs/user-guide/10-hooks.md`, issue
/// #1282) POSTs the same envelope shape but with camelCase top-level
/// keys (`sessionId`, `hookEventName`) — and adds a separate
/// `notificationType` field on `Notification` events (`idle_prompt`,
/// `permission_prompt`, `task_complete`, …). The `#[serde(alias)]`
/// attributes accept either casing so the same parser handles Claude
/// and Grok payloads.
///
/// Antigravity (agy, issue #1285, #1367) sends the same payload shape in
/// camelCase too, but uses different key names than Grok:
/// `conversationId` (not `sessionId`), `transcriptPath`, `fullyIdle`,
/// `terminationReason`, `workspacePaths`, `artifactDirectoryPath`,
/// `modelName`, `executionNum`, `error`. All aliases accept both camelCase
/// and snake_case forms; a payload that mixes casings also parses.
///
/// Note: fields like `execution_num`, `workspace_paths`, `artifact_directory_path`,
/// `model_name`, and `error` are parsed for telemetry, diagnostics, and forward
/// compatibility with future AGY revisions, but are not decision inputs in `decide()`.
#[derive(serde::Deserialize, Default, Debug, Clone, PartialEq, Eq)]
struct HookPayload {
    #[serde(alias = "sessionId", alias = "conversationId")]
    session_id: Option<String>,
    #[serde(alias = "hookEventName", alias = "hook_event_name")]
    hook_event_name: Option<String>,
    #[serde(alias = "transcriptPath", alias = "transcript_path")]
    transcript_path: Option<String>,
    /// Grok's structured notification type on `Notification` events
    /// (`permission_prompt`, `idle_prompt`, `task_complete`, …). The
    /// Grok docs note the matcher tests this field — we use it as a
    /// belt-and-braces parallel to Claude's `message`-substring check.
    /// Accepts both the wire camelCase (`notificationType`) and the
    /// grok-agent-sdk snake_case (`notification_type`).
    #[serde(alias = "notificationType", alias = "notification_type")]
    notification_type: Option<String>,
    /// Notification hooks carry the human-readable notification text, e.g.
    /// "Claude needs your permission to use Bash".
    message: Option<String>,
    /// Tool metadata used by Claude/Codex permission callbacks.
    #[serde(alias = "toolName", alias = "tool_name")]
    tool_name: Option<String>,
    #[serde(alias = "toolInput", alias = "tool_input")]
    tool_input: Option<serde_json::Value>,
    /// AGY's nested pre-tool envelope.
    #[serde(alias = "toolCall", alias = "tool_call")]
    tool_call: Option<serde_json::Value>,
    /// Grok Stop callbacks carry the final assistant text inline.
    #[serde(alias = "lastAssistantMessage", alias = "last_assistant_message")]
    last_assistant_message: Option<String>,
    /// AGY signals "the turn truly settled" with `fullyIdle: true` and
    /// "the harness is still busy on background work" with `fullyIdle:
    /// false` (issue #1285, #1367). The latter is the false-yield analogue of
    /// Claude Code's background-task detection (issue #878): we
    /// publish the turn (naming / autopilot still fire) but suppress
    /// the attention marking. Missing / unknown defaults to "idle"
    /// so a future harness that omits the field keeps working.
    #[serde(alias = "fullyIdle", alias = "fully_idle", default)]
    fully_idle: Option<bool>,
    /// AGY's `terminationReason` (e.g. `"model_stop"`,
    /// `"tool_execution_limit_reached"`, `"error"`, `"max_steps_exceeded"`).
    /// Opaque to Buildmesh today — recorded for future debugging but not a
    /// decision input in `decide()`.
    #[serde(alias = "terminationReason", alias = "termination_reason", default)]
    termination_reason: Option<String>,
    /// Present if the hook execution encountered an error.
    #[serde(default)]
    error: Option<String>,
    /// AGY execution step index or invocation count.
    #[serde(alias = "executionNum", alias = "execution_num", default)]
    execution_num: Option<i64>,
    /// AGY workspace paths.
    #[serde(alias = "workspacePaths", alias = "workspace_paths", default)]
    workspace_paths: Option<Vec<String>>,
    /// AGY artifact directory path.
    #[serde(alias = "artifactDirectoryPath", alias = "artifact_directory_path", default)]
    artifact_directory_path: Option<String>,
    /// AGY model name.
    #[serde(alias = "modelName", alias = "model_name", default)]
    model_name: Option<String>,
    /// Grok Stop callbacks carry `reason` (e.g. `"end_turn"`).
    #[serde(alias = "reason", default)]
    reason: Option<String>,
}

/// Provider-neutral action shown above an awaiting Agent Node's terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticTurn {
    kind: SemanticTurnKind,
    description: String,
}

const MAX_SEMANTIC_DESCRIPTION: usize = 240;

fn clean_description(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() { return None; }
    let mut chars = normalized.chars();
    let clipped: String = chars.by_ref().take(MAX_SEMANTIC_DESCRIPTION).collect();
    Some(if chars.next().is_some() { format!("{clipped}…") } else { clipped })
}

fn string_field<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_str().filter(|value| !value.trim().is_empty())
}

/// Pull a single key=value pair out of an `&`-delimited URL query
/// string. Used by the runtime-scoped `?token=` gate (issue #1366)
/// to defend against non-Buildmesh hook callbacks. Percent-decoding
/// of the value is left to the caller — the token is hex so no
/// escaping is needed in practice.
fn extract_query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        if k == key { Some(v) } else { None }
    })
}

/// Normalize known hook shapes without guessing from arbitrary terminal text.
fn semantic_turn(payload: &HookPayload) -> Option<SemanticTurn> {
    let nested_name = payload
        .tool_call
        .as_ref()
        .and_then(|value| string_field(value, &["name"]));
    let tool_name = payload.tool_name.as_deref().or(nested_name);
    let command = payload
        .tool_input
        .as_ref()
        .and_then(|value| {
            string_field(value, &["command"]).or_else(|| string_field(value, &["cmd"]))
        })
        .or_else(|| {
            payload.tool_call.as_ref().and_then(|value| {
                string_field(value, &["args", "command"])
                    .or_else(|| string_field(value, &["args", "cmd"]))
            })
        });

    let permission_event = matches!(
        payload.hook_event_name.as_deref(),
        Some("PermissionRequest") | Some("PreToolUse")
    ) || payload.notification_type.as_deref() == Some("permission_prompt")
    ;

    if permission_event {
        if let Some(command) = command {
            return Some(SemanticTurn {
                kind: SemanticTurnKind::CommandConfirmation,
                description: clean_description(&format!("Run: {}", command.trim()))?,
            });
        }

        let path = payload.tool_input.as_ref().and_then(|value| {
            string_field(value, &["file_path"]).or_else(|| string_field(value, &["path"]))
        });
        let description = match (tool_name, path) {
            (Some(name), Some(path)) => {
                let label = if name.eq_ignore_ascii_case("edit") { "edit" } else { name.trim() };
                format!("Allow {}: {}", label, path.trim())
            }
            (Some(name), None) => format!("Allow: {}", name.trim()),
            (None, Some(path)) => format!("Allow: {}", path.trim()),
            _ => payload.message.as_deref().map(str::trim).unwrap_or("Allow tool" ).to_owned(),
        };
        return Some(SemanticTurn {
            kind: SemanticTurnKind::PermissionRequest,
            description: clean_description(&description)?,
        });
    }

    let description = payload
        .last_assistant_message
        .as_deref()
        .or_else(|| {
            matches!(
                payload.notification_type.as_deref(),
                Some("idle_prompt") | Some("task_complete")
            )
            .then_some(payload.message.as_deref())
            .flatten()
        })?
        .trim();
    clean_description(description).map(|description| SemanticTurn {
        kind: SemanticTurnKind::TurnFinished,
        description,
    })
}

/// Extract the provider-owned UUID from a structured hook callback. An
/// arbitrary string must never enter `cli_session_id`: resume treats that
/// column as an executable CLI argument. Codex, Claude, and AGY all use
/// UUIDs; the alias on `HookPayload::session_id` makes
/// `conversationId` (AGY) parse through the same code path. Validation
/// is delegated to [`request::parse_cli_session_id`] so this route and
/// `import_and_resume` share one boundary check (issue #1237).
fn hook_session_id(body: &[u8]) -> Option<String> {
    let id = serde_json::from_slice::<HookPayload>(body).ok()?.session_id?;
    request::parse_cli_session_id(&id)
}

/// What to do with an incoming attention webhook (issue #1364).
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// The user is needed — publish the Node Turn with attention marking and
    /// land the node in `AwaitingInput`.
    MarkInput,
    /// An ordinary turn finished with no user input needed — land the node in
    /// `Ready` (never the Autopilot-only `Completed`).
    Ready,
    /// Publish the Node Turn without attention marking: the turn ended only
    /// because background tasks are still running and the harness will
    /// re-invoke itself when they finish (issue #878).
    SuppressPendingBackground,
    /// Capture any structured session id, then stop. SessionStart (and similar
    /// boot events) must not look like a turn completion.
    Ignore,
}

/// The result of classifying a hook POST body: the [`Decision`] plus the
/// provider envelope that survives into the `agent-lifecycle` event.
#[derive(Debug, PartialEq, Eq)]
struct Classified {
    decision: Decision,
    detail: crate::agent::session_lifecycle::HookSignalDetail,
}

impl Classified {
    fn mark_input(detail: crate::agent::session_lifecycle::HookSignalDetail) -> Self {
        Self { decision: Decision::MarkInput, detail }
    }
    fn ready(detail: crate::agent::session_lifecycle::HookSignalDetail) -> Self {
        Self { decision: Decision::Ready, detail }
    }
    fn suppress(detail: crate::agent::session_lifecycle::HookSignalDetail) -> Self {
        Self { decision: Decision::SuppressPendingBackground, detail }
    }
}

/// Classify a hook POST body. `count_pending` is the transcript scan
/// (`transcript_reader::count_pending_background_tasks`), injected so tests
/// don't need real transcript files.
///
/// The rules, in order:
/// 1. Unparseable/absent body → `MarkInput` (pre-#878 behaviour; old hook
///    configs that post no body keep working until the next spawn migrates
///    them) with `signal_health = Degraded` — an unknown payload is never
///    silently presented as a high-confidence signal.
/// 2. A permission-prompt Notification (Claude Code), a `PermissionRequest`
///    event (Codex's dedicated hook for tool approval, issue #884), a
///    Grok `Notification` with `notificationType == "permission_prompt"`
///    (issue #1282), or an AGY `PreToolUse` event (the harness's pre-tool
///    approval hook, issue #1285) → `MarkInput` always. The agent is blocked
///    on a tool-approval decision — that needs the user even while
///    background tasks run.
/// 3. AGY's `Stop` with `fullyIdle: false` (or explicit `fullyIdle: false`,
///    issue #1285, #1367) → suppress. The harness signalled the turn ended
///    but the agent is still busy on background work. Same false-yield
///    semantic as Claude Code's transcript-scan path (rule 4), but signalled
///    directly by the harness because AGY has no background task scan.
/// 4. Anything else (Stop, idle Notification) with launched-but-unfinished
///    background tasks in the transcript → `SuppressPendingBackground`.
/// 5. No transcript path, unreadable transcript, or no pending tasks →
///    `Ready` (issue #1364): a clean turn completion is NOT a user-input
///    request. The node lands in `Ready`, never in `AwaitingInput`.
fn classify(body: &[u8], count_pending: impl FnOnce(&Path) -> Option<usize>) -> Classified {
    let Ok(payload) = serde_json::from_slice::<HookPayload>(body) else {
        return Classified::mark_input(crate::agent::session_lifecycle::HookSignalDetail {
            signal_health: crate::agent::session_lifecycle::SignalHealth::Degraded,
            ..Default::default()
        });
    };
    // A parseable but fieldless envelope (`{}`, legacy no-field POSTs) is an
    // unknown signal — never "turn completed" and never "no attention
    // needed". Mark for attention with a degraded health so the UI can
    // render the uncertainty (issue #1364 §1). Comparing against the
    // derived `Default` keeps this total over future `HookPayload` fields.
    if payload == HookPayload::default() {
        return Classified::mark_input(crate::agent::session_lifecycle::HookSignalDetail {
            signal_health: crate::agent::session_lifecycle::SignalHealth::Degraded,
            ..Default::default()
        });
    }
    let detail = crate::agent::session_lifecycle::HookSignalDetail {
        provider_event: payload.hook_event_name.clone(),
        provider_session_id: payload
            .session_id
            .as_deref()
            .and_then(request::parse_cli_session_id),
        completion_reason: payload
            .termination_reason
            .clone()
            .or_else(|| payload.reason.clone()),
        transcript_path: payload.transcript_path.clone(),
        signal_health: crate::agent::session_lifecycle::SignalHealth::Ok,
        message: payload.message.clone(),
        // Issue #1366: Grok's structured `notificationType` (e.g.
        // `permission_prompt`, `idle_prompt`, `task_complete`) is
        // carried alongside the normalized lifecycle decision so the
        // UI can render the harness's own classification instead of
        // the collapsed shared kind. Other harnesses leave this None.
        notification_type: payload.notification_type.clone(),
        ..Default::default()
    };
    let event = payload
        .hook_event_name
        .as_deref()
        .map(str::to_ascii_lowercase);
    let event = event.as_deref();
    if event == Some("sessionstart") {
        return Classified {
            decision: Decision::Ignore,
            detail,
        };
    }
    if matches!(event, Some("permissionrequest") | Some("pretooluse")) {
        return Classified::mark_input(detail);
    }
    // Grok posts `hookEventName: "notification"` (lowercase); Claude posts
    // `"Notification"`. Match case-insensitively so the structured
    // `notificationType` handling below applies to both.
    if event == Some("notification") {

        // Claude Code's documented Notification envelope is "… needs your
        // permission to use X" — anchored to the verb phrase, not a bare
        // "permission" substring, so prose like "Permission was already
        // granted for Bash" cannot false-positive.
        if payload
            .message
            .as_deref()
            .is_some_and(|m| m.to_ascii_lowercase().contains("needs your permission"))
        {
            return Classified::mark_input(detail);
        }
        // Grok Code (issue #1282): structured notificationType =
        // "permission_prompt". A matcher on the wire might catch it
        // before us, but the runner POSTs the envelope unconditionally
        // for every matched hook entry — we still see the callback.
        match payload.notification_type.as_deref() {
            Some("permission_prompt") => return Classified::mark_input(detail),
            Some("task_complete") => {
                // Grok's structured task-complete notification: the turn
                // finished, no user input needed (issue #1364).
                return Classified::ready(detail);
            }
            // Question-shaped structured types (no current harness emits
            // these yet; reserved so a provider that adds one — Grok
            // advertises QuestionRequested — lands on the normalized kind
            // without prose guessing). Unstructured messages are NEVER
            // classified as questions from free text.
            Some("question") | Some("question_prompt") | Some("ask_user") => {
                let mut question = detail;
                question.kind =
                    Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested);
                return Classified::mark_input(question);
            }
            _ => {}
        }
    }
    // AGY `Stop` with `fullyIdle: false` is a direct false-yield signal
    // from the harness — short-circuit before the transcript scan so an
    // AGY node gets correct suppression.
    //
    // AGY-specific: in AGY Stop payloads, `hook_event_name` is either
    // explicitly "Stop" or omitted from stdin JSON. When `fullyIdle: false`
    // arrives on a Stop event or an AGY payload (with session_id / conversationId
    // or terminationReason), suppress attention without scanning transcripts.
    if (event == Some("stop")
        || (event.is_none()
            && (payload.termination_reason.is_some() || payload.session_id.is_some())))
        && payload.fully_idle == Some(false)
    {
        return Classified::suppress(detail);
    }
    // A Stop with fullyIdle: true or absent falls through to the transcript-scan
    // path so any future transcript reader hooks in normally.
    let Some(transcript_path) = payload.transcript_path.filter(|p| !p.is_empty()) else {
        return Classified::ready(detail);
    };
    // A WSL-side agent reports a Linux transcript path; convert before the
    // Windows-side read (the module rule: never hand a Linux path to a
    // Windows API).
    let host_path = crate::env::to_host_path(&transcript_path);
    match count_pending(Path::new(&host_path)) {
        Some(n) if n > 0 => Classified::suppress(detail),
        _ => Classified::ready(detail),
    }
}

/// Pure-function verifier for the attention-route token gate
/// (issue #1366 round-2 + round-3). Extracted from `handle_post`
/// so the comparator semantics can be tested in isolation
/// without spinning up a Tokio listener or a real SQLite handle.
///
/// Truth table (returns `true` = accept the callback, `false` =
/// reject with 403):
///
/// | provider      | minted       | query token   | result |
/// |---------------|--------------|---------------|--------|
/// | != `"grok"`  | (any)        | (any)         | accept |  sibling harnesses bypass entirely
/// | `"grok"`     | `None`       | (any)         | reject |  no Buildmesh runtime owns this
/// | `"grok"`     | `Some(m)`    | matches `m`   | accept |
/// | `"grok"`     | `Some(m)`    | differs/missing | reject |
fn verify_attention_token(
    provider: &str,
    query_string: Option<&str>,
    minted: Option<&str>,
) -> bool {
    if provider != "grok" {
        return true;
    }
    let Some(minted) = minted else {
        return false;
    };
    let presented = query_string.and_then(|q| extract_query_value(q, "token"));
    presented == Some(minted)
}

pub async fn handle_post(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    path_without_query: &str,
    query_string: Option<&str>,
    peer: SocketAddr,
    content_length: usize,
) {
    // Loopback-only: the Claude Code hook always posts from 127.0.0.1/::1. A
    // non-loopback peer is an external spoof attempt — refuse before doing work.
    if !peer.ip().is_loopback() {
        let _ = request::write_status_only(lines, "403 Forbidden").await;
        return;
    }

    // Parse the session id early — the token gate below needs it to
    // look up the session's provider and decide whether to enforce
    // the hook token. Bad path → 400 before any other work.
    let session_id: Option<i64> = path_without_query
        .strip_prefix("/api/attention/")
        .and_then(|s| s.parse().ok());
    let Some(session_id) = session_id else {
        let _ = request::write_status_only(lines, "400 Bad Request").await;
        return;
    };

    // Runtime-scoped token gate (issue #1366, round-2 + round-3 +
    // N1 fixes). The decision is **per-provider**, not per-`?token=`
    // presence:
    //
    //   provider = grok  → require matching ?token=<minted> against the
    //                      runtime-scoped `RUNTIME_HOOK_TOKEN` OnceLock.
    //   provider ∈ {claude, codex, agy, …} → no token check.
    //
    // Looked up by **session id** (the trusted path component). The
    // session row is the canonical record of which harness is
    // calling. The DB lookup is wrapped in `spawn_blocking` so
    // the synchronous SQLite read lock does not stall the Tokio
    // worker (round-3 review point 1). The `(cli_session_id, provider)`
    // pair is captured here and passed into the run_blocking closure
    // below — see the N1 review point about hitting SQLite twice
    // for the same row.
    let node: Option<crate::models::AgentNode> =
    tokio::task::spawn_blocking(move || crate::db::get_agent_node_by_id(session_id))
        .await
        .ok()
        .and_then(|r| r.ok());
let stored_cli_session_id: String = node
    .as_ref()
    .and_then(|n| n.cli_session_id.clone())
    .unwrap_or_default();
let node_provider: Option<String> = node.as_ref().map(|n| n.provider.clone());
let provider: &str = node_provider.as_deref().unwrap_or("");
    if !verify_attention_token(
        provider,
        query_string,
        crate::agent::runtime_hook_token().as_deref(),
    ) {
        let _ = request::write_status_only(lines, "403 Forbidden").await;
        return;
    }

    let Some(body) =
        request::read_body_or_send_error(lines, content_length, MAX_HOOK_BODY).await
    else {
        return;
    };

    let Some(app) = crate::http::app_handle() else {
        let _ = request::write_status_only(lines, "503 Service Unavailable").await;
        return;
    };

    // Cheap, CPU-only classification happens on the async worker: parse,
    // debug-log, extract the semantic turn, and classify (the transcript
    // scan is file I/O, not SQLite). Everything that touches SQLite — the
    // fill-only session capture, the stale-callback check, the signal-
    // health confirmation, the semantic-turn persist, and the status write
    // — then runs in ONE `run_blocking` dispatch below, so a single webhook
    // POST costs one blocking hop and one DB lock acquisition instead of a
    // Tokio↔SQLite ping-pong (issue #1364 review).
    let hook_uuid = hook_session_id(&body);

    // AGY surfaces its `terminationReason` (e.g. `"model_stop"`,
    // `"tool_execution_limit_reached"`) so a future debugging session can
    // distinguish "the model finished its turn" from "the harness
    // aborted the turn" — log at debug so it's there when needed without
    // polluting the happy path (issue #1285, #1367).
    let payload_parsed = serde_json::from_slice::<HookPayload>(&body);
    match payload_parsed {
        Ok(ref payload) => {
            if let Some(ref reason) = payload.termination_reason.as_deref().filter(|r| !r.is_empty()) {
                tracing::debug!(
                    "attention webhook for node {} reported terminationReason={}",
                    session_id,
                    reason
                );
            }
            if let Some(ref err) = payload.error.as_deref().filter(|e| !e.is_empty()) {
                tracing::debug!(
                    "attention webhook for node {} reported error={}",
                    session_id,
                    err
                );
            }
        }
        Err(_) => {
            tracing::warn!(
                "attention webhook for node {} received unparseable or empty body (low-confidence degraded fallback to mark attention)",
                session_id
            );
        }
    }

    let semantic = payload_parsed.as_ref().ok().and_then(semantic_turn);

    let classified = classify(&body, crate::services::transcript_reader::count_pending_background_tasks);
    let mut detail = classified.detail;
    // The semantic turn always wins over the raw message for the
    // human-facing description; keep the health the classifier set.
    detail.semantic_turn = semantic.map(|turn| SemanticTurnPayload {
        node_id: session_id,
        kind: turn.kind,
        description: turn.description,
    });

    // Issue #1389 — every step below is blocking SQLite; one `spawn_blocking`
    // hop for the whole sequence. `app` is `&'static AppHandle` (returned by
    // `crate::http::app_handle()`), which is what lets `move ||` capture it.
    // N1 fix: the row we fetched for the token gate above is also the
    // row we'll persist + check ordering-token against — share it via
    // `move ||` capture rather than re-querying SQLite. Use clear
    // shadows so the outer `Option<String>` is gone before the
    // closure constructed (avoids the `move ||` capture error
    // for `Option<String>`, which doesn't implement Copy).
    let stored_cli_session_id_owned = stored_cli_session_id;
    let provider_owned = node_provider;
    let decision = classified.decision;
    let _ = crate::commands::run_blocking(
        "http_attention_apply",
        move || -> Result<Applied, String> {
            // Codex self-assigns its thread id. PTY capture remains the
            // earliest source; SessionStart is the structured capture at
            // boot, and Stop/PermissionRequest remain the later fallback
            // (issue #1089).
            if let Some(cli_session_id) = hook_uuid.clone() {
                match crate::db::set_cli_session_id_if_missing(session_id, &cli_session_id) {
                    Ok(true) => tracing::info!(
                        "attention webhook captured session ID {} for node {}",
                        cli_session_id,
                        session_id
                    ),
                    Ok(false) => {}
                    Err(error) => tracing::warn!(
                        "attention webhook could not persist session ID for node {}: {}",
                        session_id,
                        error
                    ),
                }
            }

            // Issue #1364 §1 — ordering token: a hook whose provider session
            // id is a valid UUID that differs from the node's stored one
            // belongs to a previous process generation. It must never
            // overwrite the newer state — the POST is answered 200 (the
            // harness's fail-open contract) and dropped. N1 fix: this
            // uses `stored_cli_session_id_owned` (captured above; one
            // DB hit, not two).
            if !stored_cli_session_id_owned.is_empty() {
                if let Some(hook) = hook_uuid.as_deref() {
                    if hook != stored_cli_session_id_owned {
                        tracing::info!(
                            "attention webhook for node {}: stale callback from a previous \
                             process (hook session {hook} != active {stored_cli_session_id_owned}) \
                             — dropped (issue #1364 ordering token)",
                            session_id
                        );
                        return Ok(Applied::StaleDropped);
                    }
                }
            }
            detail.provider = provider_owned;

            // A real, high-confidence callback proves hook delivery — persist
            // the node's signal health as Ok so a provisioning failure earlier
            // in the spawn doesn't linger. A degraded (unparseable/fieldless)
            // callback does NOT clear a failure: its event says Degraded and
            // the persisted health must agree (issue #1364 §3).
            if detail.signal_health == crate::agent::session_lifecycle::SignalHealth::Ok {
                let _ = crate::db::update_agent_node_signal_health(
                    session_id,
                    Some(crate::agent::session_lifecycle::SignalHealth::Ok),
                );
            }

            match decision {
                Decision::MarkInput => {
                    let encoded = detail
                        .semantic_turn
                        .as_ref()
                        .map(|v| serde_json::to_string(v).map_err(|e| e.to_string()))
                        .transpose()?;
                    crate::db::persist_semantic_turn(session_id, encoded.as_deref())
                        .map_err(|e| e.to_string())?;
                    crate::node_turn::publish_with_signal(
                        session_id,
                        app,
                        detail.semantic_turn.clone(),
                        detail,
                    );
                }
                Decision::Ready => {
                    tracing::info!(
                        "attention webhook for node {}: clean turn completion — \
                         node lands in Ready (issue #1364)",
                        session_id
                    );
                    // Naming and the autopilot pipeline still see the turn
                    // (`publish_passive`), then the lifecycle writes `Ready`
                    // and emits `agent-lifecycle` on both transports. No
                    // `attention-needed`, no autoclear arm.
                    crate::node_turn::publish_ready(session_id, app, detail);
                }
                Decision::SuppressPendingBackground => {
                    tracing::info!(
                        "attention webhook for node {}: background tasks still pending — \
                         turn published without attention marking (issue #878)",
                        session_id
                    );
                    crate::node_turn::publish_background(session_id, app, detail);
                }
                Decision::Ignore => {
                    tracing::debug!(
                        "attention webhook for node {}: lifecycle-neutral hook, session capture only",
                        session_id
                    );
                }
            }
            Ok(Applied::Applied)
        },
    )
    .await;

    // Both outcomes answer 200 OK — the harnesses' fail-open contract (an
    // applied callback and a stale-dropped one are indistinguishable to the
    // poster; a stale drop simply changed nothing).
    let _ = request::write_status_only(lines, "200 OK").await;
}

/// Outcome of the single blocking apply pass (issue #1364 review): the
/// webhook was applied, or dropped as a stale callback from a previous
/// process generation.
enum Applied {
    Applied,
    StaleDropped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_turn_normalizes_permission_command_and_finished_payloads() {
        let permission: HookPayload = serde_json::from_value(serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Edit",
            "tool_input": { "file_path": "src/lib/auth.ts" }
        }))
        .unwrap();
        assert_eq!(
            semantic_turn(&permission),
            Some(SemanticTurn {
                kind: SemanticTurnKind::PermissionRequest,
                description: "Allow edit: src/lib/auth.ts".into(),
            })
        );

        let command: HookPayload = serde_json::from_value(serde_json::json!({
            "hook_event_name": "PreToolUse",
            "toolCall": { "name": "run_command", "args": { "cmd": "npm test -- --coverage" } }
        }))
        .unwrap();
        assert_eq!(
            semantic_turn(&command),
            Some(SemanticTurn {
                kind: SemanticTurnKind::CommandConfirmation,
                description: "Run: npm test -- --coverage".into(),
            })
        );

        let finished: HookPayload = serde_json::from_value(serde_json::json!({
            "hookEventName": "Stop",
            "lastAssistantMessage": "Implemented the auth guard."
        }))
        .unwrap();
        assert_eq!(
            semantic_turn(&finished),
            Some(SemanticTurn {
                kind: SemanticTurnKind::TurnFinished,
                description: "Implemented the auth guard.".into(),
            })
        );

        assert_eq!(semantic_turn(&HookPayload::default()), None);
    }

    /// A representative Stop-hook stdin payload.
    fn stop_body(transcript_path: &str) -> Vec<u8> {
        serde_json::json!({
            "session_id": "abc-123",
            "transcript_path": transcript_path,
            "cwd": "F:\\src\\repo",
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string()
        .into_bytes()
    }

    /// Test shim: classify and return just the [`Decision`].
    fn classify_decision(
        body: &[u8],
        count_pending: impl FnOnce(&Path) -> Option<usize>,
    ) -> Decision {
        classify(body, count_pending).decision
    }

    #[test]
    fn empty_or_garbage_body_marks_input_with_degraded_health() {
        // Pre-#878 hooks post no body at all; a broken payload must degrade to
        // the old always-mark behaviour, never to silence — but the signal is
        // unknown, so the health is Degraded, never a high-confidence mark.
        for body in [&b""[..], b"not json".as_slice()] {
            let classified = classify(body, |_| Some(5));
            assert_eq!(classified.decision, Decision::MarkInput);
            assert_eq!(
                classified.detail.signal_health,
                crate::agent::session_lifecycle::SignalHealth::Degraded,
                "an unparseable payload must record degraded signal health (issue #1364)"
            );
        }
    }

    #[test]
    fn fieldless_json_body_marks_input_with_degraded_health() {
        // A parseable `{}` with no recognized fields is an unknown signal —
        // not "turn completed". Mark with degraded health (issue #1364).
        let classified = classify(b"{}", |_| Some(0));
        assert_eq!(classified.decision, Decision::MarkInput);
        assert_eq!(
            classified.detail.signal_health,
            crate::agent::session_lifecycle::SignalHealth::Degraded
        );
    }

    #[test]
    fn structured_hook_exposes_canonical_session_uuid() {
        let body = serde_json::json!({
            "session_id": "C1234567-89AB-CDEF-0123-456789ABCDEF",
            "hook_event_name": "Stop",
        })
        .to_string();
        assert_eq!(
            hook_session_id(body.as_bytes()).as_deref(),
            Some("c1234567-89ab-cdef-0123-456789abcdef")
        );
    }

    #[test]
    fn missing_malformed_or_non_uuid_session_id_is_ignored() {
        assert_eq!(hook_session_id(b"{}"), None);
        assert_eq!(hook_session_id(b"not json"), None);
        assert_eq!(hook_session_id(br#"{"session_id":"most-recent"}"#), None);
    }

    #[test]
    fn stop_with_pending_background_tasks_suppresses() {
        let body = stop_body("/tmp/session.jsonl");
        assert_eq!(
            classify_decision(&body, |_| Some(2)),
            Decision::SuppressPendingBackground
        );
    }

    #[test]
    fn stop_with_no_pending_tasks_is_ready() {
        // Issue #1364 — a clean turn completion is NOT a user-input request;
        // the node lands in Ready, never AwaitingInput.
        let body = stop_body("/tmp/session.jsonl");
        assert_eq!(classify_decision(&body, |_| Some(0)), Decision::Ready);
    }

    #[test]
    fn unreadable_transcript_is_ready() {
        // A missing/unreadable transcript means the background scan can't
        // prove pending work — the turn is treated as completed (Ready),
        // which still never blocks a later permission callback.
        let body = stop_body("/tmp/session.jsonl");
        assert_eq!(classify_decision(&body, |_| None), Decision::Ready);
    }

    #[test]
    fn missing_transcript_path_is_ready() {
        let body = serde_json::json!({"hook_event_name": "Stop"}).to_string().into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(3)), Decision::Ready);
    }

    #[test]
    fn permission_prompt_notification_marks_input_even_with_pending_tasks() {
        // A tool-approval question blocks the whole turn — background work
        // running in parallel doesn't make the user less needed.
        let body = serde_json::json!({
            "hook_event_name": "Notification",
            "transcript_path": "/tmp/session.jsonl",
            "message": "Claude needs your permission to use Bash",
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, |_| Some(2));
        assert_eq!(classified.decision, Decision::MarkInput);
        assert_eq!(
            classified.detail.signal_health,
            crate::agent::session_lifecycle::SignalHealth::Ok,
            "a structured permission payload is a high-confidence signal"
        );
    }

    #[test]
    fn codex_permission_request_marks_input_even_with_pending_tasks() {
        // Codex raises a dedicated PermissionRequest hook event when a tool
        // needs approval (issue #884) — the user is needed, same as a Claude
        // permission Notification, regardless of background work.
        let body = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "transcript_path": "/tmp/session.jsonl",
            "tool_name": "Bash",
            "message": "Codex needs your permission to run Bash",
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(2)), Decision::MarkInput);
    }

    #[test]
    fn idle_notification_with_pending_tasks_suppresses() {
        // The 60s idle notification fires while the agent sits at its input
        // box waiting for a long background build — same false yield as Stop.
        let body = serde_json::json!({
            "hook_event_name": "Notification",
            "transcript_path": "/tmp/session.jsonl",
            "message": "Claude is waiting for your input",
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            classify_decision(&body, |_| Some(1)),
            Decision::SuppressPendingBackground
        );
    }

    // -------------------------------------------------------------------
    // Grok Code (issue #1282) — camelCase wire, no transcript_path,
    // Notification carries structured `notificationType`.
    // -------------------------------------------------------------------

    /// Grok's permission prompt: the matcher fires `Notification` with
    /// `notificationType = "permission_prompt"`. We mark the node
    /// regardless of any background-task count (defensive — Grok has
    /// no transcript reader yet, so the closure is unused, but
    /// keeping the signature uniform guards against a future
    /// transcript reader silently swallowing the permission yield).
    #[test]
    fn grok_notification_with_permission_type_marks_input() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "cwd": "/Users/you/project",
            "workspaceRoot": "/Users/you/project",
            "notificationType": "permission_prompt",
            "message": "Grok needs your permission to run Bash",
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(2)), Decision::MarkInput);
    }

    /// Grok's idle prompt (`notificationType = "idle_prompt"`) carries
    /// no transcript path. With no pending work it reads as a clean turn
    /// completion → Ready (issue #1364); it must never show the amber
    /// "Needs attention" for a turn that finished normally.
    #[test]
    fn grok_idle_notification_without_transcript_is_ready() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "notificationType": "idle_prompt",
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(2)), Decision::Ready);
    }

    /// Grok's `task_complete` notification is an explicit completion signal
    /// → Ready (issue #1364).
    #[test]
    fn grok_task_complete_notification_is_ready() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "notificationType": "task_complete",
            "message": "Task finished",
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(2)), Decision::Ready);
    }

    /// Codex SessionStart fires as soon as the TUI boots, carrying the
    /// conversation UUID. That is the structured capture we want. It is
    /// not a turn completion — treating it as Ready would fire naming and
    /// Autopilot on an empty session.
    #[test]
    fn codex_session_start_is_lifecycle_neutral() {
        let body = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "550e8400-e29b-41d4-a716-446655440000",
            "cwd": r"F:\src\buildmesh\.claude\worktrees\node",
            "source": "startup",
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(0)), Decision::Ignore);
        assert_eq!(
            hook_session_id(&body).as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    /// Grok's `Stop` event is a clean turn completion → Ready. The runner
    /// treats the route's empty 200 OK as "allow the stop" (we never return
    /// a `decision: "block"` JSON), so the agent doesn't loop on the gate.
    #[test]
    fn grok_stop_event_is_ready() {
        let body = serde_json::json!({
            "hookEventName": "stop",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "stopHookActive": false,
            "lastAssistantMessage": "Done.",
            "reason": "end_turn",
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(0)), Decision::Ready);
    }

    /// Grok's Stop carries `reason` and `stopHookActive` — parse them into
    /// the envelope's completion reason so the lifecycle event preserves
    /// the provider detail (issue #1364 §1).
    #[test]
    fn grok_stop_envelope_preserves_completion_reason() {
        let body = serde_json::json!({
            "hookEventName": "stop",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "stopHookActive": false,
            "lastAssistantMessage": "Done.",
            "reason": "end_turn",
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, |_| Some(0));
        assert_eq!(classified.decision, Decision::Ready);
        assert_eq!(classified.detail.completion_reason.as_deref(), Some("end_turn"));
        assert_eq!(
            classified.detail.provider_session_id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    /// Issue #1366 — Grok's structured `notificationType` is
    /// preserved into the classified detail end-to-end. The
    /// lifecycle event carries both the normalized decision
    /// (`MarkInput` / `Ready`) AND the harness's own string
    /// (`permission_prompt`, `task_complete`, …) so the UI can
    /// render the harness's own classification. A future refactor
    /// that drops this field trips here before the wire shape
    /// drifts.
    #[test]
    fn grok_notification_type_surfaces_in_signal_detail() {
        for (notification_type, expected_decision) in [
            ("permission_prompt", Decision::MarkInput),
            ("task_complete", Decision::Ready),
            ("idle_prompt", Decision::Ready),
            ("question", Decision::MarkInput),
            ("question_prompt", Decision::MarkInput),
            ("ask_user", Decision::MarkInput),
        ] {
            let body = serde_json::json!({
                "hookEventName": "notification",
                "sessionId": "550e8400-e29b-41d4-a716-446655440000",
                "notificationType": notification_type,
            })
            .to_string()
            .into_bytes();
            let classified = classify(&body, |_| Some(0));
            assert_eq!(
                classified.detail.notification_type.as_deref(),
                Some(notification_type),
                "{notification_type}: notificationType must round-trip into the signal detail"
            );
            assert_eq!(
                classified.decision, expected_decision,
                "{notification_type}: lifecycle decision must match the shared contract"
            );
        }
    }

    /// `notificationType` absent means "the harness did not
    /// structure the notification" — the route falls through to the
    /// transcript-scan path. Pin so a future refactor that
    /// conflates "no notification_type" with "no event" trips here.
    #[test]
    fn grok_notification_without_type_falls_through() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "transcriptPath": "/tmp/session.jsonl",
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, |_| Some(2));
        assert_eq!(classified.detail.notification_type, None);
        // 2 pending tasks → suppress (the prose-only fallback is
        // strict on "needs your permission"; untyped notifications
        // match that pattern only if the harness's prose says so).
        assert_eq!(classified.decision, Decision::SuppressPendingBackground);
    }

    /// The payload parser accepts Grok's camelCase `sessionId` and
    /// canonicalises it to the same UUID string Claude payloads do.
    /// `hook_session_id` is the only consumer of the field on the
    /// session-id side of the route — both casings must round-trip.
    #[test]
    fn hook_payload_reads_camel_case_session_id() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550E8400-E29B-41D4-A716-446655440000",
            "notificationType": "idle_prompt",
        })
        .to_string();
        assert_eq!(
            hook_session_id(body.as_bytes()).as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    /// Grok wires `notificationType` as a separate field, distinct
    /// from Claude's message-substring convention — both styles must
    /// classify as a permission yield so a hook that emits either
    /// shape gets marked.
    #[test]
    fn grok_notification_type_via_snake_case_alias_also_marks_input() {
        // The grok-agent-sdk converts camelCase top-level keys to
        // snake_case — accept both so the same parser handles both
        // delivery surfaces.
        let body = serde_json::json!({
            "hook_event_name": "Notification",
            "session_id": "550e8400-e29b-41d4-a716-446655440000",
            "notification_type": "permission_prompt",
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(2)), Decision::MarkInput);
    }

    // -------------------------------------------------------------------
    // Antigravity (agy) payload shape — issue #1285.
    //
    // AGY sends camelCase fields (`conversationId`, `transcriptPath`,
    // `fullyIdle`, `terminationReason`) and two event kinds (`Stop` and
    // `PreToolUse`). The route's `HookPayload` accepts both via serde
    // aliases; `decide` short-circuits `PreToolUse` to always-mark and
    // `Stop` with `fullyIdle: false` to always-suppress.
    // -------------------------------------------------------------------

    /// AGY's `Stop` with `fullyIdle: false` is a direct false-yield signal
    /// from the harness — the turn ended but background work is still
    /// running. Short-circuits before the transcript scan so an AGY node
    /// (which has no transcript reader) gets correct suppression. Even
    /// when `count_pending` reports zero tasks (the harness said so), the
    /// harness's own signal wins — AGY's view is authoritative for AGY.
    #[test]
    fn agy_stop_with_fully_idle_false_suppresses() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": false,
            "terminationReason": "model_stop",
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            classify_decision(&body, |_| Some(0)),
            Decision::SuppressPendingBackground
        );
    }

    /// AGY's `Stop` with `fullyIdle: true` is a genuine turn completion —
    /// falls through to the transcript-scan path. No pending tasks → Ready
    /// (issue #1364: the user is NOT needed).
    #[test]
    fn agy_stop_with_fully_idle_true_is_ready() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": true,
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(0)), Decision::Ready);
    }

    /// `fullyIdle: true` with pending tasks still suppresses — the
    /// transcript scan is consulted, not skipped, when the harness says
    /// the turn actually settled.
    #[test]
    fn agy_stop_with_fully_idle_true_and_pending_tasks_suppresses() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": true,
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            classify_decision(&body, |_| Some(2)),
            Decision::SuppressPendingBackground
        );
    }

    /// An older AGY payload that omits `fullyIdle` entirely (or any
    /// future harness that doesn't set it) falls through to the
    /// transcript-scan path — `Stop` with no transcript path → Ready,
    /// matching the issue #1364 clean-turn-completion semantics. The
    /// field is additive, not breaking.
    #[test]
    fn agy_stop_without_fully_idle_uses_transcript_scan() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(0)), Decision::Ready);

        let missing_transcript = serde_json::json!({
            "conversationId": "abc-123",
            "hook_event_name": "Stop",
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            classify_decision(&missing_transcript, |_| Some(3)),
            Decision::Ready
        );
    }

    /// AGY's `PreToolUse` fires before a tool call — analogous to Codex's
    /// `PermissionRequest`. The agent is at a tool-approval decision, so
    /// the user is needed regardless of background work. Always marks input.
    #[test]
    fn agy_pre_tool_use_marks_input_even_with_pending_tasks() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "PreToolUse",
            "toolCall": {"name": "run_command", "args": {"cmd": "ls"}},
            "stepIdx": 5,
        })
        .to_string()
        .into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(5)), Decision::MarkInput);
    }

    /// `hook_session_id` extracts the AGY UUID from `conversationId` via
    /// the alias, just like Claude Code's `session_id`. Lower-cased so
    /// the value matches what the orchestrator's spawn pipeline writes
    /// into `agent_nodes.cli_session_id`.
    #[test]
    fn hook_session_id_reads_agy_conversation_id() {
        let body = serde_json::json!({
            "conversationId": "C1234567-89AB-CDEF-0123-456789ABCDEF",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": true,
        })
        .to_string();
        assert_eq!(
            hook_session_id(body.as_bytes()).as_deref(),
            Some("c1234567-89ab-cdef-0123-456789abcdef")
        );
    }

    /// A payload that mixes snake_case (Claude Code shape) and
    /// camelCase (AGY shape) for different fields still parses — the
    /// alias is per-field, not per-payload. Both Grok's `sessionId` and
    /// AGY's `conversationId` flow through the same `session_id` field
    /// via stacked aliases.
    #[test]
    fn hook_payload_tolerates_mixed_case_field_names() {
        let body = serde_json::json!({
            "session_id": "C1234567-89AB-CDEF-0123-456789ABCDEF",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": true,
        })
        .to_string();
        assert_eq!(
            hook_session_id(body.as_bytes()).as_deref(),
            Some("c1234567-89ab-cdef-0123-456789abcdef")
        );
    }

    /// Issue #1367: Fixture for a complete AGY Stop payload emitted by current releases (1.0.0-1.1.22+).
    /// Covers all standard metadata fields, camelCase keys, and verifies exact parsing.
    #[test]
    fn agy_full_release_fixture_parses_all_fields() {
        let json_body = serde_json::json!({
            "conversationId": "550e8400-e29b-41d4-a716-446655440000",
            "executionNum": 4,
            "terminationReason": "model_stop",
            "error": "",
            "fullyIdle": true,
            "workspacePaths": ["/Users/dev/project"],
            "transcriptPath": "/Users/dev/project/.gemini/antigravity/transcript.jsonl",
            "artifactDirectoryPath": "/Users/dev/project/.gemini/antigravity/artifacts",
            "modelName": "gemini-3.7-flash"
        });
        let body = json_body.to_string().into_bytes();

        let parsed = serde_json::from_slice::<HookPayload>(&body).expect("must parse full AGY fixture");
        assert_eq!(parsed.session_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(parsed.execution_num, Some(4));
        assert_eq!(parsed.termination_reason.as_deref(), Some("model_stop"));
        assert!(parsed.error.as_deref().is_some_and(|e| e.is_empty()) || parsed.error.is_none());
        assert_eq!(parsed.fully_idle, Some(true));
        assert_eq!(parsed.workspace_paths.as_deref(), Some(&["/Users/dev/project".to_string()][..]));
        assert_eq!(parsed.model_name.as_deref(), Some("gemini-3.7-flash"));

        assert_eq!(hook_session_id(&body).as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(classify_decision(&body, |_| Some(0)), Decision::Ready);
    }

    /// Issue #1367: Background yield fixture (`fullyIdle: false`) with no hook_event_name
    /// (the natural AGY Stop hook stdin payload). Must suppress attention.
    #[test]
    fn agy_background_yield_without_hook_event_name_suppresses() {
        let json_body = serde_json::json!({
            "conversationId": "550e8400-e29b-41d4-a716-446655440000",
            "executionNum": 2,
            "terminationReason": "model_stop",
            "fullyIdle": false,
            "workspacePaths": ["/work/proj"],
            "transcriptPath": "/work/proj/transcript.jsonl"
        });
        let body = json_body.to_string().into_bytes();
        assert_eq!(classify_decision(&body, |_| Some(0)), Decision::SuppressPendingBackground);
    }

    /// Issue #1367: Various termination reasons emitted by AGY releases.
    #[test]
    fn agy_termination_reasons_classification() {
        for reason in [
            "model_stop",
            "tool_execution_limit_reached",
            "max_steps_exceeded",
            "user_interrupt",
            "error",
        ] {
            let body_idle = serde_json::json!({
                "conversationId": "550e8400-e29b-41d4-a716-446655440000",
                "terminationReason": reason,
                "fullyIdle": true,
            })
            .to_string()
            .into_bytes();
            assert_eq!(
                classify_decision(&body_idle, |_| Some(0)),
                Decision::Ready,
                "reason={reason} with fullyIdle=true must be Ready"
            );

            let body_busy = serde_json::json!({
                "conversationId": "550e8400-e29b-41d4-a716-446655440000",
                "terminationReason": reason,
                "fullyIdle": false,
            })
            .to_string()
            .into_bytes();
            assert_eq!(
                classify_decision(&body_busy, |_| Some(0)),
                Decision::SuppressPendingBackground,
                "reason={reason} with fullyIdle=false must Suppress"
            );
        }
    }

    /// Issue #1367: Malformed or unexpected JSON payloads degrade safely to
    /// MarkInput (never to a high-confidence completion or silence), with a
    /// degraded signal health.
    #[test]
    fn agy_malformed_payload_degrades_to_mark_input() {
        let malformed = b"{not: valid, json";
        let classified = classify(malformed, |_| Some(0));
        assert_eq!(classified.decision, Decision::MarkInput);
        assert_eq!(
            classified.detail.signal_health,
            crate::agent::session_lifecycle::SignalHealth::Degraded
        );

        let empty_obj = b"{}";
        let classified = classify(empty_obj, |_| Some(0));
        assert_eq!(classified.decision, Decision::MarkInput);
        assert_eq!(
            classified.detail.signal_health,
            crate::agent::session_lifecycle::SignalHealth::Degraded
        );
    }

    /// A structured question notification type maps to the normalized
    /// `QuestionRequested` kind, still a MarkInput decision (issue #1364
    /// §1). Unstructured prose is NEVER classified as a question.
    #[test]
    fn structured_question_notification_classifies_as_question_requested() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "notificationType": "question",
            "message": "Which database should I use?",
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, |_| Some(2));
        assert_eq!(classified.decision, Decision::MarkInput);
        assert_eq!(
            classified.detail.kind,
            Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested)
        );
    }

    /// Unstructured prose mentioning "question" must NOT become a
    /// QuestionRequested (issue #1364 review — no free-text guessing);
    /// it falls through the normal classification.
    #[test]
    fn prose_mentioning_question_is_not_question_requested() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "notificationType": "idle_prompt",
            "message": "I answered your question about the database",
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, |_| Some(0));
        assert_ne!(
            classified.detail.kind,
            Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested)
        );
    }

    /// "Permission was already granted for Bash" must not read as a
    /// permission request — the heuristic anchors to the documented
    /// "needs your permission" verb envelope (issue #1364 review).
    #[test]
    fn permission_already_granted_prose_is_not_permission_requested() {
        let body = serde_json::json!({
            "hookEventName": "Notification",
            "transcript_path": "/tmp/session.jsonl",
            "message": "Permission was already granted for Bash",
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, |_| Some(0));
        assert_eq!(classified.decision, Decision::Ready);
        assert_ne!(
            classified.detail.kind,
            Some(crate::agent::session_lifecycle::LifecycleKind::PermissionRequested)
        );
    }

    /// The classifier records the provider event name and a high-confidence
    /// health for a structured permission payload — the lifecycle derives
    /// `PermissionRequested` from the semantic turn (pinned in
    /// `session_lifecycle` tests).
    #[test]
    fn permission_payload_preserves_provider_event_and_health() {
        let body = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "transcript_path": "/tmp/session.jsonl",
            "tool_name": "Bash",
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, |_| Some(2));
        assert_eq!(classified.decision, Decision::MarkInput);
        assert_eq!(classified.detail.provider_event.as_deref(), Some("PermissionRequest"));
        assert_eq!(
            classified.detail.signal_health,
            crate::agent::session_lifecycle::SignalHealth::Ok
        );
    }

    // -------------------------------------------------------------------
    // Runtime hook token scoping — issue #1366.
    //
    // The hook file at `~/.grok/hooks/buildmesh-attention.json` is the
    // *global* always-trusted Grok hooks dir, so every Grok session on
    // the box (including a non-Buildmesh invocation in a shell that
    // happens to inherit `BUILDMESH_PORT`) loads our hooks. The
    // `?token=$BUILDMESH_HOOK_TOKEN` query param gives the route a
    // per-runtime secret so non-Buildmesh sessions can't deliver to a
    // Buildmesh node. The real tests below exercise the module-scope
    // OnceLock storage (round-trip) and the comparator's three
    // outcomes (match / wrong / empty). The actual HTTP loopback
    // round-trip is covered by `http::tests::attention_webhook_*`.
    // -------------------------------------------------------------------

    #[test]
    fn extract_query_value_returns_value_when_key_matches() {
        assert_eq!(
            extract_query_value("token=abc123", "token"),
            Some("abc123")
        );
        assert_eq!(
            extract_query_value("foo=bar&token=abc123", "token"),
            Some("abc123")
        );
        assert_eq!(
            extract_query_value("token=abc123&foo=bar", "token"),
            Some("abc123")
        );
    }

    #[test]
    fn extract_query_value_returns_none_when_key_absent() {
        assert_eq!(extract_query_value("foo=bar", "token"), None);
        assert_eq!(extract_query_value("", "token"), None);
        // No '=' so split_once fails — treated as absent, not errored.
        assert_eq!(extract_query_value("token", "token"), None);
    }

    /// Pin the module-scope OnceLock storage (issue #1366 review, point
/// 1.1). A previous revision kept `static TOKEN` inside two functions
/// — two separate allocations, one store and one read never agreed,
/// so `runtime_hook_token()` always returned `None`. The fix is
/// module-level storage; this test exercises the actual accessor
/// against `mint_runtime_hook_token()` round-trip.
#[test]
fn runtime_hook_token_round_trips_through_module_level_once_lock() {
    // Test setup: the runtime token is minted lazily by the Grok
    // adapter's `provision_attention_hooks` (production path),
    // not at every spawn. In a test process no Grok agent ever
    // spawns, so the OnceLock stays None until we mint here.
    // `mint_runtime_hook_token` is idempotent: it pins the same
    // value across all subsequent calls in this process, so calling
    // it from multiple tests is safe — every test that needs the
    // token will read the same value.
    let minted = crate::agent::mint_runtime_hook_token();
    let read_back = crate::agent::runtime_hook_token();
    assert_eq!(
        read_back.as_deref(),
        Some(minted.as_str()),
        "runtime_hook_token must read from the same OnceLock that \
         mint_runtime_hook_token writes to"
    );
    // Format sanity: 32 lowercase hex chars (16 random bytes).
    assert_eq!(minted.len(), 32);
    assert!(minted.chars().all(|c| c.is_ascii_hexdigit()));
}

/// The route gate at `handle_post:438-455` collapses to:
///   * no token minted  → permissive
///   * query token == minted → proceed
///   * anything else (no token, wrong token, empty token) → reject
/// This pins the comparator logic the route uses so a future refactor
/// can't silently let an unrelated harness through. The end-to-end
/// POST loopback round-trip is exercised by
/// `http::tests::attention_webhook_*` for the loopback peer path.
#[test]
fn runtime_token_validator_three_cases() {
    // `mint_runtime_hook_token` is idempotent across the process.
    // Explicitly mint here so this test passes both in isolation
    // (e.g. when selected by name) and under the default cargo-test
    // schedule. The round-trip test in this module reads the same
    // pinned value back to assert structural integrity; production
    // code only ever mints once per Buildmesh runtime lifetime.
    let minted = crate::agent::mint_runtime_hook_token();

    // Case A: matching token → accept.
    let query_with_match = format!("token={minted}");
    assert_eq!(
        extract_query_value(&query_with_match, "token").as_deref(),
        Some(minted.as_str()),
        "matching token must round-trip via extract_query_value"
    );

    // Case B: wrong token → reject.
    let wrong_token = "z".repeat(32);
    let query_wrong = format!("token={wrong_token}");
    let presented = extract_query_value(&query_wrong, "token")
        .expect("wrong-token query is non-empty");
    assert_ne!(
        presented, minted,
        "wrong token must not match the minted one"
    );

    // Case C: empty token (`$VAR` expansion on an unset env produces an
    // empty value, the typical non-Buildmesh-shell case) → reject.
    let query_empty = "token=";
    let presented_empty = extract_query_value(query_empty, "token")
        .expect("trailing '=' still parses as a key=value pair");
    assert_eq!(presented_empty, "");
    assert_ne!(presented_empty, minted);
}


/// Round-2 review fix 4 — the per-provider token gate.
///
/// Calls the production `verify_attention_token` helper directly
/// so a refactor that flips the comparator semantics or that
/// drops the per-provider discrimination fails here. The truth
/// table (minted `Some("grok_token")`, query varies):
///
///   provider="claude", no query  → accept (sibling bypass)
///   provider="claude", any token → accept (sibling bypass)
///   provider="grok",   no query  → reject (no token)
///   provider="grok",   wrong     → reject
///   provider="grok",   match     → accept
///   provider="grok",   minted=None, any → reject (defence in depth)
#[test]
fn verify_attention_token_truth_table() {
    let minted = Some("grok_token");

    // Sibling harnesses — bypass entirely even when a token is
    // minted. Their hook URLs never carry `?token=`, but more
    // importantly the per-provider lookup classifies them as
    // non-Grok so the comparator never runs.
    assert!(verify_attention_token("claude", None, minted));
    assert!(verify_attention_token("claude", Some("anything"), minted));
    assert!(verify_attention_token("codex", None, minted));
    assert!(verify_attention_token("agy", Some("token=z"), minted));
    // Empty provider string ("default anthropic" sentinel) is
    // also non-Grok — still bypass.
    assert!(verify_attention_token("", None, minted));

    // Grok callbacks — token required.
    assert!(!verify_attention_token("grok", None, minted));
    assert!(!verify_attention_token("grok", Some("token="), minted));
    assert!(!verify_attention_token("grok", Some("token=wrong"), minted));
    assert!(verify_attention_token("grok", Some("token=grok_token"), minted));
    // No minted token yet (no Grok spawn in this runtime) AND a
    // Grok callback arrives — refuse. This is the defensive 403
    // that catches a Buildmesh instance whose own Grok spawn
    // never ran but the file system somehow has a hook caller.
    assert!(!verify_attention_token("grok", Some("token=grok_token"), None));
    assert!(!verify_attention_token("grok", None, None));
}

}
