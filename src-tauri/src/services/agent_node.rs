//! Agent Node service — creation, deletion, and lifecycle orchestration

use crate::agent::provider::SpawnOptionId;
use crate::db;

/// Upgrade legacy history outside DB locks; the existing JSON column keeps the operation additive.
pub fn migrate_launch_history() -> Result<(), String> {
    let prefs = crate::preferences::load()?;
    let mut mappings: std::collections::HashMap<String, String> = prefs
        .spawn_configurations
        .iter()
        .filter(|c| c.generated.is_some())
        .filter(|c| {
            !prefs
                .spawn_configurations
                .iter()
                .any(|other| other.id == c.spawn_option_id)
        })
        .map(|c| (c.spawn_option_id.clone(), c.id.clone()))
        .collect();
    // Stored references to retired catalogue recipes (`launch/<harness>`)
    // heal to the equivalent bare Spawn Option; user-owned and explicitly
    // deleted ids are excluded by the helper.
    mappings
        .extend(crate::preferences::launch_configurations::retired_configuration_aliases(&prefs));
    db::migrate_launch_selections(&mappings).map_err(|e| e.to_string())?;
    let nodes = db::legacy_launch_nodes().map_err(|e| e.to_string())?;
    for node in nodes {
        let mut snapshot_prefs = prefs.clone();
        let selection = if let Some(configuration) = node.launch_configuration.as_ref() {
            snapshot_prefs
                .spawn_configurations
                .retain(|c| c.id != configuration.id);
            snapshot_prefs
                .spawn_configurations
                .push(configuration.clone());
            configuration.id.as_str()
        } else {
            node.provider.as_str()
        };
        match crate::preferences::launch_configurations::capture_legacy(&snapshot_prefs, selection)
        {
            Ok(plan) => db::set_node_launch_snapshot(
                node.id,
                &crate::preferences::launch_configurations::snapshot(plan),
            )?,
            Err(error) => tracing::warn!(
                node_id = node.id,
                "Legacy launch snapshot unavailable: {error}"
            ),
        }
    }
    Ok(())
}
use crate::env;
use crate::git::worktree::{self, WorktreeCloseSafety};
use crate::models::{AgentNode, PendingWorktreeRemoval, SessionStatus};
use crate::preferences::resolve_harness_provider_for;

/// Error type for agent node service operations
#[derive(Debug)]
pub enum AgentNodeError {
    Db(rusqlite::Error),
    /// Node's `SessionStatus` was not eligible for the requested
    /// operation (e.g. `regenerate` rejects `Spawning` / `Pending` /
    /// `Archived` / `Suspended`). The wrapped string is the user-
    /// facing reason.
    Status(String),
    /// The referenced `mesh_id` did not exist when this operation
    /// required it. Surfaced as a typed sentinel so callers can map
    /// to a precise HTTP status (the `/api/nodes/create` route uses
    /// this for its 400 "Mesh not found" response) instead of
    /// pattern-matching on a wrapped `String`. A regression that
    /// leaks `rusqlite::Error::QueryReturnedNoRows` past this
    /// boundary would surface as a 500 instead of a 400.
    ///
    /// Review-round-2 fix: replaces the prior string sentinel
    /// `"mesh not found"` carried inside `Status(String)`, which was
    /// brittle stringly-typed control flow.
    MeshNotFound(i64),
    /// The requested saved spawn configuration is invalid for this option.
    /// This remains typed so HTTP callers can return a client error without
    /// scraping the validation text.
    InvalidConfiguration(String),
    /// The caller included an initial prompt, and the harness that will
    /// launch does not accept one. Both surfaces return
    /// [`INITIAL_PROMPT_UNSUPPORTED`]; no row is inserted.
    InitialPromptUnsupported,
    /// A backend orchestrator call (`spawn_agent_inner` today, but
    /// the variant is intentionally generic so future downstream
    /// calls share the path) failed. The wrapped string is the
    /// underlying error message verbatim, so the frontend's toast
    /// surfaces the same text the Tauri command returns.
    Backend(String),
    /// A discovery-based resume named no harness, or one this build
    /// cannot resume. Import refuses to guess: a wrong executor turns a
    /// conversation from another harness into an empty or brand-new
    /// session (issue #2332). The wrapped string is the user-facing
    /// reason, and no row is inserted.
    UnresumableHarness(String),
    /// A remote create-and-start operation key (issue #2331) was reused for a
    /// *different* normalized request. The node INSERT is rolled back, so
    /// nothing was committed; the caller maps this to 409.
    OperationKeyConflict {
        key: String,
    },
}

impl std::fmt::Display for AgentNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentNodeError::Db(e) => write!(f, "{}", e),
            AgentNodeError::Status(e) => write!(f, "{}", e),
            AgentNodeError::MeshNotFound(mesh_id) => write!(f, "mesh {mesh_id} not found"),
            AgentNodeError::InvalidConfiguration(e) => write!(f, "{}", e),
            AgentNodeError::InitialPromptUnsupported => write!(f, "{INITIAL_PROMPT_UNSUPPORTED}"),
            AgentNodeError::Backend(e) => write!(f, "{}", e),
            AgentNodeError::UnresumableHarness(e) => write!(f, "{}", e),
            AgentNodeError::OperationKeyConflict { key } => write!(
                f,
                "operation key already used for a different request: {key}"
            ),
        }
    }
}

impl From<rusqlite::Error> for AgentNodeError {
    fn from(e: rusqlite::Error) -> Self {
        AgentNodeError::Db(e)
    }
}

/// Sentence returned when a create request includes an initial prompt and
/// the harness that will launch cannot accept one. HTTP and the desktop
/// commands share this exact text.
pub const INITIAL_PROMPT_UNSUPPORTED: &str = "Choose an agent that supports an initial prompt";

/// Worktree path and environment for one node. `None` or a blank segment is
/// a Root Node: no stored path, and the environment comes from the mesh root.
/// A non-empty segment stores `<effective dir>/<name>`.
fn place_worktree(
    mesh_path: &str,
    mesh_worktree_directory: Option<&str>,
    worktree_segment: Option<&str>,
) -> (Option<String>, crate::models::EnvType) {
    let app_dir = crate::preferences::worktree_directory();
    let effective_dir =
        env::effective_worktree_dir_raw(mesh_path, mesh_worktree_directory, app_dir.as_deref());
    let path = worktree_segment
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| env::resolve_worktree_node_raw(&effective_dir, name));
    let resolved = env::resolve_agent_path_in_dir(mesh_path, &effective_dir, worktree_segment);
    (path, resolved.env_type)
}

/// Capability only. Install detection stays on spawn preflight: a missing
/// binary is a different error, and `available_providers()` is a process-global
/// probe that must not decide whether a prompt may be stored.
fn reject_unsupported_initial_prompt(
    harness_id: &str,
    initial_prompt: Option<&str>,
) -> Result<(), AgentNodeError> {
    let Some(text) = initial_prompt else {
        return Ok(());
    };
    if text.trim().is_empty() {
        return Ok(());
    }
    let adapter = crate::models::Provider::from_db_str(harness_id).adapter();
    if adapter.supports_prefill() {
        Ok(())
    } else {
        Err(AgentNodeError::InitialPromptUnsupported)
    }
}

/// One create-family request, assembled by the caller.
///
/// The create entry points used to take their inputs as long positional
/// argument lists, so every wrapper had to remember the full arity and any
/// entry point that grew a field grew a lint suppression for the argument
/// count. The request struct makes the shape explicit at each call site
/// instead: a wrapper that never uses a field writes it as `None` (or `""`,
/// see `branch`) and the difference from its sibling stays readable.
///
/// `Copy` is load-bearing: the wrappers forward most fields unchanged with
/// `..request` while overriding the few they must pin.
#[derive(Debug, Clone, Copy)]
pub struct NodeCreateParams<'a> {
    pub mesh_id: i64,
    /// The checkout to create the node against. The shared runners
    /// ([`create_blocking`] / [`create_blocking_configured`]) look the mesh up
    /// themselves and read the path off that row, so their callers pass `""`.
    pub path: &'a str,
    /// The branch to record. For [`create_blocking`] /
    /// [`create_blocking_configured`] this is the branch *override*: an empty
    /// value means "none supplied", and a non-empty value short-circuits the
    /// `git::get_default_branch_blocking` lookup. The other entry points take
    /// the branch verbatim.
    pub branch: &'a str,
    pub provider: Option<&'a str>,
    pub source_issue: Option<i64>,
    pub source_pr: Option<i64>,
    pub source_pr_pinned_sha: Option<&'a str>,
    pub use_worktree_override: Option<bool>,
    pub name_override: Option<&'a str>,
    pub head_repo_owner: Option<&'a str>,
    pub head_repo_clone_url: Option<&'a str>,
    pub configuration: Option<&'a crate::preferences::spawn_configurations::SpawnConfiguration>,
    pub initial_prompt: Option<&'a str>,
    /// Remote create-and-start key binding (issue #2331). Only the entry
    /// points that own the key take one; the rest drop it.
    pub operation: Option<&'a db::CreateOperationBinding>,
}

