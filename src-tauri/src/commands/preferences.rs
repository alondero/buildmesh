//! Tauri commands for buildmesh-wide preferences.
//!
//! See `crate::preferences` for the persistence layer.
//!
//! Pure sync — each command is a single `preferences::load` /
//! `save` round-trip + optional `app.emit`. They run on Tauri's IPC
//! worker, NOT the bounded tokio pool. Issue #1380 review point 4.

use crate::preferences::{
    self, AppPreferences, CapabilityMaskForResolver, HarnessConfigField, HarnessConfigValue,
    HarnessProfile, ModelTiers, PairingVerification, ProviderAccount, ProviderPairing,
    ResolvedCascadeView,
};
use crate::preferences::resolver::cascade::{
    apply_capability_mask, field_inputs, harness_config_str,
};
use serde::{Deserialize, Serialize};
use tauri::{command, AppHandle, Emitter};
use ts_rs::TS;

/// Read the persisted buildmesh-wide preferences. Always returns a value —
/// a missing or malformed file yields `AppPreferences::default()`.
#[command]
pub fn get_app_preferences() -> Result<AppPreferences, String> {
    preferences::load()
}

/// Set the buildmesh-wide default provider. Pass `None` (or an empty string,
/// which is normalised away) to clear the override and restore the hardcoded
/// `claude` fallback (post-#538 unified harness id).
#[command]
pub fn set_app_default_provider(provider: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.default_provider = provider.filter(|s| !s.is_empty());
    preferences::save(prefs)
}

/// Set the buildmesh-wide reviewer Spawn Option. Pass `None` (or blank) to
/// restore the source-agent fallback. This is deliberately independent from
/// the ordinary default provider so adversarial reviews can use another
/// harness without changing implementation spawns.
///
/// The stored value is the inherit-path reviewer for every built-in review
/// run without a per-run override, so it passes the same
/// attention-compatibility gate as the Start Review picker (issue #1816):
/// a harness that can never yield a turn is refused here rather than
/// minting runs that park forever at the `verdict` gate.
#[command]
pub fn set_app_reviewer_provider(provider: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.reviewer_provider = provider
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(ref value) = prefs.reviewer_provider {
        crate::autopilot::compatibility::validate_reviewer_provider_id(value)?;
    }
    preferences::save(prefs)
}

/// Set the backend that summaries node PTY output into a slug (issue #824).
/// Distinct from [`set_app_default_provider`]: auto-naming runs on every
/// rename trigger (often), at low content complexity, so it shouldn't
/// inherit an expensive tier the spawned node happens to be on. `None` (or
/// an empty string) **disables auto-naming entirely** — nodes keep their
/// random `adjective-adjective-noun` slugs until the user picks a value.
#[command]
pub fn set_app_naming_provider(provider: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.naming_provider = provider.filter(|s| !s.is_empty());
    if let Some(selection) = &prefs.naming_provider {
        crate::session_naming::naming_backend_env(selection)?;
    }
    preferences::save(prefs)
}

/// Set the app-wide autopilot pool size — the cap on concurrently active
/// autopilot nodes across **all** meshes. `None` clears the cap (per-mesh
/// limits alone apply); `Some(0)` pauses all new autopilot spawns. Takes
/// effect on the poller's next pass — running nodes are never killed.
#[command]
pub fn set_app_autopilot_pool_size(size: Option<u32>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.autopilot_pool_size = size;
    preferences::save(prefs)
}

/// Set whether to confirm before quitting when agent sessions are active
/// (issue #1501). `true` (the default) surfaces the exit-confirmation modal
/// on window close with active nodes; `false` closes without friction.
#[command]
pub fn set_app_confirm_before_quit(confirm: bool) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.confirm_before_quit = confirm;
    preferences::save(prefs)
}

/// Set the Buildmesh-wide default Worktree Node directory (issue #1519).
/// `None` (or blank, which collapses to `None`) clears the override and
/// restores the `.claude/worktrees` default under each Mesh root.
/// Relative values resolve from each inheriting Mesh's root. Absolute
/// values are rejected here with an actionable message: one app default
/// spans meshes in both host environments (native/Windows versus WSL),
/// so an absolute path can only ever match a subset of meshes — set it
/// as a per-Mesh override in Project Settings → Worktrees instead, where
/// the backend validates the environment match. No shell/`~` expansion.
/// Changing it affects future nodes and warm-pool entries only — live
/// nodes keep their persisted `worktree_path`. Schedules a background
/// pool rebuild for inheriting meshes so idle inventory converges on the
/// new location.
#[command]
pub fn set_app_worktree_directory(app: AppHandle, directory: Option<String>) -> Result<(), String> {
    use crate::env::normalize_worktree_directory;
    let cleaned = normalize_worktree_directory(directory.as_deref());
    let cleaned = match cleaned {
        None => None,
        Some(dir) => {
            if crate::env::is_absolute_worktree_path(&dir)
                || crate::env::is_drive_relative_worktree_path(&dir)
            {
                return Err(format!(
                    "worktree directory '{dir}' is not a plain relative path, but the application default spans meshes in both environments (native/Windows versus WSL) — \
                     use a relative path like 'worktrees' resolved from each mesh root, or set this path as a per-Mesh override in Project Settings → Worktrees"
                ));
            }
            Some(crate::env::normalize_relative_worktree_dir(&dir)?)
        }
    };
    let mut prefs = preferences::load()?;
    prefs.worktree_directory = cleaned.clone();
    preferences::save(prefs)?;
    // Notify first (`None` = every inheriting mesh may have moved), then
    // enqueue the rebuild — both read the committed preferences.
    let _ = app.emit(
        crate::commands::mesh_properties::WORKTREE_DIR_CHANGED_EVENT,
        Option::<i64>::None,
    );
    // Enqueues a debounced, fill-lock-serialized rebuild for inheriting
    // meshes and returns immediately (the function owns its background
    // thread — see `rebuild_pools_for_worktree_dir_change`).
    crate::services::warm_pool::rebuild_pools_for_worktree_dir_change(&app, None);
    Ok(())
}

