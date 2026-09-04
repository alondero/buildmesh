//! Tauri command wrappers for agent spawn orchestration.
//!
//! After issue #1052 this module owns only the **spawn-orchestration** surface
//! (`spawn_agent`, `spawn_issue_agent`, `create_issue_node`, `create_pr_node`,
//! `spawn_handover_agent`, `auto_resume_agent_nodes`, `list_autopilot_runs`) +
//! the helpers that wrap their inputs (`validate_pr_spawn_inputs`) + the matching wire types.
//! The GitHub-issue / GitHub-PR prefill helpers (`format_issue_prefill`,
//! `format_pr_prefill`) used to live here; both were consolidated into
//! [`crate::agent::spawn::SpawnIntent::initial_prompt`] (issue #1180) so the
//! desktop draft, the background launch, and the Autopilot watcher all
//! derive from the same `SpawnIntent` instead of three divergent free
//! functions.
//!
//! The **process-lifecycle** home (`kill_agent` / `write_to_agent` /
//! `resize_agent` / `send_to_agent` / `is_agent_running` / `debug_*` /
//! `kill_all_sessions`) moved into `crate::agent::process`. The
//! **provider-menu** derivation (`available_providers` / `list_providers` plus
//! the unit tests pinning `compose_provider_menu`, `order_providers`, etc.)
//! moved into `crate::agent::provider_menu`. The mobile HTTP route's
//! `use crate::agent::provider_menu::available_providers` import is the
//! single direct call site; no re-export lives here.

use crate::agent::spawn::{
    spawn_with_intent, IssueContext, PullRequestContext, SpawnIntent, SpawnOutcome,
    SpawnRequest, TerminalSize,
};
use crate::db;
use serde::{Deserialize, Serialize};
use tauri::{command, AppHandle, Emitter};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Wire types — Tauri event payloads (issue #161)
// ---------------------------------------------------------------------------

/// Payload of the `node-created` Tauri event. Emitted by [`create_issue_node`]
/// after the `pending` row is committed, by the autopilot spawn path, and by
/// the HTTP-based E2E test server (`commands::test::handle_inject_test_output`'s
/// sibling). The frontend `agentNodeStore` refetches the node list on receipt
/// (issue #490 renamed this from `session-created`).
///
/// The wire key is `id` (single-field payload — issue #490 chose brevity over
/// parallelism with the `node_*` events).
///
/// Generated to `src/types/generated/NodeCreatedPayload.ts`; the TS half is
/// imported by `src/stores/agentNodeStore.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "NodeCreatedPayload.ts")]
pub struct NodeCreatedPayload {
    #[ts(as = "i32")]
    pub id: i64,
}

/// Payload of the `node-spawn-completed` Tauri event. Emitted by
/// `start_node_background` when stage-2 (slow work, registers the process
/// with `PROCESS_REGISTRY`) finishes successfully. The frontend flips the
/// node from `pending` to `running`.
///
/// Generated to `src/types/generated/NodeSpawnCompletedPayload.ts`; the TS
/// half is imported by `src/stores/agentNodeStore.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "NodeSpawnCompletedPayload.ts")]
pub struct NodeSpawnCompletedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
}

/// Payload of the `node-spawn-failed` Tauri event. Emitted by
/// `start_node_background` when stage-2 fails (e.g. the PTY could not be
/// opened, the agent CLI rejected its argv). The backend has already
/// updated the DB to `Error` before emitting, so the listener mirrors it.
///
/// Generated to `src/types/generated/NodeSpawnFailedPayload.ts`; the TS half
/// is imported by `src/stores/agentNodeStore.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "NodeSpawnFailedPayload.ts")]
pub struct NodeSpawnFailedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub error: String,
}

// ---------------------------------------------------------------------------
// Provider listing — moved to crate::agent::provider_menu (issue #1052).
//
// The Spawn Menu composition (provider_info_for / provider_info_for_pairing /
// compose_provider_menu / order_providers / order_proxied_children /
// available_providers / list_providers) lives next to its unit tests in
// crate::agent::provider_menu. The mobile HTTP route imports
// `crate::agent::provider_menu::available_providers` directly — no re-export
// remains here.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Spawn / resume
// ---------------------------------------------------------------------------

/// Wire-facing spawn intent for [`spawn_agent`]. Internally tagged so
/// resume vs first-turn prompt vs fresh boot cannot be sent together —
/// the previous `resume: Option<String>` + `prefill: Option<String>`
/// pair silently dropped the prompt whenever a session id was present.
///
/// This is a subset of [`SpawnIntent`]: Issue / PullRequest / Handover
/// stay on their dedicated commands. Generated to
/// `src/types/generated/SpawnAgentIntent.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "SpawnAgentIntent.ts")]
pub enum SpawnAgentIntent {
    Fresh,
    Resume,
    Loop { initial_prompt: String },
}

impl SpawnAgentIntent {
    /// Map the IPC intent onto the orchestrator's [`SpawnIntent`].
    /// Whitespace-only `Loop` degrades to `Fresh` (no first turn).
    pub(crate) fn into_spawn_intent(self) -> SpawnIntent {
        match self {
            Self::Fresh => SpawnIntent::Fresh,
            Self::Resume => SpawnIntent::Resume {
                cause: crate::agent::spawn::ResumeCause::Explicit,
            },
            Self::Loop { initial_prompt } => {
                if initial_prompt.trim().is_empty() {
                    SpawnIntent::Fresh
                } else {
                    SpawnIntent::Loop { initial_prompt }
                }
            }
        }
    }
}

/// IPC payload for [`spawn_agent`]. The invoke object is
/// `{ request: SpawnAgentRequest }`; field names on the payload are
/// camelCase (`sessionId`, `provider`, `intent`, `rows`, `cols`).
///
/// Generated to `src/types/generated/SpawnAgentRequest.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "SpawnAgentRequest.ts")]
pub struct SpawnAgentRequest {
    #[ts(as = "i32")]
    pub session_id: i64,
    pub provider: String,
    pub intent: SpawnAgentIntent,
    #[serde(default)]
    pub rows: Option<u16>,
    #[serde(default)]
    pub cols: Option<u16>,
}

