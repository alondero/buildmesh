//! Tauri command wrappers for agent process operations.
//!
//! All spawn/lifecycle logic lives in `crate::agent::spawn` and
//! `crate::agent::process`. This module exposes those as Tauri commands
//! and the small bits of orchestration (DB updates, scrollback clears,
//! attention events) that surround them.

use crate::agent::process::PROCESS_REGISTRY;
use crate::agent::provider::{Platform, ProviderInfo};
use crate::agent::spawn::SpawnOptions;
use crate::db;
use crate::models::SessionStatus;
use serde::{Deserialize, Serialize};
use tauri::{command, AppHandle, Emitter};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Provider listing
// ---------------------------------------------------------------------------

/// Compose the `ProviderInfo` (Spawn Option) row for a single harness profile on the
/// current host platform. Pure (no disk / DB / globals) so unit tests can
/// exercise the per-profile derivation — including the `resumable` flag —
/// without touching the preferences module's `APP_DATA_DIR` / `CACHE`
/// shared state (which is global per-process and would otherwise race
/// against other tests).
///
/// Extracted from `available_providers` so the per-profile logic can be
/// pinned without driving `harness_profiles()` (which reads from disk and
/// shares a `OnceLock`). Resolves the executor via `profile.harness` (the
/// stored profile field that names the backing [`Provider`]) rather than
/// `preferences::resolve_harness_provider(&profile.id)` — that helper
/// reads disk via `harness_profiles()` to look up an id, which would
/// defeat the test isolation. For the `available_providers` call site
/// the two paths are equivalent (every profile iterated here comes from
/// `harness_profiles()` and so its id→harness lookup is a no-op).
///
/// The row is a **native Spawn Option**: `id == profile.id` (no `:`), no
/// `provider_id`, `is_proxied = false`. `harness_id == profile.id` is the
/// grouping key the frontend uses to bucket rows under their harness header
/// (issue #575 / ADR-0016).
fn provider_info_for(profile: &crate::preferences::HarnessProfile, host: Platform) -> Option<ProviderInfo> {
    let adapter = crate::models::Provider::from_db_str(&profile.harness).adapter();
    if !adapter.available_on().contains(&host) {
        return None;
    }
    let ui = adapter.ui();
    // Backend-derived answer to "can this provider resume an archived
    // session in place?" — both flags must be true: supports_resume()
    // gates the CLI flag, produces_readable_transcript() gates the
    // coordinator read API that rehydrates the session. The archived-node
    // resume picker consumes this so a custom Claude-compatible profile
    // (e.g. "DeepSeek via Claude") shows up without the old hardcoded id
    // allow-list (#550 follow-up).
    let resumable = adapter.supports_resume() && adapter.produces_readable_transcript();
    Some(ProviderInfo {
        id: profile.id.clone(),
        label: profile.name.clone(),
        color: ui.color,
        icon: ui.icon,
        resumable,
        harness_id: profile.id.clone(),
        provider_id: None,
        is_proxied: false,
        group_key: profile.id.clone(),
    })
}

/// Compose the `ProviderInfo` (Spawn Option) row for a configured Claude-compatible
/// provider account (MiniMax, Kimi, or a custom endpoint). These spawn via the
/// Claude Code (`anthropic`) executor with the account's endpoint + per-tier
/// models injected at spawn ([`crate::preferences::resolve_provider_env`]), so
/// the row borrows the Anthropic adapter's UI and resume capability.
///
/// The row is a **Proxied Provider** Spawn Option (issue #575 / ADR-0016): the
/// composite `id` is `<harness>:<provider>` (always `claude:<account.id>`
/// today, while the multi-harness attach is deferred), and `harness_id`
/// groups it under the Claude Code header in the rendered Spawn Menu. The
/// frontend `ProviderIcon` keys off the row `id`, so a "minimax"/"kimi"/
/// custom id renders its own brand mark.
///
/// `harness_id` defaults to `"claude"` (the detected Claude Code profile id
/// from the harness profile list) and falls back to the literal string
/// `"claude"` if no such profile is present (e.g. a bare test env or a host
/// that hasn't auto-detected the binary). The fallback is safe: `claude`
/// still resolves to the Anthropic executor through
/// `preferences::resolve_harness_provider` thanks to the existing
/// `Provider::from_db_str` and profile-lookup fallbacks. Returns `None`
/// only if the Claude executor isn't available on this host.
fn provider_info_for_account(
    account: &crate::preferences::ProviderAccount,
    profiles: &[crate::preferences::HarnessProfile],
    host: Platform,
) -> Option<ProviderInfo> {
    let adapter = crate::models::Provider::Anthropic.adapter();
    if !adapter.available_on().contains(&host) {
        return None;
    }
    let ui = adapter.ui();
    // The Claude Code harness header this row groups under. We pick the
    // first detected profile whose backing executor is the Anthropic
    // adapter (i.e. a Claude Code profile) — that's the row the user
    // clicks to launch Claude Code natively, and the proxied children
    // must cluster under it. If none is present, "claude" is a safe
    // fallback: the resolver still maps it to the Anthropic executor.
    let harness_id = profiles
        .iter()
        .find(|p| p.harness == "anthropic")
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "claude".to_string());
    let id = format!("{}:{}", harness_id, account.id);
    Some(ProviderInfo {
        id,
        label: account.name.clone(),
        color: ui.color,
        icon: ui.icon,
        resumable: adapter.supports_resume() && adapter.produces_readable_transcript(),
        harness_id: harness_id.clone(),
        provider_id: Some(account.id.clone()),
        is_proxied: true,
        group_key: harness_id,
    })
}

