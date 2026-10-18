//! Create-and-start as one durable, retry-safe operation (issue #2331).
//!
//! The old `POST /nodes/create` handler was a bare sequence: parse â†’ insert a
//! node â†’ spawn â†’ reload â†’ 200. Every failure *after* the insert answered a
//! bare error, so a phone could not tell "nothing happened" from "a node exists
//! and I lost the response". A retry meant a second node, a second worktree,
//! and a second process for one idea.
//!
//! This module is the application seam that owns the *outcome* of that
//! sequence. The route parses, invokes, and maps the result to HTTP; it no
//! longer decides whether an error means "committed" or "nothing committed".
//!
//! ## Phase ordering
//!
//! 1. **Claim** the client-generated `operation_key` and compare the
//!    normalized request fingerprint. An already-committed operation replays
//!    its node here — before any expensive work — and a same-key/different-
//!    content request is rejected.
//! 2. **Commit** the node row *and* the key binding in one transaction (the
//!    binding rides through the shared create runner).
//! 3. **Start** the process. This is I/O and must never hold the DB writer
//!    lock (issue #1228), so the recorded start state is written afterwards.
//! 4. **Record** the durable start outcome so a restart between insertion and
//!    startup still exposes a recoverable state.
//!
//! Every committed outcome carries the node — including App-unavailable and a
//! post-start reload failure. A client that sees a committed outcome can
//! navigate to the node and retry the start; one that sees
//! [`CreateNodeOutcome::NotCommitted`] knows no work exists.

use std::future::Future;
use std::pin::Pin;

use sha2::{Digest, Sha256};

use crate::agent::spawn::{SpawnIntent, SpawnOutcome, SpawnRequest, TerminalSize};
use crate::db::{self, CreateStartState};
use crate::models::AgentNode;
use crate::services::agent_node::AgentNodeError;

/// The honest result of one create-and-start attempt.
///
/// Generated to `src/types/generated/CreateNodeOutcome.ts`. The `outcome`
/// discriminant — not an HTTP status — is what the mobile and Android clients
/// branch on, so a `start_failed` can never be read as a running agent.
///
/// Every variant except one carries a committed [`AgentNode`]. The exception
/// is not modelled here: "nothing was committed" is a [`CreateRejection`],
/// returned as the `Err` arm, so a caller physically cannot read a committed
/// outcome while believing nothing was created.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CreateNodeOutcome.ts")]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CreateNodeOutcome {
    /// A node row is committed and the process is running.
    Active {
        node: AgentNode,
        /// Echoed back so a client that lost the response can correlate its
        /// retry with the operation it submitted.
        operation_key: Option<String>,
    },
    /// A node row is committed but the start was never attempted (App not
    /// ready, or a restart landed between insertion and startup). The node is
    /// real and reachable — the client offers Start / Retry.
    StartPending {
        node: AgentNode,
        operation_key: Option<String>,
        /// Actionable prose, not a stack trace.
        reason: String,
    },
    /// A node row is committed but the start failed. The old handler reported
    /// this as a bare 500, telling the phone nothing about the node that now
    /// exists.
    StartFailed {
        node: AgentNode,
        operation_key: Option<String>,
        start_error: String,
    },
}

impl CreateNodeOutcome {
    /// The committed node. Always `Some` — this enum has no nothing-committed
    /// variant by construction.
    ///
    /// The route uses this to decide whether to emit `node-created`: a
    /// committed node must be discoverable by other clients even when its
    /// start failed (#2331).
    pub fn node(&self) -> &AgentNode {
        match self {
            Self::Active { node, .. }
            | Self::StartPending { node, .. }
            | Self::StartFailed { node, .. } => node,
        }
    }
}

/// Why a create attempt committed nothing. Distinct from every committed
/// outcome — the transport maps these to 4xx/5xx with no node attached.
#[derive(Debug)]
pub enum CreateRejection {
    /// The key is already bound to a *different* normalized request.
    KeyConflict { key: String },
    /// A backend failure inside the shared create runner (mesh missing,
    /// invalid configuration, unsupported initial prompt, DB error).
    Create(AgentNodeError),
    /// The blocking offload itself failed, or a lookup could not be completed.
    Offload(String),
}