/// Spawn a new agent for the given session. The `provider` argument is
/// resolved by the spawner from the persisted `AgentNode` row.
///
/// `request.intent` (issue #1413 review) is a tagged enum: `Loop` is
/// the first-turn prompt path (same verbatim-prefill intent the circuit
/// spawn-with-prompt path uses). Resume vs prompt vs fresh cannot be
/// combined on the wire.
#[command]
pub async fn spawn_agent(app: AppHandle, request: SpawnAgentRequest) -> Result<(), String> {
    crate::agent::spawn::spawn_with_intent(
        &app,
        SpawnRequest::new(
            request.session_id,
            request.intent.into_spawn_intent(),
            TerminalSize {
                rows: request.rows.unwrap_or(24),
                cols: request.cols.unwrap_or(80),
            },
        ),
    )
    .await
    .map(|_| ())
}

/// Internal implementation shared by spawn_issue_agent and spawn_handover_agent.
/// Takes a pre-fetched `&Mesh` and intent-specific context. Node creation stays
/// in the agent-node service; process policy and prefill construction belong to
/// the spawn intent seam.
///
/// `initial_name` lets the caller seed the node with a meaningful name (e.g.
/// the issue-title slug for `spawn_issue_agent`); handover leaves it `None`
/// and falls back to a random default.
async fn spawn_new_agent_impl(
    app: &AppHandle,
    mesh: &crate::models::Mesh,
    intent: SpawnIntent,
    provider: Option<String>,
    source_issue: Option<i64>,
    initial_name: Option<String>,
) -> Result<crate::models::AgentNode, String> {
    let effective_provider = crate::preferences::resolve_default_provider(
        provider,
        mesh.default_provider.clone(),
        crate::preferences::default_provider(),
    );

    let branch = crate::commands::git::get_default_branch(mesh.path.clone())
        .await;

    let node = crate::services::agent_node::create(
        mesh.id,
        &mesh.path,
        &branch,
        Some(&effective_provider),
        source_issue,
        None,
        None,
        None,
        initial_name.as_deref(),
    )
    .map_err(|e| e.to_string())?;

    let outcome = spawn_with_intent(
        app,
        SpawnRequest::new(node.id, intent, TerminalSize::default()),
    )
    .await?;

    Ok(match outcome {
        SpawnOutcome::Started(node)
        | SpawnOutcome::AlreadyActive(node)
        | SpawnOutcome::Skipped(node) => node,
    })
}

/// Spawn an agent pre-filled with a pointer to a GitHub issue (URL + title hint).
///
/// We deliberately pass just the URL and title, not the full issue body. Shipping
/// a multi-KB markdown body through the Windows PowerShell `-EncodedCommand`
/// argv path is the worst-case input for that pipeline (backticks, code fences,
/// nested quotes) and was the main reason this flow was unreliable on Windows.
/// LLMs can read the URL themselves and they need the link anyway to cite the
/// issue in the closing PR.
///
/// The `--prefill` arg is only passed for providers whose adapter declares
/// `supports_prefill() = true`; others spawn without prefill and log a warning.
#[command]
pub async fn spawn_issue_agent(
    app: AppHandle,
    mesh_id: i64,
    issue_number: i64,
    issue_title: String,
    provider: Option<String>,
) -> Result<crate::models::AgentNode, String> {
    let (mesh, owner, repo) = crate::commands::run_blocking("spawn_issue_agent_mesh", move || {
        let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
        let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(&mesh)
            .map_err(|e| format!("{} — cannot derive issue URL", e))?;
        Ok((mesh, owner, repo))
    })
    .await?;

    let intent = SpawnIntent::Issue(IssueContext {
        owner,
        repo,
        number: issue_number,
        title: issue_title.clone(),
    });
    let initial_name = crate::session_naming::issue_node_name(issue_number, &issue_title);

    let node = spawn_new_agent_impl(
        &app,
        &mesh,
        intent,
        provider,
        Some(issue_number),
        Some(initial_name),
    ).await?;

    tracing::info!("spawn_issue_agent: spawned node {} for issue #{}", node.id, issue_number);
    Ok(node)
}

// The prefill string handed to the agent on GitHub-issue spawn is owned by
// [`crate::agent::spawn::SpawnIntent::initial_prompt`] as of issue #1180 —
// this section used to host the `format_issue_prefill` helper that
// duplicated the logic. The single source of truth lives in
// `agent::spawn::intent` and is reached via
// `SpawnIntent::Issue(context).initial_prompt()` everywhere (desktop draft,
// background launch, Autopilot watcher).

// ---------------------------------------------------------------------------
// Two-stage issue spawn (fast stage-1 + background stage-2)
//
// These two commands split the original `spawn_issue_agent` into a fast DB
// write and a slow background task. The intent is to remove the 5-10s lag
// between clicking "Spawn" in the GitHub Issues dialog and the modal
// closing: the desktop frontend calls `create_issue_node` (which only
// touches the DB) and immediately closes the modal, then fires
// `start_node_background` (which does the slow git/worktree/PTY work)
// without awaiting. The original synchronous `spawn_issue_agent` is kept
// for the mobile HTTP route — its callers tolerate the wait because they
// have no interactive UI to keep responsive.
// ---------------------------------------------------------------------------

/// A new agent node draft returned from the fast stage-1 spawn command.
/// The frontend holds onto `prefill` and passes it back to
/// `start_node_background` (no DB round-trip for the prefill — it's
/// transient and <500 bytes).
///
/// Generated to src/types/generated/IssueNodeDraft.ts (issue #404). The
/// `#[serde(flatten)]` + `#[ts(flatten)]` pair makes the wire shape the
/// flat merge of `AgentNode` + `prefill`, matching the hand-typed
/// `interface IssueNodeDraft extends AgentNode` the wrapper used to carry.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "IssueNodeDraft.ts")]
pub struct IssueNodeDraft {
    #[serde(flatten)]
    #[ts(flatten)]
    pub node: crate::models::AgentNode,
    pub prefill: String,
}