/// Create a new agent node with auto-generated name, environment detection,
/// and provider resolution.
///
/// Pass `None` for `source_pr` / `source_pr_pinned_sha` /
/// `head_repo_owner` / `head_repo_clone_url` on non-PR spawns (issue-spawn,
/// handover, hand-spawn). Fork PRs call [`create_with_source_pr_fork`]
/// directly with the fork fields populated (issue #443). `source_pr_pinned_sha`
/// (issue #444) is the exact-pinning handle — `None` skips the drift check.
///
/// `source_pr` and `source_pr_pinned_sha` are **structurally** rejected here
/// (asserted at function entry) so a future caller that mistakenly passes
/// `Some(_)` for either — an importer, a migration script — fails loudly
/// at the boundary rather than silently writing a
/// `source_pr` onto a node that didn't actually come from a PR. The PR-spawn
/// entry point is [`create_with_source_pr_fork`], which is the only function
/// that should ever pass `Some(_)` for these fields. Pinning test lives in
/// `services::agent_node::tests` (issue #448).
///
/// **Issue #1658 step 5:** every live caller now reaches the create
/// sequence via [`create_blocking`], the shared runner. `create` is
/// retained for the structural `source_pr = Some(_)` boundary assert
/// test coverage (`create_rejects_source_pr_some` et al.); `#[allow]`
/// keeps the dead-code lint quiet.
#[allow(dead_code)]
pub fn create(request: NodeCreateParams<'_>) -> Result<AgentNode, AgentNodeError> {
    assert!(
        request.source_pr.is_none(),
        "services::agent_node::create refuses source_pr=Some(_); \
         the non-PR wrappers must persist source_pr=None so spawn_agent_inner's \
         'is this a PR-spawned node?' branch (node.source_pr.is_some()) stays a \
         reliable signal. Use create_with_source_pr_fork for PR spawns."
    );
    assert!(
        request.source_pr_pinned_sha.is_none(),
        "services::agent_node::create refuses source_pr_pinned_sha=Some(_); \
         use create_with_source_pr_fork for PR spawns that need SHA pinning."
    );
    // Single insert path; fork fields are None for the non-fork entry point.
    create_with_source_pr_fork(NodeCreateParams {
        head_repo_owner: None,
        head_repo_clone_url: None,
        configuration: None,
        initial_prompt: None,
        operation: None,
        ..request
    })
}

/// Like [`create`], but also records the fork's owner login and clone URL
/// when the PR is from a fork (issue #443). `spawn_agent_inner` reads
/// these to register `fork-<owner>` as a remote and fetch the head ref.
/// For same-repo PRs, pass `None` for both fork fields and call [`create`]
/// instead — the spawn path then takes the #420 `origin/<head_ref>` branch.
pub fn create_with_source_pr_fork(
    request: NodeCreateParams<'_>,
) -> Result<AgentNode, AgentNodeError> {
    create_with_source_pr_fork_configured(NodeCreateParams {
        configuration: None,
        initial_prompt: None,
        operation: None,
        ..request
    })
    .map(|created| created.node)
}

pub fn create_with_source_pr_fork_configured(
    request: NodeCreateParams<'_>,
) -> Result<CreatedNode, AgentNodeError> {
    let NodeCreateParams {
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree_override,
        name_override,
        head_repo_owner,
        head_repo_clone_url,
        configuration,
        initial_prompt,
        operation,
    } = request;
    let mesh = db::get_mesh_by_id(mesh_id)?;
    let mut prefs = crate::preferences::load().map_err(AgentNodeError::Backend)?;
    let selection = crate::preferences::resolve_default_provider(
        provider.map(str::to_string),
        mesh.default_provider.clone(),
        prefs.default_provider.clone(),
    );
    if let Some(value) = configuration {
        if value.spawn_option_id != selection && value.id != selection {
            return Err(AgentNodeError::InvalidConfiguration(
                "Configuration belongs to a different launch selection".into(),
            ));
        }
        prefs.spawn_configurations.retain(|c| c.id != value.id);
        prefs.spawn_configurations.push(value.clone());
    }
    let selected = configuration.map_or(selection.as_str(), |c| c.id.as_str());
    let plan = if let Some(plan) = configuration.and_then(|c| c.resolved.clone()) {
        plan
    } else {
        crate::preferences::launch_configurations::resolve(&prefs, selected, &Default::default())
            .map_err(AgentNodeError::InvalidConfiguration)?
    };
    let runtime = plan.harness.runtime;
    let provider_id = plan.spawn_option_id.clone();
    // After the plan is known, so a bad configuration stays the more specific
    // error, and on the harness that will launch rather than the raw request
    // string or a saved-configuration id.
    reject_unsupported_initial_prompt(&plan.harness.harness, initial_prompt)?;
    let configuration = Some(crate::preferences::launch_configurations::snapshot(plan));
    let use_worktree = use_worktree_override.unwrap_or(mesh.use_worktree);

    // Caller may supply a pre-derived name (e.g. `slugify_issue_title` for the
    // GitHub-issue spawn path). Validation/fallback is the caller's job — by
    // the time we get here, the name is assumed to be a valid `SLUG_REGEX`
    // match, which is also a safe directory name.
    let session_name = match name_override {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => crate::session_naming::on_spawn(),
    };
    // Scoped Meshes share one branch namespace, even when their checkout
    // containers differ. Qualify new identities without moving legacy nodes.
    let project = use_worktree
        .then(|| crate::git::project::resolve(path))
        .and_then(Result::ok);
    let session_name = if project
        .as_ref()
        .is_some_and(|p| !p.relative_folder.is_empty())
    {
        format!(
            "{}-m{mesh_id}",
            session_name.chars().take(85).collect::<String>()
        )
    } else {
        session_name
    };
    tracing::debug!(
        "agent_node::create: mesh_id={}, name={}, path={}, branch={}, provider={:?}, use_worktree={}, source_pr={:?}, source_pr_pinned_sha={:?}, head_repo_owner={:?}",
        mesh_id, session_name, path, branch, provider, use_worktree, source_pr, source_pr_pinned_sha, head_repo_owner
    );

    let worktree_db_name = if use_worktree {
        Some(session_name.as_str())
    } else {
        None
    };

    // Issue #1519: persist the exact resolved `worktree_path` at creation
    // (Mesh override → app default → `.worktrees`). `None` for Root
    // Nodes. Existing rows without it retain the legacy fallback via
    // `env::node_working_path`, so changing a setting affects future nodes
    // without moving live worktrees.
    let (worktree_path_owned, placed_env) =
        place_worktree(path, mesh.worktree_directory.as_deref(), worktree_db_name);
    let env_type = runtime.unwrap_or(placed_env);
    // Store the harness/profile id verbatim (issue #535) — no premature parse
    // to the legacy `Provider` enum, which would flatten an unknown profile id
    // to Anthropic. Resolution happens at the spawn seam. An absent provider
    // defaults to "anthropic", matching the prior `Provider::Anthropic` default.

    // Issue #2331: when the caller supplies a remote operation key, the key
    // binding shares this INSERT's transaction, so a lost response can never
    // orphan a node and a retry can never double-create one.
    //
    // `replayed` must survive the return: it is the only signal that a
    // *concurrent* duplicate already owns this key's start attempt. Collapsing
    // it into a plain `AgentNode` let a losing racer walk on into the start
    // phase and overwrite the winner's recorded `StartFailed` with `Started`,
    // reporting a node as running when it never started (#2331 review).
    let created = match operation {
        Some(binding) => match db::create_agent_node_with_operation(
            db::NewAgentNode {
                mesh_id,
                name: &session_name,
                path,
                branch,
                env: env_type,
                provider: &provider_id,
                worktree_name: worktree_db_name,
                source_issue,
                source_pr,
                source_pr_pinned_sha,
                use_worktree,
                head_repo_owner,
                head_repo_clone_url,
                worktree_path: worktree_path_owned.as_deref(),
                spawn_configuration: None,
            },
            configuration.as_ref(),
            binding,
        ) {
            // A concurrent duplicate already bound this key: return *that*
            // node, and tell the caller this was a replay so it never advances
            // to the start phase for work somebody else owns.
            Ok(db::CreateAgentNodeOutcome::Replayed(node)) => {
                return Ok(CreatedNode {
                    node,
                    replayed: true,
                })
            }
            Ok(db::CreateAgentNodeOutcome::Created(node)) => CreatedNode {
                node,
                replayed: false,
            },
            Err(db::CreateNodeWithOperationError::KeyConflict { key }) => {
                return Err(AgentNodeError::OperationKeyConflict { key })
            }
            Err(db::CreateNodeWithOperationError::Backend { source }) => {
                return Err(AgentNodeError::Db(source))
            }
        },
        None => db::create_agent_node_configured(
            db::NewAgentNode {
                mesh_id,
                name: &session_name,
                path,
                branch,
                env: env_type,
                provider: &provider_id,
                worktree_name: worktree_db_name,
                source_issue,
                source_pr,
                source_pr_pinned_sha,
                use_worktree,
                head_repo_owner,
                head_repo_clone_url,
                worktree_path: worktree_path_owned.as_deref(),
                spawn_configuration: None,
            },
            configuration.as_ref(),
        )
        .map(|node| CreatedNode {
            node,
            replayed: false,
        })?,
    };

    if let Some(project) = project {
        db::save_project_scope(created.node.id, &project.relative_folder)?;
        return Ok(CreatedNode {
            node: db::get_agent_node_by_id(created.node.id)?,
            replayed: created.replayed,
        });
    }

    Ok(created)
}