/// Effective Worktree Node directory config for one Mesh (issue #1519).
/// Returns the Mesh override, the application default, and the resolved
/// effective container dir so Settings → General (app default) and
/// Project Settings → Worktrees (override + inherited effective) can
/// render without re-spelling the precedence rule.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "WorktreeDirectoryConfig.ts")]
pub struct WorktreeDirectoryConfig {
    pub mesh_directory: Option<String>,
    pub app_directory: Option<String>,
    pub effective_directory: String,
}

#[command]
pub fn get_worktree_directory_config(mesh_id: i64) -> Result<WorktreeDirectoryConfig, String> {
    let mesh = crate::db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let app_dir = preferences::worktree_directory();
    let effective = crate::env::effective_worktree_dir_raw(
        &mesh.path,
        mesh.worktree_directory.as_deref(),
        app_dir.as_deref(),
    );
    Ok(WorktreeDirectoryConfig {
        mesh_directory: mesh.worktree_directory.clone(),
        app_directory: app_dir,
        effective_directory: effective,
    })
}

/// Persist the user's spawn-menu harness order (issue #573). `order` is the list
/// of harness-row ids in the desired top-to-bottom order; `Terminal` is filtered
/// out backend-side (it's always forced last). Emits `provider-list-changed` so
/// every spawn surface (sidebar menu, Probe tabs) drops its cached provider list
/// and re-reads the reordered menu — the same cross-component invalidation used
/// by the account commands.
#[command]
pub fn set_harness_order(app: AppHandle, order: Vec<String>) -> Result<(), String> {
    preferences::set_harness_order(order)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

/// Persist the **Proxied Provider** child order under one harness (issue
/// #577). `provider_ids` is the top-to-bottom list of `provider_id`s as the
/// user arranged them in the drag list on the harness-config page; only
/// registered [`crate::preferences::ProviderAccount`] ids are persisted
/// (unknown ids are silently dropped — the order seam would never render
/// them anyway, and a stale UI send can't pollute the preferences file).
/// Cross-harness drag is disallowed at the UI layer (each `HarnessCard`
/// is its own `DndContext`), so the harness_id + provider_ids pair is the
/// entire scope the command accepts.
///
/// Emits `provider-list-changed` so every spawn surface (sidebar, Probe
/// tabs, archived-resume, mobile) drops its cached provider list and
/// re-reads the reordered menu — the same invalidation [`set_harness_order`]
/// fires for the harness-level reorder.
#[command]
pub fn set_proxied_provider_order(
    app: AppHandle,
    harness_id: String,
    provider_ids: Vec<String>,
) -> Result<(), String> {
    preferences::set_proxied_provider_order(harness_id, provider_ids)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

/// The effective model-provider account list — self-auth built-ins plus any
/// keyed first-class / generic accounts the user has added (ADR-0025).
#[command]
pub fn get_provider_accounts() -> Result<Vec<ProviderAccount>, String> {
    Ok(preferences::provider_accounts())
}

/// Keyed first-class catalog templates (MiniMax, Kimi, OpenRouter) for the
/// Providers-page "Add provider" picker (ADR-0025). The UI filters out ids
/// already present in [`get_provider_accounts`].
#[command]
pub fn get_keyed_first_class_catalog() -> Result<Vec<ProviderAccount>, String> {
    Ok(preferences::keyed_first_class_catalog())
}

/// Attach-form defaults for `(harness_id, provider_id)` — first-class published
/// endpoint + tiers when available; `None` when the pair is incompatible
/// (ADR-0025). Does not require a stored pairing.
#[command]
pub fn get_pairing_defaults(
    harness_id: String,
    provider_id: String,
) -> Result<Option<ProviderPairing>, String> {
    Ok(preferences::pairing_for(&harness_id, &provider_id))
}

/// Create or update a model-provider account (issue #537 / ADR-0025). For a
/// custom (non-built-in) account the row is added; spawn-menu visibility
/// requires an explicit attach under the Harnesses page. Invalidates the
/// usage cache so a changed key/enabled-state is reflected on the next panel
/// refresh.
///
/// Emits `provider-list-changed` so frontend consumers (Sidebar spawn menu,
/// Probe tabs that list provider options) drop their locally-cached provider
/// list and re-read. The tauri.ts `listProviders` cache is also busted in the
/// JS wrapper, but that only helps callers within the same component — other
/// components with their own `providerData` state need an explicit signal.
#[command]
pub fn upsert_provider_account(
    app: AppHandle,
    account: ProviderAccount,
) -> Result<(), String> {
    let account_id = account.id.clone();
    let codex_harnesses = {
        let mut prefs = preferences::load()?;
        preferences::upsert_provider_account(&mut prefs, account);
        let harnesses: Vec<String> = prefs
            .provider_pairings
            .iter()
            .filter(|pairing| {
                pairing.provider_id == account_id
                    && pairing.surface == preferences::ApiSurface::OpenAI
            })
            .map(|pairing| pairing.harness_id.clone())
            .collect();
        preferences::save(prefs)?;
        crate::services::usage::invalidate_cache();
        harnesses
    };
    let _ = app.emit("provider-list-changed", ());
    for harness_id in codex_harnesses {
        schedule_pairing_verification(app.clone(), harness_id, account_id.clone());
    }
    Ok(())
}

/// Remove a stored provider account. Removing a self-auth built-in just
/// reverts it to the code-defined default; keyed-first-class and generic
/// rows are deleted outright (re-adding them starts from the catalog /
/// blank). Stored pairings for the removed id are filtered out at spawn
/// time — detach separately if the goal is hiding the row from the spawn
/// menu only. Emits `provider-list-changed` for cross-component
/// invalidation (same reason as [`upsert_provider_account`]).
#[command]
pub fn remove_provider_account(app: AppHandle, id: String) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    preferences::remove_provider_account(&mut prefs, &id);
    preferences::save(prefs)?;
    crate::services::usage::invalidate_cache();
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

// ---------------------------------------------------------------------------
// Proxied Provider pairings (ADR-0016 §4, issue #576)
// ---------------------------------------------------------------------------

/// The full effective set of **Proxied Provider** pairings — stored pairings
/// for proxiable accounts only (ADR-0025). The harness-config page renders this
/// to show what's attached under each harness (issue #576).
#[command]
pub fn get_provider_pairings() -> Result<Vec<ProviderPairing>, String> {
    Ok(preferences::effective_provider_pairings())
}

#[command]
pub fn get_pairing_verifications(
    env_type: Option<crate::models::EnvType>,
) -> Result<Vec<PairingVerification>, String> {
    let env_type = env_type.unwrap_or(crate::models::EnvType::Windows);
    Ok(crate::services::provider_verification::current_statuses(env_type))
}

#[command]
pub fn verify_provider_pairing(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
    env_type: Option<crate::models::EnvType>,
) -> Result<PairingVerification, String> {
    let env_type = env_type.unwrap_or(crate::models::EnvType::Windows);
    let record = crate::services::provider_verification::verify_pairing_blocking(
        &harness_id,
        &provider_id,
        env_type,
    )?;
    let _ = app.emit("pairing-verification-changed", &record);
    let _ = app.emit("provider-list-changed", ());
    Ok(record)
}

/// Schedule a pairing-verification probe (network call) for one
/// `(harness_id, provider_id, env_type)` tuple. Spawns an async task
/// because the probe is HTTP-bound (can take seconds); runs the
/// blocking verification work via `run_blocking` so the tokio worker
/// stays free for streaming.
pub(crate) fn schedule_pairing_verification(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
) {
    for env_type in [crate::models::EnvType::Windows, crate::models::EnvType::Wsl] {
        schedule_pairing_verification_for_runtime(
            app.clone(),
            harness_id.clone(),
            provider_id.clone(),
            env_type,
        );
    }
}

pub(crate) fn schedule_pairing_verification_for_runtime(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
    env_type: crate::models::EnvType,
) {
    tauri::async_runtime::spawn(async move {
        let result = crate::commands::run_blocking("verify_provider_pairing", move || {
            crate::services::provider_verification::verify_pairing_blocking(
                &harness_id,
                &provider_id,
                env_type,
            )
        })
        .await;
        if let Ok(record) = result {
            let _ = app.emit("pairing-verification-changed", record);
            let _ = app.emit("provider-list-changed", ());
        }
    });
}

/// The **Model Providers** offered by "Add proxied provider" under `harness_id`,
/// surface-matched: only providers whose **Compatible API surface** that harness
/// speaks (issue #576). Empty for a native-only harness (Terminal, etc.).
#[command]
pub fn compatible_providers_for_harness(
    harness_id: String,
) -> Result<Vec<ProviderAccount>, String> {
    Ok(preferences::compatible_providers_for_harness(&harness_id))
}

/// Attach a **Model Provider** to a harness over the harness's surface — the
/// "Add proxied provider" action (issue #576 / ADR-0025). Starts from
/// [`preferences::pairing_for`] defaults (first-class published endpoint, or a
/// surface-only shell for generics), then overlays optional `base_url` /
/// `model_tiers`. Requires a non-empty `base_url` after overlay (fill from
/// first-class or supply). `api_key`, when present, seeds the provider's
/// **global** key only if it has none (set-if-absent).
#[command]
pub fn attach_proxied_provider(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
    api_key: Option<String>,
    base_url: Option<String>,
    model_tiers: Option<ModelTiers>,
) -> Result<(), String> {
    let should_verify = {
        let surface =
            preferences::harness_surface(&harness_id).ok_or_else(|| {
                format!("harness '{harness_id}' does not speak a proxy-capable surface")
            })?;
        let mut pairing =
            preferences::pairing_for(&harness_id, &provider_id).unwrap_or_else(|| {
                ProviderPairing {
                    harness_id: harness_id.clone(),
                    provider_id: provider_id.clone(),
                    surface,
                    base_url: None,
                    model_tiers: ModelTiers::default(),
                }
            });
        // Surface-match gate: refuse when the provider doesn't expose this surface.
        let accounts = preferences::provider_accounts();
        let account = accounts.iter().find(|a| a.id == provider_id);
        // Keyed first-class may not be materialised yet — check catalog too.
        let surfaces = account
            .map(preferences::provider_surfaces)
            .or_else(|| {
                preferences::keyed_first_class_catalog()
                    .into_iter()
                    .find(|a| a.id == provider_id)
                    .map(|a| preferences::provider_surfaces(&a))
            })
            .unwrap_or_default();
        if !surfaces.contains(&surface) {
            return Err(format!(
                "provider '{provider_id}' is not compatible with harness '{harness_id}'"
            ));
        }
        if let Some(url) = base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            pairing.base_url = Some(url.to_string());
        }
        if let Some(tiers) = model_tiers {
            pairing.model_tiers = tiers;
        }
        if pairing.base_url.as_deref().is_none_or(|s| s.trim().is_empty()) {
            return Err(format!(
                "base_url is required to attach provider '{provider_id}' to harness '{harness_id}'"
            ));
        }
        let compatibility = preferences::pairing_compatibility(&pairing);
        if !compatibility.compatible {
            return Err(compatibility
                .reason
                .unwrap_or_else(|| "pairing does not satisfy the harness capability contract".into()));
        }
        let mut prefs = preferences::load()?;
        if let Some(key) = api_key.as_deref().filter(|k| !k.is_empty()) {
            preferences::set_account_key_if_absent(&mut prefs, &provider_id, key);
        }
        preferences::upsert_provider_pairing(&mut prefs, pairing);
        preferences::save(prefs)?;
        crate::services::usage::invalidate_cache();
        surface == preferences::ApiSurface::OpenAI
    };
    let _ = app.emit("provider-list-changed", ());
    if should_verify {
        schedule_pairing_verification(app, harness_id, provider_id);
    }
    Ok(())
}

/// Update `base_url` and/or `model_tiers` on an existing stored pairing
/// (ADR-0025 — Harnesses page inline edit). Errors if no pairing is stored for
/// the `(harness_id, provider_id)` key.
#[command]
pub fn update_provider_pairing(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
    base_url: Option<String>,
    model_tiers: Option<ModelTiers>,
) -> Result<(), String> {
    let should_verify = {
        let mut prefs = preferences::load()?;
        let pairing = prefs
            .provider_pairings
            .iter_mut()
            .find(|p| p.harness_id == harness_id && p.provider_id == provider_id)
            .ok_or_else(|| {
                format!("no stored pairing for harness '{harness_id}' / provider '{provider_id}'")
            })?;
        if let Some(url) = base_url {
            let trimmed = url.trim();
            pairing.base_url = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Some(tiers) = model_tiers {
            pairing.model_tiers = tiers;
        }
        if pairing.base_url.as_deref().is_none_or(|s| s.trim().is_empty()) {
            return Err("base_url must be non-empty".to_string());
        }
        let should_verify = pairing.surface == preferences::ApiSurface::OpenAI;
        preferences::save(prefs)?;
        should_verify
    };
    let _ = app.emit("provider-list-changed", ());
    if should_verify {
        schedule_pairing_verification(app, harness_id, provider_id);
    }
    Ok(())
}

/// Detach a stored **Proxied Provider** pairing (issue #576 / ADR-0025). Emits
/// `provider-list-changed` so the spawn menu drops the detached row.
#[command]
pub fn remove_provider_pairing(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    preferences::remove_provider_pairing(&mut prefs, &harness_id, &provider_id);
    preferences::save(prefs)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

// ---------------------------------------------------------------------------
// Application-level Agent Harness defaults (issue #1150 / #1148)
// ---------------------------------------------------------------------------

/// Upsert the **application-level default** for one Agent Harness (issue
/// #1150 / #1148 step 2). Validates `value` against the harness's capability
/// descriptor (issue #1148 AC #5):
///
/// * Unknown harness id → `Err("unknown harness id …")`.
/// * Effort value outside the harness's `EffortControlKind::allowed` vocabulary
///   → `Err("effort … is not allowed for harness …")`.
/// * Harness without effort control → the effort field is dropped before
///   storage (the capability mask applies here, not just at the resolver).
///
/// If `value` carries no fields after normalisation (every field blank), the
/// sparse map entry is **removed** rather than stored as `{model: None,
/// effort: None}` (issue #1148 AC #6: "Blank values are normalized to
/// absent, and an empty harness configuration removes its sparse entry").
///
/// Writes through the existing `load → mutate → save` path so the in-process
/// cache refreshes on a successful save — subsequent spawns see the new
/// default without restart. Does NOT touch the DB and does NOT nest a
/// preferences mutex inside an existing lock (issue #1148 acceptance
/// criteria 4: "Do not introduce nested preference or database locks").
///
/// `profile_id` is the Spawn-Menu row id (built-ins like `"claude"`,
/// `"codex"`, `"agy"`, plus user-defined custom profiles). The seam
/// resolves it through [`preferences::resolve_harness_provider`] so a custom
/// Claude-compatible profile (`"deepseek-via-claude"`) maps to the Anthropic
/// capability descriptor.
#[command]
pub fn set_harness_default(profile_id: String, value: HarnessConfigValue) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    preferences::upsert_harness_default(&mut prefs, &profile_id, value)?;
    preferences::save(prefs)
}

/// Remove the **application-level default** for one Agent Harness (issue
/// #1150 / #1148 step 2). Idempotent — clearing a harness that had no
/// stored default is a no-op (so the UI's "Reset" affordance never errors).
/// The resolver then falls through to "no application override" for that
/// harness (native behaviour). Writes through the same cache-refreshing
/// path as [`set_harness_default`].
#[command]
pub fn clear_harness_default(profile_id: String) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    preferences::remove_harness_default(&mut prefs, &profile_id);
    preferences::save(prefs)
}

// ---------------------------------------------------------------------------
// Resolved harness view (issue #1656)
// ---------------------------------------------------------------------------
//
// Single IPC entry point that returns the full per-harness cascade — the
// Settings modal and the spawn menu both consume this so the UI no longer
// reconstructs the cascade client-side. The spawn path
// (`agent::capabilities::resolve_agent_config`) computes the same value at
// process launch; both call sites route through
// `preferences::resolver::cascade` so the helper-level cascade tests
// (`agent::capabilities::tests::resolver_cascade_*`) protect both.
//
// The IPC returns the UN-MASKED layer breakdown + the capability-masked
// resolved value. The UI renders "inherited from application" / "overridden
// by mesh" / "no value" hints from the layer breakdown; the resolved value
// already passed the same capability mask the spawn path enforces.

/// Per-harness cascade view returned by [`get_resolved_harness_view`].
/// Carries the harness profile + the four-layer breakdown for both `model`
/// and `effort` + the capability-masked resolved value.
///
/// **Generated** to `src/types/generated/ResolvedHarnessView.ts`. The IPC
/// `cmd="get_resolved_harness_view"` is the wire-level mirror; the modal
/// consumes this struct verbatim (no client-side merge).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ResolvedHarnessView.ts")]
pub struct ResolvedHarnessView {
    /// The harness profile id the view was computed for. The UI can use
    /// this as a React `key` so a refreshed view re-keys the card without
    /// leaking stale state.
    pub harness_id: String,
    /// Optional mesh id the view was computed against. `None` when the
    /// caller asked for the application-level only view (no mesh override
    /// layer). The settings modal passes `None`; the Mesh Properties tab
    /// passes the active mesh id so the mesh_override + mesh_legacy layers
    /// participate.
    ///
    /// `#[ts(as = "Option<i32>")]` mirrors the project convention (CLAUDE.md
    /// hard rule: 64-bit ints need the annotation so TS sees `number`, not
    /// `bigint`). Buildmesh DBs treat `mesh_id` as ROWID and JS already
    /// loses precision past 2^53, so the `i32` ceiling is safe in practice.
    /// ts-rs does NOT cross `as = "i32"` over `Option<T>` — the annotation
    /// must name the wrapped type explicitly (matches every other
    /// `Option<i64>` mesh_id in the repo, e.g. `pipeline.rs`,
    /// `circuit/model.rs`, `telemetry.rs`, `ws_ticket.rs`).
    #[ts(as = "Option<i32>")]
    pub mesh_id: Option<i64>,
    /// Resolved harness profile (built-in or stored user profile). `None`
    /// when the id doesn't name a known harness — the UI surfaces this as
    /// "unknown harness" rather than fabricating a profile.
    pub resolved_profile: Option<HarnessProfile>,
    /// Resolved executor id (matches [`Provider::adapter()`] for the
    /// harness). `None` when the harness id isn't known — the resolver
    /// does NOT fabricate a `Provider::from_db_str` default ("anthropic")
    /// because that would mislead the UI into thinking we have a
    /// concrete adapter for the unknown id. See the IPC command's
    /// executor-resolution logic for the "known id but binary
    /// detection fails" path (still `Some(...)`).
    pub resolved_executor: Option<String>,
    /// Harness capability descriptor (model override + effort control +
    /// extra args + …). `None` when the harness is unknown — same
    /// fallback shape as `resolved_profile`.
    pub capabilities: Option<CapabilityMaskForResolver>,
    /// Per-harness default from `AppPreferences.harness_defaults`. The
    /// sparse map only carries an entry when the user explicitly set a
    /// default; otherwise the layer is empty and the cascade falls through.
    pub application_default: HarnessConfigValue,
    /// Per-Mesh override from `meshes.harness_overrides[harness_id]`.
    /// `None` when no mesh id was passed OR when the mesh has no override
    /// for this harness (the sparse-map invariant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_override: Option<HarnessConfigValue>,
    /// Per-Mesh legacy `meshes.model` / `meshes.effort` columns. `None` on
    /// a healthy v33+ DB (the migration copied non-empty legacy values
    /// into `mesh_override["claude"]`). Surfaced so a pre-v33 read shape
    /// still resolves through the same IPC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_legacy: Option<HarnessConfigValue>,
    /// Cascade breakdown for the `model` field. Includes all four layers
    /// plus the capability-masked resolved value.
    pub model: ResolvedCascadeView,
    /// Cascade breakdown for the `effort` field. Same shape as `model`.
    pub effort: ResolvedCascadeView,
}

/// Compute the resolved harness view for one profile id (issue #1656).
/// Pure of side effects — reads `AppPreferences` + the optional mesh's
/// `harness_overrides` + the optional mesh's legacy `model`/`effort`
/// columns + the harness profile / capability catalog.
///
/// `mesh_id == None` skips the per-Mesh layers so the IPC can be called
/// from the App Settings modal without an active mesh context.
#[command]
pub fn get_resolved_harness_view(
    harness_id: String,
    mesh_id: Option<i64>,
) -> Result<ResolvedHarnessView, String> {
    let trimmed = harness_id.trim();
    if trimmed.is_empty() {
        return Err("harness_id must be a non-empty string".to_string());
    }
    let harness_id = trimmed.to_string();

    // Application layer (always present, may be `EMPTY_DEFAULT`).
    let prefs = preferences::load()?;
    let application_default = prefs
        .harness_defaults
        .get(&harness_id)
        .cloned()
        .unwrap_or_default();

    // Per-Mesh layers — only fetched when a mesh_id was supplied.
    let (mesh_override, mesh_legacy) = match mesh_id {
        Some(id) => read_mesh_layers(id, &harness_id)?,
        None => (None, None),
    };

    // Harness profile + capabilities + executor (canonical resolver path).
    //
    // `resolved_executor` is `None` only when the harness id is unknown
    // (no built-in adapter, no stored HarnessProfile). For KNOWN
    // harnesses, we resolve through the executor unconditionally —
    // `resolved_harness_profile` requires a binary-detection step that
    // can be `None` even for a known id (e.g. on a test machine without
    // the harness installed). Tying the executor to the binary-detection
    // outcome would understate the resolver's contract.
    let resolved_profile = preferences::resolved_harness_profile(&harness_id);
    let capabilities = preferences::harness_capabilities_for(&harness_id);
    let resolved_executor = capabilities
        .as_ref()
        .map(|_| {
            preferences::resolve_harness_provider(&harness_id)
                .adapter()
                .id()
                .to_string()
        });

    // Capability mask descriptor — same fields the spawn pipeline reads.
    let mask_descriptor = capabilities
        .as_ref()
        .map(|caps| CapabilityMaskForResolver {
            supports_model_override: caps.supports_model_override,
            effort_control: caps.effort_control.clone(),
        });

    // Build the per-field cascade + apply the capability mask.
    let model = build_cascade_view(
        HarnessConfigField::Model,
        None,
        mesh_override.as_ref(),
        mesh_legacy.as_ref(),
        &application_default,
        mask_descriptor.as_ref(),
    );
    let effort = build_cascade_view(
        HarnessConfigField::Effort,
        None,
        mesh_override.as_ref(),
        mesh_legacy.as_ref(),
        &application_default,
        mask_descriptor.as_ref(),
    );

    Ok(ResolvedHarnessView {
        harness_id,
        mesh_id,
        resolved_profile,
        resolved_executor,
        capabilities: mask_descriptor,
        application_default,
        mesh_override,
        mesh_legacy,
        model,
        effort,
    })
}

/// Read the two per-Mesh layers (override map entry + legacy
/// `meshes.model`/`meshes.effort` columns). Both are `None` for a fresh
/// Mesh that has never set an override; legacy columns are `None` on a
/// healthy v33+ DB.
fn read_mesh_layers(
    mesh_id: i64,
    harness_id: &str,
) -> Result<(Option<HarnessConfigValue>, Option<HarnessConfigValue>), String> {
    let row = match crate::db::get_mesh_by_id(mesh_id) {
        Ok(r) => r,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok((None, None)),
        Err(e) => return Err(format!("failed to load mesh {mesh_id}: {e}")),
    };
    let mesh_override = row.harness_overrides.get(harness_id).cloned();
    // Legacy columns are inert post-v33 (the migration copied non-empty
    // values into `mesh_override["claude"]`). Surface them only when the
    // migration hasn't run on this row, so a stale read shape still
    // produces a correct cascade.
    let legacy = HarnessConfigValue {
        model: row.model.clone(),
        effort: row.effort.clone(),
    };
    let mesh_legacy = if legacy.model.is_some() || legacy.effort.is_some() {
        Some(legacy)
    } else {
        None
    };
    Ok((mesh_override, mesh_legacy))
}

/// Build one cascade view (model or effort) from the four layers + apply
/// the capability mask. Pulled out so the two fields share one code path.
fn build_cascade_view(
    field: HarnessConfigField,
    explicit: Option<&HarnessConfigValue>,
    mesh_override: Option<&HarnessConfigValue>,
    mesh_legacy: Option<&HarnessConfigValue>,
    application: &HarnessConfigValue,
    mask: Option<&CapabilityMaskForResolver>,
) -> ResolvedCascadeView {
    let layer_str = |src: Option<&HarnessConfigValue>| -> Option<String> {
        src.and_then(|v| harness_config_str(v, field))
    };
    let view = ResolvedCascadeView::for_field(field_inputs(
        explicit
            .and_then(|v| harness_config_str(v, field))
            .as_deref(),
        layer_str(mesh_override).as_deref(),
        layer_str(mesh_legacy).as_deref(),
        harness_config_str(application, field).as_deref(),
    ));
    match mask {
        Some(m) => apply_capability_mask(view, field_name(field), m),
        // Unknown harness → no capability mask; return the un-masked
        // cascade so the UI displays "no value" rather than fabricating
        // one. The `resolved_profile == None` arm in the caller surfaces
        // the unknown-harness state for the modal to render.
        None => view,
    }
}

/// String field name passed to [`apply_capability_mask`]. Mirrors the
/// mask's discriminator so the mask can pick the right gate.
fn field_name(field: HarnessConfigField) -> &'static str {
    match field {
        HarnessConfigField::Model => "model",
        HarnessConfigField::Effort => "effort",
    }
}