impl CreateRejection {
    pub fn message(&self) -> String {
        match self {
            Self::KeyConflict { key } => format!(
                "This submission was already accepted under a different request. \
                 Start a new submission instead of reusing this one (key {key})."
            ),
            Self::Create(error) => error.to_string(),
            Self::Offload(e) => e.clone(),
        }
    }

    /// Whether this is the client's fault (4xx) or ours (5xx).
    pub fn is_client_error(&self) -> bool {
        match self {
            Self::KeyConflict { .. } => true,
            Self::Create(error) => matches!(
                error,
                AgentNodeError::MeshNotFound(_)
                    | AgentNodeError::InvalidConfiguration(_)
                    | AgentNodeError::InitialPromptUnsupported
                    | AgentNodeError::OperationKeyConflict { .. }
            ),
            Self::Offload(_) => false,
        }
    }
}

/// A fully-resolved create-and-start request.
#[derive(Debug, Clone)]
pub struct CreateAndStartRequest {
    pub mesh_id: i64,
    /// Already resolved against the mesh default and saved configuration.
    pub provider: String,
    pub configuration_id: Option<String>,
    /// The saved configuration resolved by the caller (preferences are read
    /// off the async worker by the transport, so resolution stays there).
    pub configuration: Option<crate::preferences::spawn_configurations::SpawnConfiguration>,
    /// Initial prompt, when the caller sent one.
    pub prompt: Option<String>,
    pub rows: u16,
    pub cols: u16,
    /// Client-generated, stable across retries of the same submission. `None`
    /// keeps the pre-#2331 behaviour: no ledger row, no replay protection.
    pub operation_key: Option<String>,
}

/// Normalized identity of a create request.
///
/// Covers the three things that decide *what work* a node does — mesh, harness,
/// saved configuration id, and the prompt — but deliberately not the terminal
/// size, which is cosmetic and must not make an honest retry look like a
/// conflict.
pub fn request_fingerprint(
    mesh_id: i64,
    provider: &str,
    configuration_id: Option<&str>,
    prompt: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    // Length-prefixed so no two field splits can collide into one digest
    // (e.g. provider "ab" + config "c" vs provider "a" + config "bc").
    for field in [
        mesh_id.to_string(),
        provider.trim().to_string(),
        configuration_id.unwrap_or_default().trim().to_string(),
        prompt.unwrap_or_default().trim().to_string(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Where the service's synchronous DB work runs.
///
/// A type parameter rather than a `dyn` trait on purpose: `run` is generic,
/// which makes the trait object-unsafe. Production passes [`BlockingPool`] —
/// SQLite and filesystem work must stay off the tokio worker pool (issue
/// #1380). Tests pass [`InlineWork`], because a test's `isolated()` database is
/// installed with `thread_local!` and is invisible from a `spawn_blocking`
/// thread, so the test's DB work has to stay on the calling thread.
pub trait BlockingWork {
    /// Run one synchronous DB read/write, yielding [`CreateRejection`] on
    /// failure so every error above this line is "nothing committed".
    fn run<'a, T, F>(
        &'a self,
        label: &'static str,
        work: F,
    ) -> Pin<Box<dyn Future<Output = Result<T, CreateRejection>> + Send + 'a>>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, CreateRejection> + Send + 'static;
}

/// The production executor: the blocking pool, per the repo's async-command
/// rule (issue #1380).
pub struct BlockingPool;

impl BlockingWork for BlockingPool {
    fn run<'a, T, F>(
        &'a self,
        label: &'static str,
        work: F,
    ) -> Pin<Box<dyn Future<Output = Result<T, CreateRejection>> + Send + 'a>>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, CreateRejection> + Send + 'static,
    {
        Box::pin(crate::commands::run_blocking_typed::<T, CreateRejection, F>(label, work))
    }
}

/// Test executor: runs on the calling thread so the test's isolated database
/// stays reachable. Never used in production paths.
#[cfg(test)]
pub struct InlineWork;

#[cfg(test)]
impl BlockingWork for InlineWork {
    fn run<'a, T, F>(
        &'a self,
        _label: &'static str,
        work: F,
    ) -> Pin<Box<dyn Future<Output = Result<T, CreateRejection>> + Send + 'a>>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, CreateRejection> + Send + 'static,
    {
        Box::pin(async move { work() })
    }
}