/// What one create produced, and whether this caller owns the start attempt.
///
/// `replayed` is `true` only when a *concurrent* duplicate already bound the
/// same operation key. The node row this transaction inserted is rolled back
/// and the recorded node is returned instead, so the caller must not start it:
/// somebody else owns that key's single start (issue #2331).
#[derive(Debug, Clone)]
pub struct CreatedNode {
    pub node: AgentNode,
    pub replayed: bool,
}

/// Single shared "create a fresh node" runner used by BOTH Tauri commands
/// (`commands::agent::{spawn_issue_agent, spawn_handover_agent, create_issue_node}`,
/// `commands::agent_node::create_agent_node`) and HTTP routes
/// (`http::routes::nodes::create`). Folds the mesh-lookup +
/// optional-branch-resolution + node-create sequence into one helper so
/// every entry point has the exact same shape (issue #1658 step 5).
///
/// Mirrors the `*_blocking` convention established by
/// [`regenerate_load_blocking`] / [`regenerate_apply_blocking`] /
/// [`regenerate_reload_blocking`] (see also `regenerate_agent_node`):
/// pure sync, returns [`AgentNodeError`], the caller wraps a single
/// `crate::commands::run_blocking` around the call. The offload
/// boundary stays explicit at every entry point.
///
/// # Arguments
///
/// * [`NodeCreateParams::mesh_id`] — the mesh to scope the new node under. A missing
///   mesh id surfaces as [`AgentNodeError::MeshNotFound`]. HTTP create
///   maps that variant to 400; HTTP import maps it to 404.
/// * [`NodeCreateParams::provider`] — the harness id to persist verbatim. The legacy
///   `provider.unwrap_or("anthropic")` fallback lives in
///   [`create_with_source_pr_fork`] (issue #538 default), so a `None`
///   here writes `"anthropic"` to the row. Callers that want the
///   mesh-level / app-wide default chain resolve
///   beforehand (`preferences::resolve_default_provider`) and pass
///   the resolved string in.
/// * [`NodeCreateParams::branch`] — a non-empty value short-circuits the
///   `git::get_default_branch_blocking` lookup. The mobile route
///   uses `"main"` verbatim; desktop paths that already resolved
///   the branch via the async wrapper pass their resolved string. An
///   empty value means "no override supplied" and resolves through
///   `commands::git::get_default_branch_blocking`
///   (with `"main"` fallback on open failure).
/// * [`NodeCreateParams::path`] — ignored by this runner, which reads the
///   path off the mesh row it looks up itself; callers pass `""`.
/// * [`NodeCreateParams::source_issue`] / [`NodeCreateParams::name_override`] /
///   [`NodeCreateParams::use_worktree_override`] —
///   forwarded unchanged to the underlying create family. PR-spawn
///   fields stay `None` (callers reach for
///   `create_pending_with_source_pr_fork` directly for that flow).
/// * `pending` — `true` lands the row in
///   [`SessionStatus::Pending`] via
///   [`create_pending_with_source_pr_fork`] (the desktop
///   two-stage stage-1 path); `false` lands in Idle via
///   [`create_with_source_pr_fork`].
///
/// # Why a `pending` parameter instead of two helpers?
///
/// The branch resolution + mesh lookup are identical between the
/// Idle and Pending paths. Splitting into `create_blocking` and
/// `create_pending_blocking` would duplicate those two steps.
/// `pending` is a one-bit switch on the final delegation and is
/// checked once at the bottom of the function.
pub fn create_blocking(
    request: NodeCreateParams<'_>,
    pending: bool,
) -> Result<AgentNode, AgentNodeError> {
    create_blocking_configured(
        NodeCreateParams {
            configuration: None,
            operation: None,
            ..request
        },
        pending,
    )
    .map(|created| created.node)
}

pub fn create_blocking_configured(
    request: NodeCreateParams<'_>,
    pending: bool,
) -> Result<CreatedNode, AgentNodeError> {
    let NodeCreateParams {
        mesh_id,
        branch,
        provider,
        source_issue,
        use_worktree_override,
        name_override,
        configuration,
        initial_prompt,
        operation,
        ..
    } = request;
    // 1. Mesh lookup. Map the missing-row case to the typed
    //    `AgentNodeError::MeshNotFound(mesh_id)` variant so the route
    //    can match on `Err(_)` patterns instead of inspecting a
    //    `String` payload. Other DB errors propagate as
    //    `AgentNodeError::Db` via the `From<rusqlite::Error>` impl above.
    let mesh = match db::get_mesh_by_id(mesh_id) {
        Ok(m) => m,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(AgentNodeError::MeshNotFound(mesh_id));
        }
        Err(e) => return Err(AgentNodeError::Db(e)),
    };

    // 2. Branch resolution. A non-empty `branch` short-circuits the git lookup
    //    entirely (the mobile route pins "main"; Tauri commands that already
    //    resolved via the async wrapper reuse their result). An empty value —
    //    the "no override supplied" sentinel callers pass — falls through to
    //    the canonical sync helper, with `"main"` as the open-failure fallback
    //    — matching `commands::git::get_default_branch`'s infallible contract.
    let branch = if branch.is_empty() {
        crate::commands::git::get_default_branch_blocking(mesh.path.clone())
            .unwrap_or_else(|_| "main".to_string())
    } else {
        branch.to_string()
    };

    // 3. Delegate to the existing create family. The fork-source and
    //    SHA-pinning fields stay `None` here (non-PR spawn callers); the
    //    PR-spawn entry point is `create_pending_with_source_pr_fork`
    //    directly, intentionally not folded into this runner (out of
    //    scope for #1658 step 5).
    //
    //    **Review-round-2 fix** (the previous delegation dropped
    //    `use_worktree_override` on `pending = true` because
    //    `create_pending_with_source_pr_fork` is hardcoded to forward
    //    `None`). We call the private
    //    `create_pending_with_source_pr_fork_and_worktree` directly
    //    (same-module access; in scope) so the override rides through
    //    both paths. A circuit-worker caller passing
    //    `use_worktree_override = Some(false)` now sees its override
    //    honored on the Pending path, not silently swallowed.
    if pending {
        create_pending_with_source_pr_fork_and_worktree(NodeCreateParams {
            mesh_id,
            // The path comes from the mesh row looked up above, which is why
            // callers of this runner pass `path: ""`.
            path: &mesh.path,
            branch: &branch,
            provider,
            source_issue,
            source_pr: None,
            source_pr_pinned_sha: None,
            use_worktree_override,
            name_override,
            head_repo_owner: None,
            head_repo_clone_url: None,
            configuration,
            initial_prompt,
            operation: None,
        })
        .map(|node| CreatedNode {
            node,
            replayed: false,
        })
    } else {
        create_with_source_pr_fork_configured(NodeCreateParams {
            mesh_id,
            // The path comes from the mesh row looked up above, which is why
            // callers of this runner pass `path: ""`.
            path: &mesh.path,
            branch: &branch,
            provider,
            source_issue,
            source_pr: None,
            source_pr_pinned_sha: None,
            use_worktree_override,
            name_override,
            head_repo_owner: None,
            head_repo_clone_url: None,
            configuration,
            initial_prompt,
            operation,
        })
    }
}