/// Build the spawn menu from the two configuration lists. Pure (no disk/globals)
/// so the derivation is the unit-test seam.
///
/// Harness profiles (Terminal + startup-detected Claude/Codex/Antigravity/OpenCode)
/// come first, then every **configured** Claude-compatible provider account —
/// enabled, with a non-empty API key — is appended as a Claude-Code-backed row
/// (deduped by composite id). This is what surfaces a keyed built-in MiniMax/Kimi
/// or a custom endpoint in the menu without a second stored list to keep in sync;
/// clearing the key or disabling the account drops the row automatically
/// (issue #568).
///
/// **Dedup semantics** (issue #575 / ADR-0016): the composite id
/// `<harness>:<provider>` is unique per (harness profile id, account id)
/// pair, so a duplicate-row check by `info.id` only fires when the same
/// (harness, account) pair was added twice. The pre-#575 dedup by bare
/// `account.id` — which suppressed a user-typed "claude" custom account
/// in favour of the native "claude" harness row — is **no longer
/// applicable** under the composite id format: a "claude" custom account
/// produces a `claude:claude` row, distinct from the native `claude`
/// harness row. The `compose_menu_does_not_duplicate_a_detected_profile`
/// test (pre-#575) is the regression case the new format sidesteps —
/// `provider_info_for` already filters non-Anthropic harnesses, and the
/// account's harness_id is independent of any profile id.
fn compose_provider_menu(
    profiles: Vec<crate::preferences::HarnessProfile>,
    accounts: Vec<crate::preferences::ProviderAccount>,
    host: Platform,
    order: &[String],
) -> Vec<ProviderInfo> {
    let mut rows: Vec<ProviderInfo> = profiles
        .iter()
        .filter_map(|profile| provider_info_for(profile, host))
        .collect();
    for account in accounts {
        let keyed = account.api_key.as_deref().is_some_and(|k| !k.is_empty());
        if account.claude_compatible
            && account.enabled
            && keyed
        {
            if let Some(info) = provider_info_for_account(&account, &profiles, host) {
                // Composite id dedup — guards against accidentally
                // appending the same (harness, account) pair twice if
                // a future change passes the same account through this
                // loop twice. The pre-#575 bare-id suppression is no
                // longer needed; see the doc-comment above.
                if !rows.iter().any(|r| r.id == info.id) {
                    rows.push(info);
                }
            }
        }
    }
    order_providers(rows, order)
}

/// Returns the list of agent providers available on this host platform.
/// Each provider declares which platforms it runs on via `AgentProvider::available_on()`.
pub(crate) fn available_providers() -> Vec<ProviderInfo> {
    compose_provider_menu(
        crate::preferences::harness_profiles(),
        crate::preferences::provider_accounts(),
        Platform::current(),
        &crate::preferences::harness_order(),
    )
}

/// Order the Spawn Menu by the user's stored harness order, with the plain
/// `terminal` row always pinned to the bottom (issue #534 / #573).
///
/// `order` is the persisted list of **harness profile ids** (Terminal excluded
/// — it's forced last regardless of where it appears). Each row's rank is
/// derived from its `harness_id` (the grouping key) — not its composite `id`
/// — so a Proxied Provider row like `claude:minimax` clusters under its
/// `claude` harness header instead of ranking at the bottom. A row whose
/// `harness_id` isn't in the list (a newly-detected harness) ranks just below
/// `usize::MAX` so it appends at the end *above* Terminal. An uninstalled
/// harness simply isn't among `providers`, so its id sits dormant in `order`
/// and its slot is restored verbatim when it reappears.
///
/// Pure and stable (no disk / globals) so the ordering is the unit-test seam:
/// the `(is_terminal, rank)` tuple key sorts Terminal last via the bool, ranks
/// the rest by stored harness order, and the stable sort keeps equal-rank
/// rows in their input order (a harness's native row is built first in
/// `compose_provider_menu`, so it lands first in each group).
fn order_providers(mut providers: Vec<ProviderInfo>, order: &[String]) -> Vec<ProviderInfo> {
    providers.sort_by_key(|p| {
        let is_terminal = p.harness_id == "terminal";
        let rank = order
            .iter()
            .position(|id| *id == p.harness_id)
            .unwrap_or(usize::MAX - 1);
        (is_terminal, rank)
    });
    providers
}

#[command]
pub async fn list_providers() -> Vec<ProviderInfo> {
    available_providers()
}

// ---------------------------------------------------------------------------
// Spawn / resume
// ---------------------------------------------------------------------------

