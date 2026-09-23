//! Agent Node service — creation, deletion, and lifecycle orchestration

use crate::agent::provider::SpawnOptionId;
use crate::db;

/// Upgrade legacy history outside DB locks; the existing JSON column keeps the operation additive.
pub fn migrate_launch_history() -> Result<(), String> {
    let prefs = crate::preferences::load()?;
    let mut mappings: std::collections::HashMap<String, String> = prefs.spawn_configurations.iter().filter(|c| c.generated.is_some())
        .filter(|c| !prefs.spawn_configurations.iter().any(|other| other.id == c.spawn_option_id))
        .map(|c| (c.spawn_option_id.clone(), c.id.clone())).collect();
    // Stored references to retired catalogue recipes (`launch/<harness>`)
    // heal to the equivalent bare Spawn Option; user-owned and explicitly
    // deleted ids are excluded by the helper.
    mappings.extend(crate::preferences::launch_configurations::retired_configuration_aliases(&prefs));
    db::migrate_launch_selections(&mappings).map_err(|e| e.to_string())?;
    let nodes = db::legacy_launch_nodes().map_err(|e| e.to_string())?;
    for node in nodes {
        let mut snapshot_prefs = prefs.clone();
        let selection = if let Some(configuration) = node.launch_configuration.as_ref() {
            snapshot_prefs.spawn_configurations.retain(|c| c.id != configuration.id);
            snapshot_prefs.spawn_configurations.push(configuration.clone());
            configuration.id.as_str()
        } else { node.provider.as_str() };
        let harness = crate::agent::provider::SpawnOptionId::from(node.provider.as_str()).harness_id;
        let mesh = db::get_mesh_harness_overrides(node.mesh_id).map_err(|e| e.to_string())?
            .and_then(|values| values.get(&harness).cloned()).unwrap_or_default();
        match crate::preferences::launch_configurations::capture_legacy(&snapshot_prefs, selection, &mesh) {
            Ok(plan) => db::set_node_launch_snapshot(node.id, &crate::preferences::launch_configurations::snapshot(plan))?,
            Err(error) => tracing::warn!(node_id = node.id, "Legacy launch snapshot unavailable: {error}"),
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
    /// A backend orchestrator call (`spawn_agent_inner` today, but
    /// the variant is intentionally generic so future downstream
    /// calls share the path) failed. The wrapped string is the
    /// underlying error message verbatim, so the frontend's toast
    /// surfaces the same text the Tauri command returns.
    Backend(String),
}

impl std::fmt::Display for AgentNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentNodeError::Db(e) => write!(f, "{}", e),
            AgentNodeError::Status(e) => write!(f, "{}", e),
            AgentNodeError::MeshNotFound(mesh_id) => write!(f, "mesh {mesh_id} not found"),
            AgentNodeError::InvalidConfiguration(e) => write!(f, "{}", e),
            AgentNodeError::Backend(e) => write!(f, "{}", e),
        }
    }
}

impl From<rusqlite::Error> for AgentNodeError {
    fn from(e: rusqlite::Error) -> Self {
        AgentNodeError::Db(e)
    }
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
#[allow(dead_code, clippy::too_many_arguments)]
pub fn create(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    assert!(
        source_pr.is_none(),
        "services::agent_node::create refuses source_pr=Some(_); \
         the non-PR wrappers must persist source_pr=None so spawn_agent_inner's \
         'is this a PR-spawned node?' branch (node.source_pr.is_some()) stays a \
         reliable signal. Use create_with_source_pr_fork for PR spawns."
    );
    assert!(
        source_pr_pinned_sha.is_none(),
        "services::agent_node::create refuses source_pr_pinned_sha=Some(_); \
         use create_with_source_pr_fork for PR spawns that need SHA pinning."
    );
    // Single insert path; fork fields are None for the non-fork entry point.
    create_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree_override,
        name_override,
        None,
        None,
    )
}