/// Like [`create_with_source_pr_fork`], but sets the initial status to [`SessionStatus::Pending`]
/// so the frontend can distinguish "node row exists, stage-2 not yet
/// started" from "node row exists, agent is idle and ready to re-spawn".
///
/// This is the fast stage-1 of the two-stage issue-spawn flow. The caller
/// is expected to invoke `start_node_background` to do the slow work
/// (git fetch, worktree create, PTY spawn) and update the status to
/// `Running` on success or `Error` on failure. The source-pr / fork-repo
/// fields follow the same contract as [`create`]: `source_pr` and
/// `source_pr_pinned_sha` must be `None` here; the PR-spawn entry point
/// is [`create_pending_with_source_pr_fork`] (issue #448).
///
/// **Issue #1658 step 5:** every live caller now reaches the Pending-status
/// path through [`create_blocking`] with `pending: true`. `create_pending`
/// is retained for the structural `source_pr = Some(_)` boundary assert
/// test coverage; `#[allow]` keeps the dead-code lint quiet.
#[allow(dead_code)]
pub fn create_pending(request: NodeCreateParams<'_>) -> Result<AgentNode, AgentNodeError> {
    assert!(
        request.source_pr.is_none(),
        "services::agent_node::create_pending refuses source_pr=Some(_); \
         use create_pending_with_source_pr_fork for PR spawns."
    );
    assert!(
        request.source_pr_pinned_sha.is_none(),
        "services::agent_node::create_pending refuses source_pr_pinned_sha=Some(_); \
         use create_pending_with_source_pr_fork for PR spawns that need SHA pinning."
    );
    // Single insert path; fork fields are None for the non-fork entry point.
    create_pending_with_source_pr_fork(NodeCreateParams {
        use_worktree_override: None,
        head_repo_owner: None,
        head_repo_clone_url: None,
        configuration: None,
        initial_prompt: None,
        operation: None,
        ..request
    })
}

/// Pending-node creation with an explicit worktree policy. Issue-driven
/// circuits use this instead of creating a legacy `autopilot_runs` row: the
/// circuit ledger remains the owner of orchestration state while the shared
/// Agent Node row still records the branch-backed environment it needs.
pub fn create_pending_with_worktree_override_configured(
    request: NodeCreateParams<'_>,
) -> Result<AgentNode, AgentNodeError> {
    create_pending_with_source_pr_fork_and_worktree(NodeCreateParams {
        source_pr: None,
        source_pr_pinned_sha: None,
        head_repo_owner: None,
        head_repo_clone_url: None,
        initial_prompt: None,
        ..request
    })
}

/// Like [`create_pending`], but also records the fork's owner login and
/// clone URL when the PR is from a fork (issue #443) and the PR's head
/// commit SHA for exact-pinning (issue #444). `commands::agent::create_pr_node`
/// calls this directly with the fork fields populated for fork PRs and
/// `None, None` for same-repo PRs.
pub fn create_pending_with_source_pr_fork(
    request: NodeCreateParams<'_>,
) -> Result<AgentNode, AgentNodeError> {
    create_pending_with_source_pr_fork_configured(NodeCreateParams {
        use_worktree_override: None,
        configuration: None,
        initial_prompt: None,
        operation: None,
        ..request
    })
}

pub fn create_pending_with_source_pr_fork_configured(
    request: NodeCreateParams<'_>,
) -> Result<AgentNode, AgentNodeError> {
    create_pending_with_source_pr_fork_and_worktree(NodeCreateParams {
        use_worktree_override: None,
        operation: None,
        ..request
    })
}

fn create_pending_with_source_pr_fork_and_worktree(
    request: NodeCreateParams<'_>,
) -> Result<AgentNode, AgentNodeError> {
    let mut node = create_with_source_pr_fork_configured(NodeCreateParams {
        operation: None,
        ..request
    })?
    .node;
    // Two writes (insert + status update) is one extra ~1ms SQLite round
    // trip. Acceptable: this function is on the fast path, and the second
    // write is the whole point — without it the new node would look
    // identical to a freshly-closed, ready-to-resume idle node.
    // Routes through SessionLifecycle (issue #132). `DbOnlySink` because
    // this function has no `AppHandle` and `Pending` doesn't emit any
    // events anyway.
    crate::agent::session_lifecycle::on_created(
        &crate::agent::session_lifecycle::DbOnlySink,
        node.id,
    )
    .map_err(AgentNodeError::Backend)?;
    node.status = SessionStatus::Pending;
    Ok(node)
}

/// Import a discovered CLI session as a node and store its session id.
///
/// `provider` is the conversation's originating harness, carried from
/// `ArchivedAgentNode::provider` (issue #2332). It is required: the archived
/// conversation only exists inside that harness, so an absent or unusable
/// value is refused with [`AgentNodeError::UnresumableHarness`] rather than
/// defaulted to `anthropic`, which used to launch a Cursor/AGY/Command Code
/// conversation through Claude Code.
///
/// The id is stored verbatim once validated, so a Proxied Provider row
/// (`claude:minimax`) keeps working. This path does not resolve a launch
/// configuration: an import is a resume of an existing conversation, not a new
/// launch. The mesh path comes from the mesh row. `use_worktree` follows
/// whether a name was supplied, including an empty one; the stored path is set
/// only when that name trims to something.
pub fn import_discovered_session(
    mesh_id: i64,
    cli_session_id: &str,
    branch: &str,
    worktree_name: Option<&str>,
    provider: &str,
) -> Result<AgentNode, AgentNodeError> {
    // Validate before the first DB call: an unknown executor must leave no row
    // behind at all, and this reads preferences rather than holding a
    // connection.
    resolve_resumable_harness(provider)?;
    let mesh = match db::get_mesh_by_id(mesh_id) {
        Ok(mesh) => mesh,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(AgentNodeError::MeshNotFound(mesh_id));
        }
        Err(error) => return Err(AgentNodeError::Db(error)),
    };
    let session_name = crate::session_naming::on_spawn();
    let use_worktree = worktree_name.is_some();
    let (mut worktree_path, mut env_type) = place_worktree(
        &mesh.path,
        mesh.worktree_directory.as_deref(),
        worktree_name,
    );
    // Import resumes an existing checkout. Built-in layouts remain valid
    // discovery locations even after the user changes the configured dir.
    if let Some(name) = worktree_name.map(str::trim).filter(|name| !name.is_empty()) {
        let current_exists = worktree_path
            .as_deref()
            .is_some_and(crate::git::project::is_materialized_checkout);
        if !current_exists {
            let default_dir = env::effective_worktree_dir_raw(&mesh.path, None, None);
            let built_in_paths = [
                env::resolve_agent_path_in_dir(&mesh.path, &default_dir, Some(name)),
                env::resolve_agent_path(&mesh.path, Some(name)),
            ];
            if let Some(existing) = built_in_paths
                .into_iter()
                .find(|path| crate::git::project::is_materialized_checkout(&path.host_path))
            {
                worktree_path = Some(existing.raw_path);
                env_type = existing.env_type;
            }
        }
    }
    let provider_id = provider;
    let node = db::create_agent_node(db::NewAgentNode {
        mesh_id,
        name: &session_name,
        path: &mesh.path,
        branch,
        env: env_type,
        provider: provider_id,
        worktree_name,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: worktree_path.as_deref(),
        spawn_configuration: None,
    })?;
    db::update_cli_session_id(node.id, cli_session_id)?;
    db::get_agent_node_by_id(node.id).map_err(AgentNodeError::from)
}

/// Sentence returned when a discovery-based resume names no harness, or a
/// harness that cannot resume. Both the mobile route and the desktop command
/// surface it verbatim (issue #2332).
pub const UNRESUMABLE_HARNESS: &str =
    "Buildmesh does not know which agent harness created this conversation, so it cannot resume it. \
Update Buildmesh to scan this harness, or start a new session instead.";

/// Refuse a discovery-based resume whose executor is missing or unusable, and
/// otherwise hand back the executor that will run it.
///
/// The value may be an adapter id (`cursor`) *or* a Spawn Option id
/// (`claude:minimax`) — a Proxied Provider row is stored verbatim on the node,
/// so import keeps accepting both. Only the harness half decides anything, so
/// the id is split with the existing [`crate::agent::provider::SpawnOptionId`]
/// parser and the harness half is validated with `harness_capabilities_for`,
/// the existing known-harness check (issue #1148): `None` for an unknown id,
/// which is what keeps an absent or unrecognised value from falling through to
/// `Provider::from_db_str`'s Anthropic default. A plain Terminal or a
/// non-resumable harness (DSH) passes the known-id question and fails the
/// second: there is a CLI session id to resume, but nothing to resume it with.
///
/// The typed [`crate::models::Provider`] is what the HTTP route needs to pick
/// the provider-specific session-id gate
/// (`http::request::parse_session_id_for_provider`), so this is the one place
/// that answers "which executor, and can it resume at all".
pub fn resolve_resumable_harness(
    provider: &str,
) -> Result<crate::models::Provider, AgentNodeError> {
    let trimmed = provider.trim();
    if trimmed.is_empty() {
        return Err(AgentNodeError::UnresumableHarness(
            UNRESUMABLE_HARNESS.to_string(),
        ));
    }
    let harness_id = crate::agent::provider::SpawnOptionId::from(trimmed)
        .harness_id()
        .to_string();
    let Some(capabilities) = crate::preferences::harness_capabilities_for(&harness_id) else {
        return Err(AgentNodeError::UnresumableHarness(
            UNRESUMABLE_HARNESS.to_string(),
        ));
    };
    if !capabilities.supports_resume {
        let label = crate::agent::harness_catalog::inspector_label_for_adapter(&harness_id)
            .unwrap_or(harness_id.as_str());
        return Err(AgentNodeError::UnresumableHarness(format!(
            "{label} cannot resume an archived conversation."
        )));
    }
    Ok(crate::preferences::resolve_harness_provider(trimmed))
}