/// Spawn a new agent for the given session with the specified provider.
#[command]
pub async fn spawn_agent(
    app: AppHandle,
    session_id: i64,
    provider: String,
    resume: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<(), String> {
    crate::agent::spawn::spawn_agent_inner(&app, SpawnOptions {
        session_id,
        provider: crate::preferences::resolve_harness_provider(&provider),
        resume,
        rows: rows.unwrap_or(24),
        cols: cols.unwrap_or(80),
        prefill: None,
        node: None,
    }).await
}

/// Internal implementation shared by spawn_issue_agent and spawn_handover_agent.
/// Takes a pre-fetched `&Mesh` and a fully-formatted prefill string. Source-specific
/// shaping (e.g. GitHub issue → URL+title) happens in the caller before this is
/// called, so the impl is just: resolve provider → create node → spawn.
///
/// `initial_name` lets the caller seed the node with a meaningful name (e.g.
/// the issue-title slug for `spawn_issue_agent`); handover leaves it `None`
/// and falls back to a random default.
async fn spawn_new_agent_impl(
    app: &AppHandle,
    mesh: &crate::models::Mesh,
    prefill: String,
    provider: Option<String>,
    source_issue: Option<i64>,
    initial_name: Option<String>,
) -> Result<crate::models::AgentNode, String> {
    let effective_provider = crate::preferences::resolve_default_provider(
        provider,
        mesh.default_provider.clone(),
        crate::preferences::default_provider(),
    );

    let branch = crate::commands::git::get_default_branch(mesh.path.clone());

    let node = crate::services::agent_node::create(
        mesh.id,
        &mesh.path,
        &branch,
        Some(&effective_provider),
        source_issue,
        None, // source_pr — non-PR spawn path (issue #450)
        None, // source_pr_pinned_sha — non-PR spawn path (issue #444)
        None, // use_worktree_override — None falls back to mesh default
        initial_name.as_deref(),
    ).map_err(|e| e.to_string())?;

    // Resolve once and reuse — the profile lookup isn't free, and any future
    // change to the resolution rule should only need to update one site.
    let provider = crate::preferences::resolve_harness_provider(&effective_provider);

    let prefill_text = if provider.adapter().supports_prefill() {
        Some(prefill)
    } else {
        tracing::warn!(
            "spawn_new_agent_impl: --prefill not supported for provider '{}', skipping",
            effective_provider
        );
        None
    };

    let node_id = node.id;
    crate::agent::spawn::spawn_agent_inner(app, SpawnOptions {
        session_id: node_id,
        provider,
        resume: None,
        rows: 24,
        cols: 80,
        prefill: prefill_text,
        node: Some(node),
    }).await?;

    db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
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
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(&mesh)
        .map_err(|e| format!("{} — cannot derive issue URL", e))?;

    let prefill = format_issue_prefill(&owner, &repo, issue_number, &issue_title);
    // Issue #111: seed the node with a slugified issue title so the user can
    // identify it in the mesh list from the moment the modal closes (instead
    // of waiting on the LLM rename). The name is prefixed with `gh{N}-` so
    // the user can spot the originating issue at a glance (e.g. issue #123
    // "Fix this feature" → `gh123-fix-this-feature`). Falls back to a random
    // default name if the title doesn't yield a valid `SLUG_REGEX` match —
    // the `gh` prefix is still applied to the fallback so the user always
    // sees which issue the node came from.
    let initial_name = crate::session_naming::issue_node_name(issue_number, &issue_title);

    let node = spawn_new_agent_impl(
        &app,
        &mesh,
        prefill,
        provider,
        Some(issue_number),
        Some(initial_name),
    ).await?;

    tracing::info!("spawn_issue_agent: spawned node {} for issue #{}", node.id, issue_number);
    Ok(node)
}

/// Compose the prefill string handed to the agent on GitHub-issue spawn.
///
/// Shape: `Please work on GitHub issue #N — Title\n<html_url>` — terse imperative,
/// the issue number for cite-ability, the title as a one-line freebie so the
/// agent can usually start without round-tripping for the body, and the URL as
/// the canonical source. An empty title degrades to just `#N\n<url>` (no
/// dangling em-dash artifact).
///
/// The title is passed verbatim — the consumer is an LLM, not a parser, and
/// the prefill bytes round-trip safely through the PowerShell `-EncodedCommand`
/// path (see memory: powershell-encoding-fix). Using an em-dash instead of
/// surrounding quotes sidesteps any escape question entirely.
///
/// Pure function — exposed `pub(crate)` for unit tests in `agent_tests.rs`.
pub(crate) fn format_issue_prefill(
    owner: &str,
    repo: &str,
    issue_number: i64,
    issue_title: &str,
) -> String {
    let url = format!("https://github.com/{}/{}/issues/{}", owner, repo, issue_number);
    let trimmed = issue_title.trim();
    if trimmed.is_empty() {
        format!("Please work on GitHub issue #{}\n{}", issue_number, url)
    } else {
        format!(
            "Please work on GitHub issue #{} — {}\n{}",
            issue_number, trimmed, url
        )
    }
}

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

/// Fast stage-1 of the GitHub-issue spawn flow. Creates a `Pending` agent
/// node row, returns the row + the prefill the caller must pass to
/// `start_node_background`. Does NOT touch the network, the worktree, or
/// the process tree — so it returns in ~20ms instead of 5-10s.
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
    let prefill = format_issue_prefill(&owner, &repo, issue_number, &issue_title);
    // Issue #111: seed the node with a `gh{N}-{slug}` name (mirrors
    // `spawn_issue_agent` so the desktop modal and mobile route produce
    // identical names). The `gh` prefix lets the user spot the originating
    // issue at a glance. Falls back to a random default if the title doesn't
    // yield a valid slug — the prefix is still applied in that case.
    let initial_name = crate::session_naming::issue_node_name(issue_number, &issue_title);

    let effective_provider = crate::preferences::resolve_default_provider(
        provider,
        mesh.default_provider.clone(),
        crate::preferences::default_provider(),
    );
    let branch = crate::commands::git::get_default_branch(mesh.path.clone());

    let node = crate::services::agent_node::create_pending(
        mesh.id,
        &mesh.path,
        &branch,
        Some(&effective_provider),
        Some(issue_number),
        None, // source_pr — issue-spawn path, not PR-spawn (issue #450)
        None, // source_pr_pinned_sha — issue-spawn doesn't pin (issue #444)
        Some(&initial_name),
    )
    .map_err(|e| e.to_string())?;

    // The frontend `agentNodeStore.initAttentionListeners` listens for
    // `node-created` and refetches the list — issue #490 renamed this event
    // alongside the `*_agent_node` IPC surface. The `id` field is the new
    // node's row id.
    let _ = app.emit(
        "node-created",
        serde_json::json!({ "id": node.id }),
    );

    tracing::info!(
        "create_issue_node: created pending node {} for issue #{} on mesh {}",
        node.id,
        issue_number,
        mesh_id
    );

    Ok(IssueNodeDraft { node, prefill })
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

/// Compose the prefill string handed to the agent on PR spawn.
///
/// Shape: `Please review pull request #N — Title\n<html_url>` — mirrors
/// `format_issue_prefill` (issue flow) so the agent gets the same kind of
/// one-line imperative + URL hand-off. The title is passed verbatim (the
/// consumer is an LLM, not a parser); the PR's body is intentionally NOT
/// included (same rationale as the issue flow — multi-KB markdown is the
/// worst case for the PowerShell `-EncodedCommand` argv path).
///
/// Pure function — exposed `pub(crate)` so unit tests can pin the wording.
pub(crate) fn format_pr_prefill(
    owner: &str,
    repo: &str,
    pr_number: i64,
    pr_title: &str,
) -> String {
    let url = format!("https://github.com/{}/{}/pull/{}", owner, repo, pr_number);
    let trimmed = pr_title.trim();
    if trimmed.is_empty() {
        format!("Please review pull request #{}\n{}", pr_number, url)
    } else {
        format!(
            "Please review pull request #{} — {}\n{}",
            pr_number, trimmed, url
        )
    }
}

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
    let draft = create_pr_node_impl(
        mesh_id,
        pr_number,
        pr_title,
        head_ref,
        head_sha,
        provider,
        head_repo_owner,
        head_repo_clone_url,
    )?;
    // Mirrors `create_issue_node` — see that block for the rationale.
    let _ = app.emit("node-created", serde_json::json!({ "id": draft.node.id }));
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
) -> Result<IssueNodeDraft, String> {
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

    let prefill = format_pr_prefill(&owner, &repo, pr_number, &pr_title);

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

    Ok(IssueNodeDraft { node, prefill })
}

/// Slow stage-2 of the two-stage spawn flow. Runs the existing
/// `spawn_agent_inner` pipeline (git fetch, worktree create, PTY spawn,
/// workspace-trust + attention-hook write, reader thread) for a node
/// row that already exists. Fire-and-forget — the Tauri command
/// returns immediately; the work happens on a background task.
///
/// Emits `node-spawn-completed` on success and `node-spawn-failed` on
/// failure. On failure, the node's status is updated to `Error` so the
/// UI shows a red badge and the user can close the node normally.
#[command]
pub fn start_node_background(
    app: AppHandle,
    node_id: i64,
    prefill: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn(async move {
        let result = start_node_inner(&app, node_id, prefill.as_deref()).await;
        match result {
            Ok(()) => {
                let _ = app.emit(
                    "node-spawn-completed",
                    serde_json::json!({ "node_id": node_id }),
                );
                tracing::info!("start_node_background: node {} ready", node_id);
            }
            Err(e) => {
                tracing::error!(
                    "start_node_background: node {} failed: {}",
                    node_id,
                    e
                );
                let _ = db::update_agent_node_status(node_id, SessionStatus::Error);
                let _ = app.emit(
                    "node-spawn-failed",
                    serde_json::json!({ "node_id": node_id, "error": e }),
                );
            }
        }
    });
    Ok(())
}