/// Fast acceptance of a GitHub-issue spawn. The row is committed and returned
/// immediately; the same backend intent seam owns the slow worktree/PTY launch
/// and emits the completion/failure event.
#[command]
pub fn create_issue_node(
    app: AppHandle,
    mesh_id: i64,
    issue_number: i64,
    issue_title: String,
    provider: Option<String>,
) -> Result<IssueNodeDraft, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(&mesh)
        .map_err(|e| format!("{} — cannot derive issue URL", e))?;
    let intent = SpawnIntent::Issue(IssueContext {
        owner,
        repo,
        number: issue_number,
        title: issue_title.clone(),
    });
    // Issue #1180 — `initial_prompt()` is the single source of truth for
    // the GitHub-issue prefill; the same intent is then passed to
    // `spawn_with_intent` below so the background launch gets a
    // byte-identical string. `Issue(...)` always has a prompt, so
    // `unwrap_or_default()` is unreachable in practice but kept as a
    // defensive fallback matching the wire-shape contract.
    let prefill = intent
        .initial_prompt()
        .map(|p| p.into_string())
        .unwrap_or_default();
    // Issue #111: seed the node with a `gh{N}-{slug}` name (mirrors
    // `spawn_issue_agent` so the desktop modal and mobile route produce
    // identical names).
    let initial_name = crate::session_naming::issue_node_name(issue_number, &issue_title);

    let effective_provider = crate::preferences::resolve_default_provider(
        provider,
        mesh.default_provider.clone(),
        crate::preferences::default_provider(),
    );
    let branch = crate::commands::git::get_default_branch_blocking(mesh.path.clone())
        .unwrap_or_else(|_| "main".to_string());

    let node = crate::services::agent_node::create_pending(
        mesh.id,
        &mesh.path,
        &branch,
        Some(&effective_provider),
        Some(issue_number),
        None,
        None,
        Some(&initial_name),
    )
    .map_err(|e| e.to_string())?;

    let _ = app.emit("node-created", NodeCreatedPayload { id: node.id });
    let app_for_spawn = app.clone();
    let node_id = node.id;
    tauri::async_runtime::spawn(async move {
        if let Err(error) = spawn_with_intent(
            &app_for_spawn,
            SpawnRequest::new(node_id, intent, TerminalSize::default()),
        )
        .await
        {
            tracing::error!("create_issue_node: node {} failed: {}", node_id, error);
        }
    });

    tracing::info!(
        "create_issue_node: accepted pending node {} for issue #{} on mesh {}",
        node.id,
        issue_number,
        mesh_id
    );

    Ok(IssueNodeDraft { node, prefill })
}

/// One Autopilot-managed node's pipeline position, for the header pill.
/// `state` is the typed `autopilot_runs.state` union
/// (`implementing`/`finishing`/`completed`/`failed`/`merged`).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "AutopilotRunState.ts")]
pub struct AutopilotRunStateRow {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub state: crate::db::AutopilotRunState,
}