/// Load a node row for the close path, treating an already-removed row as
/// `None` rather than an error. Closing is idempotent: a node deleted out from
/// under the UI — a Circuit step's `CloseAgentNode`, or any other backend-driven
/// delete — must not fault with "Query returned no rows". Without this, the
/// frontend's Phase-1 safety check aborts the close and leaves the ghost row on
/// screen (issue: closing a review node's stale tab).
fn find_agent_node_for_close(session_id: i64) -> Result<Option<AgentNode>, AgentNodeError> {
    match db::get_agent_node_by_id(session_id) {
        Ok(node) => Ok(Some(node)),
        // Legitimate for an already-retired row, but a bogus/negative id lands
        // here too — log so a genuinely bad close is diagnosable rather than
        // silently reported as a clean success.
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            tracing::warn!(
                "close for agent node {session_id}: row already removed, treating as a no-op"
            );
            Ok(None)
        }
        Err(error) => Err(AgentNodeError::Db(error)),
    }
}

/// Return whether closing the node can remove its worktree without risking work.
pub fn get_worktree_close_safety(session_id: i64) -> Result<WorktreeCloseSafety, AgentNodeError> {
    let Some(node) = find_agent_node_for_close(session_id)? else {
        // Already gone — there is no worktree of ours left to protect.
        return Ok(WorktreeCloseSafety {
            worktree_path: None,
            has_uncommitted: false,
            has_unpushed: false,
            is_detached: false,
        });
    };
    let Some(worktree_path) = env::node_worktree_path(&node).map(|r| r.host_path) else {
        return Ok(WorktreeCloseSafety {
            worktree_path: None,
            has_uncommitted: false,
            has_unpushed: false,
            is_detached: false,
        });
    };

    Ok(worktree::close_safety(&worktree_path))
}

/// Close an agent node (Phase 1 — fast and authoritative).
///
/// This kills the agent process tree and removes the node from the database
/// immediately, but deliberately does **not** delete the worktree directory:
/// that recursive delete is slow (a `node_modules`-heavy tree is tens of
/// thousands of files) and retry-prone on Windows, so blocking the close on it
/// is what makes the UI feel frozen. Instead, when a worktree should be removed
/// we record it in the durable `pending_worktree_removals` queue — atomically
/// with deleting the row — and let `process_pending_removals` reclaim the disk
/// in the background or on next launch. The kill stays synchronous here: it's
/// fast, and it's the one step we must finish before parting with the node so we
/// never leave a live agent running or fighting the eventual removal (#239).
pub fn delete(session_id: i64, remove_worktree: bool) -> Result<(), AgentNodeError> {
    let node = find_agent_node_for_close(session_id)?;

    let removal_path = if remove_worktree {
        crate::agent::process::PROCESS_REGISTRY.kill_session(session_id);
        node.as_ref()
            .and_then(env::node_worktree_path)
            .map(|r| r.host_path)
    } else {
        None
    };

    crate::session_naming::cleanup(session_id);
    // HookState is keyed by node id and lives for the process lifetime. A
    // deleted node must release its entry even when it had no live PTY (the
    // normal `notify_process_terminated` path is otherwise never reached),
    // or every create/delete cycle permanently grows the global map.
    //
    // Only the process-scoped stores are released here. The PTY fanout channel
    // is retired *after* the row delete commits, further down: a tombstone
    // fences channel creation, so retiring a node whose row survived a failed
    // delete would strand a live node with no way to get a channel back
    // (#2019).
    crate::agent::node_teardown::release(session_id);
    // Drop autopilot state too. The ledger delete is explicit as a
    // defensive belt: the table declares ON DELETE CASCADE and the
    // bundled SQLite build (rusqlite 0.32 / SQLite 3.46.0) has FK
    // enforcement on by default, so the parent agent_nodes DELETE
    // below already cascades — but a future system-libsqlite link
    // would have FK off by default and need this explicit DELETE to
    // avoid an orphan. Removing the row also frees the issue for a
    // poller retry if it is still open + labelled.
    crate::circuit::evaluator::unregister(session_id);
    db::legacy_retirement::delete_history(session_id)?;

    // Only the row delete (and its atomic worktree enqueue) is skipped when the
    // row is already gone — every per-node cleanup above still runs, so a
    // backend-driven delete that won the race leaves no process, naming, or
    // telemetry state behind.
    if let Some(node) = node.as_ref() {
        let removal = removal_path.as_deref().map(|p| (p, node.name.as_str()));
        db::delete_agent_node_enqueueing_removal(session_id, removal)?;
        // The row is gone, so this is the first moment the node is *permanently*
        // deleted and its PTY fanout channel can be retired (issue #2019). After
        // the commit, not before: a tombstone refuses channel creation, so
        // retiring while the row could still survive an error above would leave
        // a live node that can never stream again.
        let app_data_dir = crate::preferences::app_data_dir();
        crate::agent::node_teardown::release_after_delete(session_id, app_data_dir.as_deref());
        // Announce the deletion so a frontend that did not initiate it — the
        // Circuit worker's `CloseAgentNode`, cancelled-run retirement, abort
        // compensation — drops the card and disposes its terminal. A
        // user-initiated close has already removed the row optimistically and
        // disposes on its own success path, so its listener call is a no-op.
        if let Some(app) = crate::http::app_handle() {
            let _ = tauri::Emitter::emit(
                app,
                "node-deleted",
                crate::commands::agent::NodeDeletedPayload {
                    node_id: session_id,
                },
            );
        }
    }
    // The raw-output subscription is node-scoped, so process exit/restart
    // deliberately preserves it. Once the row deletion commits, this is the
    // authoritative backend cleanup seam (idempotent with frontend disposal).
    // Both Agent and Build/Run maps key by node id, so a single helper
    // releases them in lock-step -- reaching into the globals directly would
    // risk half-cleaning a node that exists on both surfaces.
    crate::pty::sink::unregister_node_sinks(session_id);
    Ok(())
}

/// Serializes drains. Each drain processes the *whole* queue, so two running at
/// once (e.g. closing two nodes in quick succession, or a close overlapping the
/// startup reconcile) would both try to remove the same just-enqueued worktree
/// and race on its `.removing` staging directory. Holding this for the drain's
/// duration means a second drain waits, then re-lists and finds the first
/// already dequeued — no wasted work, no spurious cleanup-failed warnings.
static DRAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Drain the pending worktree-removal queue: remove each queued worktree's
/// directory, dequeuing only the ones that succeed. Removals that still fail
/// (e.g. a handle is somehow held) stay queued for the next drain or next
/// launch, and are returned so the caller can warn the user. Safe to run from
/// the close path and from startup reconcile — `remove_one_worktree` is
/// idempotent (a missing directory counts as already removed).
///
/// Issue #2139 — a failure is no longer only "returned". Each one is persisted
/// on its queue row (failed step, OS error, attempt count, backoff deadline)
/// and reported at most once per *unchanged* blocker: the drain backs off
/// instead of retrying a block it cannot clear, and the user hears about a
/// stuck worktree once instead of on every drain.
pub fn process_pending_removals() -> Vec<BlockedCleanup> {
    drain_pending_removals(now_epoch_ms(), backoff_secs)
}