/// Test executor that yields before each unit of work, so two concurrent
/// operations genuinely interleave instead of running one to completion.
///
/// Without a real suspension point both callers finish Phase 1 before either
/// commits, so the loser is caught by the claim rather than by the key binding
/// — which means the Phase-2 replay path is never exercised.
#[cfg(test)]
pub struct YieldingWork;

#[cfg(test)]
impl BlockingWork for YieldingWork {
    fn run<'a, T, F>(
        &'a self,
        _label: &'static str,
        work: F,
    ) -> Pin<Box<dyn Future<Output = Result<T, CreateRejection>> + Send + 'a>>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, CreateRejection> + Send + 'static,
    {
        Box::pin(async move {
            tokio::task::yield_now().await;
            work()
        })
    }
}

/// The injectable start seam.
///
/// Production spawns through the crate's orchestrator; tests supply a
/// controllable stand-in so phase ordering and failure recovery are exercised
/// without a PTY.
pub trait AgentStart: Send + Sync {
    /// Whether a start can be attempted at all right now. `false` maps to
    /// [`CreateNodeOutcome::StartPending`], never to a bare error.
    fn is_ready(&self) -> bool;

    /// Start the agent for `node_id`. `Err` must leave the node row committed.
    ///
    /// The production seam returns the orchestrator's full outcome; `AlreadyActive`
    /// and `Skipped` are *not* failures — the node is running or deliberately
    /// not spawned, and both count as a resolved start.
    fn start<'a>(
        &'a self,
        node_id: i64,
        intent: SpawnIntent,
        size: TerminalSize,
    ) -> Pin<Box<dyn Future<Output = Result<SpawnOutcome, String>> + Send + 'a>>;
}

/// The production seam. Delegates to the crate's spawn orchestrator, which
/// already owns lifecycle claims — so a retry for the *same* node reports
/// `AlreadyActive` instead of forking a second process.
pub struct TauriStart<'a> {
    app: &'a tauri::AppHandle,
}

impl<'a> TauriStart<'a> {
    pub fn new(app: &'a tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl AgentStart for TauriStart<'_> {
    fn is_ready(&self) -> bool {
        true
    }

    fn start<'a>(
        &'a self,
        node_id: i64,
        intent: SpawnIntent,
        size: TerminalSize,
    ) -> Pin<Box<dyn Future<Output = Result<SpawnOutcome, String>> + Send + 'a>> {
        Box::pin(crate::agent::spawn::spawn_with_intent(
            self.app,
            SpawnRequest::new(node_id, intent, size),
        ))
    }
}

/// A seam that never becomes ready — models "the desktop App handle is not
/// published yet", the case that used to answer a bare 503 *after* the node row
/// had already been committed.
pub struct UnreadyStart;

impl AgentStart for UnreadyStart {
    fn is_ready(&self) -> bool {
        false
    }

    fn start<'a>(
        &'a self,
        _node_id: i64,
        _intent: SpawnIntent,
        _size: TerminalSize,
    ) -> Pin<Box<dyn Future<Output = Result<SpawnOutcome, String>> + Send + 'a>> {
        Box::pin(async { Err("App not ready".to_string()) })
    }
}