/// Every live (non-archived) Autopilot run, so the frontend can badge
/// piloted nodes. Fetched alongside the node list; kept fresh by the
/// `autopilot-*` lifecycle events triggering a refetch.
#[command]
pub fn list_autopilot_runs() -> Result<Vec<AutopilotRunStateRow>, String> {
    db::list_autopilot_run_states()
        .map(|rows| {
            rows.into_iter()
                .map(|(node_id, state)| AutopilotRunStateRow { node_id, state })
                .collect()
        })
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Two-stage PR spawn (fast stage-1 + background stage-2) — issue #420
//
// Mirror of the issue-spawn two-stage flow above. Spawns an agent that
// checks out the PR's head branch and starts reviewing or iterating on it.
// The worktree is created off `origin/<head_ref>` rather than the mesh's
// `base_ref`, so the agent lands on the same commits the PR is built from.
// Reuses `IssueNodeDraft` for the return type (the wire shape is the same:
// flattened `AgentNode` + `prefill`); the PR-spawn flow only diverges in
// the *row* (`source_pr` is set, `branch` is the head ref) and in stage-2's
// `git fetch origin <head_ref>` worktree adoption.
// ---------------------------------------------------------------------------

// The prefill string handed to the agent on PR spawn is owned by
// [`crate::agent::spawn::SpawnIntent::initial_prompt`] as of issue #1180 —
// this section used to host the `format_pr_prefill` helper that duplicated
// the logic. The single source of truth lives in `agent::spawn::intent` and
// is reached via `SpawnIntent::PullRequest(context).initial_prompt()`
// everywhere (desktop draft, background launch).

/// Validate and normalise the inputs to a PR-spawn request.
///
/// Two independent rejections (issue #471):
///
/// 1. **Fork-info completeness** — `head_repo_owner` and `head_repo_clone_url`
///    must both be present or both absent. A fork PR with only one is
///    unfixable from the spawn path: stage-2 needs the clone URL to register
///    `git remote add fork-<login>` and the owner login for the remote alias.
///    (The original #420 gate rejected ALL fork PRs by checking
///    `head_ref.is_empty()`; that was correct at the time because the wire
///    shape didn't carry fork info. Issue #443 added fork-info fields and the
///    "fork info present" XOR check replaced it.)
///
/// 2. **Empty `head_ref`** — unconditionally rejected. Stage-2
///    (`spawn_agent_inner`) reads `node.branch` (= `head_ref`) to check out
///    the PR's commits; an empty branch lands on the mesh's `base_ref` or
///    fails outright, giving the user a wrong-commit agent with no signal.
///    This is independent of fork info: a request with `head_ref=""` AND
///    populated fork fields (e.g. a stale `head` object from a previously
///    rendered fork row) must also be rejected.
///
/// Surrounding whitespace on every input is trimmed:
/// - `head_ref`: trimmed and returned — a padded `" feat/x "` lands on
///   `node.branch` as `"feat/x"` so stage-2's `git fetch origin <ref>`
///   matches the real ref. Without this, a whitespace-padded `head_ref`
///   passes the empty check but `git checkout` fails on the persisted
///   branch.
/// - fork-info strings: trimmed and the empty-after-trim case collapses
///   to `None` so `Some(" alice ")` and `Some("alice")` reach the service
///   layer as identical values; `Some(" ")` collapses to `None` (no fork
///   info).
///
/// Returns the cleaned `(head_ref, head_repo_owner, head_repo_clone_url)`
/// triple on success so the caller can forward them without re-trimming.
/// Pure function — unit-tested exhaustively against the truth table in
/// `commands::agent_tests`.
pub(crate) fn validate_pr_spawn_inputs(
    head_ref: &str,
    head_repo_owner: Option<String>,
    head_repo_clone_url: Option<String>,
) -> Result<(String, Option<String>, Option<String>), String> {
    let head_ref = head_ref.trim().to_string();
    let head_repo_owner = head_repo_owner
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let head_repo_clone_url = head_repo_clone_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if (head_repo_owner.is_some()) ^ (head_repo_clone_url.is_some()) {
        return Err(
            "PR's fork info is incomplete (head_repo_owner and head_repo_clone_url \
             must both be present, or both absent). Reload the PR list and retry."
                .to_string(),
        );
    }

    if head_ref.is_empty() {
        return Err(
            "PR's head_ref is required (got an empty value). \
             Reload the PR list so the panel can re-fetch the head branch, \
             then retry the spawn."
                .to_string(),
        );
    }

    Ok((head_ref, head_repo_owner, head_repo_clone_url))
}

/// Fast stage-1 of the PR-spawn flow. Creates a `Pending` agent node row
/// with the originating PR number stamped on `source_pr` and the PR's head
/// ref stored in `branch`. The caller is expected to invoke
/// `start_node_background` to do the slow work (git fetch the head ref +
/// worktree create + PTY spawn), which in turn uses `source_pr` to override
/// the worktree's `base_ref` so the agent lands on the PR's actual commits
/// instead of the mesh's base ref.
///
/// Fork PRs are supported (issue #443, follow-up to #36). When the PR's
/// `head_repo_owner` differs from the mesh's destination owner, the spawn
/// path adds the fork as a remote (`fork-<login>`) and fetches the head
/// ref from it instead of `origin`. The owner / clone URL are recorded on
/// the node row so the stage-2 work has them without an extra round-trip.
///
/// Still refuses a fork PR whose fork info is missing (an old frontend or a
/// partial API response) — without the clone URL we can't register the
/// remote, and silently spawning on the mesh's `base_ref` would land the
/// agent on the wrong commits.
///
/// Returns the same `IssueNodeDraft` wire shape as `create_issue_node`; the
/// frontend reuses the generated `IssueNodeDraft.ts` import (the structure
/// is identical: flattened `AgentNode` + `prefill`).
///
/// Thin Tauri wrapper — all logic lives in [`create_pr_node_impl`], which is
/// `pub(crate)` so unit tests can exercise the gate + DB + name/prefill
/// + provider-resolution seams without a Tauri `AppHandle` (which is only
/// needed to emit the `node-created` event here). Mirrors the
/// `validate_pr_spawn_inputs` extraction in #471 — same pattern: a pure
/// inner function is testable in isolation, the `#[command]` macro wraps it
/// with the AppHandle-bound event emission.
#[command]
#[allow(clippy::too_many_arguments)]
pub fn create_pr_node(
    app: tauri::AppHandle,
    mesh_id: i64,
    pr_number: i64,
    pr_title: String,
    head_ref: String,
    head_sha: String,
    provider: Option<String>,
    head_repo_owner: Option<String>,
    head_repo_clone_url: Option<String>,
) -> Result<IssueNodeDraft, String> {
    // Issue #1180 — the impl now returns the `SpawnIntent::PullRequest`
    // it built (owner/repo resolved from the mesh + the supplied
    // pr_number/pr_title). The wrapper reuses that single intent for
    // `spawn_with_intent` so the desktop draft, the background launch,
    // and the (future) marker derivation all agree on byte-identical
    // prefill text. Previously the wrapper re-built the intent here
    // and the impl called `format_pr_prefill` independently — a silent
    // drift waiting to happen.
    let (draft, intent) = create_pr_node_impl(
        mesh_id,
        pr_number,
        pr_title,
        head_ref,
        head_sha,
        provider,
        head_repo_owner,
        head_repo_clone_url,
    )?;
    let _ = app.emit(
        "node-created",
        NodeCreatedPayload { id: draft.node.id },
    );

    let app_for_spawn = app.clone();
    let node_id = draft.node.id;
    tauri::async_runtime::spawn(async move {
        if let Err(error) = spawn_with_intent(
            &app_for_spawn,
            SpawnRequest::new(node_id, intent, TerminalSize::default()),
        )
        .await
        {
            tracing::error!("create_pr_node: node {} failed: {}", node_id, error);
        }
    });

    Ok(draft)
}

/// Inner implementation of [`create_pr_node`] — see that function's doc
/// comment for the high-level overview. Exposed as `pub(crate)` so the
/// integration tests in `mod tests` can pin the seams (gate, mesh lookup,
/// name+prefill wiring, provider resolution, SHA exact-pinning) without
/// needing a Tauri `AppHandle`. Side effects limited to the DB write
/// (`create_pending_with_source_pr_fork`) and a `tracing::info!` log —
/// the `node-created` Tauri event is the only AppHandle-bound concern,
/// and it stays in the command wrapper.
///
/// Returns the `(draft, intent)` pair so the wrapper can hand the
/// **same** `SpawnIntent` to [`spawn_with_intent`] (issue #1180). The
/// intent is built once from `(owner, repo, pr_number)` (with `pr_title`
/// used solely for session naming via `pr_node_name`) and
/// the prefill surfaced on the desktop draft comes from
/// [`SpawnIntent::initial_prompt`] — the single source of truth shared
/// with the background launch path and the Autopilot watcher.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_pr_node_impl(
    mesh_id: i64,
    pr_number: i64,
    pr_title: String,
    head_ref: String,
    head_sha: String,
    provider: Option<String>,
    head_repo_owner: Option<String>,
    head_repo_clone_url: Option<String>,
) -> Result<(IssueNodeDraft, SpawnIntent), String> {
    // Issue #471 — the gate is split into two independent rejections. See
    // `validate_pr_spawn_inputs` for the truth table; both guards are tested
    // exhaustively in `commands::agent_tests`. The helper also returns the
    // trimmed `head_ref` so a whitespace-padded ref (e.g. `" feat/x "` from
    // an upstream payload quirk) is normalised before it reaches the
    // service layer as `node.branch` — without the trim, stage-2's
    // `git fetch origin <ref>` would fail on the persisted branch.
    let (head_ref, head_repo_owner, head_repo_clone_url) = validate_pr_spawn_inputs(
        &head_ref,
        head_repo_owner,
        head_repo_clone_url,
    )?;

    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(&mesh)
        .map_err(|e| format!("{} — cannot derive PR URL", e))?;

    // Build the `SpawnIntent` ONCE here so the prefill the desktop draft
    // surfaces (and the `spawn_with_intent` background task forwards to the
    // harness) come from the same `initial_prompt()` source. Issue #1180
    // closed the previous `format_pr_prefill` helper duplication — three
    // sites (commands, services/autopilot, autopilot/launch) used to
    // recompute the prompt independently and could silently drift.
    let intent = SpawnIntent::PullRequest(PullRequestContext {
        owner,
        repo,
        number: pr_number,
    });
    let prefill = intent
        .initial_prompt()
        .map(|p| p.into_string())
        .unwrap_or_default();

    // Seed the node with a `pr{N}-{slug}` name (mirrors `issue_node_name`) so
    // the user can identify it in the mesh list from the moment the row
    // appears. Falls back to a random default if the title doesn't yield a
    // valid slug; the `pr` prefix is still applied so the user can spot the
    // originating PR at a glance.
    let initial_name = crate::session_naming::pr_node_name(pr_number, &pr_title);

    let effective_provider = crate::preferences::resolve_default_provider(
        provider,
        mesh.default_provider.clone(),
        crate::preferences::default_provider(),
    );

    // Issue #444 — `head_sha` is the exact-pinning handle. We persist it as
    // `source_pr_pinned_sha` so `spawn_agent_inner` can verify the local
    // `origin/<head_ref>` SHA matches after `git fetch` and emit a
    // `pr_sha_drift` warning via `mesh-sync-warning` if the PR was
    // force-pushed / rebased between click-time and spawn-time. An empty
    // `head_sha` (partial GitHub response, fork payload) skips the drift
    // check — same fail-open semantics as `pr_head_unfetchable`.
    let pinned_sha_opt: Option<&str> = if head_sha.is_empty() { None } else { Some(&head_sha) };

    let node = crate::services::agent_node::create_pending_with_source_pr_fork(
        mesh.id,
        &mesh.path,
        &head_ref,
        Some(&effective_provider),
        None,                 // source_issue
        Some(pr_number),      // source_pr — the key field for stage-2 worktree adoption
        pinned_sha_opt,       // source_pr_pinned_sha — exact-pinning handle (issue #444)
        Some(&initial_name),
        head_repo_owner.as_deref(),  // fork meta (issue #443) — None for same-repo PRs
        head_repo_clone_url.as_deref(),
    )
    .map_err(|e| e.to_string())?;

    tracing::info!(
        "create_pr_node: created pending node {} for PR #{} (head_ref={}, head_sha={}, head_repo_owner={:?}) on mesh {}",
        node.id,
        pr_number,
        head_ref,
        head_sha,
        head_repo_owner,
        mesh_id
    );

    Ok((IssueNodeDraft { node, prefill }, intent))
}