#[cfg(test)]
mod resolved_view_tests {
    //! Cascade pin tests for [`get_resolved_harness_view`] (issue #1656).
    //!
    //! The spawn pipeline (`agent::capabilities::resolve_agent_config`) and
    //! the IPC resolver view share the same cascade helper
    //! (`preferences::resolver::resolve_field`); the helper-level cascade
    //! tests in `agent::capabilities::tests` already gate both call sites.
    //! The tests below pin the IPC-specific shape:
    //!
    //! * `mesh_override > application` precedence (issue #1151 layer 2).
    //! * Capability mask drops unsupported fields on the resolved value.
    //! * Unknown harness id returns `resolved_profile = None` rather than
    //!   silently falling back through the resolver (issue #1148 AC #5).
    //! * Empty input is rejected (matches the validator pattern).

    use super::*;
    use crate::agent::capabilities::EffortControlKind;
    use crate::preferences::resolver::{
        apply_capability_mask as cascade_apply_capability_mask, field_inputs as cascade_field_inputs,
    };

    #[test]
    fn application_default_wins_when_no_mesh_override() {
        let view = ResolvedCascadeView::for_field(cascade_field_inputs(
            None, None, None, Some("opus-4"),
        ));
        assert_eq!(view.resolved.as_deref(), Some("opus-4"));
        assert_eq!(view.layers.application.as_deref(), Some("opus-4"));
    }

