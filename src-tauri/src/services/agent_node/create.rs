//! Fresh and pending Agent Node creation, launch selection, and worktree placement.

use super::*;
/// Worktree path and environment for one node. `None` or a blank segment is
/// a Root Node: no stored path, and the environment comes from the mesh root.
/// A non-empty segment stores `<effective dir>/<name>`.
pub(super) fn place_worktree(
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
pub(super) fn reject_unsupported_initial_prompt(
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
    if adapter.accepts_initial_prompt() {
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

pub fn create_pending_with_source_pr_fork_configured(
    request: NodeCreateParams<'_>,
) -> Result<AgentNode, AgentNodeError> {
    create_pending_with_source_pr_fork_and_worktree(NodeCreateParams {
        use_worktree_override: None,
        operation: None,
        ..request
    })
}

pub(super) fn create_pending_with_source_pr_fork_and_worktree(
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