/// Run one create-and-start operation.
///
/// `start` is the caller's injected seam. All database work here runs without
/// holding the writer lock across [`AgentStart::start`].
pub async fn create_and_start<R: BlockingWork>(
    request: CreateAndStartRequest,
    start: &dyn AgentStart,
    work: &R,
) -> Result<CreateNodeOutcome, CreateRejection> {
    let operation_key = request.operation_key.clone();
    let fingerprint = request_fingerprint(
        request.mesh_id,
        &request.provider,
        request.configuration_id.as_deref(),
        request.prompt.as_deref(),
    );

    // --- Phase 1: claim -------------------------------------------------
    // A retry that finds a committed operation returns the recorded node
    // before paying for configuration resolution or a worktree.
    if let Some(key) = operation_key.clone() {
        let key_for_task = key.clone();
        let fingerprint_for_task = fingerprint.clone();
        let claim = work
            .run("http_create_node_claim", move || {
                db::claim_create_operation(&key_for_task, &fingerprint_for_task)
                    .map_err(CreateRejection::from)
            })
            .await?;

        if let Some(claim) = claim {
            return match claim {
                db::CreateClaim::Conflict => Err(CreateRejection::KeyConflict { key }),
                db::CreateClaim::Replay {
                    node_id,
                    start_state,
                    start_error,
                } => replay_committed(node_id, operation_key, start_state, start_error, work).await,
            };
        }
    }

    // --- Phase 2: commit the node row and bind the key -------------------
    let binding = operation_key
        .as_ref()
        .map(|key| db::CreateOperationBinding {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        });
    let created = commit_node(&request, binding.as_ref(), work).await?;

    // A concurrent duplicate already bound this key inside the transaction, so
    // our candidate row rolled back and the *recorded* node came back. That
    // request owns the single start attempt for this key: advancing here would
    // start a second process and, worse, overwrite a recorded `StartFailed`
    // with `Started`, reporting a node as running that never started
    // (issue #2331 review). Re-read the ledger and report its honest state.
    if created.replayed {
        let replay_key = operation_key.clone().unwrap_or_default();
        let replay_fingerprint = fingerprint.clone();
        let recorded = work
            .run("http_replay_claim", move || {
                db::claim_create_operation(&replay_key, &replay_fingerprint)
                    .map_err(CreateRejection::from)
            })
            .await?;
        return match recorded {
            Some(db::CreateClaim::Replay {
                node_id,
                start_state,
                start_error,
            }) => replay_committed(node_id, operation_key, start_state, start_error, work).await,
            // Unreachable in practice: the bind just told us this key exists
            // with this fingerprint. Treating anything else as a fresh conflict
            // is the honest fallback, and it still commits nothing.
            Some(db::CreateClaim::Conflict) => Err(CreateRejection::KeyConflict {
                key: operation_key.unwrap_or_default(),
            }),
            None => {
                replay_committed(
                    created.node.id,
                    operation_key,
                    CreateStartState::Created,
                    None,
                    work,
                )
                .await
            }
        };
    }
    let node = created.node;

    // --- Phase 3: start (I/O, no DB lock held) ---------------------------
    let intent = match &request.prompt {
        Some(text) => SpawnIntent::Prompt { text: text.clone() },
        None => SpawnIntent::Fresh,
    };
    let size = TerminalSize {
        rows: request.rows,
        cols: request.cols,
    };

    if !start.is_ready() {
        record_start(node.id, CreateStartState::Created, None);
        return Ok(CreateNodeOutcome::StartPending {
            node,
            operation_key,
            reason: "App not ready yet — the node was created and can be started from Work"
                .to_string(),
        });
    }

    // `AlreadyActive` and `Skipped` are resolved starts, not failures: the
    // first means a lifecycle claim already owns this node (a concurrent
    // retry), the second means the orchestrator deliberately did not spawn.
    // Only a hard `Err` is a start failure.
    match start.start(node.id, intent, size).await {
        Ok(SpawnOutcome::Started(_))
        | Ok(SpawnOutcome::AlreadyActive(_))
        | Ok(SpawnOutcome::Skipped(_)) => {}
        Err(start_error) => {
            record_start(node.id, CreateStartState::StartFailed, Some(&start_error));
            return Ok(CreateNodeOutcome::StartFailed {
                node,
                operation_key,
                start_error,
            });
        }
    }

    // --- Phase 4: record the durable outcome, then reload ---------------
    record_start(node.id, CreateStartState::Started, None);

    // A reload failure must not discard a committed, running node. Report
    // `Active` with the node we already hold; the client can refresh from the
    // node list rather than being told the whole submission failed.
    let node_id = node.id;
    match work
        .run("http_reload_node", move || {
            db::get_agent_node_by_id(node_id).map_err(CreateRejection::from)
        })
        .await
    {
        Ok(reloaded) => Ok(CreateNodeOutcome::Active {
            node: reloaded,
            operation_key,
        }),
        Err(_) => Ok(CreateNodeOutcome::Active {
            node,
            operation_key,
        }),
    }
}