    #[test]
    fn mesh_override_beats_application_default() {
        // The IPC builder stitches mesh_override above application in
        // `build_cascade_view`; pin that ordering here so a future refactor
        // can't silently drop layer-2 precedence.
        let view = ResolvedCascadeView::for_field(cascade_field_inputs(
            None,
            Some("claude-mesh-override"),
            None,
            Some("opus-app-default"),
        ));
        assert_eq!(view.resolved.as_deref(), Some("claude-mesh-override"));
        assert_eq!(
            view.layers.mesh_override.as_deref(),
            Some("claude-mesh-override")
        );
        assert_eq!(view.layers.application.as_deref(), Some("opus-app-default"));
    }

    #[test]
    fn capability_mask_drops_unsupported_model_on_resolved_value() {
        // Mirror of `capability_mask_drops_model_when_unsupported` from
        // `preferences::resolver::cascade::tests` — the same helper backs
        // both the spawn path and the IPC, so this is a redundant-but-
        // useful pin at the IPC layer.
        let view = ResolvedCascadeView::for_field(cascade_field_inputs(
            None, None, None, Some("opus-4"),
        ));
        let caps = CapabilityMaskForResolver {
            supports_model_override: false,
            effort_control: EffortControlKind::None,
        };
        let masked = cascade_apply_capability_mask(view, "model", &caps);
        assert_eq!(masked.resolved, None);
        // The un-masked layer breakdown is preserved so the UI can still
        // render "configured but not applied" hints.
        assert_eq!(masked.layers.application.as_deref(), Some("opus-4"));
    }