/// Body of stage-2: load the node, optionally filter the prefill through
/// the provider's `supports_prefill()` check, and delegate to the
/// existing `spawn_agent_inner`. The same function is reused by
/// `auto_resume_sessions` (via `spawn_agent_inner` directly) and the
/// handover path (via `spawn_handover_agent`); this is the issue-spawn
/// entry point.
async fn start_node_inner(
    app: &AppHandle,
    node_id: i64,
    prefill: Option<&str>,
) -> Result<(), String> {
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    // `node.provider` is the stored harness id (String); resolve it to a
    // concrete executor at this spawn seam (issue #535).
    let provider = crate::preferences::resolve_harness_provider(&node.provider);
    let prefill_text = if provider.adapter().supports_prefill() {
        prefill.filter(|s| !s.is_empty()).map(String::from)
    } else {
        if prefill.is_some() {
            tracing::warn!(
                "start_node_inner: --prefill not supported for provider '{:?}', skipping",
                provider
            );
        }
        None
    };

    crate::agent::spawn::spawn_agent_inner(
        app,
        crate::agent::spawn::SpawnOptions {
            session_id: node_id,
            provider,
            resume: None,
            rows: 24,
            cols: 80,
            prefill: prefill_text,
            node: Some(node),
        },
    )
    .await
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
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let node = spawn_new_agent_impl(
        &app,
        &mesh,
        prefill,
        provider,
        None,
        None,
    ).await?;

    tracing::info!("spawn_handover_agent: spawned node {} via handover", node.id);
    Ok(node)
}