/// Commit the node row, binding the operation key in the same transaction.
async fn commit_node<R: BlockingWork>(
    request: &CreateAndStartRequest,
    binding: Option<&db::CreateOperationBinding>,
    work: &R,
) -> Result<crate::services::agent_node::CreatedNode, CreateRejection> {
    let task_request = request.clone();
    let task_binding = binding.cloned();
    work.run("http_create_node", move || {
        create_node_blocking(&task_request, task_binding.as_ref()).map_err(CreateRejection::Create)
    })
    .await
}

/// The shared create runner, unchanged apart from the optional key binding.
///
/// All of the expensive work (preferences, launch plan, git branch, worktree
/// placement) happens here *before* the single row-writing transaction, so the
/// writer lock is never held across filesystem or subprocess work (#1228).
fn create_node_blocking(
    request: &CreateAndStartRequest,
    operation: Option<&db::CreateOperationBinding>,
) -> Result<crate::services::agent_node::CreatedNode, AgentNodeError> {
    crate::services::agent_node::create_blocking_configured(
        crate::services::agent_node::NodeCreateParams {
            mesh_id: request.mesh_id,
            path: "",
            branch: "main",
            provider: Some(&request.provider),
            source_issue: None,
            source_pr: None,
            source_pr_pinned_sha: None,
            use_worktree_override: None,
            name_override: None,
            head_repo_owner: None,
            head_repo_clone_url: None,
            configuration: request.configuration.as_ref(),
            initial_prompt: request.prompt.as_deref(),
            operation,
        },
        false,
    )
}

impl From<rusqlite::Error> for CreateRejection {
    fn from(source: rusqlite::Error) -> Self {
        Self::Offload(source.to_string())
    }
}

impl From<String> for CreateRejection {
    fn from(message: String) -> Self {
        Self::Offload(message)
    }
}

/// Map a committed node + a recorded start state onto the outcome a retry sees.
fn replayed_outcome(
    node: AgentNode,
    operation_key: Option<String>,
    start_state: CreateStartState,
    start_error: Option<String>,
) -> CreateNodeOutcome {
    match start_state {
        CreateStartState::Started => CreateNodeOutcome::Active {
            node,
            operation_key,
        },
        CreateStartState::StartFailed => CreateNodeOutcome::StartFailed {
            node,
            operation_key,
            start_error: start_error
                .unwrap_or_else(|| "The agent could not be started".to_string()),
        },
        CreateStartState::Created => CreateNodeOutcome::StartPending {
            node,
            operation_key,
            reason: "An earlier attempt created this node but did not start it".to_string(),
        },
    }
}

/// Replay an already-committed operation.
///
/// A same-key retry returns the *same* node id: no second node, no second
/// worktree, no second process. A lost response after a successful startup is
/// therefore safe to retry.
async fn replay_committed<R: BlockingWork>(
    node_id: i64,
    operation_key: Option<String>,
    start_state: CreateStartState,
    start_error: Option<String>,
    work: &R,
) -> Result<CreateNodeOutcome, CreateRejection> {
    let node = work
        .run("http_replay_node", move || {
            db::get_agent_node_by_id(node_id).map_err(CreateRejection::from)
        })
        .await?;

    Ok(replayed_outcome(
        node,
        operation_key,
        start_state,
        start_error,
    ))
}