/// The drain itself, with its clock and backoff schedule injected.
///
/// Production passes the wall clock and the exponential schedule above; the
/// tests inject a fixed clock and a zero backoff so they can run a *real*
/// second drain immediately instead of either waiting out the first window or
/// simulating the bookkeeping half by hand (issue #2139 review round 1).
pub(crate) fn drain_pending_removals(
    now: i64,
    backoff: impl Fn(i64) -> i64,
) -> Vec<BlockedCleanup> {
    let _guard = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let pending = match db::list_pending_worktree_removals() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("could not read pending worktree removals: {}", e);
            return Vec::new();
        }
    };

    // Backoff gate: an entry inside its retry window is left untouched — no
    // attempt, no new failure row, no notification. This is what stops the
    // drain from hammering a block it already knows about (issue #2139).
    let due: Vec<PendingWorktreeRemoval> = pending
        .into_iter()
        .filter(|removal| {
            if removal.retry_due(now) {
                true
            } else {
                tracing::debug!(
                    "worktree cleanup for {} is backing off until {} ms; skipping",
                    removal.node_name,
                    removal.retry_not_before
                );
                false
            }
        })
        .collect();

    let failures = process_removals(
        due,
        crate::git::worktree::remove_one_worktree_and_branch_detailed,
        db::delete_pending_worktree_removal,
        // Issue #653 deleter-side guard: skip paths whose `warm_worktrees`
        // row is currently `claimed` (a live spawn has just adopted the
        // directory). The skip deques the pending removal — claim supersedes
        // tombstone intent, and the live agent's worktree must survive. When
        // the spawn's `forget_after_spawn` later drops the row, the path is
        // no longer claimed and any *new* pending removal (e.g. a subsequent
        // close of the same node) flows through normally.
        db::warm_pool_claims_path,
    );

    record_blocked_cleanups(failures, now, backoff)
}

/// Persist each failed attempt, then decide which ones the user still needs to
/// hear about. Kept separate from [`process_removals`] so the "what happened"
/// half stays pure and DB-free while the bookkeeping half is the only DB-aware
/// part of the drain.
///
/// A blocker that the user has already been told about (`notified_error` holds
/// its signature) produces no event: a worktree that is still held by the same
/// process in the same way is not new information (issue #2139).
fn record_blocked_cleanups(
    failures: Vec<(
        PendingWorktreeRemoval,
        crate::git::worktree::WorktreeRemovalFailure,
    )>,
    now: i64,
    backoff: impl Fn(i64) -> i64,
) -> Vec<BlockedCleanup> {
    let mut blocked = Vec::new();
    for (removal, failure) in failures {
        let attempts = removal.attempt_count + 1;
        let retry_not_before = now + backoff(attempts) * 1000;
        if let Err(e) = db::record_pending_removal_failure(
            &removal.worktree_path,
            failure.operation,
            &failure.detail,
            now,
            retry_not_before,
        ) {
            tracing::error!(
                "worktree cleanup for {} failed but its failure could not be recorded: {}",
                removal.node_name,
                e
            );
        }

        let notify = !removal.already_notified(failure.operation, &failure.detail, now);
        if notify {
            if let Err(e) = db::mark_pending_removal_notified(
                &removal.worktree_path,
                &crate::models::blocker_signature(failure.operation, &failure.detail),
                now,
            ) {
                tracing::error!(
                    "could not record that the user was warned about {}: {}",
                    removal.node_name,
                    e
                );
            }
        }

        let record =
            removal.with_failure(failure.operation, &failure.detail, now, retry_not_before);
        tracing::warn!(
            "worktree cleanup for {} still blocked after {} attempt(s): {} failed — {} \
             (next automatic retry not before {} ms{})",
            record.node_name,
            record.attempt_count,
            record.last_operation.as_deref().unwrap_or("unknown step"),
            failure.detail,
            retry_not_before,
            if notify {
                ""
            } else {
                "; blocker unchanged, warning suppressed"
            }
        );
        blocked.push(BlockedCleanup {
            removal: record,
            notify,
        });
    }
    blocked
}

/// A worktree cleanup that is still blocked, together with the evidence the UI
/// needs to make it actionable (issue #2139): the queue row after the failed
/// attempt, and whether this blocker is being reported for the first time.
pub struct BlockedCleanup {
    pub removal: PendingWorktreeRemoval,
    /// `false` when the user has already been told about this exact blocker —
    /// the drain still retries (with backoff), it just does not warn again.
    pub notify: bool,
}

/// The backoff schedule for automatic worktree-removal retries, in seconds.
///
/// A blocked cleanup is usually blocked by something the user must close (an
/// Explorer window, a stray helper). Retrying every few milliseconds buys
/// nothing and floods the log and the UI, so the drain waits: 5s, 10s, 20s …
/// capped at 5 minutes. The cap matters because a worktree blocked overnight
/// must still be attempted soon after launch rather than much later.
///
/// `attempt_count` is the number of failures *including* the one just made.
pub fn backoff_secs(attempt_count: i64) -> i64 {
    let exponent = attempt_count.saturating_sub(1).clamp(0, 30) as u32;
    (CLEANUP_BACKOFF_BASE_SECS << exponent).min(CLEANUP_BACKOFF_MAX_SECS)
}

const CLEANUP_BACKOFF_BASE_SECS: i64 = 5;
const CLEANUP_BACKOFF_MAX_SECS: i64 = 300;

/// Epoch milliseconds, used for the queue's attempt/backoff bookkeeping.
fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Every queued worktree cleanup with its current bookkeeping, in queue order.
/// The blocked-cleanup surface (and its actions) reads from here so what the
/// user sees is exactly what the drain knows.
pub fn list_blocked_worktree_cleanups() -> db::SqlResult<Vec<PendingWorktreeRemoval>> {
    db::list_pending_worktree_removals()
}

/// Retry one blocked cleanup now, ignoring its backoff.
///
/// A failed retry records the failure and schedules the next backoff, so
/// hammering a blocked path through the UI cannot turn into a retry storm.
///
/// Issue #653 guard, same as the drain: a path the warm pool currently holds as
/// `claimed` belongs to a live spawn's worktree, so the removal is skipped and
/// the tombstone is dequeued (claim supersedes the close's intent). Without
/// this check a manual Retry could delete a live agent's directory — the drain
/// is careful, the manual path has to be too.
pub fn retry_pending_worktree_removal(path: &str) -> CleanupRetryOutcome {
    // Serialise against the drain: a background drain and a manual retry must
    // not both remove (and both record) the same worktree.
    let _guard = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let record = match db::get_pending_worktree_removal(path) {
        Ok(Some(record)) => record,
        Ok(None) => return CleanupRetryOutcome::Gone,
        Err(e) => {
            // A read failure is not evidence that nothing is queued: say so,
            // attempt nothing, and leave the row exactly as it is.
            tracing::error!(
                "could not read the pending worktree removal for {}; not retrying: {}",
                path,
                e
            );
            return CleanupRetryOutcome::QueueReadFailed;
        }
    };

    match db::warm_pool_claims_path(path) {
        Ok(true) => {
            tracing::info!(
                "warm_pool: skipping manual retry for {} — path is claimed by a warm-pool spawn",
                path
            );
            if let Err(e) = db::delete_pending_worktree_removal(path) {
                tracing::error!(
                    "warm_pool: failed to dequeue superseded tombstone for claimed path {}: {}",
                    path,
                    e
                );
            }
            return CleanupRetryOutcome::ClaimedByLiveSpawn;
        }
        Ok(false) => {}
        Err(e) => {
            // Same fail-closed rule as the drain: an unreadable claim means we
            // do not know whether a live spawn owns this directory, so nothing
            // is touched.
            tracing::error!(
                "warm_pool: is_claimed check failed for {} ({}); not retrying",
                path,
                e
            );
            return CleanupRetryOutcome::QueueReadFailed;
        }
    }

    match crate::git::worktree::remove_one_worktree_and_branch_detailed(path) {
        Ok(()) => {
            if let Err(e) = db::delete_pending_worktree_removal(path) {
                tracing::error!(
                    "worktree {} was removed but failed to dequeue it: {}",
                    record.node_name,
                    e
                );
            }
            CleanupRetryOutcome::Removed
        }
        Err(failure) => {
            let now = now_epoch_ms();
            let attempts = record.attempt_count + 1;
            let retry_not_before = now + backoff_secs(attempts) * 1000;
            if let Err(e) = db::record_pending_removal_failure(
                path,
                failure.operation,
                &failure.detail,
                now,
                retry_not_before,
            ) {
                tracing::error!(
                    "could not record the failed retry for {}: {}",
                    record.node_name,
                    e
                );
            }
            CleanupRetryOutcome::StillBlocked(record.with_failure(
                failure.operation,
                &failure.detail,
                now,
                retry_not_before,
            ))
        }
    }
}

/// The outcome of a user-initiated retry of one blocked cleanup.
pub enum CleanupRetryOutcome {
    /// The directory and its git bookkeeping are gone; the queue entry is
    /// dequeued.
    Removed,
    /// Still blocked. Carries the updated row so the UI can show the new
    /// evidence without a second read.
    StillBlocked(PendingWorktreeRemoval),
    /// The queue entry no longer exists — already cleaned or dismissed.
    Gone,
    /// The warm pool has adopted this path as a live spawn's worktree. The
    /// tombstone was dequeued and the directory was left alone (issue #653).
    ClaimedByLiveSpawn,
    /// The queue row or the claim guard could not be read, so nothing was
    /// attempted. Distinct from `Gone`, which is a definite "nothing queued".
    QueueReadFailed,
}