/// Spawn a new agent node pre-filled with selected text from a parent terminal.
/// Used by the "Handover to new Node" context menu option.
#[command]
pub async fn spawn_handover_agent(
    app: AppHandle,
    mesh_id: i64,
    prefill: String,
    provider: Option<String>,
) -> Result<crate::models::AgentNode, String> {
    let mesh = crate::commands::run_blocking("spawn_handover_agent_mesh", move || {
        db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())
    })
    .await?;
    let node = spawn_new_agent_impl(
        &app,
        &mesh,
        SpawnIntent::Handover {
            selected_text: prefill,
        },
        provider,
        None,
        None,
    ).await?;

    tracing::info!("spawn_handover_agent: spawned node {} via handover", node.id);
    Ok(node)
}

/// Recover missing harness identities, then auto-resume suspended nodes.
/// Called by the frontend on startup after event listeners are ready.
#[command]
pub async fn auto_resume_agent_nodes(app: AppHandle) -> Result<Vec<i64>, String> {
    let nodes = crate::commands::run_blocking("auto_resume_agent_nodes", || {
        db::list_suspended_nodes().map_err(|e| e.to_string())
    })
    .await?;

    if nodes.is_empty() {
        tracing::info!("auto_resume_agent_nodes: no suspended nodes to resume");
        return Ok(vec![]);
    }

    tracing::info!("auto_resume_agent_nodes: resuming {} nodes", nodes.len());
    let mut resumed: Vec<i64> = Vec::new();

    for node in &nodes {
        if let Err(error) = crate::services::session_recovery::recover_suspended_node(node.clone()).await {
            tracing::warn!("auto_resume_agent_nodes: identity recovery failed for {}: {error}", node.id);
        }
        match spawn_with_intent(
            &app,
            SpawnRequest::new(
                node.id,
                SpawnIntent::Resume {
                    cause: crate::agent::spawn::ResumeCause::Startup,
                },
                TerminalSize::default(),
            ),
        )
        .await
        {
            Ok(SpawnOutcome::Started(_) | SpawnOutcome::AlreadyActive(_)) => {
                resumed.push(node.id);
                tracing::info!("auto_resume_agent_nodes: resumed node {}", node.id);
            }
            Ok(SpawnOutcome::Skipped(_)) => {
                tracing::info!("auto_resume_agent_nodes: skipped node {}", node.id);
            }
            Err(e) => {
                tracing::error!(
                    "auto_resume_agent_nodes: failed to resume node {}: {}",
                    node.id,
                    e
                );
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(resumed)
}

// ---------------------------------------------------------------------------
// Process-lifecycle Tauri commands moved to crate::agent::process (issue #1052).
//
// The deep module owns `kill_agent` / `kill_agent_blocking` /
// `kill_all_sessions` (renamed from `kill_all_agents` for naming consistency
// with `kill_session`) / `resize_agent` / `write_to_agent` /
// `write_to_agent_signal_blocking` / `write_to_agent_blocking` (cfg(test)) /
// `send_to_agent` / `is_agent_running` / `AgentDebugState` /
// `debug_list_agents` / `CrashSnapshot` / `debug_crash_snapshot` plus the
// `should_skip_attention_signals` / `provider_is_plain_terminal` helpers.
// The dependency arrow from `services::agent_node.rs` and `agent::spawn.rs`
// (the inverted calls #1052 closes) now points at
// `agent::process::kill_agent` directly.
// ---------------------------------------------------------------------------


#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;

    // ----- create_pr_node_impl seams (issue #445) -----------------
    //
    // `create_pr_node` is a Tauri command that needs an `AppHandle` only to
    // emit `node-created`. The inner `create_pr_node_impl` is the unit of
    // logic — gate + DB lookup + name/prefill + provider chain + SHA pin —
    // and is what these tests pin.
    //
    // The provider-menu derivation (`order_providers`,
    // `compose_provider_menu`, `provider_info_for`, etc.) and its tests
    // migrated to `crate::agent::provider_menu::tests` (issue #1052);
    // `provider_is_plain_terminal` and the matching two tests + the
    // `write_to_agent_blocking` regression test migrated to
    // `crate::agent::process::tests`.

    // ----- create_pr_node_impl seams (issue #445) -----------------------
    //
    // `create_pr_node` is a Tauri command that needs an `AppHandle` only to
    // emit `node-created`. The inner `create_pr_node_impl` is the unit of
    // logic — gate + DB lookup + name/prefill + provider chain + SHA pin —
    // and is what these tests pin.
    //
    // These tests stand up a real on-disk SQLite DB via `db::init` (the
    // global `DB` OnceCell is one-shot per process, so we serialise on
    // `TEST_LOCK` and only initialise on the first test). The `meshes.path`
    // column is UNIQUE — we point each test at its own temp git repo, and
    // `resolve_github_owner_repo` only needs `origin` set to a GitHub URL
    // for the mesh-lookup seam to produce (owner, repo).
    //
    // The exact-pinning / fork-meta / provider / name assertions all read
    // back the persisted `AgentNode` row via `IssueNodeDraft.node`, so a
    // refactor that drops one of the fields surfaced in the function body
    // (e.g. the SHA pin, the fork fields, the `pr{N}-` prefix) would fail
    // the corresponding test rather than silently regress to the legacy
    // `base_ref`-fallback path on stage-2.

    /// Serialises tests that touch the global DB — `db::init` is a one-shot
    /// OnceCell and the test files share one process. Held for the duration
    /// of every test in this section.
    static PR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// One-shot guard for `db::init`. The first test to run triggers
    /// initialisation; subsequent tests see the Once has fired and skip.
    /// `std::sync::Once` is process-scoped, so this is automatically
    /// correct across the whole `cargo test` invocation — unlike a
    /// marker file, which persists across runs and would let the
    /// (process-fresh) DB OnceCell stay `None` on the next run.
    ///
    /// We combine this with a `db::is_initialized()` check so we don't
    /// race with other test files (e.g. `db::mesh_tests`) that also
    /// call `db::init` directly: whichever test runs first wins, and
    /// the other tests see "already initialised" and skip without
    /// unwrapping the error (which is what those tests do, and why
    /// they break if we beat them to it).
    static DB_INIT: std::sync::Once = std::sync::Once::new();

    /// Per-process scratch path for the test DB. `db::init` is one-shot,
    /// so the first test to call this picks a path and every later
    /// call gets the same one — fine because the global DB static
    /// remembers the result regardless of path.
    ///
    /// The path ends in `.db` because `db::init` calls
    /// `Connection::open(path)`, which expects a *file* (not a
    /// directory). A bare `temp_dir()/buildmesh_pr_node_test_<pid>` would
    /// create a directory, and `Connection::open` would fail with
    /// "Not a database" (or, on first open, succeed but then
    /// misbehave). `db::mesh_tests` uses the same `*.db` suffix — the
    /// shape is the contract.
    fn pr_test_db_path() -> std::path::PathBuf {
        use std::sync::OnceLock;
        static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let p = std::env::temp_dir().join(format!(
                "buildmesh_pr_node_test_{}.db",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&p);
            p
        })
        .clone()
    }

    /// `db::init` the global DB if it hasn't been already. Called from
    /// each test that touches `create_pr_node_impl` (the function reads
    /// `db::get_mesh_by_id` which panics on an uninitialised DB).
    ///
    /// The `DB` static is a one-shot `OnceCell` — if another test in the
    /// same binary (e.g. `db::mesh_tests`) already initialised it to a
    /// different path, we leave it alone: the schema is identical
    /// (always migrated to the current `SCHEMA_VERSION`), and
    /// `db::create_mesh` / `db::get_mesh_by_id` operate on whichever
    /// DB is global — so we share whatever the other test set up. The
    /// `db::is_initialized()` check is the polite form of "don't
    /// trample a peer's init" so `db::mesh_tests` doesn't break on
    /// `.unwrap()`.
    fn ensure_pr_db() {
        if crate::db::is_initialized() {
            return;
        }
        DB_INIT.call_once(|| {
            let _ = crate::db::init(&pr_test_db_path());
        });
    }

    /// Create a temp git repo with a known `origin` URL, and insert a
    /// `meshes` row pointing at it. Returns `(temp_dir, mesh_id)` — caller
    /// MUST hold `temp_dir` for the test's lifetime (its `Drop` wipes the
    /// path). Each test gets its own dir so the `meshes.path` UNIQUE
    /// constraint is satisfied.
    fn create_test_mesh(name: &str, origin_url: &str) -> (tempfile::TempDir, i64) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().to_path_buf();
        // `Repository::init` returns a handle to the new repo — use it
        // directly for `remote_set_url` rather than re-opening. The URL
        // itself is never fetched from in these tests; we only need
        // `parse_owner_repo` to recognise it as GitHub-shaped.
        let repo = git2::Repository::init(&path).expect("git init");
        repo.remote_set_url("origin", origin_url)
            .expect("remote_set_url");
        let mesh = crate::db::create_mesh(name, path.to_str().unwrap())
            .expect("create_mesh");
        (tmp, mesh.id)
    }

    /// The gate is split across `validate_pr_spawn_inputs` (truth table in
    /// `commands::agent_tests`) and `db::get_mesh_by_id` (mesh existence).
    /// Pin the "fork-PR guard" half here: a request with `head_ref = ""`
    /// must short-circuit at the gate, BEFORE we even look at the DB —
    /// the spawn path can't check out a branch named `""` on stage-2.
    ///
    /// The user-facing wording matters: the panel renders this verbatim
    /// and the user needs to know to "Reload the PR list" rather than
    /// "fork PRs aren't supported" (the legacy wording pre-#443).
    #[test]
    fn create_pr_node_impl_rejects_empty_head_ref() {
        let _guard = PR_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        ensure_pr_db();

        let err = create_pr_node_impl(
            1,           // mesh_id — irrelevant; gate short-circuits before DB read
            420,
            "any title".to_string(),
            "".to_string(),
            "".to_string(),
            None,
            None,
            None,
        )
        .expect_err("head_ref=\"\" must be rejected (cannot check out branch \"\")");

        assert!(
            err.contains("head_ref") && err.to_lowercase().contains("required"),
            "error must name the missing head_ref, got: {:?}",
            err
        );
        // Must NOT use the pre-#443 wording ("fork PRs not supported") —
        // that's the legacy gate, the post-#443 path accepts fork PRs
        // that carry full fork info.
        assert!(
            !err.to_lowercase().contains("fork prs not supported"),
            "error must use the post-#443 wording, not the legacy 'fork PRs not supported': {:?}",
            err
        );
    }

    /// The XOR gate in `validate_pr_spawn_inputs` (issue #471) must
    /// surface as a "fork info is incomplete" error from the public
    /// function — not silently proceed (which would spawn on the wrong
    /// commits because stage-2 can't register a fork remote).
    #[test]
    fn create_pr_node_impl_rejects_incomplete_fork_info() {
        let _guard = PR_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        ensure_pr_db();

        let err = create_pr_node_impl(
            1,
            420,
            "any title".to_string(),
            "feat/443-fork".to_string(),
            "".to_string(),
            None,
            Some("alice".to_string()),     // owner present
            None,                          // clone_url MISSING — XOR violation
        )
        .expect_err("owner without clone_url must be rejected");

        assert!(
            err.contains("fork info is incomplete"),
            "error must name the fork-info completeness gate, got: {:?}",
            err
        );
    }

    /// A `mesh_id` that doesn't exist in the DB must propagate as an
    /// error from `db::get_mesh_by_id` — not panic, not silently
    /// fall through to `db::create_mesh`. The spawn flow has no recovery
    /// here; surfacing the error to the panel is the only sane behaviour.
    #[test]
    fn create_pr_node_impl_rejects_unknown_mesh_id() {
        let _guard = PR_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        ensure_pr_db();

        // Use a mesh id that the freshly-init'd DB cannot possibly have
        // (no `create_mesh` was called in this test).
        let err = create_pr_node_impl(
            999_999_999,
            420,
            "any title".to_string(),
            "feat/420-pr-spawn".to_string(),
            "".to_string(),
            None,
            None,
            None,
        )
        .expect_err("unknown mesh_id must be rejected");

        // `db::get_mesh_by_id` returns a rusqlite error containing
        // "Query returned no rows" when the id is missing — that
        // wording is the diagnostic, not a contract the panel parses.
        // We just assert it's a non-empty error string.
        assert!(
            !err.is_empty(),
            "mesh-not-found must produce a non-empty error, got empty string"
        );
    }

    /// The seam the issue calls out most explicitly: the returned
    /// `IssueNodeDraft` must carry the prefill from `SpawnIntent::initial_prompt`
    /// AND the node name from `pr_node_name`. Both pieces are computed
    /// from the SAME `(pr_number, pr_title)` pair, so a refactor that
    /// accidentally passes a different value to one of them (e.g. a
    /// stale `pr_title` from a closure) would break the user's mental
    /// model: the agent gets a prefill referring to a different PR
    /// than the one whose name appears in the sidebar. (Issue #1180 —
    /// the prefill helper consolidation means we now build the intent
    /// and read `initial_prompt()` here rather than calling
    /// `format_pr_prefill`.)
    #[test]
    fn create_pr_node_impl_wires_name_and_prefill_seam() {
        let _guard = PR_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        ensure_pr_db();

        let (_tmp, mesh_id) = create_test_mesh(
            "pr-seam-test",
            "https://github.com/alondero/buildmesh.git",
        );

        let pr_number: i64 = 445;
        let pr_title = "test(pr-spawn): Rust unit tests for create_pr_node + fetch_single_ref";

        let (draft, intent) = create_pr_node_impl(
            mesh_id,
            pr_number,
            pr_title.to_string(),
            "feat/445-pr-spawn".to_string(),
            "0123456789abcdef0123456789abcdef01234567".to_string(),
            None,
            None,
            None,
        )
        .expect("create_pr_node_impl with valid inputs must succeed");

        // Prefill seam: matches the SpawnIntent's `initial_prompt()`
        // exactly (issue #1180 — the single source of truth). We
        // construct the same `SpawnIntent::PullRequest` the wrapper
        // would use for the background launch and compare. A divergence
        // here would mean the agent gets a different PR URL on the
        // desktop draft vs the background launch.
        let expected_prefill = intent
            .initial_prompt()
            .map(|p| p.into_string())
            .expect("PullRequest intent always has an initial prompt");
        assert_eq!(
            draft.prefill, expected_prefill,
            "returned prefill must match SpawnIntent::initial_prompt exactly — \
             a divergence means the agent gets the wrong PR URL"
        );

        // Name seam: matches `pr_node_name` (the `pr{N}-` prefix) and
        // matches what the service layer persisted on `node.name`.
        let expected_name = crate::session_naming::pr_node_name(pr_number, pr_title);
        assert_eq!(
            draft.node.name, expected_name,
            "returned node.name must match pr_node_name exactly — \
             a divergence means the sidebar shows a different PR than the agent is reviewing"
        );
        // Sanity: the name carries the `pr` prefix (vs `gh` for issue-spawn).
        assert!(
            draft.node.name.starts_with("pr445-"),
            "PR-spawned node name must use the pr{{N}}- prefix, got: {:?}",
            draft.node.name
        );

        // Also: the head_ref is persisted on `node.branch` (stage-2
        // reads it from there, not from the original input).
        assert_eq!(
            draft.node.branch, "feat/445-pr-spawn",
            "head_ref must be persisted on node.branch for stage-2 worktree adoption"
        );
        assert_eq!(
            draft.node.source_pr,
            Some(pr_number),
            "source_pr must be set to the originating PR number"
        );
    }

    /// `resolve_default_provider` is layered: explicit (caller) → per-mesh
    /// (DB) → app-wide → "anthropic" fallback. The PR-spawn path
    /// threads `(caller, mesh.default_provider, default_provider())`
    /// through — pin each tier so a refactor that swaps the order
    /// (e.g. accidentally calls `default_provider()` BEFORE the
    /// per-mesh value) surfaces as a test failure.
    #[test]
    fn create_pr_node_impl_resolves_provider_chain() {
        let _guard = PR_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        ensure_pr_db();

        // Case 1: explicit caller value wins (no mesh, no app default).
        let (_tmp, mesh_id) =
            create_test_mesh("pr-provider-explicit", "https://github.com/x/y.git");
        let draft = create_pr_node_impl(
            mesh_id,
            1,
            "t".to_string(),
            "feat/a".to_string(),
            "".to_string(),
            Some("minimax".to_string()), // explicit
            None,
            None,
        )
        .map(|(draft, _intent)| draft)
        .expect("explicit provider must be accepted");
        assert_eq!(
            draft.node.provider, "minimax",
            "explicit caller value must override mesh/app defaults"
        );
        // Cleanup so the next sub-case can use a fresh mesh with the
        // same path (UNIQUE constraint).
        crate::db::delete_mesh(mesh_id).expect("delete_mesh");

        // Case 2: per-mesh default wins when caller is absent. We
        // mutate the mesh row's `default_provider` via direct SQL
        // (no app-level setter for it; the column is set on insert via
        // `meshes.default_provider` and only the React UI mutates it
        // through `commands::mesh_properties`, not a `db::` helper).
        let (_tmp2, mesh_id2) =
            create_test_mesh("pr-provider-mesh", "https://github.com/x/y.git");
        let db = crate::db::write_conn();
        db.execute(
            "UPDATE meshes SET default_provider = ?1 WHERE id = ?2",
            rusqlite::params!["agy", mesh_id2],
        )
        .expect("set default_provider");
        drop(db);
        let draft = create_pr_node_impl(
            mesh_id2,
            2,
            "t".to_string(),
            "feat/b".to_string(),
            "".to_string(),
            None, // no explicit — fall through
            None,
            None,
        )
        .map(|(draft, _intent)| draft)
        .expect("mesh default must be accepted");
        assert_eq!(
            draft.node.provider, "agy",
            "per-mesh default must win when no explicit caller value is given"
        );
    }

    /// Issue #444 — the SHA exact-pinning seam. A non-empty `head_sha`
    /// must be persisted on `node.source_pr_pinned_sha` so stage-2 can
    /// compare it against the post-fetch local SHA and emit a
    /// `pr_sha_drift` `mesh-sync-warning` if the PR was force-pushed.
    /// An empty `head_sha` (partial GitHub response) must persist as
    /// `None` so the drift check is skipped (fail-open semantics).
    ///
    /// Each case uses its own tempdir (different `meshes.path`), so the
    /// `UNIQUE(path)` constraint is satisfied without any explicit DB
    /// cleanup — the rows just accumulate for the test process and get
    /// swept at the global tempdir's `Drop`.
    #[test]
    fn create_pr_node_impl_persists_pinned_sha_for_drift_check() {
        let _guard = PR_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        ensure_pr_db();

        // Case A: non-empty head_sha persists verbatim.
        let (_tmp_a, mesh_id_a) =
            create_test_mesh("pr-sha-pin-a", "https://github.com/x/y.git");
        let sha = "abcdef0123456789abcdef0123456789abcdef01";
        let draft_a = create_pr_node_impl(
            mesh_id_a,
            1,
            "t".to_string(),
            "feat/a".to_string(),
            sha.to_string(),
            None,
            None,
            None,
        )
        .map(|(draft, _intent)| draft)
        .expect("non-empty head_sha must be accepted");
        assert_eq!(
            draft_a.node.source_pr_pinned_sha.as_deref(),
            Some(sha),
            "non-empty head_sha must persist as source_pr_pinned_sha for stage-2 drift check"
        );

        // Case B: empty head_sha persists as None.
        let (_tmp_b, mesh_id_b) =
            create_test_mesh("pr-sha-pin-b", "https://github.com/x/y.git");
        let draft_b = create_pr_node_impl(
            mesh_id_b,
            2,
            "t".to_string(),
            "feat/b".to_string(),
            "".to_string(),
            None,
            None,
            None,
        )
        .map(|(draft, _intent)| draft)
        .expect("empty head_sha must be accepted (drift check skipped)");
        assert_eq!(
            draft_b.node.source_pr_pinned_sha, None,
            "empty head_sha must persist as None (skip drift check, fail-open)"
        );
    }

    // ProviderInfo.resumable flag tests (`available_providers_marks_*` and
    // `provider_info_marks_*_as_resumable`) and `write_to_agent_blocking_unknown_*`
    // migrated with their fns to `crate::agent::provider_menu::tests` and
    // `crate::agent::process::tests` (issue #1052).

    #[test]
    fn spawn_agent_intent_fresh_maps_to_orchestrator_fresh() {
        assert_eq!(
            super::SpawnAgentIntent::Fresh.into_spawn_intent(),
            SpawnIntent::Fresh
        );
        assert_eq!(
            super::SpawnAgentIntent::Loop {
                initial_prompt: "   ".into()
            }
            .into_spawn_intent(),
            SpawnIntent::Fresh,
            "whitespace-only Loop is not a first turn"
        );
    }

    #[test]
    fn spawn_agent_intent_loop_maps_to_orchestrator_loop() {
        assert_eq!(
            super::SpawnAgentIntent::Loop {
                initial_prompt: "fix the flaky test".into()
            }
            .into_spawn_intent(),
            SpawnIntent::Loop {
                initial_prompt: "fix the flaky test".into(),
            },
        );
    }

    #[test]
    fn spawn_agent_intent_resume_maps_to_explicit_resume() {
        assert_eq!(
            super::SpawnAgentIntent::Resume.into_spawn_intent(),
            SpawnIntent::Resume {
                cause: crate::agent::spawn::ResumeCause::Explicit,
            },
        );
    }

    #[test]
    fn spawn_agent_intent_rejects_resume_plus_prompt_on_the_wire() {
        // The tagged enum cannot represent resume+loop at once. Pin the
        // three legal shapes so a future flatten back into Option pairs
        // fails this test rather than silently dropping the prompt.
        let fresh = serde_json::to_value(super::SpawnAgentIntent::Fresh).unwrap();
        let resume = serde_json::to_value(super::SpawnAgentIntent::Resume).unwrap();
        let loop_ = serde_json::to_value(super::SpawnAgentIntent::Loop {
            initial_prompt: "go".into(),
        })
        .unwrap();
        assert_eq!(fresh["type"], "fresh");
        assert_eq!(resume["type"], "resume");
        assert_eq!(loop_["type"], "loop");
        assert!(loop_.get("initial_prompt").is_some());
        assert!(fresh.get("initial_prompt").is_none());
        assert!(resume.get("initial_prompt").is_none());
    }
}