/// Like [`create`], but also records the fork's owner login and clone URL
/// when the PR is from a fork (issue #443). `spawn_agent_inner` reads
/// these to register `fork-<owner>` as a remote and fetch the head ref.
/// For same-repo PRs, pass `None` for both fork fields and call [`create`]
/// instead — the spawn path then takes the #420 `origin/<head_ref>` branch.
#[allow(clippy::too_many_arguments)]
pub fn create_with_source_pr_fork(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    create_with_source_pr_fork_configured(
        mesh_id, path, branch, provider, source_issue, source_pr,
        source_pr_pinned_sha, use_worktree_override, name_override,
        head_repo_owner, head_repo_clone_url, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_with_source_pr_fork_configured(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
    configuration: Option<&crate::preferences::spawn_configurations::SpawnConfiguration>,
) -> Result<AgentNode, AgentNodeError> {
    let mesh = db::get_mesh_by_id(mesh_id)?;
    let mut prefs = crate::preferences::load().map_err(AgentNodeError::Backend)?;
    let selection = crate::preferences::resolve_default_provider(
        provider.map(str::to_string), mesh.default_provider.clone(), prefs.default_provider.clone());
    if let Some(value) = configuration {
        if value.spawn_option_id != selection && value.id != selection {
            return Err(AgentNodeError::InvalidConfiguration("Configuration belongs to a different launch selection".into()));
        }
        prefs.spawn_configurations.retain(|c| c.id != value.id);
        prefs.spawn_configurations.push(value.clone());
    }
    let selected = configuration.map_or(selection.as_str(), |c| c.id.as_str());
    let option = prefs.spawn_configurations.iter().find(|c| c.id == selected)
        .map_or(selected, |c| c.spawn_option_id.as_str());
    let harness_id = crate::agent::provider::SpawnOptionId::from(option).harness_id;
    let mesh_values = db::get_mesh_harness_overrides(mesh_id).map_err(AgentNodeError::Db)?
        .and_then(|values| values.get(&harness_id).cloned()).unwrap_or_default();
    let plan = if let Some(plan) = configuration.and_then(|c| c.resolved.clone()) { plan } else {
        crate::preferences::launch_configurations::resolve(&prefs, selected,
            &Default::default(), &mesh_values).map_err(AgentNodeError::InvalidConfiguration)?
    };
    let runtime = plan.harness.runtime;
    let provider_id = plan.spawn_option_id.clone();
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
    // (Mesh override → app default → `.claude/worktrees`). `None` for Root
    // Nodes. Existing rows without it retain the legacy fallback via
    // `env::node_working_path`, so changing a setting affects future nodes
    // without moving live worktrees.
    let app_dir = crate::preferences::worktree_directory();
    let effective_dir = env::effective_worktree_dir_raw(
        path,
        mesh.worktree_directory.as_deref(),
        app_dir.as_deref(),
    );
    let worktree_path_owned: Option<String> = worktree_db_name
        .map(|n| env::resolve_worktree_node_raw(&effective_dir, n));
    let resolved = env::resolve_agent_path_in_dir(path, &effective_dir, worktree_db_name);
    let env_type = runtime.unwrap_or(resolved.env_type);
    // Store the harness/profile id verbatim (issue #535) — no premature parse
    // to the legacy `Provider` enum, which would flatten an unknown profile id
    // to Anthropic. Resolution happens at the spawn seam. An absent provider
    // defaults to "anthropic", matching the prior `Provider::Anthropic` default.

    let node = db::create_agent_node_configured(
        mesh_id,
        &session_name,
        path,
        branch,
        env_type,
        &provider_id,
        worktree_db_name,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree,
        head_repo_owner,
        head_repo_clone_url,
        worktree_path_owned.as_deref(),
        configuration.as_ref(),
    )?;

    Ok(node)
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
/// * `mesh_id` — the mesh to scope the new node under. A missing
///   mesh id surfaces as `AgentNodeError::Status("mesh not found")`
///   — the route maps that exact sentinel to its 400 response.
/// * `provider` — the harness id to persist verbatim. The legacy
///   `provider.unwrap_or("anthropic")` fallback lives in
///   [`create_with_source_pr_fork`] (issue #538 default), so a `None`
///   here writes `"anthropic"` to the row. Callers that want the
///   mesh-level / app-wide default chain resolve
///   beforehand (`preferences::resolve_default_provider`) and pass
///   the resolved string in.
/// * `branch_override` — `Some(branch)` short-circuits the
///   `git::get_default_branch_blocking` lookup. The mobile route
///   uses `"main"` verbatim; desktop paths that already resolved
///   the branch via the async wrapper pass `Some(resolved)`. `None`
///   resolves through `commands::git::get_default_branch_blocking`
///   (with `"main"` fallback on open failure).
/// * `source_issue` / `name_override` / `use_worktree_override` —
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
#[allow(clippy::too_many_arguments)]
pub fn create_blocking(
    mesh_id: i64,
    provider: Option<&str>,
    branch_override: Option<&str>,
    source_issue: Option<i64>,
    name_override: Option<&str>,
    use_worktree_override: Option<bool>,
    pending: bool,
) -> Result<AgentNode, AgentNodeError> {
    create_blocking_configured(
        mesh_id, provider, branch_override, source_issue, name_override,
        use_worktree_override, pending, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_blocking_configured(
    mesh_id: i64,
    provider: Option<&str>,
    branch_override: Option<&str>,
    source_issue: Option<i64>,
    name_override: Option<&str>,
    use_worktree_override: Option<bool>,
    pending: bool,
    configuration: Option<&crate::preferences::spawn_configurations::SpawnConfiguration>,
) -> Result<AgentNode, AgentNodeError> {
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

    // 2. Branch resolution. `Some(b)` short-circuits the git lookup entirely
    //    (the mobile route pins "main"; Tauri commands that already resolved
    //    via the async wrapper reuse their result). `None` falls through to
    //    the canonical sync helper, with `"main"` as the open-failure fallback
    //    — matching `commands::git::get_default_branch`'s infallible contract.
    let branch = match branch_override {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => crate::commands::git::get_default_branch_blocking(mesh.path.clone())
            .unwrap_or_else(|_| "main".to_string()),
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
        create_pending_with_source_pr_fork_and_worktree(
            mesh_id,
            &mesh.path,
            &branch,
            provider,
            source_issue,
            None, // source_pr
            None, // source_pr_pinned_sha
            name_override,
            None, // head_repo_owner
            None, // head_repo_clone_url
            use_worktree_override,
            configuration,
        )
    } else {
        create_with_source_pr_fork_configured(
            mesh_id,
            &mesh.path,
            &branch,
            provider,
            source_issue,
            None, // source_pr
            None, // source_pr_pinned_sha
            use_worktree_override,
            name_override,
            None, // head_repo_owner
            None, // head_repo_clone_url
            configuration,
        )
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
#[allow(dead_code, clippy::too_many_arguments)]
pub fn create_pending(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    assert!(
        source_pr.is_none(),
        "services::agent_node::create_pending refuses source_pr=Some(_); \
         use create_pending_with_source_pr_fork for PR spawns."
    );
    assert!(
        source_pr_pinned_sha.is_none(),
        "services::agent_node::create_pending refuses source_pr_pinned_sha=Some(_); \
         use create_pending_with_source_pr_fork for PR spawns that need SHA pinning."
    );
    // Single insert path; fork fields are None for the non-fork entry point.
    create_pending_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        name_override,
        None,
        None,
    )
}

/// Pending-node creation with an explicit worktree policy. Issue-driven
/// circuits use this instead of creating a legacy `autopilot_runs` row: the
/// circuit ledger remains the owner of orchestration state while the shared
/// Agent Node row still records the branch-backed environment it needs.
#[allow(clippy::too_many_arguments)]
pub fn create_pending_with_worktree_override_configured(
    mesh_id: i64, path: &str, branch: &str, provider: Option<&str>, source_issue: Option<i64>,
    name_override: Option<&str>, use_worktree_override: Option<bool>,
    configuration: Option<&crate::preferences::spawn_configurations::SpawnConfiguration>,
) -> Result<AgentNode, AgentNodeError> {
    create_pending_with_source_pr_fork_and_worktree(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        None,
        None,
        name_override,
        None,
        None,
        use_worktree_override,
        configuration,
    )
}

/// Like [`create_pending`], but also records the fork's owner login and
/// clone URL when the PR is from a fork (issue #443) and the PR's head
/// commit SHA for exact-pinning (issue #444). `commands::agent::create_pr_node`
/// calls this directly with the fork fields populated for fork PRs and
/// `None, None` for same-repo PRs.
#[allow(clippy::too_many_arguments)]
pub fn create_pending_with_source_pr_fork(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    create_pending_with_source_pr_fork_configured(
        mesh_id, path, branch, provider, source_issue, source_pr,
        source_pr_pinned_sha, name_override, head_repo_owner,
        head_repo_clone_url, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_pending_with_source_pr_fork_configured(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
    configuration: Option<&crate::preferences::spawn_configurations::SpawnConfiguration>,
) -> Result<AgentNode, AgentNodeError> {
    create_pending_with_source_pr_fork_and_worktree(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        name_override,
        head_repo_owner,
        head_repo_clone_url,
        None,
        configuration,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_pending_with_source_pr_fork_and_worktree(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
    use_worktree_override: Option<bool>,
    configuration: Option<&crate::preferences::spawn_configurations::SpawnConfiguration>,
) -> Result<AgentNode, AgentNodeError> {
    let mut node = create_with_source_pr_fork_configured(
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
    )?;
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
    crate::agent::node_teardown::release(session_id);
    // Drop autopilot state too. The ledger delete is explicit as a
    // defensive belt: the table declares ON DELETE CASCADE and the
    // bundled SQLite build (rusqlite 0.32 / SQLite 3.46.0) has FK
    // enforcement on by default, so the parent agent_nodes DELETE
    // below already cascades — but a future system-libsqlite link
    // would have FK off by default and need this explicit DELETE to
    // avoid an orphan. Removing the row also frees the issue for a
    // poller retry if it is still open + labelled.
    crate::autopilot::evaluator::unregister(session_id);
    db::delete_autopilot_run(session_id)?;

    // Only the row delete (and its atomic worktree enqueue) is skipped when the
    // row is already gone — every per-node cleanup above still runs, so a
    // backend-driven delete that won the race leaves no process, naming, or
    // telemetry state behind.
    if let Some(node) = node.as_ref() {
        let removal = removal_path.as_deref().map(|p| (p, node.name.as_str()));
        db::delete_agent_node_enqueueing_removal(session_id, removal)?;
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
pub fn process_pending_removals() -> Vec<(PendingWorktreeRemoval, String)> {
    let _guard = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let pending = match db::list_pending_worktree_removals() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("could not read pending worktree removals: {}", e);
            return Vec::new();
        }
    };

    process_removals(
        pending,
        crate::git::worktree::remove_one_worktree_and_branch,
        db::delete_pending_worktree_removal,
        // Issue #653 deleter-side guard: skip paths whose `warm_worktrees`
        // row is currently `claimed` (a live spawn has just adopted the
        // directory). The skip deques the pending removal — claim supersedes
        // tombstone intent, and the live agent's worktree must survive. When
        // the spawn's `forget_after_spawn` later drops the row, the path is
        // no longer claimed and any *new* pending removal (e.g. a subsequent
        // close of the same node) flows through normally.
        db::warm_pool_claims_path,
    )
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
    remove: impl Fn(&str) -> Result<(), String>,
    dequeue: impl Fn(&str) -> db::SqlResult<()>,
    is_claimed: impl Fn(&str) -> db::SqlResult<bool>,
) -> Vec<(PendingWorktreeRemoval, String)> {
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
                            removal.worktree_path, e
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "worktree removal for {} failed, will retry: {}",
                        removal.node_name, e
                    );
                    failures.push((removal, e));
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
pub fn regenerate_load_blocking(
    node_id: i64,
) -> Result<(String, bool), AgentNodeError> {
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
    let option = prefs.spawn_configurations.iter().find(|c| c.id == new_provider)
        .map_or(new_provider, |c| c.spawn_option_id.as_str());
    let harness_id = SpawnOptionId::from(option).harness_id;
    let mesh = db::get_mesh_harness_overrides(node.mesh_id)?
        .and_then(|values| values.get(&harness_id).cloned()).unwrap_or_default();
    let plan = crate::preferences::launch_configurations::resolve(&prefs, new_provider, &Default::default(), &mesh)
        .map_err(AgentNodeError::InvalidConfiguration)?;
    let resume = decide_resume(old_provider, &plan.spawn_option_id, node.cli_session_id.as_deref()).is_some();
    let runtime = plan.harness.runtime.unwrap_or_else(|| crate::env::resolve_raw_path(&crate::env::node_working_path(&node).raw_path).env_type);
    db::replace_node_launch(node_id, &crate::preferences::launch_configurations::snapshot(plan), runtime).map_err(AgentNodeError::Backend)?;
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
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let tmp =
                std::env::temp_dir().join(format!("buildmesh_agent_node_test_{}_{}", pid, id));
            Self(tmp)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sig() -> git2::Signature<'static> {
        git2::Signature::now("test", "test@example.com").unwrap()
    }

    fn init_repo(path: &Path) -> git2::Repository {
        fs::create_dir_all(path).unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(path, &opts).unwrap();

        fs::write(path.join("file.txt"), "initial").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let s = sig();
        repo.commit(Some("HEAD"), &s, &s, "initial commit", &tree, &[])
            .unwrap();
        drop(tree);

        repo
    }

    fn add_worktree(root_repo: &git2::Repository, root: &TempDir, name: &str) -> PathBuf {
        let head = root_repo.head().unwrap().peel_to_commit().unwrap();
        let branch = root_repo.branch(name, &head, false).unwrap();
        let reference = branch.into_reference();
        let worktree_path = root.path().join(".claude").join("worktrees").join(name);
        fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
        root_repo
            .worktree(
                name,
                &worktree_path,
                Some(git2::WorktreeAddOptions::new().reference(Some(&reference))),
            )
            .unwrap();
        worktree_path
    }

    #[test]
    fn drain_removes_real_worktree_and_dequeues_only_on_success() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let good = add_worktree(&root_repo, &root, "wt-good");

        let pending = vec![
            PendingWorktreeRemoval {
                worktree_path: good.to_string_lossy().to_string(),
                node_name: "wt-good".to_string(),
            },
            PendingWorktreeRemoval {
                worktree_path: "/nonexistent/wt-bad".to_string(),
                node_name: "wt-bad".to_string(),
            },
        ];

        let dequeued = std::cell::RefCell::new(Vec::<String>::new());
        let failures = process_removals(
            pending,
            |path| {
                // A missing parent repo can't be opened → genuine failure.
                if path.starts_with("/nonexistent") {
                    return Err("not a removable worktree".to_string());
                }
                crate::git::worktree::remove_one_worktree(path)
            },
            |path| {
                dequeued.borrow_mut().push(path.to_string());
                Ok(())
            },
            // Issue #653: this test predates the deleter-side guard and its
            // fixture doesn't touch `warm_worktrees`, so the guard is a
            // pure no-op. `_| Ok(false)` keeps the original behaviour intact.
            |_path| Ok(false),
        );

        assert!(!good.exists(), "the real worktree directory must be removed");
        assert_eq!(
            dequeued.into_inner(),
            vec![good.to_string_lossy().to_string()],
            "only the successful removal is dequeued"
        );
        assert_eq!(failures.len(), 1, "the failed removal is returned for warning");
        assert_eq!(failures[0].0.node_name, "wt-bad");
    }

    /// Issue #653 deleter-side guard: a pending-removal entry whose path is
    /// currently `claimed` in `warm_worktrees` (a live spawn has just adopted
    /// the directory) must be SKIPPED — `remove` is not called (the directory
    /// belongs to a live agent) and the tombstone IS dequeued (claim
    /// supersedes the close's intent, so the entry is dead). It must NOT be
    /// reported as a failure (no user-facing warning for a healthy state).
    ///
    /// Without this guard, a concurrent close + spawn on the same path would
    /// race `process_pending_removals` to delete the directory out from under
    /// the spawning agent — issue #653's exact symptom (Error node + stranded
    /// `claimed` row).
    #[test]
    fn process_removals_skips_claimed_path_and_dequeues() {
        let pending = vec![
            // Live spawn's worktree: warm pool row is `claimed`. The drain
            // must skip — no `remove`, dequeue + no failure.
            PendingWorktreeRemoval {
                worktree_path: "/repo/m/.claude/worktrees/wt-claimed".to_string(),
                node_name: "wt-claimed".to_string(),
            },
            // Normal cold removal: not claimed. The drain proceeds.
            PendingWorktreeRemoval {
                worktree_path: "/repo/m/.claude/worktrees/wt-cold".to_string(),
                node_name: "wt-cold".to_string(),
            },
        ];

        let removed = std::cell::RefCell::new(Vec::<String>::new());
        let dequeued = std::cell::RefCell::new(Vec::<String>::new());
        let failures = process_removals(
            pending,
            |path| {
                removed.borrow_mut().push(path.to_string());
                Ok(())
            },
            |path| {
                dequeued.borrow_mut().push(path.to_string());
                Ok(())
            },
            // Warm pool says `claimed` for the first path only.
            |path| Ok(path.ends_with("wt-claimed")),
        );

        assert_eq!(
            removed.into_inner(),
            vec!["/repo/m/.claude/worktrees/wt-cold".to_string()],
            "only the non-claimed path is touched by the removal driver — the live spawn's directory must survive"
        );
        // Both paths are dequeued: the skip branch consumes the tombstone
        // (claim supersedes intent), and the successful removal consumes its
        // own tombstone. The shared log is what proves the order-agnostic
        // "both paths leave the queue" invariant.
        let dequeued = dequeued.into_inner();
        assert!(
            dequeued.contains(&"/repo/m/.claude/worktrees/wt-claimed".to_string()),
            "the superseded tombstone must be dequeued (claim supersedes close intent)"
        );
        assert!(
            dequeued.contains(&"/repo/m/.claude/worktrees/wt-cold".to_string()),
            "the normal removal's tombstone must be dequeued"
        );
        assert_eq!(dequeued.len(), 2, "every pending entry must exit the queue");
        assert!(
            failures.is_empty(),
            "a `claimed` skip is not a failure — no user warning warranted (got: {failures:?})"
        );
    }

    /// A transient `is_claimed` error (e.g. SQLite lock contention mid-drain)
    /// must fail CLOSED: don't remove the directory (could be a live spawn)
    /// and don't drop the tombstone — the next drain retries. The caller
    /// MUST NOT see this as a worktree-cleanup failure: only `remove()`
    /// errors reach `failures`, so a guard-query error stays silent on the
    /// user side (no spurious "worktree cleanup failed" toast) and the
    /// tombstone survives for the next drain to retry.
    #[test]
    fn process_removals_fails_closed_when_is_claimed_errors() {
        let pending = vec![PendingWorktreeRemoval {
            worktree_path: "/repo/m/.claude/worktrees/wt-uncertain".to_string(),
            node_name: "wt-uncertain".to_string(),
        }];

        let removed = std::cell::RefCell::new(Vec::<String>::new());
        let dequeued = std::cell::RefCell::new(Vec::<String>::new());
        let failures = process_removals(
            pending,
            |path| {
                removed.borrow_mut().push(path.to_string());
                Ok(())
            },
            |path| {
                dequeued.borrow_mut().push(path.to_string());
                Ok(())
            },
            |_path| Err(rusqlite::Error::QueryReturnedNoRows), // guard query failed
        );

        assert!(
            removed.borrow().is_empty(),
            "must NOT touch the directory when the guard check errors — could be a live spawn"
        );
        assert!(
            dequeued.borrow().is_empty(),
            "must NOT dequeue when the guard check errors — leave the tombstone for the next drain"
        );
        assert!(
            failures.is_empty(),
            "a guard-query error must NOT surface as a worktree-cleanup failure \
             (would emit a misleading user toast); the next drain retries silently"
        );
    }

    // -------------------------------------------------------------------
    // Issue #448 — pin the `source_pr = None` invariant on the issue-spawn
    // and hand-spawn paths. `services::agent_node::create` and
    // `create_pending` are the only entry points non-PR spawns go through
    // (issue-spawn, handover, generic mobile spawn, Tauri hand-spawn);
    // `spawn_agent_inner` uses `node.source_pr.is_some()` to decide whether
    // to take the worktree-adoption path. A future caller — an importer,
    // a migration script — could silently write a
    // `source_pr` onto a node that didn't actually come from a PR, and the
    // user would see a confusing "could not fetch PR head ref" warning on
    // a node spawned from a regular issue.
    //
    // The wrappers now refuse `Some(_)` for `source_pr` and
    // `source_pr_pinned_sha` at function entry (PR-spawn fields). The PR-spawn
    // entry points (`create_with_source_pr_fork` /
    // `create_pending_with_source_pr_fork`) remain the only functions that
    // can write a non-`None` `source_pr` — those tests live with the PR-spawn
    // callers in `commands::agent_tests`.
    //
    // Two complementary tests cover the invariant:
    //
    //   * Positive (happy-path) tests exercise `create` / `create_pending`
    //     with `source_pr = None` and assert the persisted row reads back
    //     `source_pr = None`. These need a real DB row, so they call
    //     `crate::db::test_support::ensure_db_for_tests()` which lazily inits the global DB.
    //
    //   * Negative `#[should_panic]` tests call the wrappers with
    //     `source_pr = Some(_)`. The assertion fires before any DB call, so
    //     these don't need a DB at all — they catch a regression at the
    //     wrapper boundary with zero infrastructure.
    // -------------------------------------------------------------------

    // DB init routes through `db::test_support::ensure_db_for_tests`.

    #[test]
    fn spawn_configurations_snapshot_reaches_idle_and_pending_nodes() {
        use crate::preferences::spawn_configurations::SpawnConfiguration;
        let mesh_id = fresh_mesh();
        let mut configuration = SpawnConfiguration {
            id: "sol".into(), name: "Sol Max".into(), spawn_option_id: "codex".into(),
            model: Some("gpt-5.6-sol".into()), effort: None, extra_args: Some("--search".into()),
            ..Default::default()
        };
        for pending in [false, true] {
            let node = create_blocking_configured(mesh_id, Some("codex"), Some("main"),
                None, None, Some(false), pending, Some(&configuration)).unwrap();
            assert_eq!(node.provider, "codex");
            assert_eq!(node.status, if pending { SessionStatus::Pending } else { SessionStatus::Idle });
            let saved = db::node_spawn_configuration(node.id, "codex").unwrap().unwrap();
            assert_eq!(saved.id, configuration.id);
            assert_eq!(saved.model, configuration.model);
            assert_eq!(saved.resolved.unwrap().harness.harness, "codex");
        }
        let node = create_blocking_configured(mesh_id, Some("codex"), Some("main"),
            None, None, Some(false), false, Some(&configuration)).unwrap();
        configuration.model = Some("edited-later".into());
        assert_eq!(db::node_spawn_configuration(node.id, "codex").unwrap().unwrap().model.as_deref(), Some("gpt-5.6-sol"));
        db::set_agent_node_provider(node.id, "claude:minimax").unwrap();
        assert!(db::node_spawn_configuration(node.id, "claude:minimax").unwrap().is_none());
        db::set_agent_node_provider(node.id, "codex").unwrap();
        assert!(db::node_spawn_configuration(node.id, "codex").unwrap().is_none());
        let defaults = create_blocking(mesh_id, Some("codex"), Some("main"), None, None, Some(false), false).unwrap();
        assert_eq!(db::node_spawn_configuration(defaults.id, "codex").unwrap().unwrap().id, "launch/codex");
        assert!(db::node_spawn_configuration(i64::MAX, "codex").unwrap().is_none());
    }

    /// Create a fresh mesh in the global DB at a unique per-test path and
    /// return its id. Each call uses a monotonic counter so parallel tests
    /// can't collide on the `meshes.path` UNIQUE constraint.
    fn fresh_mesh() -> i64 {
        crate::db::test_support::ensure_db_for_tests();
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let path = format!("/tmp/buildmesh_invariant_test_{}", id);
        crate::db::create_mesh(&format!("invariant-{}", id), &path)
            .expect("fresh_mesh: create_mesh should succeed")
            .id
    }

    #[test]
    fn create_returns_node_with_source_pr_none() {
        // The wrapper contract: passing `source_pr = None` for an
        // issue-spawn / hand-spawn call persists `source_pr = None`.
        let mesh_id = fresh_mesh();

        let node = create(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create with source_pr=None must succeed");

        assert_eq!(
            node.source_pr, None,
            "issue-spawn wrapper must persist source_pr=None"
        );
        assert_eq!(
            node.source_pr_pinned_sha, None,
            "issue-spawn wrapper must persist source_pr_pinned_sha=None"
        );
        assert_eq!(
            node.source_issue, None,
            "hand-spawn wrapper must persist source_issue=None when not supplied"
        );
    }

    #[test]
    fn create_pending_returns_node_with_source_pr_none_and_pending_status() {
        // `create_pending` is the fast stage-1 of the two-stage issue-spawn
        // flow — it must also persist `source_pr = None` so stage-2's
        // `node.source_pr.is_some()` branch doesn't accidentally fire.
        let mesh_id = fresh_mesh();

        let node = create_pending(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
        )
        .expect("create_pending with source_pr=None must succeed");

        assert_eq!(
            node.source_pr, None,
            "create_pending must persist source_pr=None"
        );
        assert_eq!(
            node.source_pr_pinned_sha, None,
            "create_pending must persist source_pr_pinned_sha=None"
        );
        assert_eq!(
            node.status,
            SessionStatus::Pending,
            "create_pending must leave the row in Pending so the frontend \
             can distinguish stage-1-done from idle-and-ready-to-resume"
        );
    }

    #[test]
    fn create_with_source_issue_set_keeps_source_pr_independent() {
        // Optional step 4 from the issue: when `source_issue = Some(N)`,
        // `source_pr` must still come back as `None` — the two source
        // fields are independent columns on the row.
        let mesh_id = fresh_mesh();

        let node = create(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            Some(123),
            None,
            None,
            None,
            Some("gh123-test-slug"),
        )
        .expect("create with source_issue=Some(N), source_pr=None must succeed");

        assert_eq!(
            node.source_issue,
            Some(123),
            "source_issue must round-trip independently of source_pr"
        );
        assert_eq!(
            node.source_pr, None,
            "source_pr must remain None even when source_issue is set"
        );
        assert_eq!(node.source_pr_pinned_sha, None);
    }

    #[test]
    #[should_panic(expected = "source_pr=Some(_)")]
    fn create_rejects_source_pr_some() {
        // A future caller — importer, migration script — tries to set
        // `source_pr` on a node that didn't actually come
        // from a PR. The wrapper must refuse at the boundary rather than
        // silently persist it. The assertion fires before any DB call, so
        // we don't even need `crate::db::test_support::ensure_db_for_tests()` here — a missing DB is the
        // strongest possible failure signal for this regression.
        let _ = create(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            Some(42),
            None,
            None,
            None,
        );
    }

    #[test]
    #[should_panic(expected = "source_pr=Some(_)")]
    fn create_pending_rejects_source_pr_some() {
        let _ = create_pending(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            Some(42),
            None,
            None,
        );
    }

    #[test]
    #[should_panic(expected = "source_pr_pinned_sha=Some(_)")]
    fn create_rejects_source_pr_pinned_sha_some() {
        // Same boundary for the SHA-pinning field (#444) — a caller that
        // tries to pin the head SHA on a non-PR spawn gets the same
        // refused-at-entry treatment.
        let _ = create(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            None,
            Some("deadbeef"),
            None,
            None,
        );
    }

    // -----------------------------------------------------------------------
    // `create_blocking` (issue #1658 step 5) — single shared runner used by
    // Tauri commands and HTTP routes to fold the mesh-lookup +
    // branch-resolution + node-create sequence into one helper. Every test
    // here is a regression pin against a feature drop: a future refactor
    // that loses a `branch_override` short-circuit, the Pending-status
    // branch, or the "mesh not found" sentinel fails review by failing these
    // tests first.
    // -----------------------------------------------------------------------

    #[test]
    fn create_blocking_with_branch_override_matches_legacy_create_row() {
        // The headline equivalence pin: when `branch_override = Some("main")`,
        // `create_blocking` must produce a row identical to today's
        // `services::agent_node::create(mesh_id, &mesh.path, "main", provider, ...)`
        // would have produced. The four entry points that migrate to
        // `create_blocking` (issue #1658 step 5) depend on this byte-identical
        // row, so a regression that drops the explicit branch, or mangles the
        // provider column, fails the assertion rather than silently regressing
        // the mobile/desktop row shape.
        let mesh_id = fresh_mesh();

        let explicit_branch_row = create_blocking(
            mesh_id,
            Some("anthropic"),
            Some("main"),
            None,
            None,
            None,
            false, /* pending */
        )
        .expect("create_blocking with explicit branch must succeed");

        let legacy_row = create(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("legacy create must succeed for the same inputs");

        assert_eq!(
            explicit_branch_row.branch, legacy_row.branch,
            "explicit branch_override must reach the persisted row identically"
        );
        assert_eq!(
            explicit_branch_row.provider, legacy_row.provider,
            "provider must round-trip identically between helper and legacy create"
        );
        assert_eq!(
            explicit_branch_row.mesh_id, legacy_row.mesh_id,
            "mesh_id must match"
        );
        assert_eq!(
            explicit_branch_row.status, legacy_row.status,
            "Idle status must be preserved when pending=false"
        );
    }

    #[test]
    fn create_blocking_with_pending_true_returns_pending_status() {
        // `create_issue_node` (the desktop two-stage stage-1) and the
        // HTTP mobile route diverge on initial status: Pending vs Idle.
        // `pending: true` must land the row in Pending so the frontend
        // can distinguish "stage-1-done, awaiting background spawn"
        // from "ready-to-resume idle". Without this pin, a future
        // refactor that flattens the helper to always-Idle would
        // silently break the desktop flow.
        let mesh_id = fresh_mesh();

        let node = create_blocking(
            mesh_id,
            Some("anthropic"),
            Some("main"),
            Some(123), /* source_issue */
            Some("gh123-test-slug"),
            None,
            true, /* pending */
        )
        .expect("create_blocking with pending=true must succeed");

        assert_eq!(
            node.status,
            SessionStatus::Pending,
            "pending=true must leave the row in Pending"
        );
        assert_eq!(
            node.source_issue,
            Some(123),
            "source_issue must round-trip through the helper"
        );
        assert_eq!(
            node.source_pr, None,
            "non-PR spawn must persist source_pr=None (mirrors legacy create)"
        );
    }

    #[test]
    fn create_blocking_returns_mesh_not_found_for_unknown_mesh_id() {
        // Initialise the per-test DB up front — without this, the
        // unknown-id lookup fails on "database not initialized"
        // instead of surfacing the intended `QueryReturnedNoRows`,
        // which masks the sentinel we're trying to pin.
        crate::db::test_support::ensure_db_for_tests();
        // The HTTP route (`http::routes::nodes::create`) maps the
        // typed `AgentNodeError::MeshNotFound(_)` variant to its
        // "400 Bad Request — Mesh not found" response. A regression
        // that loses the mapping — falling back to a raw
        // `AgentNodeError::Db(rusqlite::Error::QueryReturnedNoRows)` —
        // would surface as "500 Internal Server Error" on the SPA
        // even though the user typo'd a mesh id.
        let err = create_blocking(
            /* mesh_id */ -1, /* not in the per-test DB */
            Some("anthropic"),
            Some("main"),
            None,
            None,
            None,
            false, /* pending */
        )
        .expect_err("create_blocking on unknown mesh_id must return Err");

        match err {
            AgentNodeError::MeshNotFound(mesh_id) => assert_eq!(
                mesh_id, -1,
                "the variant must carry the rejected mesh id so the route can include it in any log"
            ),
            other => panic!(
                "create_blocking on unknown mesh_id must return MeshNotFound(-1), got {:?}",
                other
            ),
        }
    }

    #[test]
    fn create_blocking_with_branch_override_none_resolves_via_git() {
        // **Review-round-2 pin** (Finding #3): the prior test suite
        // exercised `branch_override = Some("main")` exclusively.
        // The `None` fallback path (which calls
        // `commands::git::get_default_branch_blocking`) was never
        // tested. Pin it: the helper must resolve the branch through
        // the canonical sync helper when no override is supplied,
        // landing on `"main"` because the per-test mesh path
        // (`/tmp/buildmesh_invariant_test_<id>`) doesn't exist as a
        // git repo and `get_default_branch_blocking` falls back.
        let mesh_id = fresh_mesh();

        let node = create_blocking(
            mesh_id,
            Some("anthropic"),
            /* branch_override = */ None,
            None,
            None,
            None,
            false, /* pending */
        )
        .expect("create_blocking with branch_override=None must succeed");

        assert_eq!(
            node.branch, "main",
            "branch_override=None must fall back to get_default_branch_blocking's \
             open-failure default (\"main\"); the per-test mesh has no git repo, so \
             the open-failure branch fires"
        );
    }

    #[test]
    fn create_blocking_propagates_use_worktree_override_on_idle_path() {
        // **Review-round-2 pin** (Finding #1+#3): the prior test
        // suite never exercised `use_worktree_override = Some(_)`.
        // Idle path (`pending = false`) delegates to
        // `create_with_source_pr_fork`, which forwards the override.
        // Pinned here so a future refactor that drops the argument
        // (silently swallowing the caller intent) fails this test
        // before shipping.
        let mesh_id = fresh_mesh();

        let node = create_blocking(
            mesh_id,
            Some("anthropic"),
            Some("main"),
            None,
            None,
            /* use_worktree_override = */ Some(false),
            false, /* pending */
        )
        .expect("create_blocking on idle path must succeed");

        // `create_with_source_pr_fork` writes `use_worktree` only if
        // `session_name` is non-empty AND the override is not None.
        // `Some(false)` writes `false` (Root Node shape — `worktree_name = None`).
        assert_eq!(
            node.use_worktree, false,
            "use_worktree_override=Some(false) must persist as use_worktree=false on the row"
        );
        assert_eq!(
            node.worktree_name, None,
            "use_worktree=false on a fresh mesh must produce a Root Node (no worktree_name)"
        );
    }

    #[test]
    fn create_blocking_propagates_use_worktree_override_on_pending_path() {
        // **Review-round-2 pin** (Finding #1+#3): the symmetric
        // path on `pending = true`. The original PR delegated through
        // `create_pending_with_source_pr_fork`, which drops the
        // override (`use_worktree_override = None` is hardcoded into
        // its inner call). The fix delegates directly to
        // `create_pending_with_source_pr_fork_and_worktree`, which
        // DOES forward the override. Pin BOTH paths so a future
        // refactor that re-introduces the asymmetric drop fails this
        // test.
        let mesh_id = fresh_mesh();

        let node = create_blocking(
            mesh_id,
            Some("anthropic"),
            Some("main"),
            None,
            None,
            /* use_worktree_override = */ Some(false),
            true, /* pending */
        )
        .expect("create_blocking on pending path must succeed");

        assert_eq!(
            node.use_worktree, false,
            "use_worktree_override=Some(false) must persist as use_worktree=false \
             on the Pending row too — Finding #1's regression"
        );
        assert_eq!(
            node.status,
            SessionStatus::Pending,
            "pending=true must still land in Pending on the override-forwarding path"
        );
        assert_eq!(
            node.worktree_name, None,
            "use_worktree=false on a fresh mesh must produce a Root Node even on Pending rows"
        );
    }

    // -----------------------------------------------------------------------
    // Regenerate (issue #775) — validation + resume-decision surface.
    //
    // The three pure helpers (`validate_status_eligible`,
    // `should_skip_kill_for_regenerate`, `decide_resume`) own the new
    // logic. The async `regenerate` orchestrator is exercised end-to-end
    // via the Playwright e2e suite and the manual smoke test, so the
    // unit tests here stay focused on the rules the orchestrator
    // composes.
    // -----------------------------------------------------------------------

    #[test]
    fn validate_status_eligible_allows_running_idle_awaiting_error_suspended_completed() {
        // The six states the regenerate orchestrator accepts:
        //   * Running / Idle / AwaitingInput / Error — "process is or
        //     was running", so kill + respawn makes sense.
        //   * Suspended — no live process to kill; the orchestrator
        //     branches on `should_skip_kill_for_regenerate` to avoid
        //     the unconditional `on_idle` tail of `kill_agent`
        //     (commands/agent.rs:1172) clobbering Suspended→Idle
        //     before the new spawn lands. The frontend gates the
        //     user-driven Resume affordance on `cli_session_id` non-
        //     empty to keep the autopilot-gate case (Suspended +
        //     NULL session id) from surfacing a confusing toast.
        //   * Completed — autopilot-finished wrap-up (#485). The agent
        //     PTY is still alive at the moment of `complete_autopilot_run`
        //     (knowledge claim, not unit-tested: see
        //     autopilot::pipeline::complete_autopilot_run — the function
        //     never calls `kill_agent`), so we route through the
        //     kill+respawn path. `decide_resume` reuses the captured
        //     `cli_session_id` so a same-harness Regenerate picks up
        //     exactly where autopilot left off — which is the "I want
        //     to keep interacting with this session" affordance that
        //     #774 ticket 04 framed.
        for status in [
            SessionStatus::Running,
            SessionStatus::Idle,
            SessionStatus::AwaitingInput,
            SessionStatus::Error,
            SessionStatus::Suspended,
            SessionStatus::Completed,
        ] {
            assert!(
                validate_status_eligible(status).is_ok(),
                "expected {status:?} to be eligible, got: {:?}",
                validate_status_eligible(status),
            );
        }
    }

    #[test]
    fn validate_status_eligible_rejects_spawning_pending_archived() {
        // The three "no process to swap" states — reject at the
        // boundary. `Suspended` moved to the allowed list (see the
        // matching positive test above) when user-driven recovery for
        // Suspended nodes shipped.
        for status in [
            SessionStatus::Spawning,
            SessionStatus::Pending,
            SessionStatus::Archived,
        ] {
            assert!(
                validate_status_eligible(status).is_err(),
                "expected {status:?} to be rejected",
            );
        }
    }

    #[test]
    fn validate_status_eligible_message_includes_status() {
        // The error message must name the rejected state so the frontend
        // toast (or a dev grepping `buildmesh.log`) can tell *why* the
        // regenerate was refused without a stack trace.
        let err = validate_status_eligible(SessionStatus::Spawning).unwrap_err();
        assert!(
            err.contains("spawning"),
            "error must mention the rejected status, got: {err}",
        );
    }

    #[test]
    fn should_skip_kill_for_regenerate_only_true_for_suspended() {
        // The kill-skip branch is the load-bearing pairing with
        // `validate_status_eligible` accepting Suspended — without it,
        // the unconditional `on_idle` tail of `kill_agent_blocking`
        // would clobber Suspended→Idle before the new spawn lands.
        assert!(
            should_skip_kill_for_regenerate(SessionStatus::Suspended),
            "Suspended must skip the kill (no live process to terminate)"
        );
        for status in [
            SessionStatus::Idle,
            SessionStatus::AwaitingInput,
            SessionStatus::Error,
            SessionStatus::Running,
            // Completed — autopilot wrap-up flipped the status, but the
            // agent PTY is still alive (Claude Code sits at the prompt
            // after `gh pr create`). Regenerate must kill it so
            // `spawn_agent_inner`'s `is_agent_already_running` short-
            // circuit doesn't return early at spawn.rs:743.
            SessionStatus::Completed,
        ] {
            assert!(
                !should_skip_kill_for_regenerate(status),
                "{status:?} must NOT skip the kill (live or recently-live process)",
            );
        }
        // Sanity: Spawning / Pending / Archived never reach the
        // orchestrator's step 3 (validate_status_eligible rejects
        // them), so the skip rule is intentionally undefined for them.
        // The helper still returns a deterministic value — pinned here
        // so a future refactor doesn't accidentally expand the skip
        // set without a test witness.
        assert!(!should_skip_kill_for_regenerate(SessionStatus::Spawning));
        assert!(!should_skip_kill_for_regenerate(SessionStatus::Pending));
        assert!(!should_skip_kill_for_regenerate(SessionStatus::Archived));
    }

    #[test]
    fn decide_resume_continues_same_harness_with_session_id() {
        // Same harness, supports resume, has session id → continue.
        assert_eq!(
            decide_resume("claude:minimax", "claude:openrouter", Some("uuid-abc")),
            Some("uuid-abc".to_string()),
        );
        // Native → proxied within the same harness is also a continue.
        assert_eq!(
            decide_resume("claude", "claude:minimax", Some("uuid-abc")),
            Some("uuid-abc".to_string()),
        );
    }

    #[test]
    fn decide_resume_starts_fresh_when_no_session_id() {
        // Same harness, supports resume, but no captured session id
        // (e.g. the agent was killed before the reader caught one).
        assert_eq!(
            decide_resume("claude:minimax", "claude:openrouter", None),
            None,
        );
    }

    #[test]
    fn decide_resume_starts_fresh_on_cross_harness() {
        // The load-bearing case: a Claude → Codex swap with a Claude
        // session id must NOT resume — the UUID isn't a valid Codex
        // session id. Without this, the new agent would crash on
        // startup with "Conversation not found" and the user would
        // lose the work.
        assert_eq!(
            decide_resume("claude:minimax", "codex", Some("uuid-abc")),
            None,
            "cross-harness swaps must not pass the old session id to the new harness",
        );
        // And the reverse: Codex → Claude with a Codex id also fresh.
        assert_eq!(
            decide_resume("codex", "claude:minimax", Some("codex-session-xyz")),
            None,
        );
    }

    #[test]
    fn decide_resume_starts_fresh_when_new_provider_lacks_resume() {
        // Terminal has no session identity. Even a same-harness
        // Terminal → Terminal swap is fresh.
        assert_eq!(
            decide_resume("terminal", "terminal", Some("oc-session")),
            None,
            "a same-harness swap to a non-resumable adapter must start fresh",
        );
    }

    #[test]
    fn decide_resume_continues_opencode_with_captured_ses_id() {
        assert_eq!(
            decide_resume(
                "opencode",
                "opencode",
                Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
            ),
            Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()),
            "OpenCode now supports --session resume; a captured ses_ id must continue",
        );
    }

    #[test]
    fn set_agent_node_provider_persists_and_round_trips() {
        // Issue #775 — the Regenerate command mutates `agent_nodes.provider`
        // to swap the Model Provider on respawn. Verify the column write
        // persists and reads back the new value, and that a re-write is
        // idempotent (the function comment claims it as a property).
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_provider_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node should succeed");
        assert_eq!(node.provider, "anthropic");

        db::set_agent_node_provider(node.id, "claude:minimax")
            .expect("set_agent_node_provider should succeed");
        let reloaded = db::get_agent_node_by_id(node.id)
            .expect("get_agent_node_by_id should succeed");
        assert_eq!(
            reloaded.provider, "claude:minimax",
            "provider column must round-trip through the UPDATE",
        );

        // Idempotent — a no-op write should still succeed.
        db::set_agent_node_provider(node.id, "claude:minimax")
            .expect("idempotent write should succeed");
    }

    #[test]
    fn close_phase1_then_phase2_is_idempotent_for_both_worktree_modes() {
        // Regression: a Circuit step's `CloseAgentNode` (or any backend-driven
        // delete) can remove a node's row while the frontend still holds it as a
        // tab. The real close is Phase 1 `get_worktree_close_safety` then Phase 2
        // `delete`; both used to fault with `QueryReturnedNoRows` on the retry,
        // aborting the close and leaving the stale card on screen behind an
        // error toast. Exercise that exact order for both `remove_worktree`
        // values, then repeat it against the now-missing row.
        for remove_worktree in [false, true] {
            let mesh_id = fresh_mesh();
            let node = create(
                mesh_id,
                &format!("/tmp/buildmesh_close_idempotent_{remove_worktree}"),
                "main",
                Some("anthropic"),
                None,
                None,
                None,
                None,
                None,
            )
            .expect("seed node should succeed");

            let state_owner = crate::agent::hook_state::for_node(node.id);
            state_owner
                .lock()
                .question("pending", crate::agent::hook_state::QuestionKind::Foreground);

            // Real close order: Phase 1 (safety) precedes Phase 2 (delete).
            get_worktree_close_safety(node.id)
                .expect("Phase 1 safety on a live row must succeed");
            delete(node.id, remove_worktree).expect("Phase 2 close must succeed");
            let replacement_state = crate::agent::hook_state::for_node(node.id);
            assert!(
                !std::sync::Arc::ptr_eq(&state_owner, &replacement_state),
                "deleting a node must release its process-lifetime HookState entry",
            );
            crate::agent::node_teardown::release(node.id);
            assert!(
                matches!(
                    db::get_agent_node_by_id(node.id),
                    Err(rusqlite::Error::QueryReturnedNoRows),
                ),
                "the row must be gone after close (remove_worktree={remove_worktree})",
            );

            // The ghost retry: both phases must tolerate the missing row.
            let safety = get_worktree_close_safety(node.id)
                .expect("Phase 1 must tolerate a missing row");
            assert_eq!(safety.worktree_path, None);
            assert!(!safety.has_uncommitted && !safety.has_unpushed && !safety.is_detached);
            delete(node.id, remove_worktree)
                .expect("Phase 2 must tolerate a missing row (remove_worktree=true included)");
        }
    }

    // PR #1388 review feedback 2 — the service-layer `regenerate`
    // used to orchestrate three interleaved `spawn_blocking` hops and
    // a chain of pre-clones (`new_provider_for_update`,
    // `old_provider_for_update`) to satisfy the `'static` borrow on
    // each closure. The refactor splits the work into three single-
    // purpose sync helpers that the command boundary wraps once each,
    // threading owned data via return tuples instead of captured clones.

    #[test]
    fn regenerate_load_blocking_returns_old_provider_and_skip_kill() {
        // Idle node: provider is captured verbatim, kill is NOT skipped.
        let mesh_id = fresh_mesh();
        let idle_node = create(
            mesh_id,
            "/tmp/buildmesh_regen_load_idle",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed idle node");
        let (old_provider, skip_kill) =
            regenerate_load_blocking(idle_node.id).expect("load should succeed");
        assert_eq!(old_provider, "anthropic");
        assert!(
            !skip_kill,
            "Idle has no live process to skip, but the skip rule is about Suspended only",
        );

        // Suspended node: skip_kill must be true (this is the whole
        // point of the helper — `kill_agent_blocking`'s `on_idle`
        // tail would clobber Suspended→Idle if we killed).
        let suspended_node = create(
            mesh_id,
            "/tmp/buildmesh_regen_load_suspended",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed suspended node");
        db::update_agent_node_status(suspended_node.id, SessionStatus::Suspended)
            .expect("set suspended status");
        let (suspended_provider, suspended_skip_kill) =
            regenerate_load_blocking(suspended_node.id).expect("load should succeed");
        assert_eq!(suspended_provider, "anthropic");
        assert!(suspended_skip_kill, "Suspended must skip the kill");
    }

    #[test]
    fn regenerate_load_blocking_rejects_spawning_status() {
        // The status guard from `validate_status_eligible` must surface
        // as an error so the command boundary can map it to the user's
        // "regenerate unavailable: node is in X state" toast.
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_regen_load_spawning",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node");
        db::update_agent_node_status(node.id, SessionStatus::Spawning)
            .expect("set spawning status");
        let err = regenerate_load_blocking(node.id).unwrap_err();
        match err {
            AgentNodeError::Status(msg) => assert!(
                msg.contains("spawning"),
                "error must name the rejected status, got: {msg}"
            ),
            other => panic!("expected AgentNodeError::Status, got {other:?}"),
        }
    }

    #[test]
    fn regenerate_apply_blocking_writes_provider_and_decides_resume() {
        let prefs_dir = tempfile::tempdir().unwrap();
        crate::preferences::init_for_tests(prefs_dir.path().into());
        crate::preferences::save(serde_json::from_value(serde_json::json!({
            "provider_accounts": [{"id":"minimax","name":"MiniMax","enabled":true,"billing_mode":"pay_as_you_go","api_key":"test-key"}],
            "provider_pairings": [{"harness_id":"claude","provider_id":"minimax","surface":"anthropic","base_url":"https://api.minimax.io/anthropic","model_tiers":{"default":"MiniMax-M3[1m]"}}]
        })).unwrap()).unwrap();
        // Same-harness provider swap with a captured cli_session_id:
        // apply_blocking must write the new provider AND report
        // `resume = true` (because `decide_resume` continues the
        // session for same-harness swaps).
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_regen_apply",
            "main",
            Some("claude"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node");
        db::update_cli_session_id(node.id, "sess-abc")
            .expect("set cli_session_id");

        let resume = regenerate_apply_blocking(node.id, "claude", "claude:minimax")
            .expect("apply should succeed");
        assert!(resume, "same-harness swap with a session id must resume");

        let reloaded = db::get_agent_node_by_id(node.id).expect("reload");
        assert_eq!(
            reloaded.provider, "claude:minimax",
            "apply_blocking must persist the new provider before returning"
        );
        assert_eq!(
            reloaded.cli_session_id.as_deref(),
            Some("sess-abc"),
            "apply_blocking must NOT clear cli_session_id — the spawner owns that policy",
        );
    }

    #[test]
    fn regenerate_apply_blocking_starts_fresh_on_cross_harness() {
        let prefs_dir = tempfile::tempdir().unwrap();
        crate::preferences::init_for_tests(prefs_dir.path().into());
        // Claude → Codex: the captured Claude session id is not a valid
        // Codex id, so the apply step must report `resume = false`.
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_regen_apply_cross",
            "main",
            Some("claude"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node");
        db::update_cli_session_id(node.id, "claude-uuid")
            .expect("set cli_session_id");

        let resume = regenerate_apply_blocking(node.id, "claude:anthropic", "codex")
            .expect("apply should succeed");
        assert!(
            !resume,
            "cross-harness swap must report resume=false so spawn picks Fresh intent"
        );
    }

    #[test]
    fn regenerate_reload_blocking_returns_fresh_node() {
        // The reload helper is the final hop in the orchestrator — it
        // must return the current row so the command can hand it back
        // to the frontend store.
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_regen_reload",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node");
        let reloaded = regenerate_reload_blocking(node.id).expect("reload should succeed");
        assert_eq!(reloaded.id, node.id);
        assert_eq!(reloaded.provider, "anthropic");
    }
}