/// What "Keep worktree" found on disk, so the UI can tell the truth about what
/// it did (issue #2139). The user-visible wording lives in [`Self::message`];
/// this type is not sent over the wire.
#[derive(Debug, Clone)]
pub enum WorktreeCleanupDismissal {
    /// The worktree was exactly where the queue row said, and is still there.
    /// Only the cleanup intent was cancelled.
    Unchanged,
    /// A removal had already renamed the worktree to `<path>.removing`; it has
    /// been moved back so the directory is usable again.
    RestoredFromStaging,
    /// The staged copy was moved back, but the staged delete had already
    /// removed part of it, so the restored directory is incomplete.
    RestoredIncomplete,
    /// The original path is absent and there is no staging copy to restore, so
    /// there is nothing left on disk to keep.
    NothingOnDisk,
    /// The staged copy could not be moved back — something still holds it — so
    /// the worktree is *not* where the queue row says it is. The cleanup intent
    /// is preserved: the row stays queued and the drain keeps trying, exactly
    /// as it would for any other blocked removal (issue #2139 review round 2).
    RestoreFailed,
}

impl WorktreeCleanupDismissal {
    /// What the user is told, given the worktree path that was requested.
    pub fn message(&self, path: &str) -> String {
        match self {
            Self::Unchanged => format!("Worktree kept in place: {path}"),
            Self::RestoredFromStaging => {
                format!("Worktree restored to {path} — the cleanup had already moved it aside")
            }
            Self::RestoredIncomplete => format!(
                "Worktree restored to {path}, but the cleanup had already deleted part of it"
            ),
            Self::NothingOnDisk => {
                format!("Nothing left to keep at {path} — the cleanup had already removed it")
            }
            Self::RestoreFailed => format!(
                "Couldn't move the worktree back to {path} — something still holds {path}.removing. \
                 The worktree is still there and the cleanup stays queued, so close what is \
                 holding it and try Keep worktree again."
            ),
        }
    }
}

/// The result of "Keep worktree": what happened on disk, and what happened to
/// the cleanup intent.
pub struct WorktreeCleanupDismissed {
    pub kind: WorktreeCleanupDismissal,
    /// Whether the cleanup intent was cancelled (the queue row is gone). False
    /// when the restore could not finish: the intent is deliberately preserved
    /// so the drain keeps trying, rather than abandoning a worktree that is
    /// not even where the queue row says it is.
    pub cancelled: bool,
    /// Set when the row *should* have been dequeued but could not be (only
    /// possible for the non-`RestoreFailed` outcomes).
    pub dequeue_error: Option<String>,
}

/// "Keep worktree" — the user has decided to leave the directory where it is.
///
/// The cleanup intent is cancelled so the drain stops retrying and stops
/// warning. The disk is only touched in the one case where a removal had already
/// moved the worktree aside (the `<path>.removing` staging from
/// `remove_worktree_dir_with_retry`): the staged copy is moved back, because
/// leaving it there would strand the agent's work under a name the user never
/// chose while the queue row claimed the worktree was still at `path`. A staged
/// delete that had already removed part of the tree is reported as such rather
/// than promised as an intact restore.
///
/// If that move-back fails — something still holds the staged copy — nothing is
/// cancelled: the disk no longer matches what the queue row says, so cancelling
/// the intent would abandon both the folder and the git bookkeeping. The row
/// stays queued, the drain keeps trying, and the user is told exactly that.
pub fn dismiss_pending_worktree_removal(path: &str) -> WorktreeCleanupDismissed {
    let kind = restore_staged_worktree(path);
    if matches!(kind, WorktreeCleanupDismissal::RestoreFailed) {
        return WorktreeCleanupDismissed {
            kind,
            cancelled: false,
            dequeue_error: None,
        };
    }
    let dequeue_error = db::delete_pending_worktree_removal(path).err().map(|e| {
        tracing::error!(
            "keep worktree: cancelled the intent for {} but failed to dequeue it: {}",
            path,
            e
        );
        e.to_string()
    });
    WorktreeCleanupDismissed {
        kind,
        cancelled: dequeue_error.is_none(),
        dequeue_error,
    }
}

/// Undo the staging rename a removal may have left behind at `path`, reporting
/// what was actually found. Returns `Unchanged` when the worktree is where the
/// queue row says it is.
fn restore_staged_worktree(path: &str) -> WorktreeCleanupDismissal {
    let staging = format!("{}.removing", path.trim_end_matches(['/', '\\']));
    if std::path::Path::new(path).exists() {
        // The worktree is intact where it belongs; any leftover staging is a
        // separate, already-proven-unheld directory that the next removal will
        // reclaim on its own.
        return WorktreeCleanupDismissal::Unchanged;
    }
    if !std::path::Path::new(&staging).exists() {
        return WorktreeCleanupDismissal::NothingOnDisk;
    }
    match std::fs::rename(&staging, path) {
        Ok(()) => {
            // A staged delete that had started would have left the copy
            // partial; say so rather than promising an intact worktree.
            if staged_delete_had_started(path) {
                WorktreeCleanupDismissal::RestoredIncomplete
            } else {
                WorktreeCleanupDismissal::RestoredFromStaging
            }
        }
        Err(e) => {
            // The rename back failed, so something still holds the staged copy.
            // Nothing is restored, and the caller must keep the cleanup intent
            // rather than cancel it: the worktree is not where the queue row
            // says it is (issue #2139 review round 2).
            tracing::warn!(
                "keep worktree: could not move {} back to {}: {}",
                staging,
                path,
                e
            );
            WorktreeCleanupDismissal::RestoreFailed
        }
    }
}

/// Whether a staged copy shows signs that its delete had already begun. The
/// `.git` link is what makes a directory a worktree at all, so a staged copy
/// without it was gutted mid-delete — as was one with nothing left inside.
fn staged_delete_had_started(path: &str) -> bool {
    let dir = std::path::Path::new(path);
    if !dir.join(".git").exists() {
        return true;
    }
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().count() == 0)
        .unwrap_or(false)
}

/// Pure orchestration for the drain: for each removal, attempt the directory
/// remove; on success dequeue it; on failure keep it queued and collect it.
/// Dependencies are injected so the "dequeue only on success" invariant — the
/// thing that guarantees failed cleanups get retried rather than lost — is
/// testable without the global DB or a real filesystem.
///
/// `is_claimed` is the issue #653 deleter-side guard: when it returns `true`
/// for a removal's path, the drain skips the `remove` call (the directory
/// belongs to a live spawn's worktree — deleting it would destroy the
/// agent's work) AND dequeues the pending entry (claim supersedes tombstone
/// intent). It does NOT append to `failures` — the path is alive, not
/// broken; no user warning is warranted.
fn process_removals(
    pending: Vec<PendingWorktreeRemoval>,
    remove: impl Fn(&str) -> Result<(), crate::git::worktree::WorktreeRemovalFailure>,
    dequeue: impl Fn(&str) -> db::SqlResult<()>,
    is_claimed: impl Fn(&str) -> db::SqlResult<bool>,
) -> Vec<(
    PendingWorktreeRemoval,
    crate::git::worktree::WorktreeRemovalFailure,
)> {
    let mut failures = Vec::new();
    for removal in pending {
        match is_claimed(&removal.worktree_path) {
            Ok(true) => {
                // Issue #653 — the warm pool has this path as a live spawn's
                // worktree. Dequeue the tombstone (the spawn's intent
                // supersedes the close's intent) but do NOT touch the
                // directory. The spawn's `forget_after_spawn` will drop the
                // `claimed` row when the agent is up; any subsequent close
                // that enqueues a fresh tombstone at this path will be
                // handled normally because the row is then gone.
                tracing::info!(
                    "warm_pool: skipping pending removal for {} — path is claimed by a warm-pool spawn",
                    removal.worktree_path
                );
                if let Err(e) = dequeue(&removal.worktree_path) {
                    tracing::error!(
                        "warm_pool: failed to dequeue superseded tombstone for claimed path {}: {}",
                        removal.worktree_path,
                        e
                    );
                }
            }
            Ok(false) => match remove(&removal.worktree_path) {
                Ok(()) => {
                    if let Err(e) = dequeue(&removal.worktree_path) {
                        tracing::error!(
                            "removed worktree {} but failed to dequeue it: {}",
                            removal.worktree_path,
                            e
                        );
                    }
                }
                Err(failure) => {
                    tracing::warn!(
                        "worktree removal for {} failed during {}, will retry: {}",
                        removal.node_name,
                        failure.operation,
                        failure.detail
                    );
                    failures.push((removal, failure));
                }
            },
            Err(e) => {
                // The guard query failed — fail closed (don't remove the
                // directory) but DON'T drop the tombstone either, so the
                // next drain retries. This is the conservative choice: a
                // transient DB error must not cascade into "agent's worktree
                // silently deleted because we couldn't read the pool table".
                //
                // Deliberately NOT pushing to `failures`: the caller
                // (`commands::agent_node::drain_pending_removals`) emits a
                // `worktree-cleanup-failed` Tauri event per failure, which
                // renders a "worktree cleanup failed" toast to the user —
                // but no worktree cleanup was attempted here, only a pool-
                // claim read. A transient SQLite error during the guard
                // check must NOT surface as user-facing "cleanup failed"
                // noise; the next drain retries silently.
                tracing::warn!(
                    "warm_pool: is_claimed check failed for {} ({}); leaving tombstone queued for retry",
                    removal.worktree_path,
                    e
                );
            }
        }
    }
    failures
}