/// Auto-resume all suspended nodes that have a stored CLI session ID.
/// Called by the frontend on startup after event listeners are ready.
#[command]
pub async fn auto_resume_agent_nodes(app: AppHandle) -> Result<Vec<i64>, String> {
    let nodes = db::list_suspended_nodes().map_err(|e| e.to_string())?;

    if nodes.is_empty() {
        tracing::info!("auto_resume_agent_nodes: no suspended nodes to resume");
        return Ok(vec![]);
    }

    tracing::info!("auto_resume_agent_nodes: resuming {} nodes", nodes.len());
    let mut resumed: Vec<i64> = Vec::new();

    for node in &nodes {
        let cli_id = match &node.cli_session_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => {
                tracing::warn!("auto_resume_agent_nodes: node {} has no cli_session_id, skipping", node.id);
                db::update_agent_node_status(node.id, SessionStatus::Idle).ok();
                continue;
            }
        };

        // `node.provider` is the stored harness id (String); resolve it to a
        // concrete executor for both the capability check and the spawn (#535).
        let provider = crate::preferences::resolve_harness_provider(&node.provider);
        if !provider.adapter().auto_resume_on_startup() {
            tracing::info!("auto_resume_agent_nodes: skipping non-resumable node {} ({:?})", node.id, node.provider);
            db::update_agent_node_status(node.id, SessionStatus::Idle).ok();
            continue;
        }

        match crate::agent::spawn::spawn_agent_inner(&app, SpawnOptions {
            session_id: node.id,
            provider,
            resume: Some(cli_id),
            rows: 24,
            cols: 80,
            prefill: None,
            node: Some(node.clone()),
        }).await {
            Ok(()) => {
                resumed.push(node.id);
                tracing::info!("auto_resume_agent_nodes: resumed node {}", node.id);
            }
            Err(e) => {
                tracing::error!("auto_resume_agent_nodes: failed to resume node {}: {}", node.id, e);
                db::update_agent_node_status(node.id, SessionStatus::Error).ok();
                let _ = app.emit("resume-failed", serde_json::json!({
                    "node_id": node.id,
                    "error": e
                }));
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(resumed)
}

// ---------------------------------------------------------------------------
// Lifecycle commands
// ---------------------------------------------------------------------------

/// Kill all running agent processes. Used during graceful shutdown.
pub fn kill_all_agents() {
    for id in PROCESS_REGISTRY.session_ids() {
        PROCESS_REGISTRY.kill_session(id);
        PROCESS_REGISTRY.remove(&id);
        crate::http_server::clear_scrollback(id);
        tracing::info!("kill_all_agents: killed agent for session {}", id);
    }
}

#[command]
pub async fn resize_agent(session_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    PROCESS_REGISTRY.resize_pty(session_id, cols, rows)
}

#[command]
pub async fn write_to_agent(app: AppHandle, session_id: i64, data: String) -> Result<(), String> {
    PROCESS_REGISTRY.write_bytes(session_id, data.as_bytes())?;

    if data.bytes().any(|b| b == b'\n' || b == b'\r')
        && !should_skip_attention_signals(session_id)
    {
        db::update_agent_node_status(session_id, SessionStatus::Running).ok();
        let _ = app.emit("attention-cleared", serde_json::json!({ "session_id": session_id }));
    }
    Ok(())
}

/// Returns true if a newline in `write_to_agent` should NOT flip the
/// node to `Running` and should NOT emit `attention-cleared`. A plain
/// terminal's "Enter" is just shell input — the node has no LLM
/// attention state to clear, and flipping status would render a
/// spurious cyan "Running" badge for a shell sitting at a prompt.
fn should_skip_attention_signals(session_id: i64) -> bool {
    db::get_agent_node_by_id(session_id)
        .ok()
        .map(|n| provider_is_plain_terminal(&n.provider))
        .unwrap_or(false)
}

/// Whether the stored harness id resolves to a plain shell. Resolving through
/// the harness profile (not just the legacy enum) ensures a node spawned via
/// the **Terminal profile** still skips LLM attention signals (issue #535).
fn provider_is_plain_terminal(provider: &str) -> bool {
    crate::preferences::resolve_harness_provider(provider)
        .adapter()
        .is_plain_terminal()
}

#[command]
pub async fn send_to_agent(app: AppHandle, session_id: i64, input: String) -> Result<(), String> {
    write_to_agent(app, session_id, format!("{}\n", input)).await
}

#[command]
pub async fn kill_agent(session_id: i64) -> Result<(), String> {
    crate::session_naming::reset_buffers(session_id);
    PROCESS_REGISTRY.kill_session(session_id);
    PROCESS_REGISTRY.remove(&session_id);
    crate::http_server::clear_scrollback(session_id);
    db::update_agent_node_status(session_id, SessionStatus::Idle).map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn is_agent_running(session_id: i64) -> bool {
    PROCESS_REGISTRY.is_alive(&session_id)
}

// ---------------------------------------------------------------------------
// Debug
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct AgentDebugState {
    pub session_id: i64,
    pub is_alive: bool,
}

#[command]
pub async fn debug_list_agents() -> Vec<AgentDebugState> {
    PROCESS_REGISTRY
        .session_ids()
        .into_iter()
        .map(|id| AgentDebugState {
            session_id: id,
            is_alive: PROCESS_REGISTRY.is_alive(&id),
        })
        .collect()
}

/// Snapshot of all relevant state at the time of a crash, for post-mortem diagnosis.
/// Call this via invoke('debug_crash_snapshot') immediately after a crash to get
/// a consistent view of what the backend was doing.
#[derive(serde::Serialize)]
pub struct CrashSnapshot {
    pub process_registry_ids: Vec<i64>,
    pub session_count: usize,
    pub renamed_sessions: usize,
    pub buffers_size_bytes: usize,
    pub turn_counters_entries: usize,
}

#[command]
pub async fn debug_crash_snapshot() -> CrashSnapshot {
    let process_ids = PROCESS_REGISTRY.session_ids();
    let session_count = db::list_agent_nodes().map(|s| s.len()).unwrap_or(0);
    let buffers_size = crate::session_naming::buffers_size_bytes();

    CrashSnapshot {
        process_registry_ids: process_ids,
        session_count,
        renamed_sessions: 0,
        buffers_size_bytes: buffers_size,
        turn_counters_entries: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terminal_provider_skips_attention_signals() {
        // The "terminal" harness id resolves to the plain-shell executor.
        assert!(provider_is_plain_terminal("terminal"));
    }

    #[test]
    fn llm_providers_do_emit_attention_signals() {
        // Every non-terminal harness id resolves to an LLM executor, so none skip
        // attention signals. "minimax"/"kimi" are legacy ids that now resolve to
        // the Anthropic executor (still an LLM, still not plain-terminal).
        for id in ["anthropic", "minimax", "kimi", "agy", "opencode", "codex"] {
            assert!(
                !provider_is_plain_terminal(id),
                "LLM provider {id:?} should not skip attention signals"
            );
        }
    }

    #[test]
    fn available_providers_lists_only_harness_profiles_with_no_legacy_rows() {
        // Issue #538: the list is purely the dynamic harness profiles — no
        // hardcoded enum rows. In a bare test env (no detection) that's just the
        // code-defined Terminal default, present exactly once (no duplicate
        // legacy Terminal).
        let providers = available_providers();
        let terminals: Vec<_> = providers.iter().filter(|p| p.id == "terminal").collect();
        assert_eq!(
            terminals.len(),
            1,
            "expected exactly one (profile-sourced) Terminal row, got {}",
            terminals.len()
        );
        assert_eq!(terminals[0].label, "Terminal");
        // The retired legacy-only enum rows (e.g. bare "anthropic") must NOT
        // appear without a matching harness profile.
        assert!(
            !providers.iter().any(|p| p.id == "anthropic"),
            "legacy enum rows must not be listed once the profile list is the sole source"
        );
    }

    /// Issue #534: Terminal is the least-common pick, so it must sort to the
    /// bottom of the provider menu while every real harness keeps its relative
    /// order. `order_providers` is the pure seam (no disk / globals) so the
    /// ordering can be pinned without driving `harness_profiles()`.
    #[test]
    fn order_providers_sorts_terminal_to_the_bottom() {
        let row = |id: &str| ProviderInfo {
            id: id.to_string(),
            label: id.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            // Native Spawn Option: harness_id mirrors the row id, no
            // provider, group_key follows the harness (issue #575).
            harness_id: id.to_string(),
            provider_id: None,
            is_proxied: false,
            group_key: id.to_string(),
        };
        // With no stored order, the two real harnesses keep their input order
        // and Terminal sorts last.
        let ordered = order_providers(vec![row("terminal"), row("claude"), row("codex")], &[]);
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "codex", "terminal"]);
    }

    /// Issue #573: the stored harness order drives the row order, Terminal still
    /// pinned last even if it appears mid-list in the stored order.
    #[test]
    fn order_providers_applies_stored_order() {
        let row = |id: &str| ProviderInfo {
            id: id.to_string(),
            label: id.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            // Native Spawn Option: harness_id mirrors the row id, no
            // provider, group_key follows the harness (issue #575).
            harness_id: id.to_string(),
            provider_id: None,
            is_proxied: false,
            group_key: id.to_string(),
        };
        let order = vec!["codex".to_string(), "terminal".to_string(), "claude".to_string()];
        let ordered = order_providers(vec![row("claude"), row("terminal"), row("codex")], &order);
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        // codex before claude per the stored order; terminal forced last
        // despite sitting in the middle of `order`.
        assert_eq!(ids, vec!["codex", "claude", "terminal"]);
    }

    /// Issue #573 AC: a newly-detected harness (not yet in the stored order)
    /// appends at the end of the real harnesses, above Terminal.
    #[test]
    fn order_providers_new_harness_appends_above_terminal() {
        let row = |id: &str| ProviderInfo {
            id: id.to_string(),
            label: id.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            // Native Spawn Option: harness_id mirrors the row id, no
            // provider, group_key follows the harness (issue #575).
            harness_id: id.to_string(),
            provider_id: None,
            is_proxied: false,
            group_key: id.to_string(),
        };
        let order = vec!["claude".to_string(), "codex".to_string()];
        // "newbie" was just detected and isn't in the saved order.
        let ordered = order_providers(
            vec![row("terminal"), row("newbie"), row("codex"), row("claude")],
            &order,
        );
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "codex", "newbie", "terminal"]);
    }

    /// Issue #573 AC: an uninstalled harness keeps its saved slot — when it
    /// reappears among the rows it lands back in its stored position rather than
    /// being appended.
    #[test]
    fn order_providers_uninstalled_keeps_slot() {
        let row = |id: &str| ProviderInfo {
            id: id.to_string(),
            label: id.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            // Native Spawn Option: harness_id mirrors the row id, no
            // provider, group_key follows the harness (issue #575).
            harness_id: id.to_string(),
            provider_id: None,
            is_proxied: false,
            group_key: id.to_string(),
        };
        // Saved order had minimax between claude and codex; it was uninstalled
        // (absent from rows) for a while, now it's back.
        let order = vec![
            "claude".to_string(),
            "minimax".to_string(),
            "codex".to_string(),
        ];
        let ordered = order_providers(
            vec![row("codex"), row("claude"), row("minimax"), row("terminal")],
            &order,
        );
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "minimax", "codex", "terminal"]);
    }

    // ----- Spawn Option wire shape (issue #575 / ADR-0016) ---------------
    //
    // `compose_provider_menu` is the pure seam for the Spawn Menu derivation.
    // The grouped render (harness header + Proxied children) needs three
    // things to be true:
    //
    // 1. Each row carries `harness_id`, `provider_id`, `is_proxied`, and
    //    `group_key` (the frontend `groupBy(opt => opt.group_key)`).
    // 2. `order_providers` ranks by `harness_id` so a Proxied child
    //    clusters under its native harness header, not at the bottom of
    //    the stored order.
    // 3. `parse_spawn_option_id` splits a composite id on the first `:`
    //    so the resolver chain can pick the executor from the harness
    //    part and the credentials from the provider part.

    /// The native row for a harness profile has `provider_id = None`,
    /// `is_proxied = false`, and `group_key == harness_id == id`. A
    /// detected Claude Code profile is the canonical "clickable harness
    /// header" row.
    #[test]
    fn provider_info_for_marks_native_row_with_no_provider_id() {
        let claude = crate::preferences::HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "anthropic".to_string(),
        };
        let info = provider_info_for(&claude, Platform::Windows)
            .expect("claude profile is available on Windows");
        assert_eq!(info.id, "claude");
        assert_eq!(info.harness_id, "claude");
        assert!(info.provider_id.is_none(), "native row must have no provider_id");
        assert!(!info.is_proxied);
        assert_eq!(info.group_key, "claude");
    }

    /// A configured Claude-compatible account surfaces as a Proxied
    /// Provider row with the composite id `<harness>:<provider>`, and
    /// `harness_id` / `group_key` follow the Claude Code profile (the
    /// only Proxied-attached harness today). The frontend uses these to
    /// bucket the row under the right harness header.
    #[test]
    fn provider_info_for_account_marks_proxied_row_with_composite_id() {
        let profiles = vec![crate::preferences::HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "anthropic".to_string(),
        }];
        let mm = crate::preferences::ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-mm".to_string()),
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            model_tiers: crate::preferences::ModelTiers::default(),
            models: Vec::new(),
        };
        let info = provider_info_for_account(&mm, &profiles, Platform::Windows)
            .expect("claude executor is available on Windows");
        assert_eq!(info.id, "claude:minimax");
        assert_eq!(info.harness_id, "claude");
        assert_eq!(info.provider_id.as_deref(), Some("minimax"));
        assert!(info.is_proxied);
        assert_eq!(info.group_key, "claude");
    }

    /// A configured account that has no detected Claude Code profile on
    /// this host falls back to the literal harness id `"claude"` for
    /// `harness_id` and `group_key`. The resolver still maps the
    /// bare `"claude"` to the Anthropic executor, so the grouped render
    /// stays correct.
    #[test]
    fn provider_info_for_account_falls_back_to_claude_harness_id() {
        let profiles: Vec<crate::preferences::HarnessProfile> = vec![];
        let mm = crate::preferences::ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-mm".to_string()),
            base_url: None,
            model_tiers: crate::preferences::ModelTiers::default(),
            models: Vec::new(),
        };
        let info = provider_info_for_account(&mm, &profiles, Platform::Windows).unwrap();
        assert_eq!(info.harness_id, "claude");
        assert_eq!(info.group_key, "claude");
        assert_eq!(info.id, "claude:minimax");
    }

    /// A Proxied Provider row clusters under its harness header
    /// (`harness_id` rank), not under its composite `id` (which isn't in
    /// the stored order). A naive `position(p.id)` would push the child
    /// to `usize::MAX - 1`; ranking by `harness_id` keeps it next to
    /// its native header so the frontend's `groupBy` groups them
    /// together.
    #[test]
    fn order_providers_ranks_proxied_rows_by_harness_id_not_composite_id() {
        let native = |harness_id: &str| ProviderInfo {
            id: harness_id.to_string(),
            label: harness_id.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            harness_id: harness_id.to_string(),
            provider_id: None,
            is_proxied: false,
            group_key: harness_id.to_string(),
        };
        let proxied = |harness_id: &str, provider_id: &str| ProviderInfo {
            id: format!("{}:{}", harness_id, provider_id),
            label: provider_id.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            harness_id: harness_id.to_string(),
            provider_id: Some(provider_id.to_string()),
            is_proxied: true,
            group_key: harness_id.to_string(),
        };
        let order = vec!["claude".to_string(), "codex".to_string()];
        let rows = vec![
            proxied("claude", "minimax"),
            native("terminal"),
            native("codex"),
            native("claude"),
        ];
        let ordered = order_providers(rows, &order);
        // All four rows have the same `harness_id` group ("claude",
        // "codex", or "terminal"), so the rank sort is by harness_id:
        // rank 0 = claude, rank 1 = codex, terminal = usize::MAX
        // (last). Within the same rank, the stable sort preserves the
        // input order, so the Proxied child (first input row, rank 0)
        // stays ahead of the native claude row. The frontend's
        // `groupBy(group_key)` then buckets them into the same
        // "Claude Code" group regardless of which row is first.
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["claude:minimax", "claude", "codex", "terminal"],
            "Proxied child must cluster under its harness header (stable sort keeps equal-rank input order)"
        );
    }

    // ----- parse_spawn_option_id resolver (issue #575) ------------------
    //
    // The composite id format `<harness>` (native) or `<harness>:<provider>`
    // (proxied) is split on the first `:` by the resolver chain
    // (`preferences::resolve_harness_provider` and
    // `preferences::resolve_provider_env`). A provider id containing `:`
    // (a theoretical edge case — the id is user-chosen) is preserved
    // intact on the right side.

    #[test]
    fn parse_spawn_option_id_splits_bare_into_native() {
        let (harness, provider) =
            crate::agent::provider::parse_spawn_option_id("claude");
        assert_eq!(harness, "claude");
        assert!(provider.is_none());
    }

    #[test]
    fn parse_spawn_option_id_splits_composite_into_harness_and_provider() {
        let (harness, provider) =
            crate::agent::provider::parse_spawn_option_id("claude:minimax");
        assert_eq!(harness, "claude");
        assert_eq!(provider, Some("minimax"));
    }

    #[test]
    fn parse_spawn_option_id_keeps_provider_part_intact_on_duplicate_colons() {
        // A provider id with its own `:` (theoretical today, but the
        // id is user-chosen so we can't rule it out) lands entirely in
        // the provider slot. The first `:` is the split, not the last.
        let (harness, provider) =
            crate::agent::provider::parse_spawn_option_id("claude:weird:id");
        assert_eq!(harness, "claude");
        assert_eq!(provider, Some("weird:id"));
    }

    /// The resolver chain (`resolve_harness_provider`) splits a composite
    /// id on the first `:` and uses only the harness part to pick the
    /// executor. `claude:minimax` resolves to the Anthropic executor
    /// (Claude Code), not to a nonexistent `claude:minimax` Provider.
    /// The composite-id path is exercised through
    /// `Provider::from_db_str` in `models::tests`; the split logic
    /// itself is the `parse_spawn_option_id` test above. Here we pin
    /// the post-#538 legacy fallback for a bare `minimax` id so a
    /// pre-migration archived node still resolves correctly.
    #[test]
    fn resolve_harness_provider_legacy_minimax_id_falls_through_to_anthropic() {
        // `Provider::from_db_str("minimax")` is a static lookup —
        // doesn't touch the preferences cache or APP_DATA_DIR. So we
        // can verify the legacy fallback (issue #538 cutover) here
        // without driving the temp-dir helper.
        use crate::models::Provider;
        assert_eq!(Provider::from_db_str("minimax"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str("kimi"), Provider::Anthropic);
    }

    // ----- v19 migration (issue #575) -----------------------------------
    //
    // The `migrate_agent_node_provider_id_to_composite` rewrite
    // (src-tauri/src/db/mod.rs) is exercised by the integration tests
    // in `db::migration_tests`. The unit tests below pin the pure
    // helpers (id format + the first-class/custom id classification
    // rule) so a refactor that drops a category surfaces here.

    /// The legacy `minimax` / `kimi` ids are always Proxied Provider
    /// rows — they're the two first-class built-ins (issue #566) and
    /// always pair with Claude Code. The migration always rewrites
    /// them. (Pin the static list so adding a new first-class provider
    /// in the future requires a paired test update.)
    #[test]
    fn first_class_legacy_ids_are_known_proxied() {
        // The migration's first block rewrites exactly these two ids.
        for id in ["minimax", "kimi"] {
            assert!(
                crate::preferences::is_claude_compatible_id(id),
                "{id} must be classified as claude_compatible so the migration picks it up"
            );
        }
    }

    /// A user-typed custom account id (e.g. "deepseek") is also
    /// Proxied — the migration's second block catches it as long as
    /// the live `provider_accounts()` read surfaces it as
    /// `claude_compatible`. A disabled or non-claude_compatible
    /// account is left alone (its nodes fall through to the
    /// Anthropic default at spawn time, which is the legacy
    /// behaviour).
    #[test]
    fn custom_claude_compatible_accounts_are_known_proxied() {
        // `is_claude_compatible_id` is the public classification — a
        // custom id is Proxied iff it isn't in the self-auth set.
        assert!(crate::preferences::is_claude_compatible_id("deepseek"));
        assert!(!crate::preferences::is_claude_compatible_id("anthropic"));
        assert!(!crate::preferences::is_claude_compatible_id("codex"));
    }

    // ----- compose_provider_menu (issue #568) ----------------------------
    //
    // The spawn menu is derived: harness profiles + configured Claude-compatible
    // accounts. `compose_provider_menu` is the pure seam so account-inclusion can
    // be pinned without driving `provider_accounts()` (disk + globals).

    fn profile(id: &str, harness: &str) -> crate::preferences::HarnessProfile {
        crate::preferences::HarnessProfile {
            id: id.to_string(),
            name: id.to_string(),
            harness: harness.to_string(),
        }
    }

    fn acct(id: &str, enabled: bool, key: Option<&str>) -> crate::preferences::ProviderAccount {
        crate::preferences::ProviderAccount {
            id: id.to_string(),
            name: format!("{id} acct"),
            enabled,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: crate::preferences::is_claude_compatible_id(id),
            api_key: key.map(str::to_string),
            base_url: Some("https://api.example.com/anthropic".to_string()),
            model_tiers: crate::preferences::ModelTiers::default(),
            models: Vec::new(),
        }
    }

    #[test]
    fn compose_menu_adds_enabled_keyed_claude_compatible_accounts() {
        // Issue #575 / ADR-0016: a configured claude_compatible account
        // surfaces as a Proxied Provider row with the composite id
        // `<harness>:<provider>`, grouping under the detected Claude
        // Code harness profile (here `claude`).
        let menu = compose_provider_menu(
            vec![profile("claude", "anthropic"), profile("terminal", "terminal")],
            vec![acct("minimax", true, Some("sk-mm")), acct("kimi", true, Some("sk-moon"))],
            Platform::Windows,
            &[],
        );
        let ids: Vec<_> = menu.iter().map(|p| p.id.as_str()).collect();
        // Composite ids — resolver splits on ':' to get (executor, creds).
        assert!(
            ids.contains(&"claude:minimax"),
            "keyed MiniMax must appear in the menu as `claude:minimax` (Proxied Provider), got {ids:?}"
        );
        assert!(
            ids.contains(&"claude:kimi"),
            "keyed Kimi must appear in the menu as `claude:kimi` (Proxied Provider), got {ids:?}"
        );
        // The MiniMax row carries its own composite id (and brand label) so
        // the frontend renders the brand icon keyed off the provider half.
        let mm = menu.iter().find(|p| p.id == "claude:minimax").unwrap();
        assert_eq!(mm.label, "minimax acct");
        assert_eq!(mm.harness_id, "claude");
        assert_eq!(mm.provider_id.as_deref(), Some("minimax"));
        assert!(mm.is_proxied);
        assert_eq!(mm.group_key, "claude");
        // Terminal still sorts last.
        assert_eq!(ids.last(), Some(&"terminal"));
    }

    #[test]
    fn compose_menu_excludes_unconfigured_or_disabled_accounts() {
        let menu = compose_provider_menu(
            vec![profile("terminal", "terminal")],
            vec![
                acct("minimax", true, None),          // enabled but no key
                acct("kimi", false, Some("sk-moon")), // keyed but disabled
                acct("anthropic", true, Some("x")),   // self-auth → not claude_compatible
            ],
            Platform::Windows,
            &[],
        );
        let ids: Vec<_> = menu.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["terminal"], "none of these should reach the menu");
    }

    #[test]
    fn compose_menu_does_not_duplicate_a_detected_profile() {
        // A detected "claude" harness profile and a (hypothetical) same-id account
        // must not both appear — the profile wins, no duplicate row.
        let menu = compose_provider_menu(
            vec![profile("claude", "anthropic")],
            vec![acct("claude", true, Some("k"))],
            Platform::Windows,
            &[],
        );
        assert_eq!(menu.iter().filter(|p| p.id == "claude").count(), 1);
    }

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
    /// `IssueNodeDraft` must carry the prefill from `format_pr_prefill`
    /// AND the node name from `pr_node_name`. Both pieces are computed
    /// from the SAME `(pr_number, pr_title)` pair, so a refactor that
    /// accidentally passes a different value to one of them (e.g. a
    /// stale `pr_title` from a closure) would break the user's mental
    /// model: the agent gets a prefill referring to a different PR
    /// than the one whose name appears in the sidebar.
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

        let draft = create_pr_node_impl(
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

        // Prefill seam: matches the helper exactly.
        let expected_prefill = format_pr_prefill(
            "alondero",
            "buildmesh",
            pr_number,
            pr_title,
        );
        assert_eq!(
            draft.prefill, expected_prefill,
            "returned prefill must match format_pr_prefill exactly — \
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
        let db = crate::db::get().lock().unwrap();
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
        .expect("empty head_sha must be accepted (drift check skipped)");
        assert_eq!(
            draft_b.node.source_pr_pinned_sha, None,
            "empty head_sha must persist as None (skip drift check, fail-open)"
        );
    }

    // -----------------------------------------------------------------------
    // ProviderInfo.resumable flag (issue #550 follow-up)
    //
    // The frontend's archived-node resume picker used to filter providers via
    // a hardcoded `['anthropic','minimax','kimi']` allow-list, which silently
    // hid custom Claude-compatible harness profiles (e.g. "DeepSeek via
    // Claude") from the picker. The fix is a backend-supplied `resumable` flag
    // on ProviderInfo, derived from the resolved adapter's
    // `supports_resume() && produces_readable_transcript()` so every dynamic
    // Claude-compatible profile (which all share the `anthropic` executor,
    // see `preferences::resolve_harness_provider`) advertises itself correctly.
    // -----------------------------------------------------------------------

    /// Pin the negative case for `resumable`: Terminal is always present
    /// (code-defined default), and a plain shell neither supports resume nor
    /// produces a readable transcript, so the flag must be `false`. This
    /// test runs in the bare-test env (no `with_temp_dir`), so the only row
    /// `available_providers()` sees is Terminal.
    #[test]
    fn available_providers_marks_terminal_as_not_resumable() {
        let providers = available_providers();
        let terminal = providers
            .iter()
            .find(|p| p.id == "terminal")
            .expect("Terminal profile always present");
        assert!(
            !terminal.resumable,
            "Terminal is a plain shell — resumable must be false"
        );
    }

    /// Pin the positive case: a stored harness profile whose backing executor
    /// is the Claude-backed `anthropic` adapter must advertise `resumable=true`
    /// so it shows up in the archived-node resume picker. Mirrors the
    /// "DeepSeek via Claude" / "Kimi via Claude" pattern from issue #537 —
    /// any id, any user-chosen name; the resumability is purely a property of
    /// the resolved adapter, not the stored id.
    ///
    /// Tested against `provider_info_for` (the pure helper) rather than
    /// `available_providers` so this test doesn't drive the preferences
    /// module's `APP_DATA_DIR` / `CACHE` globals — which are shared across
    /// the `preferences::tests` module's `with_temp_dir` and would race.
    #[test]
    fn provider_info_marks_claude_backed_profile_as_resumable() {
        use crate::preferences::HarnessProfile;
        let deepseek = HarnessProfile {
            // Custom id + user-chosen name — the exact pattern that the old
            // allow-list silently filtered out.
            id: "deepseek-via-claude".to_string(),
            name: "DeepSeek (via Claude)".to_string(),
            harness: "anthropic".to_string(),
        };
        let info = provider_info_for(&deepseek, Platform::Windows)
            .expect("anthropic-backed profile is available on Windows");
        assert!(
            info.resumable,
            "claude-backed profile must be resumable=true so it shows up in the resume picker"
        );
    }

    /// Negative companion to the previous test: the `harness` field must
    /// actually drive the executor resolution. If a profile pins
    /// `harness: "opencode"`, `resumable` must be `false` even though the
    /// id is user-chosen and the test env has no stored profiles. This
    /// pins that `provider_info_for` consults `profile.harness` directly
    /// (via `Provider::from_db_str`) rather than silently falling through
    /// to Anthropic on the id.
    #[test]
    fn provider_info_consults_harness_field_not_id_fallback() {
        use crate::preferences::HarnessProfile;
        let custom_opencode = HarnessProfile {
            // Custom id, but `harness: "opencode"` — must NOT collapse to
            // Anthropic. (The previous version of `provider_info_for`
            // resolved via `resolve_harness_provider(&profile.id)`, whose
            // fallback path returned Anthropic for unknown ids and would
            // have silently made this test pass for the wrong reason.)
            id: "custom-opencode-flavor".to_string(),
            name: "Custom OpenCode".to_string(),
            harness: "opencode".to_string(),
        };
        let info = provider_info_for(&custom_opencode, Platform::Windows)
            .expect("OpenCode-backed profile is available on Windows");
        assert!(
            !info.resumable,
            "OpenCode-backed profile must be resumable=false; the harness field drives resolution"
        );
    }

    /// Pin that the same pure helper marks a non-resumable profile correctly.
    /// `Terminal`'s adapter (`is_plain_terminal`) returns false for
    /// `produces_readable_transcript` and false for `supports_resume`, so
    /// the derived flag must be false regardless of host.
    #[test]
    fn provider_info_marks_terminal_as_not_resumable() {
        use crate::preferences::HarnessProfile;
        let terminal = HarnessProfile {
            id: "terminal".to_string(),
            name: "Terminal".to_string(),
            harness: "terminal".to_string(),
        };
        let info = provider_info_for(&terminal, Platform::Windows)
            .expect("Terminal is available on Windows");
        assert!(
            !info.resumable,
            "Terminal is plain shell — resumable must be false"
        );
    }

    /// Pin the legacy-id contract: the `minimax`/`kimi` ids that archived
    /// nodes still carry on disk resolve to the Anthropic executor (per
    /// `Provider::from_db_str`) and therefore advertise `resumable=true`.
    /// This is the regression case the frontend's hardcoded
    /// `['anthropic','minimax','kimi']` allow-list used to encode as a
    /// stringly-typed list — now it's a single derivation rule.
    #[test]
    fn provider_info_marks_legacy_minimax_kimi_ids_as_resumable() {
        use crate::preferences::HarnessProfile;
        for (id, label) in [("minimax", "Minimax"), ("kimi", "Kimi")] {
            let profile = HarnessProfile {
                id: id.to_string(),
                name: label.to_string(),
                harness: id.to_string(),
            };
            let info = provider_info_for(&profile, Platform::Windows)
                .unwrap_or_else(|| panic!("{id} must be available on Windows"));
            assert!(
                info.resumable,
                "legacy id {id:?} resolves to Anthropic (issue #538) and must be resumable"
            );
        }
    }
}