    #[test]
    fn unknown_harness_id_is_handled_in_resolver_layer() {
        // `resolved_harness_profile` returns `None` for unknown ids
        // (mirrors `is_known_harness_id == false`); pin the contract.
        let profile = preferences::resolved_harness_profile("__definitely-not-a-real-id__");
        assert!(
            profile.is_none(),
            "unknown harness id must not fabricate a profile"
        );
        let caps = preferences::harness_capabilities_for("__definitely-not-a-real-id__");
        assert!(
            caps.is_none(),
            "unknown harness id must not surface a capability descriptor"
        );
    }

    #[test]
    fn unknown_harness_id_returns_none_executor_via_ipc() {
        // End-to-end pin: invoking the IPC with an unknown harness id
        // returns a view with `resolved_profile: None`,
        // `resolved_executor: None`, and `capabilities: None`. The IPC
        // must NOT fabricate an executor from `Provider::from_db_str`'s
        // Anthropic default — the UI relies on these `None`s to render
        // "unknown harness" instead of pretending to have one.
        let tmp = std::env::temp_dir().join(format!(
            "buildmesh-resolved-view-unknown-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
        ));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        crate::preferences::init_for_tests(tmp);
        let view = get_resolved_harness_view(
            "__definitely-not-a-real-id__".to_string(),
            None,
        )
        .expect("IPC must succeed even for an unknown harness id");
        assert!(
            view.resolved_profile.is_none(),
            "unknown harness id must not fabricate a profile"
        );
        assert!(
            view.capabilities.is_none(),
            "unknown harness id must not surface capabilities"
        );
        assert_eq!(
            view.resolved_executor, None,
            "unknown harness id must not fabricate an executor (would mislead the UI)"
        );
        crate::preferences::reset_for_tests();
    }