/// Validate that a node's status is eligible for regeneration.
///
/// Allowed: `Idle`, `AwaitingInput`, `Error`, `Running`, `Suspended`,
/// `Completed` — the node is in a state we can either kill-and-replace
/// or, in the `Suspended` case, just respawn (no live process to kill).
/// The orchestrator (see `regenerate` step 3) branches on `Suspended` to
/// skip the unconditional `kill_agent` call.
/// Rejected: `Spawning` / `Pending` (another spawn is in flight) and
/// `Archived` (the node is closed-but-historical).
///
/// Extracted from the #775 spec (step 2) so the orchestrator and the
/// tests share one source of truth. `Suspended` was added so the
/// Regenerate picker can also act as a "Resume" entry for Suspended
/// nodes — those have no live process to kill (crash-recovery was
/// killed on app exit; autopilot-gate never spawned). The kill-skip
/// branch in `regenerate` prevents `kill_agent_blocking`'s
/// unconditional `on_idle` tail (commands/agent.rs:1172) from
/// clobbering Suspended→Idle before the new spawn lands.
///
/// `Completed` was added alongside (the second issue surfaced after
/// the autopilot wrap-up flow matured — issue #485 left autopilot
/// nodes stuck in a "viewable but un-interactable" state from the
/// Regenerate picker's perspective). The agent PTY is still alive at
/// that point (autopilot doesn't kill the process when it writes the
/// wrap-up complete), so the orchestrator's normal kill+respawn path
/// applies — `should_skip_kill_for_regenerate(Completed)` stays
/// `false`. `decide_resume` reuses the captured `cli_session_id`, so a
/// same-harness Regenerate picks up exactly where the autopilot node
/// left off.
pub fn validate_status_eligible(status: SessionStatus) -> Result<(), String> {
    match status {
        SessionStatus::Idle
        | SessionStatus::AwaitingInput
        | SessionStatus::Error
        | SessionStatus::Running
        | SessionStatus::Suspended
        | SessionStatus::Completed
        | SessionStatus::Ready => Ok(()),
        _ => Err(format!(
            "regenerate unavailable: node is in {} state (must be idle, awaiting_input, error, running, suspended, completed, or ready)",
            status.to_db_str()
        )),
    }
}

/// Whether the [`regenerate`] orchestrator should skip its pre-spawn
/// `kill_agent` call. True when the node has no live process to
/// terminate; `kill_agent_blocking`'s tail calls
/// `session_lifecycle::on_idle` which unconditionally writes `Idle`
/// (commands/agent.rs:1172), and for a Suspended node that write
/// would clobber Suspended→Idle BEFORE the new spawn lands.
///
/// Pure — extracted from `regenerate` so the rule is unit-testable
/// without Tauri or a real DB. Keep the helper narrow: only `Suspended`
/// qualifies today. Future "skip" states (e.g. an `Archived` revival)
/// would be added here with a corresponding test.
pub fn should_skip_kill_for_regenerate(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Suspended)
}

/// Decide whether to pass the existing `cli_session_id` to the new
/// agent's spawn as a `--resume` arg, or start a fresh session.
///
/// Continue iff ALL of:
///   * the new provider's adapter has `supports_resume() == true`
///     (the executor accepts the resume flag);
///   * the new provider shares the same Agent Harness as the old —
///     the existing `cli_session_id` is bound to the old harness's
///     session format (Claude Code UUIDs are not Codex session ids,
///     so a cross-harness resume would land on a session the new
///     harness can't read);
///   * the node actually has a `cli_session_id` to resume from.
///
/// Pure — extracted from `regenerate` so the rule is unit-testable
/// without Tauri or a real DB. The harness comparison uses the
/// leading `harness_id` part of the composite id
/// (`<harness>:<provider_id>`, issue #575): `claude:minimax` and
/// `claude:openrouter` share the `claude` harness, so a swap between
/// them continues the session.
pub fn decide_resume(
    old_provider: &str,
    new_provider: &str,
    cli_session_id: Option<&str>,
) -> Option<String> {
    // Parse both providers through the typed `SpawnOptionId` once each
    // (issue #1659 item 1) and compare the harness halves. The
    // provider half is irrelevant here — resume lives on the executor
    // keyed by `old_harness == new_harness`. Route the executor lookup
    // through the typed entry point so `new_provider` is parsed exactly
    // once on this path (review #1730 fix: previously
    // `resolve_harness_provider(&str)` re-parsed the id internally).
    let old_id = SpawnOptionId::from(old_provider);
    let new_id = SpawnOptionId::from(new_provider);
    let same_harness = old_id.harness_id() == new_id.harness_id();
    let new_provider_enum = resolve_harness_provider_for(&new_id);
    if same_harness && new_provider_enum.adapter().supports_resume() {
        cli_session_id.map(String::from)
    } else {
        None
    }
}

/// Sync helper for step 1 of the regenerate orchestrator: load the
/// node, validate its status, and return the old provider + whether
/// the kill step should be skipped (true only for `Suspended`).
///
/// Pure — no async, no thread-pool orchestration. The command boundary
/// (`commands::agent_node::regenerate_agent_node`) is the single place
/// that wraps this in `crate::commands::run_blocking` (issue #1380
/// review round-2 feedback 1).
///
/// # Architecture
///
/// The previous async `regenerate` orchestrator that called the three
/// sync helpers DIRECTLY on the Tokio runtime was a phantom offload —
/// the helpers are pure sync, but the orchestrator's `await` never
/// crossed a thread-pool boundary. Round-2 review caught it. The
/// orchestrator is now inline in the command, where each helper
/// call IS wrapped in `run_blocking`.
pub fn regenerate_load_blocking(node_id: i64) -> Result<(String, bool), AgentNodeError> {
    let node = db::get_agent_node_by_id(node_id)?;
    validate_status_eligible(node.status).map_err(AgentNodeError::Status)?;
    Ok((
        node.provider.clone(),
        should_skip_kill_for_regenerate(node.status),
    ))
}

/// Sync helper for steps 4–6 of the regenerate orchestrator: write the
/// new `provider` column, reload the row, and return whether the new
/// spawn should resume the existing session (same-harness with a
/// captured `cli_session_id`) or start fresh.
///
/// Takes `&str` references — the caller owns the strings and the
/// helper does NOT need to capture them into a `'static` closure.
/// `decide_resume` owns the cross-harness rejection rule.
pub fn regenerate_apply_blocking(
    node_id: i64,
    old_provider: &str,
    new_provider: &str,
) -> Result<bool, AgentNodeError> {
    let node = db::get_agent_node_by_id(node_id)?;
    let prefs = crate::preferences::load().map_err(AgentNodeError::Backend)?;
    let plan = crate::preferences::launch_configurations::resolve(
        &prefs,
        new_provider,
        &Default::default(),
    )
    .map_err(AgentNodeError::InvalidConfiguration)?;
    let resume = decide_resume(
        old_provider,
        &plan.spawn_option_id,
        node.cli_session_id.as_deref(),
    )
    .is_some();
    let runtime = plan.harness.runtime.unwrap_or_else(|| {
        crate::env::resolve_raw_path(&crate::env::node_working_path(&node).raw_path).env_type
    });
    db::replace_node_launch(
        node_id,
        &crate::preferences::launch_configurations::snapshot(plan),
        runtime,
    )
    .map_err(|error| AgentNodeError::Backend(error.to_string()))?;
    Ok(resume)
}

/// Sync helper for step 8 of [`regenerate`]: reload the node row
/// after the spawn lands so the command returns the post-spawn
/// state. The spawn pipeline may have updated `cli_session_id`,
/// `status_changed_at`, etc.
pub fn regenerate_reload_blocking(node_id: i64) -> Result<AgentNode, AgentNodeError> {
    db::get_agent_node_by_id(node_id).map_err(AgentNodeError::from)
}

#[cfg(test)]
#[path = "agent_node_tests.rs"]
mod tests;
