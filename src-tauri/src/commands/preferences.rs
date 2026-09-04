//! Tauri commands for buildmesh-wide preferences.
//!
//! See `crate::preferences` for the persistence layer.
//!
//! Pure sync — each command is a single `preferences::load` /
//! `save` round-trip + optional `app.emit`. They run on Tauri's IPC
//! worker, NOT the bounded tokio pool. Issue #1380 review point 4.

use crate::preferences::{
    self, AppPreferences, HarnessConfigValue, ModelTiers, PairingVerification, ProviderAccount,
    ProviderPairing,
};
use tauri::{command, AppHandle, Emitter};

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
    if let Some(ref dir) = cleaned {
        if crate::env::is_absolute_worktree_path(dir) {
            return Err(format!(
                "worktree directory '{}' is absolute, but the application default spans meshes in both environments (native/Windows versus WSL) — \
                 use a relative path like 'worktrees' resolved from each mesh root, or set this absolute path as a per-Mesh override in Project Settings → Worktrees",
                dir
            ));
        }
    }
    let mut prefs = preferences::load()?;
    prefs.worktree_directory = cleaned.clone();
    preferences::save(prefs)?;
    // Rebuild idle inventory for inheriting meshes (those with no
    // per-Mesh override) on a background thread — `git worktree remove`
    // + `git worktree add` are blocking syscalls that must not park the
    // IPC worker (same pattern as `update_mesh_pool_size`).
    let app_clone = app.clone();
    std::thread::spawn(move || {
        crate::services::warm_pool::rebuild_pools_for_worktree_dir_change(
            &app_clone,
            None,
        );
    });
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