    #[test]
    fn empty_harness_id_is_rejected_at_the_command_boundary() {
        // Pin the validation contract: an empty / whitespace harness id is
        // rejected with a clear error rather than silently resolving to
        // the Anthropic fallback.
        let result = get_resolved_harness_view("   ".to_string(), None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn known_harness_id_resolves_to_known_profile() {
        // Round-trip a built-in id through the resolver and confirm the
        // shape of the returned `ResolvedHarnessView` — executor matches
        // the adapter id, capabilities carry `supports_model_override`
        // (the Anthropic harness supports it).
        //
        // `resolved_harness_profile` does binary detection on the host
        // (issue #535); on a test machine without the harness binaries
        // installed the profile may be `None`. We don't pin that here —
        // the executor + capability descriptors are what the UI consumes,
        // and both come from the pure resolver (no host I/O).
        //
        // The preferences module must be initialised before any resolver
        // call reads from disk — `init_for_tests` sets up a tempdir so the
        // test never touches the user's real `preferences.json`.
        let tmp = std::env::temp_dir().join(format!(
            "buildmesh-resolved-view-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
        ));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        crate::preferences::init_for_tests(tmp);
        let view = get_resolved_harness_view("claude".to_string(), None)
            .expect("'claude' is a known built-in harness id");
        assert_eq!(view.harness_id, "claude");
        // `resolved_executor` is `Some("anthropic")` for the 'claude'
        // built-in — the harness is known so the resolver resolves
        // through the canonical Anthropic adapter. (`Option<String>`,
        // not String, so unknown harnesses surface `None` rather than
        // fabricating an executor — see the IPC command's docstring.)
        assert_eq!(view.resolved_executor.as_deref(), Some("anthropic"));
        let caps = view.capabilities.expect("built-in caps must be present");
        assert!(
            caps.supports_model_override,
            "anthropic harness must report model override support"
        );
        crate::preferences::reset_for_tests();
    }

    /// Issue #1816: the app-wide Reviewer provider is the inherit-path
    /// reviewer for runs without a per-run override, so the settings
    /// command holds the same attention-compatibility gate as the Start
    /// Review picker. Ineligible harnesses are refused (and leave the
    /// stored value untouched); eligible ones persist; blank clears.
    #[test]
    fn app_reviewer_provider_rejects_harness_without_turn_signal() {
        let tmp = std::env::temp_dir().join(format!(
            "buildmesh-app-reviewer-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
        ));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        crate::preferences::init_for_tests(tmp);
        // Eligible harness persists.
        set_app_reviewer_provider(Some("codex".to_string())).expect("codex can yield a turn");
        assert_eq!(
            crate::preferences::reviewer_provider().as_deref(),
            Some("codex")
        );
        // Ineligible harnesses are refused with the harness-named reason
        // and the stored value is untouched.
        for picked in ["freebuff", "terminal", "dsh:minimax"] {
            let err = set_app_reviewer_provider(Some(picked.to_string())).unwrap_err();
            assert!(
                err.contains("reviewer provider"),
                "{picked:?} must be refused, got {err:?}"
            );
        }
        assert_eq!(
            crate::preferences::reviewer_provider().as_deref(),
            Some("codex"),
            "refused writes must not clobber the stored value"
        );
        // Blank clears back to the source-agent fallback.
        set_app_reviewer_provider(Some("   ".to_string())).expect("blank clears");
        assert_eq!(crate::preferences::reviewer_provider(), None);
        crate::preferences::reset_for_tests();
    }
}