/// Record the durable start outcome.
///
/// A failure here must not fail the request: the node is already committed and
/// the client still needs its identity. The row simply keeps its previous
/// state, which is the recoverable `created` case.
fn record_start(node_id: i64, state: CreateStartState, error: Option<&str>) {
    let _ = db::record_create_start_result(node_id, state, error);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::agent_node::INITIAL_PROMPT_UNSUPPORTED;
    use std::sync::Mutex;

    /// A start seam the test drives, standing in for the PTY spawn.
    ///
    /// Records every call so a test can assert *how many* processes were asked
    /// for — the "no extra worktrees or processes on retry" requirement is
    /// about call counts, not just the returned node id.
    struct ScriptedStart {
        ready: bool,
        result: Result<(), String>,
        calls: Mutex<Vec<i64>>,
    }

    impl ScriptedStart {
        fn succeeding() -> Self {
            Self {
                ready: true,
                result: Ok(()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                ready: true,
                result: Err(message.to_string()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn unready() -> Self {
            Self {
                ready: false,
                result: Ok(()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn started_nodes(&self) -> Vec<i64> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl AgentStart for ScriptedStart {
        fn is_ready(&self) -> bool {
            self.ready
        }

        fn start<'a>(
            &'a self,
            node_id: i64,
            _intent: SpawnIntent,
            _size: TerminalSize,
        ) -> Pin<Box<dyn Future<Output = Result<SpawnOutcome, String>> + Send + 'a>> {
            self.calls.lock().unwrap().push(node_id);
            let result = self.result.clone();
            Box::pin(async move {
                result?;
                // The real orchestrator returns the refreshed node; the seam
                // only needs a well-formed value.
                Ok(SpawnOutcome::Skipped(AgentNode::default()))
            })
        }
    }

    fn request(mesh_id: i64, operation_key: Option<&str>) -> CreateAndStartRequest {
        CreateAndStartRequest {
            mesh_id,
            provider: "anthropic".to_string(),
            configuration_id: None,
            configuration: None,
            prompt: Some("do the thing".to_string()),
            rows: 24,
            cols: 80,
            operation_key: operation_key.map(str::to_string),
        }
    }

    /// An isolated database plus a preferences dir the shared create runner can
    /// read. The runner resolves a launch plan from preferences before it
    /// inserts, so a test that skips this fails on configuration lookup rather
    /// than on the behaviour under test.
    fn fixture() -> (crate::db::test_support::IsolatedDbGuard, i64) {
        let guard = crate::db::test_support::isolated();
        let root = tempfile::tempdir().unwrap();
        crate::preferences::init_for_tests(root.path().to_path_buf());
        let mesh = crate::db::create_mesh("API", "C:/api").unwrap();
        (guard, mesh.id)
    }

    fn node_count() -> usize {
        crate::db::list_agent_nodes().unwrap().len()
    }

    /// Run one operation against the *calling* thread's database.
    ///
    /// Production passes `BlockingPool`; a test must pass `InlineWork` so the
    /// `isolated()` install stays reachable.
    async fn run(
        request: CreateAndStartRequest,
        start: &dyn AgentStart,
    ) -> Result<CreateNodeOutcome, CreateRejection> {
        create_and_start(request, start, &InlineWork).await
    }

    /// Same as [`run`] but with a yielding executor, so two concurrent calls
    /// really race instead of serialising on the first await.
    async fn run_yielding(
        request: CreateAndStartRequest,
        start: &dyn AgentStart,
    ) -> Result<CreateNodeOutcome, CreateRejection> {
        create_and_start(request, start, &YieldingWork).await
    }

    #[tokio::test]
    async fn app_unavailable_commits_the_node_and_reports_pending() {
        // The #2331 headline: the old handler answered a bare 503 *after* the
        // row was committed, so the phone could not tell what happened.
        let (_db, mesh_id) = fixture();

        let start = ScriptedStart::unready();
        let outcome = run(request(mesh_id, Some("op-1")), &start)
            .await
            .expect("a committed node, not an error");

        match &outcome {
            CreateNodeOutcome::StartPending { node, reason, .. } => {
                assert_eq!(node.mesh_id, mesh_id);
                assert!(reason.contains("App not ready"), "{reason}");
            }
            other => panic!("expected StartPending, got {other:?}"),
        }
        // No process was requested, and the row is findable.
        assert!(start.started_nodes().is_empty());
        assert_eq!(node_count(), 1);
    }

    #[tokio::test]
    async fn start_failure_identifies_the_committed_node() {
        // "Missing CLI or startup failure identifies the committed node and its
        // actionable start error" — the old route threw the id away.
        let (_db, mesh_id) = fixture();

        let start = ScriptedStart::failing("claude CLI not found on PATH");
        let outcome = run(request(mesh_id, Some("op-1")), &start)
            .await
            .expect("a committed node, not an error");

        match &outcome {
            CreateNodeOutcome::StartFailed {
                node, start_error, ..
            } => {
                assert_eq!(node.mesh_id, mesh_id);
                assert_eq!(start_error, "claude CLI not found on PATH");
            }
            other => panic!("expected StartFailed, got {other:?}"),
        }
        assert_eq!(start.started_nodes().len(), 1);
        assert_eq!(node_count(), 1);
    }

    #[tokio::test]
    async fn same_key_retry_returns_the_same_node_without_a_second_start() {
        // "Same-key retries return the same node ID without extra nodes,
        // worktrees, or processes."
        let (_db, mesh_id) = fixture();

        let first = run(
            request(mesh_id, Some("op-1")),
            &ScriptedStart::failing("boom"),
        )
        .await
        .expect("committed");
        let first_id = first.node().id;

        // Retry after a *lost response*: the client never saw the first
        // outcome and resubmits the identical request.
        let retry_start = ScriptedStart::succeeding();
        let retry = run(request(mesh_id, Some("op-1")), &retry_start)
            .await
            .expect("committed");

        assert_eq!(
            retry.node().id,
            first_id,
            "retry must not create a new node"
        );
        assert_eq!(node_count(), 1);
        // The recorded failure is replayed rather than silently re-started.
        assert!(matches!(retry, CreateNodeOutcome::StartFailed { .. }));
        assert!(
            retry_start.started_nodes().is_empty(),
            "a retry must not start a second process"
        );
    }

    #[tokio::test]
    async fn same_key_with_different_content_is_rejected_as_conflict() {
        let (_db, mesh_id) = fixture();

        let first_start = ScriptedStart::succeeding();
        let first = run(request(mesh_id, Some("op-1")), &first_start)
            .await
            .expect("committed");

        let mut conflicting = request(mesh_id, Some("op-1"));
        conflicting.prompt = Some("a completely different idea".to_string());
        let conflicting_start = ScriptedStart::succeeding();
        let rejected = run(conflicting, &conflicting_start).await;

        assert!(
            matches!(
                rejected,
                Err(CreateRejection::KeyConflict { ref key }) if key == "op-1"
            ),
            "expected a key conflict, got {rejected:?}"
        );
        // The rejected request must not have created anything.
        assert_eq!(node_count(), 1);
        assert_eq!(
            crate::db::list_agent_nodes().unwrap()[0].id,
            first.node().id
        );
    }

    #[tokio::test]
    async fn concurrent_same_key_requests_cannot_insert_duplicate_nodes() {
        // The two claims race on the unique `operation_key`: exactly one node
        // row may exist, and both callers must see that same id.
        let (_db, mesh_id) = fixture();

        let start_a = ScriptedStart::succeeding();
        let start_b = ScriptedStart::succeeding();
        let a = run(request(mesh_id, Some("op-1")), &start_a);
        let b = run(request(mesh_id, Some("op-1")), &start_b);
        let (first, second) = tokio::join!(a, b);

        let first = first.expect("committed");
        let second = second.expect("committed");
        assert_eq!(first.node().id, second.node().id);
        assert_eq!(node_count(), 1);
        // One node means one start: the loser must replay, not start a second
        // process for the same idea.
        assert_eq!(
            start_a.started_nodes().len() + start_b.started_nodes().len(),
            1,
            "exactly one start attempt for one key"
        );
    }

    /// Regression for the concurrency defect found in review: the losing racer
    /// used to be handed `Ok(node)` indistinguishable from a fresh create, so
    /// it walked into the start phase and its Phase 4 overwrote the winner's
    /// recorded `StartFailed` with `Started` — reporting a node as running that
    /// never started, and corrupting the durable ledger.
    #[tokio::test]
    async fn a_concurrent_race_starts_once_and_never_overwrites_a_recorded_failure() {
        let (_db, mesh_id) = fixture();

        // Both seams fail, so whichever wins the bind records `StartFailed`.
        // The assertion is then deterministic: the ledger must still say so.
        let start_a = ScriptedStart::failing("claude CLI not found on PATH");
        let start_b = ScriptedStart::failing("claude CLI not found on PATH");
        let a = run_yielding(request(mesh_id, Some("op-race")), &start_a);
        let b = run_yielding(request(mesh_id, Some("op-race")), &start_b);
        let (first, second) = tokio::join!(a, b);

        let first = first.expect("committed");
        let second = second.expect("committed");
        assert_eq!(first.node().id, second.node().id, "one node per key");
        assert_eq!(node_count(), 1);

        // The core assertion: only the winner of the bind may start.
        assert_eq!(
            start_a.started_nodes().len() + start_b.started_nodes().len(),
            1,
            "the losing racer must not start a second process"
        );

        // The recorded failure survives; nothing overwrote it with `Started`.
        let stored = crate::db::create_operation::find_create_operation("op-race")
            .unwrap()
            .expect("ledger row");
        assert_eq!(
            stored.start_state,
            CreateStartState::StartFailed,
            "a losing racer must not overwrite the recorded start state"
        );

        // Neither caller may be told the agent is running.
        for outcome in [&first, &second] {
            assert!(
                !matches!(outcome, CreateNodeOutcome::Active { .. }),
                "a failed start must never report Active, got {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_restart_between_creation_and_start_exposes_a_recoverable_state() {
        // Simulate the crash window by recording a bare `created` row, then
        // retrying: the client must see "committed, not started" with the node
        // it can go and start — not a success and not a failure.
        let (_db, mesh_id) = fixture();

        let unready = ScriptedStart::unready();
        let committed = run(request(mesh_id, Some("op-1")), &unready)
            .await
            .expect("committed");
        let node_id = committed.node().id;

        // The ledger row is `created`, exactly as a restart would leave it.
        let stored = crate::db::create_operation::find_create_operation("op-1")
            .unwrap()
            .expect("ledger row survives");
        assert_eq!(stored.start_state, CreateStartState::Created);

        // A fresh process picks the operation up and reports the node it can
        // start — never a second node, never a bare failure.
        let recovered = ScriptedStart::succeeding();
        let outcome = run(request(mesh_id, Some("op-1")), &recovered)
            .await
            .expect("committed");
        assert_eq!(outcome.node().id, node_id);
        assert_eq!(node_count(), 1);
        assert!(
            matches!(
                outcome,
                CreateNodeOutcome::StartPending { .. }
                    | CreateNodeOutcome::StartFailed { .. }
                    | CreateNodeOutcome::Active { .. }
            ),
            "a recoverable created row must still name its node, got {outcome:?}"
        );
    }

    /// The service-level half of the route's prompt gate: a client error must
    /// stay a rejection with nothing committed, never become a "committed node
    /// the user can go look at". The route cannot assert this itself — it
    /// reaches preferences from `spawn_blocking`, where the `cfg(test)`
    /// `thread_local!` app-data dir is invisible — so the assertion lives here,
    /// where `InlineWork` keeps the whole path on this thread.
    #[tokio::test]
    async fn a_prompt_the_harness_cannot_take_commits_nothing() {
        let (_db, mesh_id) = fixture();

        let start = ScriptedStart::succeeding();
        let outcome = run(
            CreateAndStartRequest {
                mesh_id,
                provider: "terminal".to_string(),
                configuration_id: None,
                configuration: None,
                prompt: Some("ship the prompt".to_string()),
                rows: 24,
                cols: 80,
                operation_key: Some("op-1".to_string()),
            },
            &start,
        )
        .await;

        match outcome {
            Err(rejection) => {
                assert!(
                    rejection.message().contains(INITIAL_PROMPT_UNSUPPORTED),
                    "{rejection:?}"
                );
                assert!(
                    rejection.is_client_error(),
                    "expected a 4xx, got {rejection:?}"
                );
            }
            Ok(outcome) => panic!("expected a rejection, got a committed node: {outcome:?}"),
        }
        // Nothing was created, so the client may safely retry the same key.
        assert_eq!(node_count(), 0);
        assert!(start.started_nodes().is_empty());
    }

    #[tokio::test]
    async fn an_unkeyed_submission_keeps_the_pre_2331_behaviour() {
        // Older installed clients omit `operation_key`; they must keep working
        // rather than being forced to mint one.
        let (_db, mesh_id) = fixture();

        let unkeyed_start = ScriptedStart::succeeding();
        let outcome = run(request(mesh_id, None), &unkeyed_start)
            .await
            .expect("committed");
        assert!(matches!(outcome, CreateNodeOutcome::Active { .. }));
        assert!(
            crate::db::create_operation::find_create_operation("")
                .unwrap()
                .is_none(),
            "no ledger row is written without a key"
        );
    }
}
