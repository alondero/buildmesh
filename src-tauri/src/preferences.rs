//! Buildmesh-wide preferences, persisted as JSON in `app_data_dir/preferences.json`.
//!
//! This is the **application-level** layer of configuration, distinct from:
//!   - `meshes` DB columns — per-mesh overrides (e.g. `mesh.default_provider`)
//!   - `.claude/settings.json` — per-mesh Claude Code config (worktree.baseRef etc.)
//!
//! Precedence is applied at the call site: per-mesh value → app pref → hardcoded
//! fallback (`anthropic` for providers).

use crate::models::Provider;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use ts_rs::TS;

/// A user-selectable **Agent Harness** profile (ADR-0014 / PRD #534).
///
/// The harness (the executor binary recipe) is being split out from the
/// **Model Provider** (credentials/endpoint). This struct is the first
/// concrete shape of that split: `id` is the value stored in the DB
/// `provider` column and on the wire, `name` is the menu label, and
/// `harness` names the backing executor — for now a legacy [`Provider`]
/// id, resolved by [`resolve_harness_provider`]. Later slices will give
/// `harness` richer meaning (its own binary recipe) and retire the
/// duplicated legacy [`Provider`] enum.
///
/// Generated to src/types/generated/HarnessProfile.ts (issue #535).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "HarnessProfile.ts")]
pub struct HarnessProfile {
    /// Stable id — stored in `agent_nodes.provider` and sent over the wire.
    pub id: String,
    /// Menu label shown in the launch dropdown.
    pub name: String,
    /// Backing executor; for this slice a legacy [`Provider`] id.
    pub harness: String,
}

/// How a [`ProviderAccount`] is billed — drives how usage is rendered (issue #537).
///
/// `Plan` accounts (seat/subscription) show utilization percentage bars; `PayAsYouGo`
/// accounts (API credits) show a cash [`crate::services::usage::BillingBalance`] card.
///
/// Generated to src/types/generated/BillingMode.ts (issue #537).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "BillingMode.ts")]
pub enum BillingMode {
    /// Seat / subscription plan — utilization is a percentage of a quota.
    #[default]
    Plan,
    /// Pay-as-you-go API credits — utilization is a remaining cash balance.
    PayAsYouGo,
}

/// Per-tier Claude model overrides for a Claude-compatible provider account
/// (issue #567 — restores the Claude Code model-tier capability).
///
/// Claude Code asks its backend for several model *aliases*: a primary, a cheap
/// "small/fast" model for background tasks (titles, etc.), and the Sonnet / Opus
/// / Haiku defaults. Each field here, when set, maps to the matching env var the
/// `claude` binary reads (see [`provider_account_env`]):
///   - `default`    → `ANTHROPIC_MODEL`
///   - `small_fast` → `ANTHROPIC_SMALL_FAST_MODEL`
///   - `sonnet`     → `ANTHROPIC_DEFAULT_SONNET_MODEL`
///   - `opus`       → `ANTHROPIC_DEFAULT_OPUS_MODEL`
///   - `fable`      → `ANTHROPIC_DEFAULT_FABLE_MODEL`
///   - `haiku`      → `ANTHROPIC_DEFAULT_HAIKU_MODEL`
///
/// Only meaningful for Claude-compatible providers (MiniMax, custom endpoints) — it's
/// irrelevant for Antigravity / Codex, and for Kimi Code post-#918 which is
/// self-auth and stores credentials in `~/.kimi/config.toml`. The UI shows
/// these fields only for Claude-compatible accounts. The built-in MiniMax
/// account ships pre-filled with the values the absorbed `cwrap` launcher
/// used (byte-for-byte parity).
///
/// Generated to src/types/generated/ModelTiers.ts (issue #567).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "ModelTiers.ts")]
pub struct ModelTiers {
    /// Primary model — `ANTHROPIC_MODEL`.
    #[serde(default)]
    pub default: Option<String>,
    /// Cheap background model — `ANTHROPIC_SMALL_FAST_MODEL`.
    #[serde(default)]
    pub small_fast: Option<String>,
    /// `ANTHROPIC_DEFAULT_SONNET_MODEL`.
    #[serde(default)]
    pub sonnet: Option<String>,
    /// `ANTHROPIC_DEFAULT_OPUS_MODEL`.
    #[serde(default)]
    pub opus: Option<String>,
    /// `ANTHROPIC_DEFAULT_FABLE_MODEL` — the Claude 5 Fable tier. Unset falls
    /// back to the `opus` tier at env-build time (Fable sits above Opus, so a
    /// provider's Opus-grade model is the closest configured substitute).
    #[serde(default)]
    pub fable: Option<String>,
    /// `ANTHROPIC_DEFAULT_HAIKU_MODEL`.
    #[serde(default)]
    pub haiku: Option<String>,
}

impl ModelTiers {
    /// True when no tier is set — used to fall back to the legacy flat `models`
    /// list for back-compat (issue #567).
    fn is_empty(&self) -> bool {
        let blank = |s: &Option<String>| s.as_deref().is_none_or(|v| v.is_empty());
        blank(&self.default)
            && blank(&self.small_fast)
            && blank(&self.sonnet)
            && blank(&self.opus)
            && blank(&self.fable)
            && blank(&self.haiku)
    }
}

/// A **Compatible API surface** — the wire protocol a harness expects of its
/// backend (CONTEXT.md, ADR-0016 §4, issue #576).
///
/// A harness speaks exactly one surface (Claude Code → `Anthropic`, Codex →
/// `OpenAI`); a **Model Provider** may expose more than one, so the same
/// provider can be proxied through harnesses on different surfaces (MiniMax via
/// Claude Code over its Anthropic-compatible endpoint *and* via Codex over its
/// OpenAI-compatible endpoint). The surface is what drives which env vars
/// [`surface_env`] emits at spawn (`ANTHROPIC_*` vs `OPENAI_*`).
///
/// Generated to src/types/generated/ApiSurface.ts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "ApiSurface.ts")]
pub enum ApiSurface {
    /// Anthropic-compatible wire protocol — the `claude` binary's backend
    /// (`ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_*_MODEL`).
    Anthropic,
    /// OpenAI-compatible wire protocol — the `codex` binary's backend
    /// (`OPENAI_BASE_URL` / `OPENAI_API_KEY` / `OPENAI_MODEL`).
    #[serde(rename = "openai")]
    OpenAI,
}

/// One harness's ordered **Proxied Provider** children (issue #577).
///
/// Mirrors the per-harness shape of the Spawn Menu — the wire-side list is
/// just `provider_id`s in top-to-bottom order, scoped to the named harness.
/// `Terminal` is excluded (it's a native harness with no proxied children).
/// A harness without an entry keeps its natural (insertion) order; a
/// newly-attached provider appends to the end on the next refresh.
///
/// Generated to src/types/generated/ProxiedProviderOrder.ts (issue #577).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "ProxiedProviderOrder.ts")]
pub struct ProxiedProviderOrder {
    /// Harness profile id this child order applies to (e.g. `"claude"`).
    pub harness_id: String,
    /// `provider_id`s in user-chosen top-to-bottom order.
    pub provider_ids: Vec<String>,
}

/// A **Proxied Provider** pairing — one harness×provider attachment (ADR-0016
/// §4, issue #576).
///
/// This is the *per-pairing* half of the config split: the **API key is global**
/// to the [`ProviderAccount`] (entered once on the Providers page, reused across
/// pairings), while the **Compatible API surface + endpoint URL + model-tier
/// remap are per pairing** and live here. One [`ProviderAccount`] can have
/// several `ProviderPairing`s — e.g. MiniMax paired with both Claude Code
/// (`surface = Anthropic`) and Codex (`surface = OpenAI`), each with its own
/// `base_url` and `model_tiers`.
///
/// Only **user-added** pairings are persisted in
/// [`AppPreferences::provider_pairings`]; the default Anthropic pairing for a
/// keyed first-class/custom account is *derived* at read time (see
/// [`effective_pairings`]), so existing MiniMax-via-Claude setups keep working
/// with no migration. A stored pairing for the same `(harness_id, provider_id)`
/// overrides the derived default.
///
/// `model_tiers` carries the per-tier Claude alias map for an `Anthropic`-surface
/// pairing; for an `OpenAI`-surface pairing only `model_tiers.default` is
/// meaningful (Codex takes a single model), mapped to `OPENAI_MODEL`.
///
/// Generated to src/types/generated/ProviderPairing.ts (issue #576).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "ProviderPairing.ts")]
pub struct ProviderPairing {
    /// Harness profile id this provider is proxied through (e.g. `"claude"`,
    /// `"codex"`). Matches a [`HarnessProfile::id`].
    pub harness_id: String,
    /// [`ProviderAccount::id`] supplying the global credential.
    pub provider_id: String,
    /// The wire protocol this pairing speaks — must be one the harness speaks.
    pub surface: ApiSurface,
    /// Endpoint base URL for this surface. For a first-class provider this is
    /// published (see [`first_class_surfaces`]); for a Generic provider it's
    /// declared once at creation. Injected as `ANTHROPIC_BASE_URL` /
    /// `OPENAI_BASE_URL` by surface.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Per-tier model remap for this pairing (see struct doc for the surface
    /// difference).
    #[serde(default)]
    pub model_tiers: ModelTiers,
}

/// A first-class provider's published endpoint for one **Compatible API
/// surface** — the surface→URL(+default model map) the attach flow reads so a
/// pairing only has to *name* the surface (ADR-0016 §4). Not persisted; returned
/// by [`first_class_surfaces`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceEndpoint {
    pub surface: ApiSurface,
    pub base_url: String,
    pub model_tiers: ModelTiers,
}

/// A user-configurable **Model Provider account** (ADR-0014 / PRD #534).
///
/// This is the credentials/endpoint half of the harness↔provider split: a
/// [`HarnessProfile`] names the executor, a `ProviderAccount` names *where* and
/// *with what key* it talks to a model service. Built-in accounts (anthropic,
/// codex, agy, minimax) are code-defined in [`default_provider_accounts`] and
/// merged over by `id`, exactly like harness profiles; users may add custom
/// Claude-compatible accounts (e.g. "DeepSeek via Claude Code") with their own
/// base URL and key.
///
/// `base_url`/`api_key`/`model_tiers` are injected at spawn time by
/// [`resolve_provider_env`] for a Claude-compatible profile (#538/#567).
///
/// Generated to src/types/generated/ProviderAccount.ts (issue #537).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "ProviderAccount.ts")]
pub struct ProviderAccount {
    /// Stable id — "anthropic" | "minimax" | a custom id like "deepseek".
    pub id: String,
    /// Display label shown in the Accounts panel.
    pub name: String,
    /// When false, usage is not polled and the account is not offered for spawning.
    pub enabled: bool,
    /// Billing model — chooses percentage bars vs cash-balance card.
    pub billing_mode: BillingMode,
    /// Whether this is a Claude-compatible provider configured with its own
    /// key/endpoint (MiniMax, custom Claude-compatible accounts). When true the
    /// UI shows credential + model-tier fields, and a keyed+enabled account
    /// appears in the spawn menu as a Claude-Code-backed provider (#568).
    /// False for self-authenticating
    /// built-ins (anthropic/codex/agy), which hold no creds in Buildmesh.
    ///
    /// **Derived from `id` on read** ([`merge_provider_accounts`] normalizes it) —
    /// the stored value is not authoritative, so an older `preferences.json` that
    /// predates this field still gates correctly.
    #[serde(default)]
    pub claude_compatible: bool,
    /// API key for usage fetching / custom endpoints. Stored plaintext in
    /// preferences.json (matches the legacy `minimax_api_key` convention).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Custom Claude-compatible base URL injected as `ANTHROPIC_BASE_URL` at spawn.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Per-tier Claude model overrides injected as `ANTHROPIC_*` model vars at
    /// spawn (issue #567). Supersedes the flat `models` list below.
    #[serde(default)]
    pub model_tiers: ModelTiers,
    /// **Deprecated** by `model_tiers` (#567). Retained for back-compat reads: when
    /// `model_tiers` is empty, [`provider_account_env`] still derives the primary
    /// (`models[0]`) and small/fast (`models[1]`) from this list.
    #[serde(default)]
    pub models: Vec<String>,
}

/// User-editable, persisted preferences applied across all meshes.
///
/// Generated to src/types/generated/AppPreferences.ts (issue #404).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "AppPreferences.ts")]
pub struct AppPreferences {
    /// Buildmesh-wide default provider id (e.g. "anthropic", "minimax").
    /// `None` means "no app-wide override — use the hardcoded fallback".
    #[serde(default)]
    pub default_provider: Option<String>,
    /// MiniMax API key for usage fetching. **Deprecated** by `provider_accounts`
    /// (#537) — kept so existing preferences.json files still load and the stored
    /// key survives via [`minimax_api_key_resolved`]'s read-through fallback.
    #[serde(default)]
    pub minimax_api_key: Option<String>,
    /// Google Cloud project for Antigravity/Gemini quota API. Defaults to "cloudshell-gca".
    #[serde(default)]
    pub google_cloud_project: Option<String>,
    /// User customizations to the code-defined default harness profiles.
    /// Merged over [`default_harness_profiles`] by `id` (user wins) in
    /// [`harness_profiles`]; the defaults are always present even when this
    /// is empty, so a built-in like Terminal can never go missing.
    #[serde(default)]
    pub harness_profiles: Vec<HarnessProfile>,
    /// User customizations to the code-defined default model-provider accounts.
    /// Merged over [`default_provider_accounts`] by `id` (user wins) in
    /// [`provider_accounts`]; the built-ins are always present even when this is
    /// empty. Custom (non-built-in) entries are appended (issue #537).
    #[serde(default)]
    pub provider_accounts: Vec<ProviderAccount>,
    /// User-chosen order of the spawn-menu harness rows, as a list of row ids
    /// (issue #573 / ADR-0016). `Terminal` is excluded — it's always forced to
    /// the bottom by `commands::agent::order_providers`. A row whose id isn't
    /// listed (a newly-detected harness) appends after the listed ones; an
    /// uninstalled harness keeps its saved slot here until it reappears.
    #[serde(default)]
    pub harness_order: Vec<String>,
    /// User-added **Proxied Provider** pairings (ADR-0016 §4, issue #576). Each
    /// entry attaches a [`ProviderAccount`] to a harness over a chosen
    /// **Compatible API surface** with its own endpoint URL + model-tier remap.
    /// The default Anthropic pairing for a keyed account is *derived* (see
    /// [`effective_pairings`]) and not stored here, so this list holds only the
    /// extra surfaces/harnesses the user explicitly attached (e.g. MiniMax via
    /// Codex). An additive field — an older `preferences.json` without it loads
    /// with an empty list.
    #[serde(default)]
    pub provider_pairings: Vec<ProviderPairing>,
    /// User-chosen order of the **Proxied Provider** children under each
    /// harness (issue #577). One [`ProxiedProviderOrder`] per harness the
    /// user has reordered; a harness without an entry keeps its natural
    /// (insertion) order — the per-harness bucket in the Spawn Menu
    /// preserves the order the backend emits it in. The drag-to-reorder UI
    /// lives on the harness-config page (Settings → Harnesses); cross-harness
    /// drag is disallowed (each `HarnessCard` is its own `DndContext`, so a
    /// draggable can only be dropped on a sibling in the same card). An
    /// additive field — an older `preferences.json` without it loads with an
    /// empty list.
    #[serde(default)]
    pub proxied_provider_order: Vec<ProxiedProviderOrder>,
    /// The backend that summaries PTY output into a slug (issue #824).
    /// Distinct from the node's own provider — auto-rename runs frequently
    /// and often sits well below the model's intelligence threshold, so the
    /// user opts in via Settings rather than inheriting whatever expensive
    /// tier the spawned node is on. `None` (the default) means auto-naming
    /// is disabled — nodes keep their `adjective-adjective-noun` random
    /// slugs until the user explicitly configures this. `Some(spawn_id)`
    /// forwards through [`crate::preferences::resolve_provider_env`] so
    /// whatever provider the user picks resolves through the same
    /// configured-account pipeline that node spawns use. Built-in
    /// Anthropic is special-cased in `session_naming` to pin a cheap
    /// haiku tier instead of the user's main subscription default.
    #[serde(default)]
    pub naming_provider: Option<String>,
    /// Buildmesh-wide cap on **concurrently active autopilot nodes across all
    /// meshes** — the global "pool" the per-mesh `autopilot_concurrency_limit`
    /// slots draw from. Per-mesh limits alone can't protect the machine: ten
    /// meshes × 2 nodes each is still twenty concurrent agents. `None` (the
    /// default) means no global cap — per-mesh limits alone apply, exactly the
    /// pre-existing behaviour. `Some(0)` pauses all new autopilot spawns
    /// without touching any mesh's enabled flag. Enforced by the poller
    /// (`services::autopilot::run_poll_pass`), never by killing running nodes
    /// — lowering the cap below the current active count just stops new
    /// spawns until enough slots free up.
    #[serde(default)]
    pub autopilot_pool_size: Option<u32>,
}

/// Set during Tauri `setup()` so callers don't need an `AppHandle`.
static APP_DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// In-process cache, refreshed on every write. Reads consult the file only if
/// the cache is empty (first read).
static CACHE: Mutex<Option<AppPreferences>> = Mutex::new(None);

pub fn init(app_data_dir: PathBuf) {
    *APP_DATA_DIR.lock().unwrap_or_else(|p| p.into_inner()) = Some(app_data_dir);
}

#[cfg(test)]
pub(crate) fn init_for_tests(app_data_dir: PathBuf) {
    init(app_data_dir);
    *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    *APP_DATA_DIR.lock().unwrap_or_else(|p| p.into_inner()) = None;
    *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// The app-data directory `init` was wired to, for sibling config files
/// that live next to `preferences.json` (e.g. Autopilot's `finish.md`,
/// issue #484). `None` before `init` runs (tests without a Tauri setup).
pub(crate) fn app_data_dir() -> Option<PathBuf> {
    APP_DATA_DIR
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

fn preferences_path() -> Result<PathBuf, String> {
    app_data_dir()
        .map(|d| d.join("preferences.json"))
        .ok_or_else(|| "preferences module not initialized".to_string())
}

fn read_from_disk() -> Result<AppPreferences, String> {
    let path = preferences_path()?;
    if !path.exists() {
        return Ok(AppPreferences::default());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read preferences.json: {}", e))?;
    // Tolerate malformed/empty files — preferences are non-critical.
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn write_to_disk(prefs: &AppPreferences) -> Result<(), String> {
    let path = preferences_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create app data dir: {}", e))?;
    }
    let json = serde_json::to_string_pretty(prefs)
        .map_err(|e| format!("failed to serialize preferences: {}", e))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("failed to write preferences.json: {}", e))
}

/// Load preferences, populating the in-process cache on first call.
pub fn load() -> Result<AppPreferences, String> {
    let mut guard = CACHE.lock().unwrap();
    if let Some(cached) = guard.as_ref() {
        return Ok(cached.clone());
    }
    let prefs = read_from_disk()?;
    *guard = Some(prefs.clone());
    Ok(prefs)
}

/// Persist preferences to disk and refresh the cache.
pub fn save(prefs: AppPreferences) -> Result<(), String> {
    write_to_disk(&prefs)?;
    let mut guard = CACHE.lock().unwrap();
    *guard = Some(prefs);
    Ok(())
}

/// Convenience: returns the app-wide default provider id, if any.
/// Empty strings are treated as `None` to match how the per-mesh column
/// is normalized elsewhere (see `commands::mesh::get_default_provider`).
///
/// A load failure (e.g. preferences module not initialised, or unreadable
/// file) is logged once and treated as "no override". We don't propagate
/// the error because the precedence chain has a hardcoded fallback — but
/// without the warn! a misconfigured environment would silently ignore the
/// user's setting with no trace.
pub fn default_provider() -> Option<String> {
    match load() {
        Ok(prefs) => prefs.default_provider.filter(|s| !s.is_empty()),
        Err(e) => {
            tracing::warn!("preferences::default_provider load failed, falling back: {}", e);
            None
        }
    }
}

/// The user-configured backend the session-naming helper uses to summarise
/// PTY output into a slug (issue #824). `None` means "auto-naming is off" —
/// the session_naming module short-circuits and nodes retain their random
/// `adjective-adjective-noun` slug. An empty string is normalised to `None`
/// here so a save with `""` from the frontend acts the same as a clear.
///
/// Distinct from `default_provider`: the naming helper runs frequently on
/// content that is well below the front-line model's intelligence
/// threshold, so the user explicitly opts in via Settings rather than
/// inheriting whatever provider a spawned node happens to be on (which can
/// be an expensive tier like Opus with xhigh effort).
///
/// The naming helper treats this as a *spawn-option id* (e.g. `"minimax"`,
/// `"claude:minimax"`, `"claude:openrouter"`) and resolves it through
/// [`crate::preferences::resolve_provider_env`] the same way node spawns
/// do — so a user who already configured a Provider Account can reuse it
/// here for free. Built-in Anthropic (`"anthropic"`) is special-cased
/// inside `session_naming` to pin a cheap haiku tier; the historical
/// `minimax_backend_env()` side-channel is no longer the implicit
/// default.
pub fn naming_provider() -> Option<String> {
    match load() {
        Ok(prefs) => prefs.naming_provider.filter(|s| !s.is_empty()),
        Err(e) => {
            tracing::warn!(
                "preferences::naming_provider load failed, falling back: {}",
                e
            );
            None
        }
    }
}

/// The global autopilot pool size — the app-wide cap on concurrently active
/// autopilot nodes across every mesh (see [`AppPreferences::autopilot_pool_size`]).
/// `None` means "no global cap" — the per-mesh `autopilot_concurrency_limit`
/// values are the only gate, which was the behaviour before this setting
/// existed. A load failure is logged and treated as "no cap" for the same
/// reason as [`default_provider`]: the poller must keep working even when
/// preferences are unreadable.
pub fn autopilot_pool_size() -> Option<u32> {
    match load() {
        Ok(prefs) => prefs.autopilot_pool_size,
        Err(e) => {
            tracing::warn!(
                "preferences::autopilot_pool_size load failed, treating as uncapped: {}",
                e
            );
            None
        }
    }
}

/// One-shot normalization of legacy bare `default_provider` values to
/// the post-#575 composite form (`minimax` → `claude:minimax`).
///
/// The v19 Spawn Option composite-id migration rewrote
/// `agent_nodes.provider` but never touched `preferences.json::default_provider`
/// (issue #575 / ADR-0016 §6). A user whose app-wide default was set
/// before #575 lands keeps the legacy bare form in their preferences.json
/// — and the bare form routes through `resolve_provider_env` to the keyed
/// **account** instead of the post-#575 proxied pairing, which silently
/// spawns Claude-CLI sessions against the wrong endpoint.
///
/// `kimi` is intentionally absent post-#918: bare `"kimi"` now resolves
/// to the native Kimi Code harness via `Provider::from_db_str`, so a
/// legacy bare `kimi` preference reads through to `Provider::Kimi`
/// directly without a rewrite. Rewriting to `claude:kimi` would put the
/// user in a state with no Proxied row in the spawn menu (Kimi Code is
/// self_auth, not Claude-compatible).
///
/// Called from `lib.rs::setup` immediately after `preferences::init`,
/// so this can `load()` against the real on-disk file. Idempotent —
/// already-composite values, native harness ids, and `None` are left
/// alone. On a no-op (the common case after the first launch) this is a
/// single cached read plus an equality check.
pub(crate) fn ensure_default_provider_normalized() -> Result<(), String> {
    let mut prefs = load()?;
    let normalized = prefs
        .default_provider
        .as_deref()
        .and_then(normalize_legacy_default_provider);
    if let Some(new_value) = normalized {
        tracing::info!(
            "default_provider normalized: {} → {}",
            prefs.default_provider.as_deref().unwrap_or(""),
            new_value
        );
        prefs.default_provider = Some(new_value.to_string());
        save(prefs)?;
    }
    Ok(())
}

/// Pure translation table for [`ensure_default_provider_normalized`].
/// Kept separate so it's the single seam to extend if a future legacy
/// bare id lands (every addition is one match arm + one unit test).
fn normalize_legacy_default_provider(bare: &str) -> Option<&'static str> {
    match bare {
        "minimax" => Some("claude:minimax"),
        // `kimi` removed post-#918 (Kimi Code is a native harness, not a
        // Claude-compatible Proxied row). See the fn docstring.
        _ => None,
    }
}

/// The code-defined harness profiles that always exist regardless of what
/// `preferences.json` stores. Terminal is the first (and, this slice, only)
/// one — a plain-shell harness that injects no provider env, which is why
/// it's the tracer-bullet for the dynamic profile machinery (issue #535).
pub fn default_harness_profiles() -> Vec<HarnessProfile> {
    vec![HarnessProfile {
        id: "terminal".to_string(),
        name: "Terminal".to_string(),
        harness: "terminal".to_string(),
    }]
}

/// The effective harness profile list: the code-defined defaults with the
/// user's stored `harness_profiles` merged over them by `id`. A stored
/// profile whose `id` matches a default replaces it (user wins); a stored
/// profile with a new `id` is appended. Defaults are always present, so a
/// built-in like Terminal can never be removed by an empty or partial
/// `preferences.json`.
pub fn harness_profiles() -> Vec<HarnessProfile> {
    let mut profiles = default_harness_profiles();
    let stored = match load() {
        Ok(prefs) => prefs.harness_profiles,
        Err(e) => {
            tracing::warn!("preferences::harness_profiles load failed, using defaults: {}", e);
            Vec::new()
        }
    };
    for profile in stored {
        if let Some(existing) = profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        } else {
            profiles.push(profile);
        }
    }
    profiles
}

/// The user's stored spawn-menu harness order — a list of row ids applied by
/// `commands::agent::order_providers` (issue #573). Empty when never set, in
/// which case the menu keeps its natural derivation order (Terminal still last).
/// A load failure is logged and treated as "no stored order".
pub fn harness_order() -> Vec<String> {
    match load() {
        Ok(prefs) => prefs.harness_order,
        Err(e) => {
            tracing::warn!("preferences::harness_order load failed, using natural order: {}", e);
            Vec::new()
        }
    }
}

/// Persist the spawn-menu harness order (issue #573). `Terminal` is filtered out
/// before storing — it's always forced last by the ordering logic, so keeping it
/// out of the stored list avoids a redundant (and potentially misleading) slot.
///
/// `order` only covers the harnesses installed *right now* (the UI can't render a
/// row for an uninstalled one), so a plain overwrite would silently evict the
/// saved slot of any harness that happens to be uninstalled while the user
/// reorders — breaking the "uninstalled keeps its slot" promise. We instead merge
/// the new order into the stored one via `merge_harness_order`, which keeps each
/// dormant id pinned at its stored slot. Duplicate ids are dropped (first wins).
pub fn set_harness_order(order: Vec<String>) -> Result<(), String> {
    let mut prefs = load()?;
    let incoming = dedupe_keeping_first(order.into_iter().filter(|id| id != "terminal"));
    prefs.harness_order = merge_harness_order(&prefs.harness_order, incoming);
    save(prefs)
}

/// Dedupe an id sequence keeping the first occurrence of each id, preserving
/// order. A malformed caller (or hand-edited prefs) sending `[claude, claude]`
/// would otherwise persist a duplicate that shifts every later harness's index.
fn dedupe_keeping_first(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.filter(|id| seen.insert(id.clone())).collect()
}

/// Merge a user-supplied harness order (which only covers currently-installed
/// harnesses) into the stored order, preserving the saved slot of every *dormant*
/// id — a harness uninstalled right now and so absent from `incoming` (issue
/// #573). Each dormant id holds its stored index; the present ids refill the
/// remaining slots in the user's new order; any brand-new id in `incoming` (never
/// stored before) appends at the end. `incoming` is assumed already deduped and
/// Terminal-free.
fn merge_harness_order(stored: &[String], incoming: Vec<String>) -> Vec<String> {
    let is_dormant = |id: &String| !incoming.iter().any(|x| x == id);
    // Reserve each stored slot: dormant ids keep their place, present ids leave a
    // `None` gap to be refilled by `incoming` below.
    let mut slots: Vec<Option<String>> = stored
        .iter()
        .map(|id| is_dormant(id).then(|| id.clone()))
        .collect();
    let mut gaps = slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.is_none())
        .map(|(i, _)| i)
        .collect::<Vec<_>>()
        .into_iter();
    for id in incoming {
        match gaps.next() {
            Some(i) => slots[i] = Some(id),
            None => slots.push(Some(id)), // brand-new id beyond the stored slots
        }
    }
    slots.into_iter().flatten().collect()
}

/// Merge startup-detected harness profiles into stored preferences (issue #536).
///
/// Additive and idempotent: a detected profile whose `id` is not already stored
/// is appended; existing entries are never overwritten or removed. So a profile
/// the user renamed survives, and re-running the scan every launch (the chosen
/// cadence) only ever *adds* newly-installed tools. Returns the number of
/// profiles added; disk is written only when that is non-zero.
///
/// Detected ids never collide with the code-defined defaults (Terminal), which
/// live outside the stored `harness_profiles` list and are re-merged on read by
/// [`harness_profiles`].
pub fn merge_detected_profiles(detected: Vec<HarnessProfile>) -> Result<usize, String> {
    let mut prefs = load()?;
    let before = prefs.harness_profiles.len();
    for profile in detected {
        if !prefs.harness_profiles.iter().any(|p| p.id == profile.id) {
            prefs.harness_profiles.push(profile);
        }
    }
    let added = prefs.harness_profiles.len() - before;
    if added > 0 {
        save(prefs)?;
    }
    Ok(added)
}

/// Resolve a stored `provider`/profile id to the legacy [`Provider`] executor
/// that should actually spawn it. If the id names a harness profile, the
/// profile's `harness` field is parsed; otherwise the id is parsed directly —
/// the "alongside-legacy" path, so existing enum ids (`"anthropic"`, etc.)
/// still resolve without a matching profile. Unknown ids fall through
/// `Provider::from_db_str`'s Anthropic default (preserving prior behaviour).
///
/// **Composite Spawn Option ids** (issue #575 / ADR-0016): a Proxied
/// Provider id has the shape `<harness>:<provider>` (e.g. `claude:minimax`).
/// Only the *harness* part drives the executor choice — the provider part
/// is just a credential key. We split on the first `:` via
/// [`crate::agent::provider::parse_spawn_option_id`] so the legacy bare
/// ids (`"minimax"`, `"kimi"`, custom account ids) still resolve through
/// the same path during the post-#575 migration window, and the post-
/// migration composite ids (`"claude:minimax"`) resolve to the same
/// Anthropic executor as the bare form did.
pub fn resolve_harness_provider(profile_id: &str) -> Provider {
    let (harness_id, _provider_id) =
        crate::agent::provider::parse_spawn_option_id(profile_id);
    match harness_profiles().into_iter().find(|p| p.id == harness_id) {
        Some(profile) => Provider::from_db_str(&profile.harness),
        None => Provider::from_db_str(harness_id),
    }
}

/// A row in the code-defined built-in Model Provider account list (issue
/// #571). The **single source of truth** for which built-ins exist and which
/// are self-authenticating — [`default_provider_accounts`] materialises the
/// full [`ProviderAccount`] from it, and [`is_claude_compatible_id`]
/// classifies an id by looking it up here. Adding a new built-in means adding
/// one row; the classification then follows automatically.
#[derive(Debug, Clone, Copy)]
struct BuiltInProviderAccount {
    id: &'static str,
    name: &'static str,
    /// True for harnesses that authenticate via their own CLI (`~/.claude`,
    /// `~/.codex`, …) and therefore hold no credentials in Buildmesh. False
    /// for Claude-compatible keyed providers (MiniMax, custom) that ship a
    /// base URL + per-tier model map (issue #568). `kimi` is self_auth post-
    /// #918 (Kimi Code CLI handles auth via `~/.kimi/config.toml`).
    self_auth: bool,
}

const BUILTIN_PROVIDER_ACCOUNTS: &[BuiltInProviderAccount] = &[
    BuiltInProviderAccount { id: "anthropic", name: "Anthropic / Claude",   self_auth: true  },
    BuiltInProviderAccount { id: "codex",     name: "OpenAI / Codex",        self_auth: true  },
    BuiltInProviderAccount { id: "agy",       name: "Google / Antigravity",  self_auth: true  },
    BuiltInProviderAccount { id: "grok",      name: "xAI / Grok",           self_auth: true  },
    BuiltInProviderAccount { id: "kimi",      name: "Moonshot / Kimi Code", self_auth: true  },
    BuiltInProviderAccount { id: "opencode",  name: "OpenCode",             self_auth: true  },
    BuiltInProviderAccount { id: "minimax",   name: "MiniMax",               self_auth: false },
    BuiltInProviderAccount { id: "openrouter",name: "OpenRouter",            self_auth: false },
    // Companion First-class Model Provider for the Moonshot Kimi LLM endpoint
    // (issue #918 reclaimed the `kimi` id for the native Kimi Code CLI
    // harness). Without this row the only path is the "Add custom provider"
    // workaround, which is undiscoverable for users who don't already know
    // the Moonshot base URL + tier map.
    BuiltInProviderAccount { id: "kimi-via-claude", name: "Kimi via Claude Code", self_auth: false },
];

/// Whether `id` names a Claude-compatible keyed provider — one that carries an
/// API key/endpoint, shows credential + model-tier fields in the UI, and can
/// appear in the spawn menu once configured. Self-authenticating built-ins are
/// the only exceptions (issue #568). The classification is **derived from
/// [`BUILTIN_PROVIDER_ACCOUNTS`]** so a new self-auth built-in can't drift
/// out of sync with the account definition in `default_provider_accounts`.
pub fn is_claude_compatible_id(id: &str) -> bool {
    BUILTIN_PROVIDER_ACCOUNTS
        .iter()
        .find(|b| b.id == id)
        .map(|b| !b.self_auth)
        .unwrap_or(true) // unknown id = custom = claude_compatible
}

/// The **Compatible API surfaces** a first-class **Model Provider** publishes,
/// each with its endpoint URL and a sensible default model-tier map (ADR-0016
/// §4, issue #576). This is the "first-class provider publishes its surface→URL
/// map" mechanism: the attach flow reads it so a pairing only names the surface
/// and Buildmesh fills in the URL + default models.
///
/// Returns empty for anything not first-class (self-auth built-ins, custom
/// Generic providers) — a Generic provider instead *declares* its single
/// surface+URL at creation, stored directly on its [`ProviderPairing`].
///
/// **OpenAI-surface URLs are best-effort and unverified on this host** (no
/// `codex` binary + provider OpenAI key to exercise them, issue #576); the
/// Anthropic-surface URLs match the long-standing Claude Code account defaults and
/// are exercised end-to-end. Live Codex verification is tracked as a follow-up.
pub fn first_class_surfaces(provider_id: &str) -> Vec<SurfaceEndpoint> {
    // OpenAI surface only consumes `default` (→ OPENAI_MODEL); the other tiers
    // are left unset since Codex takes a single model.
    let openai_tiers = |model: &str| ModelTiers {
        default: Some(model.to_string()),
        ..ModelTiers::default()
    };
    match provider_id {
        "minimax" => vec![
            SurfaceEndpoint {
                surface: ApiSurface::Anthropic,
                base_url: "https://api.minimax.io/anthropic".to_string(),
                model_tiers: minimax_default_tiers(),
            },
            SurfaceEndpoint {
                surface: ApiSurface::OpenAI,
                base_url: "https://api.minimax.io/v1".to_string(),
                model_tiers: openai_tiers("MiniMax-M3[1m]"),
            },
        ],
        // Deliberately no `kimi` arm — `kimi` is the Native Agent Harness
        // for the Kimi Code CLI (issue #918). The Moonshot Kimi LLM endpoint
        // lives on the companion `kimi-via-claude` First-class Model Provider
        // row below.
        //
        // Deliberately no `openrouter` arm — the OpenRouter integration ships
        // Anthropic-skin ONLY by a conscious scope decision (paired with empty
        // `model_tiers`, hand-rolled per-tier picks). Codex-under-OpenRouter
        // works via a Generic custom-provider account (the Anthropic-surface
        // pairing path derives from `account.base_url`); the documented
        // OpenRouter OpenAI surface is reachable but not first-class here.
        // Documented in PR #702 body so the asymmetry with MiniMax is
        // discoverable rather than silent.
        "kimi-via-claude" => vec![
            SurfaceEndpoint {
                surface: ApiSurface::Anthropic,
                base_url: "https://api.moonshot.ai/anthropic".to_string(),
                model_tiers: kimi_default_tiers(),
            },
            SurfaceEndpoint {
                surface: ApiSurface::OpenAI,
                // Codex takes a single model — the strongest pick (kimi-k3,
                // matching the Anthropic `default` / Opus tier). Mirrors the
                // MiniMax precedent (`openai_tiers("MiniMax-M3[1m]")`) where
                // the Codex-side default rides on the strong model.
                base_url: "https://api.moonshot.ai/v1".to_string(),
                model_tiers: openai_tiers("kimi-k3"),
            },
        ],
        _ => Vec::new(),
    }
}

/// The **Compatible API surface** an executor speaks — the pure half of
/// [`harness_surface`] (no disk/globals), so the surface-matching logic is
/// unit-testable. Only the two proxy-capable executors map to a surface; every
/// other harness (Terminal, Antigravity, OpenCode) is native-only and returns
/// `None`, so "Add proxied provider" is never offered for it.
pub fn surface_for_executor(provider: Provider) -> Option<ApiSurface> {
    match provider {
        Provider::Anthropic => Some(ApiSurface::Anthropic),
        Provider::Codex => Some(ApiSurface::OpenAI),
        _ => None,
    }
}

/// The **Compatible API surface** a harness profile speaks, resolving the
/// profile's backing executor first (so a custom Claude profile id like
/// `"deepseek-via-claude"` still maps to `Anthropic`). Reads disk via
/// [`resolve_harness_provider`]; the pure core is [`surface_for_executor`].
pub fn harness_surface(harness_id: &str) -> Option<ApiSurface> {
    surface_for_executor(resolve_harness_provider(harness_id))
}

/// The surfaces a **Model Provider** (account) can be proxied over. First-class
/// providers expose their published set ([`first_class_surfaces`]); a Generic
/// (custom) provider exposes exactly one surface — `Anthropic` today, the only
/// surface a custom endpoint declares (PRD #534 / ADR-0016). Self-auth built-ins
/// expose none (they're never proxied).
pub fn provider_surfaces(account: &ProviderAccount) -> Vec<ApiSurface> {
    let first_class = first_class_surfaces(&account.id);
    if !first_class.is_empty() {
        return first_class.into_iter().map(|s| s.surface).collect();
    }
    if account.claude_compatible {
        // A Generic Claude-compatible provider — one Anthropic surface.
        vec![ApiSurface::Anthropic]
    } else {
        Vec::new()
    }
}

/// The Claude Code harness id the derived default Anthropic pairings group under
/// — the first `anthropic`-backed profile, else the literal `"claude"` (which
/// still resolves to the Anthropic executor). Pure; the single source of this
/// rule so the menu (`commands::agent::compose_provider_menu`) and the pairing
/// resolver can't derive it differently.
pub(crate) fn claude_harness_id_from(profiles: &[HarnessProfile]) -> String {
    profiles
        .iter()
        .find(|p| p.harness == "anthropic")
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "claude".to_string())
}

/// Disk-reading wrapper over [`claude_harness_id_from`].
fn claude_harness_id() -> String {
    claude_harness_id_from(&harness_profiles())
}

/// The full effective pairing set (derived defaults + stored extras) read from
/// disk — the harness-config page's "what's attached to each harness" source
/// (issue #576). The pure core is [`effective_pairings`].
pub fn effective_provider_pairings() -> Vec<ProviderPairing> {
    effective_pairings(&provider_accounts(), &provider_pairings(), &claude_harness_id())
}

/// The **Model Providers** that can be attached to `harness_id` — those whose
/// published/declared surfaces include the surface the harness speaks (issue
/// #576). Drives the surface-matched "Add proxied provider" picker so only
/// compatible providers are offered. Empty for a native-only harness (Terminal,
/// Antigravity, OpenCode) that speaks no proxy surface.
pub fn compatible_providers_for_harness(harness_id: &str) -> Vec<ProviderAccount> {
    let Some(surface) = harness_surface(harness_id) else {
        return Vec::new();
    };
    provider_accounts()
        .into_iter()
        .filter(|a| provider_surfaces(a).contains(&surface))
        .collect()
}

/// The pairing the attach flow should store when proxying `provider_id` through
/// `harness_id` (issue #576): a stored pairing wins (idempotent re-attach), else
/// the published/derived default for the harness's surface. `None` when the
/// provider is incompatible with the harness's surface (the UI gates this, but
/// the command re-checks). Disk-reading wrapper over the pure [`resolve_pairing`].
pub fn pairing_for(harness_id: &str, provider_id: &str) -> Option<ProviderPairing> {
    let accounts = provider_accounts();
    let account = accounts.iter().find(|a| a.id == provider_id)?;
    resolve_pairing(harness_id, account, &provider_pairings(), harness_surface)
}

/// The code-defined model-provider accounts that always exist regardless of what
/// `preferences.json` stores. They default to **enabled** so the Accounts panel
/// keeps working out of the box; the user can disable any of them (issue #537).
///
/// MiniMax is the first-class Claude-compatible provider (issue #566): it
/// ships the base URL + per-tier model map the absorbed `cwrap` launcher
/// used (byte-for-byte parity), so the user only needs to add an API key.
/// `kimi` was first-class Claude-compatible until #918 (Kimi Code became a
/// native self-auth harness; users wanting the legacy Claude Code + Moonshot
/// setup now add a custom Claude-compatible account under a different id).
/// usage fetcher yet (pragmatic scope), so it defaults to **disabled** — enabling
/// it and adding a key is the single opt-in step before it appears in the menu.
///
/// Materialised from [`BUILTIN_PROVIDER_ACCOUNTS`], the single declaration
/// site for built-ins (issue #571). Per-builtin specifics (base URL, model
/// tiers, default-enabled) that the table can't carry (ModelTiers is not
/// `const`-constructible) are filled in by the `match` below.
pub fn default_provider_accounts() -> Vec<ProviderAccount> {
    BUILTIN_PROVIDER_ACCOUNTS
        .iter()
        .map(|b| {
            let (base_url, model_tiers) = match b.id {
                "minimax" => (
                    Some("https://api.minimax.io/anthropic".to_string()),
                    minimax_default_tiers(),
                ),
                "openrouter" => (
                    // OpenRouter's "Anthropic Skin" integration — the same
                    // `claude` CLI binary, but routed through OpenRouter's
                    // `/api` endpoint with `ANTHROPIC_AUTH_TOKEN` set to the
                    // user's `sk-or-…` key. Per-tier models are left empty
                    // so the user picks providers/models via the 5-tier
                    // fields in the UI (matches OpenRouter's polyglot
                    // catalogue rather than pre-pinning Anthropic models).
                    Some("https://openrouter.ai/api".to_string()),
                    ModelTiers::default(),
                ),
                "kimi-via-claude" => (
                    // The Moonshot Kimi LLM endpoint — exposed under the
                    // companion First-class Model Provider id (the `kimi`
                    // id is the Native Agent Harness for the Kimi Code CLI,
                    // issue #918). Pre-fills the credential section so the
                    // user only needs to paste their Moonshot API key.
                    Some("https://api.moonshot.ai/anthropic".to_string()),
                    kimi_default_tiers(),
                ),
                _ => (None, ModelTiers::default()),
            };
            // Self-auth built-ins (anthropic/codex/agy/grok/kimi/opencode) ship enabled —
            // the user has nothing to configure (their login lives in the
            // harness's own config dir, e.g. `~/.kimi/config.toml`). The
            // pre-#918 "kimi is opt-in" special case is gone: that flag existed
            // because the old Claude-compatible Kimi Moonshot account had no
            // usage fetcher; the new Kimi Code native harness is self-auth
            // like Grok, so it lands enabled on day one.
            let enabled = true;
            ProviderAccount {
                id: b.id.to_string(),
                name: b.name.to_string(),
                enabled,
                billing_mode: if b.self_auth { BillingMode::Plan } else { BillingMode::PayAsYouGo },
                claude_compatible: !b.self_auth,
                api_key: None,
                base_url,
                model_tiers,
                models: Vec::new(),
            }
        })
        .collect()
}

/// MiniMax's Claude-Code-parity default tier map. **Single source** consumed by
/// (issue #571):
/// - [`default_provider_accounts`] — the per-account `model_tiers` field
/// - [`first_class_surfaces`] for `"minimax"` — the Anthropic surface tier
/// - [`crate::agent::provider::provider_conf::minimax_backend_env`] — the
///   session-naming side-channel
///
/// Also the Anthropic surface tier map for MiniMax (Anthropic-surface pairings
/// now live-derive from the account on every resolution; the account is
/// authoritative).
pub(crate) fn minimax_default_tiers() -> ModelTiers {
    ModelTiers {
        default: Some("MiniMax-M3[1m]".to_string()),
        small_fast: Some("MiniMax-M2.7".to_string()),
        sonnet: Some("MiniMax-M3[1m]".to_string()),
        opus: Some("MiniMax-M3[1m]".to_string()),
        fable: Some("MiniMax-M3[1m]".to_string()),
        haiku: Some("MiniMax-M2.7".to_string()),
    }
}

/// Kimi's default tier map for the **First-class Model Provider** row
/// `kimi-via-claude`. **Single source** consumed by both the per-account
/// `model_tiers` (in `default_provider_accounts`) and the Anthropic surface's
/// `model_tiers` in `first_class_surfaces("kimi-via-claude")` — the same
/// pairing pattern as `minimax_default_tiers`. Any drift between the two
/// consumers is pinned by `kimi_via_claude_is_first_class_claude_compatible_for_claude_code`.
pub(crate) fn kimi_default_tiers() -> ModelTiers {
    ModelTiers {
        // Fable and Opus pick the strongest reasoning model. `default` mirrors
        // Opus per the first-class rule pinned in
        // `resolve_provider_env_composite_kimi_default_fable_matches_opus`.
        default: Some("kimi-k3".to_string()),
        opus: Some("kimi-k3".to_string()),
        fable: Some("kimi-k3".to_string()),
        // Sonnet picks the mid-tier code-tuned model.
        sonnet: Some("kimi-2.7-code".to_string()),
        // Haiku and the small/fast background-task slot both pin to the cheap
        // general-purpose model.
        haiku: Some("kimi-2.6".to_string()),
        small_fast: Some("kimi-2.6".to_string()),
    }
}

/// The effective account list: code-defined defaults with the user's stored
/// `provider_accounts` merged over them by `id` (user wins / new ids append).
/// Mirrors [`harness_profiles`] so a built-in can never be removed by an empty
/// or partial `preferences.json`.
pub fn provider_accounts() -> Vec<ProviderAccount> {
    let stored = match load() {
        Ok(prefs) => prefs.provider_accounts,
        Err(e) => {
            tracing::warn!("preferences::provider_accounts load failed, using defaults: {}", e);
            Vec::new()
        }
    };
    merge_provider_accounts(default_provider_accounts(), stored)
}

/// Pure merge — defaults first, then each stored account overrides by `id` or
/// appends. Split out from [`provider_accounts`] so it's unit-testable without disk.
///
/// `claude_compatible` is **re-derived from the id** on every account afterwards,
/// so it's always correct regardless of what was stored (an older
/// `preferences.json` predating the field, or a stale value sent from the UI).
fn merge_provider_accounts(
    mut accounts: Vec<ProviderAccount>,
    stored: Vec<ProviderAccount>,
) -> Vec<ProviderAccount> {
    for account in stored {
        if let Some(existing) = accounts.iter_mut().find(|a| a.id == account.id) {
            *existing = account;
        } else {
            accounts.push(account);
        }
    }
    for account in accounts.iter_mut() {
        account.claude_compatible = is_claude_compatible_id(&account.id);
    }
    accounts
}

/// Resolve the effective MiniMax API key: the minimax account's `api_key`, then
/// the legacy flat `minimax_api_key` field (read-through so a key stored before
/// #537 isn't lost). Empty strings are treated as absent. A single `load()` feeds
/// both layers so the result is a consistent snapshot even under a concurrent save.
pub fn minimax_api_key_resolved() -> Option<String> {
    let non_empty = |s: Option<String>| s.filter(|v| !v.is_empty());
    let prefs = match load() {
        Ok(prefs) => prefs,
        Err(_) => return None,
    };
    let from_account = merge_provider_accounts(default_provider_accounts(), prefs.provider_accounts.clone())
        .into_iter()
        .find(|a| a.id == "minimax")
        .and_then(|a| a.api_key);
    non_empty(from_account).or_else(|| non_empty(prefs.minimax_api_key))
}

/// Resolve the effective Kimi API key from the merged provider-accounts list.
/// Kimi has no legacy flat field (unlike MiniMax's `minimax_api_key`) so this
/// is a straight lookup, but lives here as the single seam so a future legacy
/// fallback (e.g. a pre-config.json migration) can be added in one place
/// without touching `commands::usage::cached_or_fetch`. Empty strings are
/// treated as absent.
pub fn kimi_api_key_resolved() -> Option<String> {
    merge_provider_accounts(default_provider_accounts(), load().ok()?.provider_accounts)
        .into_iter()
        .find(|a| a.id == "kimi")
        .and_then(|a| a.api_key)
        .filter(|v| !v.is_empty())
}

/// Resolve the effective OpenRouter API key from the merged provider-accounts
/// list. Brand-new id (post-#570 land) — no legacy flat field, identical
/// lookup shape to [`kimi_api_key_resolved`] but kept as a separate symbol so
/// each provider's single seam stays explicit.
pub fn openrouter_api_key_resolved() -> Option<String> {
    merge_provider_accounts(default_provider_accounts(), load().ok()?.provider_accounts)
        .into_iter()
        .find(|a| a.id == "openrouter")
        .and_then(|a| a.api_key)
        .filter(|v| !v.is_empty())
}

/// Upsert a provider account into `prefs` (by `id`). Pure: mutates the passed
/// `prefs` so the command layer stays a thin load→mutate→save.
///
/// No longer materializes a paired [`HarnessProfile`]: the spawn menu is now
/// *derived* from the accounts list (see `commands::agent::compose_provider_menu`),
/// so an enabled, keyed, Claude-compatible account — built-in MiniMax or a
/// custom endpoint alike — appears automatically, and clearing its key or
/// disabling it removes it with no second list to keep in sync (issue #568).
pub fn upsert_provider_account(prefs: &mut AppPreferences, account: ProviderAccount) {
    if let Some(existing) = prefs.provider_accounts.iter_mut().find(|a| a.id == account.id) {
        *existing = account;
    } else {
        prefs.provider_accounts.push(account);
    }
}

/// Remove a stored provider account by `id`. Built-in defaults can't truly be
/// deleted — removing a built-in's stored override just reverts it to the code
/// default (which carries no key, so it drops out of the derived spawn menu).
pub fn remove_provider_account(prefs: &mut AppPreferences, id: &str) {
    prefs.provider_accounts.retain(|a| a.id != id);
}

/// Upsert a **Proxied Provider** pairing into `prefs` by its
/// `(harness_id, provider_id)` key (issue #576). Pure: mutates the passed
/// `prefs` so the command layer stays a thin load→mutate→save.
pub fn upsert_provider_pairing(prefs: &mut AppPreferences, pairing: ProviderPairing) {
    if let Some(existing) = prefs
        .provider_pairings
        .iter_mut()
        .find(|p| p.harness_id == pairing.harness_id && p.provider_id == pairing.provider_id)
    {
        *existing = pairing;
    } else {
        prefs.provider_pairings.push(pairing);
    }
}

/// Remove a stored **Proxied Provider** pairing by its `(harness_id,
/// provider_id)` key (issue #576). A no-op for a *derived* default pairing
/// (none is stored): the default Claude/Anthropic row is controlled by the
/// account's enabled/keyed state on the Providers page, not detachable here.
pub fn remove_provider_pairing(prefs: &mut AppPreferences, harness_id: &str, provider_id: &str) {
    prefs
        .provider_pairings
        .retain(|p| !(p.harness_id == harness_id && p.provider_id == provider_id));
}

/// Set a provider account's **global** API key only if it currently has none
/// (the "set-if-absent from the attach flow" rule, ADR-0016 §4 / issue #576).
/// Returns whether a key was written. The canonical key editor stays on the
/// Providers page; this lets the harness-config attach flow seed a key for a
/// provider the user hasn't configured yet without ever *overwriting* one.
///
/// Operates on the *effective* account (built-in defaults included), persisting
/// the change as a stored override so a built-in like MiniMax keeps its code
/// default endpoint while gaining the user's key.
pub fn set_account_key_if_absent(prefs: &mut AppPreferences, provider_id: &str, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let effective = provider_accounts();
    let Some(account) = effective.into_iter().find(|a| a.id == provider_id) else {
        return false;
    };
    if account.api_key.as_deref().is_some_and(|k| !k.is_empty()) {
        return false; // already keyed — never overwrite
    }
    upsert_provider_account(
        prefs,
        ProviderAccount {
            api_key: Some(key.to_string()),
            ..account
        },
    );
    true
}

/// The user-added **Proxied Provider** pairings stored in preferences (issue
/// #576). The default Anthropic pairing for a keyed account is *not* in here —
/// it's derived (see [`effective_pairings`]) — so this holds only the extra
/// surfaces/harnesses the user attached. A load failure is logged and treated as
/// "no stored pairings".
pub fn provider_pairings() -> Vec<ProviderPairing> {
    match load() {
        Ok(prefs) => prefs.provider_pairings,
        Err(e) => {
            tracing::warn!("preferences::provider_pairings load failed, using none: {}", e);
            Vec::new()
        }
    }
}

/// The full stored per-harness **Proxied Provider** child orders (issue
/// #577). One [`ProxiedProviderOrder`] per harness the user has reordered;
/// a harness without an entry keeps its natural order (the backend's
/// `order_proxied_children` falls through when the per-harness lookup is
/// `None`). A load failure is logged and treated as "no stored order".
pub fn proxied_provider_order() -> Vec<ProxiedProviderOrder> {
    match load() {
        Ok(prefs) => prefs.proxied_provider_order,
        Err(e) => {
            tracing::warn!(
                "preferences::proxied_provider_order load failed, using none: {}",
                e
            );
            Vec::new()
        }
    }
}

/// The stored per-harness child order, if any (issue #577). `None` means
/// "no stored preference" — the backend's `order_proxied_children` falls
/// through to natural insertion order. The pair-id comparator
/// `(harness_id, provider_id)` is identical to the pairing key the rest of
/// the codebase uses, so this lookup is a one-liner for the spawn menu and
/// the harness-config page alike.
///
/// `pub` for the single-harness lookup symmetry with the full-list
/// [`proxied_provider_order`] getter and the [`set_proxied_provider_order`]
/// setter — the spawn-menu read path uses [`proxied_provider_order`] (a
/// single `HashMap` lookup) for performance, but a UI surface that wants
/// "what's the order under harness X?" can reach for this without parsing
/// the full vector.
#[allow(dead_code)]
pub fn proxied_order_for(harness_id: &str) -> Option<Vec<String>> {
    proxied_provider_order()
        .into_iter()
        .find(|o| o.harness_id == harness_id)
        .map(|o| o.provider_ids)
}

/// Persist the **Proxied Provider** child order for a single harness (issue
/// #577). Upserts by `harness_id`: a re-set replaces the prior entry in
/// place; a new harness appends. An empty `provider_ids` is normalised to
/// "drop the entry" — the natural order is then re-derived on read, so a
/// detach-reattach sequence can't be confused by a stale empty slot.
///
/// Defensive filters on the incoming list (mirrors `set_harness_order`'s
/// dedupe/keep-first pattern):
///   * **Dedup** — first occurrence wins; later duplicates are dropped
///     (a malformed UI send with `[a, b, a]` doesn't shift every later
///     provider's index on persist).
///   * **Drop unknown account ids** — any id that isn't a registered
///     [`ProviderAccount`] (built-in or custom) is silently dropped. The
///     ordering seam would never render it, and persisting it would let a
///     stale UI send pollute the stored preferences. Note we validate
///     against `provider_accounts()` rather than `provider_pairings()`:
///     the order is meaningful for any account the user could attach under
///     this harness, even before the pairing exists (a user can rearrange
///     after a future attach and the slot is reserved).
pub fn set_proxied_provider_order(harness_id: String, provider_ids: Vec<String>) -> Result<(), String> {
    let mut prefs = load()?;
    let known_ids: std::collections::HashSet<String> = crate::preferences::provider_accounts()
        .into_iter()
        .map(|a| a.id)
        .collect();
    let incoming: Vec<String> = dedupe_keeping_first(provider_ids.into_iter())
        .into_iter()
        .filter(|id| known_ids.contains(id))
        .collect();
    let existing = prefs
        .proxied_provider_order
        .iter_mut()
        .find(|o| o.harness_id == harness_id);
    match existing {
        Some(entry) if incoming.is_empty() => {
            // Empty list = the user cleared their order preference. Drop
            // the entry entirely so the next read returns `None` and the
            // backend falls through to natural order — a stored empty
            // would silently no-op the ordering seam.
            let _ = entry; // `entry` is borrowed only for the empty-list check
            prefs.proxied_provider_order.retain(|o| o.harness_id != harness_id);
        }
        Some(entry) => entry.provider_ids = incoming,
        None if !incoming.is_empty() => prefs.proxied_provider_order.push(ProxiedProviderOrder {
            harness_id,
            provider_ids: incoming,
        }),
        None => { /* nothing to persist */ }
    }
    save(prefs)
}

/// The **Anthropic-surface** model-tier map for an account — the precedence
/// every Anthropic-side consumer (derived pairing, spawn preflight, the
/// pairing menu renderer) shares. Account tiers (the Providers-page edits)
/// win when set; the first-class published Anthropic endpoint fills only a
/// fully-cleared account. A custom Generic provider has no published endpoint,
/// so its account fields are the only source — the pre-pairings behaviour,
/// unchanged.
///
/// Extracted from `default_anthropic_pairing` so the precedence lives in one
/// place; the comment naming the smell (`Tier source mirrors`) is the one
/// truth.
fn anthropic_tiers_for(account: &ProviderAccount) -> ModelTiers {
    let own = effective_tiers(account);
    if !own.is_empty() {
        return own;
    }
    first_class_surfaces(&account.id)
        .into_iter()
        .find(|e| e.surface == ApiSurface::Anthropic)
        .map(|e| e.model_tiers)
        .unwrap_or_default()
}

/// The **Anthropic-surface** pairing derived for a keyed account under a
/// Claude-backed harness (issue #576). Derived *live* on every resolution —
/// never persisted — so Providers-page edits reach the next spawn.
///
/// Precedence: the account's own `base_url` wins when set; the first-class
/// published Anthropic endpoint fills anything the account leaves empty.
/// Tier source: [`anthropic_tiers_for`].
fn default_anthropic_pairing(account: &ProviderAccount, claude_harness_id: &str) -> ProviderPairing {
    let published = first_class_surfaces(&account.id)
        .into_iter()
        .find(|e| e.surface == ApiSurface::Anthropic);
    let base_url = account
        .base_url
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| published.as_ref().map(|e| e.base_url.clone()));
    ProviderPairing {
        harness_id: claude_harness_id.to_string(),
        provider_id: account.id.clone(),
        surface: ApiSurface::Anthropic,
        base_url,
        model_tiers: anthropic_tiers_for(account),
    }
}

/// Resolve the effective pairing for a single `(harness_id, provider_id)` — a
/// stored [`ProviderPairing`] wins; otherwise a default is derived for the
/// harness's surface (issue #576). Returns `None` when no sensible pairing
/// exists (e.g. a Generic provider asked for a surface it never declared, or a
/// harness with no proxy surface). Pure: the surface comes from `surface_of` so
/// the disk-reading [`harness_surface`] stays out of the unit-test seam.
fn resolve_pairing(
    harness_id: &str,
    account: &ProviderAccount,
    stored: &[ProviderPairing],
    surface_of: impl Fn(&str) -> Option<ApiSurface>,
) -> Option<ProviderPairing> {
    let stored_pairing = stored
        .iter()
        .find(|p| p.harness_id == harness_id && p.provider_id == account.id);
    let surface = stored_pairing
        .map(|p| p.surface)
        .or_else(|| surface_of(harness_id))?;
    // Anthropic-surface pairings are attach-time snapshots — the UI never
    // edits their payload, only attaches/detaches — so always re-derive from
    // the account (see `default_anthropic_pairing`).
    if surface == ApiSurface::Anthropic && account.claude_compatible {
        return Some(default_anthropic_pairing(account, harness_id));
    }
    if let Some(p) = stored_pairing {
        return Some(p.clone());
    }
    // No stored pairing: derive from the published first-class endpoint for
    // this surface. A Generic provider on a non-Anthropic surface has nothing
    // to fall back to.
    first_class_surfaces(&account.id)
        .into_iter()
        .find(|e| e.surface == surface)
        .map(|ep| ProviderPairing {
            harness_id: harness_id.to_string(),
            provider_id: account.id.clone(),
            surface,
            base_url: Some(ep.base_url),
            model_tiers: ep.model_tiers,
        })
}

/// The full set of **Proxied Provider** pairings to render in the Spawn Menu
/// (issue #576): the derived default Anthropic pairing for every proxiable
/// account, overlaid with the user's stored pairings (a stored pairing wins on
/// its `(harness_id, provider_id)` key). Pure (no disk/globals) — the unit-test
/// seam for the menu derivation.
///
/// "Proxiable" = enabled, Claude-compatible, and keyed (non-empty API key) —
/// the same gate the pre-#576 menu used, so a keyless or disabled account
/// contributes no rows.
pub(crate) fn effective_pairings(
    accounts: &[ProviderAccount],
    stored: &[ProviderPairing],
    claude_harness_id: &str,
) -> Vec<ProviderPairing> {
    let keyed = |a: &ProviderAccount| a.api_key.as_deref().is_some_and(|k| !k.is_empty());
    let is_proxiable = |a: &ProviderAccount| a.enabled && a.claude_compatible && keyed(a);
    // Resolve the proxiable set once so the stored-pairing loop is a hash lookup,
    // not a re-scan per pairing.
    let proxiable_ids: std::collections::HashSet<&str> = accounts
        .iter()
        .filter(|a| is_proxiable(a))
        .map(|a| a.id.as_str())
        .collect();
    let mut out: Vec<ProviderPairing> = accounts
        .iter()
        .filter(|a| is_proxiable(a))
        .map(|a| default_anthropic_pairing(a, claude_harness_id))
        .collect();
    for p in stored {
        // Only surface a stored pairing whose account is still proxiable.
        if !proxiable_ids.contains(p.provider_id.as_str()) {
            continue;
        }
        // Anthropic-surface pairings are attach-time snapshots — render the
        // live account-derived config so the page matches what the spawn path
        // injects (see `default_anthropic_pairing`).
        let resolved = if p.surface == ApiSurface::Anthropic {
            accounts
                .iter()
                .find(|a| a.id == p.provider_id)
                .map(|a| default_anthropic_pairing(a, &p.harness_id))
                .unwrap_or_else(|| p.clone())
        } else {
            p.clone()
        };
        match out
            .iter_mut()
            .find(|o| o.harness_id == p.harness_id && o.provider_id == p.provider_id)
        {
            Some(existing) => *existing = resolved,
            None => out.push(resolved),
        }
    }
    out
}

/// Build the backend-selecting environment for a **Spawn Option** (issue #576) —
/// the pairing-scoped, surface-aware successor to the #538 account-only resolver.
///
/// Resolution by id shape ([`crate::agent::provider::parse_spawn_option_id`]):
///   * **Composite proxied id** (`<harness>:<provider>`, e.g. `claude:minimax`,
///     `codex:minimax`): resolve the `(harness, provider)` **pairing** (a stored
///     [`ProviderPairing`] wins, else a default for the harness's surface) and
///     emit env for that pairing's surface — `ANTHROPIC_*` for `Anthropic`,
///     `OPENAI_*` for `OpenAI` — using the account's **global** API key. This is
///     what lets one provider attach across surfaces (AC #1).
///   * **Bare id** (`minimax`, a custom account id, or a native harness id):
///     the pre-#576 path — look the bare id up as an account and emit its default
///     Anthropic env. Preserves legacy `agent_nodes.provider` values stored
///     before the composite-id migration, and keeps the built-in Anthropic
///     subscription on a clean slate (empty env, vanilla `claude`).
///
/// Returns empty when no account matches or the pairing carries no endpoint —
/// the spawn path resets inherited backend vars first (see
/// [`crate::agent::provider::AgentProvider::resets_backend_env`]), so empty means
/// a clean slate, not a leaked override.
pub fn resolve_provider_env(spawn_option_id: &str) -> Vec<(String, String)> {
    let (harness_id, provider_id) =
        crate::agent::provider::parse_spawn_option_id(spawn_option_id);
    let accounts = provider_accounts();
    match provider_id {
        Some(provider_id) => {
            let Some(account) = accounts.iter().find(|a| a.id == provider_id) else {
                return Vec::new();
            };
            let Some(pairing) =
                resolve_pairing(harness_id, account, &provider_pairings(), harness_surface)
            else {
                return Vec::new();
            };
            surface_env(
                pairing.surface,
                pairing.base_url.as_deref(),
                account.api_key.as_deref(),
                &pairing.model_tiers,
            )
        }
        None => provider_account_env(accounts.iter().find(|a| a.id == spawn_option_id)),
    }
}

/// Preflight gate run by `spawn_agent_inner` BEFORE [`resolve_provider_env`].
/// Catches the silent-fail trap where a Claude-compatible custom endpoint
/// (OpenRouter, or any Generic provider with a non-empty `base_url`) launches
/// `claude` against a third-party backend without a primary model pinned —
/// `claude` then sends its hardcoded `claude-3-5-sonnet-<date>` default, the
/// third-party rejects it (OpenRouter expects `provider/model` slugs), and the
/// user only sees a server-side `tracing::warn` they can't reach. Returning
/// `Err` here surfaces the issue as the spawn result, so the UI can prompt
/// the user to fill the `Default model` tier field on the Providers page.
pub fn preflight_resolve_provider_env(spawn_option_id: &str) -> Result<(), String> {
    let (harness_id, provider_id) =
        crate::agent::provider::parse_spawn_option_id(spawn_option_id);
    let accounts = provider_accounts();
    // Mirror `resolve_provider_env`'s account resolution so the gate sees the
    // same effective state as the env builder.
    let account_opt: Option<&ProviderAccount> = match provider_id {
        Some(pid) => {
            let Some(account) = accounts.iter().find(|a| a.id == pid) else {
                return Ok(());
            };
            let Some(_pairing) =
                resolve_pairing(harness_id, account, &provider_pairings(), harness_surface)
            else {
                return Ok(());
            };
            Some(account)
        }
        None => accounts.iter().find(|a| a.id == spawn_option_id),
    };
    preflight_account_env(account_opt)
}

/// Pure helper shared with the unit tests — given the already-resolved
/// account (or `None` for the vanilla-Anthropic path), returns `Err` iff the
/// account routes through a non-empty `base_url` but has no primary model
/// pinned at the surface the env builder would use. Split out from the
/// disk-reading wrapper so the rule is testable without touching the global
/// preferences cache.
///
/// Preflight gate for a custom Claude-compatible endpoint — refuses to spawn
/// when no primary model is pinned (OpenRouter-style 400 trap). Tier source:
/// [`anthropic_tiers_for`] — same precedence as `default_anthropic_pairing`,
/// so a user-cleared MiniMax with empty `model_tiers` doesn't false-positive
/// (the env builder still emits `ANTHROPIC_MODEL` from the published surface).
fn preflight_account_env(account: Option<&ProviderAccount>) -> Result<(), String> {
    let Some(account) = account else {
        return Ok(());
    };
    let base_url_is_set = account
        .base_url
        .as_deref()
        .is_some_and(|s| !s.is_empty());
    if !base_url_is_set {
        return Ok(());
    }
    let tiers = anthropic_tiers_for(account);
    if tiers.default.is_none() {
        return Err(format!(
            "Custom Claude-compatible endpoint '{}' requires the 'Default model' tier to be set. Open the Providers page and configure it (e.g. 'anthropic/claude-3-5-sonnet-latest' for Claude via OpenRouter).",
            account.id
        ));
    }
    Ok(())
}

/// Resolve an account's effective per-tier models: its [`ModelTiers`] if set,
/// otherwise derived from the deprecated flat `models` list for back-compat
/// (issue #567) — `models[0]` is the primary and Sonnet/Opus, `models[1]` (if
/// present, else `models[0]`) is small/fast and Haiku, exactly the mapping the
/// old flat-list path produced.
fn effective_tiers(account: &ProviderAccount) -> ModelTiers {
    if !account.model_tiers.is_empty() {
        return account.model_tiers.clone();
    }
    let models: Vec<&str> = account.models.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
    let Some(&primary) = models.first() else {
        return ModelTiers::default();
    };
    let fast = models.get(1).copied().unwrap_or(primary);
    ModelTiers {
        default: Some(primary.to_string()),
        small_fast: Some(fast.to_string()),
        sonnet: Some(primary.to_string()),
        opus: Some(primary.to_string()),
        // fable left None — `anthropic_surface_env` falls back to the opus tier
        // (which itself falls back to primary), so a legacy flat-list account
        // doesn't pin a redundant fable = primary.
        fable: None,
        haiku: Some(fast.to_string()),
    }
}

/// Pure translation of an optional account into `ANTHROPIC_*` env pairs — the
/// disk-free half of [`resolve_provider_env`], unit-tested directly. Only
/// non-empty fields emit a var, so a partially-filled account never injects a
/// blank `ANTHROPIC_BASE_URL` (which `claude` would treat as a real, broken URL).
///
/// For a **custom endpoint** (non-empty `base_url`) this pins the full per-tier
/// model routing and a long timeout, reproducing what the deleted MiniMax/Kimi
/// adapters (and `cwrap` before them) did: Claude Code asks for several model
/// *aliases* — a primary, a cheap "small/fast" model for background work, and the
/// Sonnet/Opus/Haiku defaults — and its built-in `claude-*` slugs would 404
/// against a third-party endpoint, so every alias is mapped onto a configured
/// model from [`ModelTiers`]. On the default Anthropic endpoint the slugs are
/// valid, so only `ANTHROPIC_MODEL` is pinned and Claude Code keeps its own alias
/// routing.
fn provider_account_env(account: Option<&ProviderAccount>) -> Vec<(String, String)> {
    let Some(account) = account else {
        return Vec::new();
    };
    anthropic_surface_env(
        account.base_url.as_deref(),
        account.api_key.as_deref(),
        &effective_tiers(account),
    )
}

/// Emit the spawn env for a pairing's **Compatible API surface** (issue #576).
/// Dispatches to the per-surface emitter so the surface enum is the single fork
/// between the `claude` and `codex` backend-selection conventions.
fn surface_env(
    surface: ApiSurface,
    base_url: Option<&str>,
    api_key: Option<&str>,
    tiers: &ModelTiers,
) -> Vec<(String, String)> {
    match surface {
        ApiSurface::Anthropic => anthropic_surface_env(base_url, api_key, tiers),
        ApiSurface::OpenAI => openai_surface_env(base_url, api_key, tiers),
    }
}

/// Build the `ANTHROPIC_*` env the `claude` binary reads to target a
/// Claude-compatible endpoint (the Anthropic [`ApiSurface`]). Only non-empty
/// fields emit a var, so a partially-filled pairing never injects a blank
/// `ANTHROPIC_BASE_URL` (which `claude` would treat as a real, broken URL).
///
/// For a **custom endpoint** (non-empty `base_url`) this pins the full per-tier
/// model routing and a long timeout, reproducing what the deleted MiniMax/Kimi
/// adapters (and `cwrap` before them) did: Claude Code asks for several model
/// *aliases* — a primary, a cheap "small/fast" model for background work, and the
/// Sonnet/Opus/Haiku defaults — and its built-in `claude-*` slugs would 404
/// against a third-party endpoint, so every alias is mapped onto a configured
/// model from [`ModelTiers`]. On the default Anthropic endpoint the slugs are
/// valid, so only `ANTHROPIC_MODEL` is pinned and Claude Code keeps its own alias
/// routing.
fn anthropic_surface_env(
    base_url: Option<&str>,
    api_key: Option<&str>,
    tiers: &ModelTiers,
) -> Vec<(String, String)> {
    let base_url = base_url.filter(|s| !s.is_empty());
    let mut env = Vec::new();
    if let Some(base) = base_url {
        env.push(("ANTHROPIC_BASE_URL".to_string(), base.to_string()));
        // Force `ANTHROPIC_API_KEY=""` so a shell-set Anthropic key can't
        // make Claude Code fall back to direct Anthropic auth instead of
        // routing through the configured third-party endpoint. Required
        // by OpenRouter's Anthropic Skin; matches the behaviour `cwrap`
        // applied for every Claude-compatible endpoint (MiniMax, Kimi,
        // OpenRouter alike). `CLAUDE_BACKEND_ENV_VARS` deliberately omits
        // this var — adding it there would break the default-Anthropic
        // subscription path, which currently honours a shell-set key.
        env.push(("ANTHROPIC_API_KEY".to_string(), String::new()));
    }
    if let Some(key) = api_key.filter(|s| !s.is_empty()) {
        env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), key.to_string()));
    }
    let model = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_string);
    if let Some(primary) = model(&tiers.default) {
        env.push(("ANTHROPIC_MODEL".to_string(), primary.clone()));
        if base_url.is_some() {
            // A custom endpoint needs every alias pinned. Each tier falls back to
            // the primary so a partially-filled map never sends a `claude-*` slug;
            // fable falls back to the opus pick (which itself falls back to
            // primary), mirroring the tier precedence for Claude 5's newest alias.
            let fast = model(&tiers.small_fast).unwrap_or_else(|| primary.clone());
            let opus = model(&tiers.opus).unwrap_or_else(|| primary.clone());
            for (k, v) in [
                ("ANTHROPIC_SMALL_FAST_MODEL", fast.clone()),
                ("ANTHROPIC_DEFAULT_SONNET_MODEL", model(&tiers.sonnet).unwrap_or_else(|| primary.clone())),
                ("ANTHROPIC_DEFAULT_OPUS_MODEL", opus.clone()),
                ("ANTHROPIC_DEFAULT_FABLE_MODEL", model(&tiers.fable).unwrap_or(opus)),
                ("ANTHROPIC_DEFAULT_HAIKU_MODEL", model(&tiers.haiku).unwrap_or(fast)),
            ] {
                env.push((k.to_string(), v));
            }
        }
    } else if base_url.is_some() {
        tracing::warn!(
            "proxied pairing sets a custom base_url but no model — claude will send its default model id to the custom endpoint and likely be rejected"
        );
    }
    // Third-party models can stream slowly; give a custom endpoint the long
    // timeout the absorbed cwrap MiniMax/Kimi arms used instead of Claude Code's
    // short default.
    if base_url.is_some() {
        env.push(("API_TIMEOUT_MS".to_string(), "3000000".to_string()));
    }
    env
}

/// Build the `OPENAI_*` env the `codex` binary reads to target an
/// OpenAI-compatible endpoint (the OpenAI [`ApiSurface`], issue #576). Codex
/// takes a single model, so only `ModelTiers::default` is consumed
/// (→ `OPENAI_MODEL`); the per-tier alias map is Anthropic-only.
///
/// **Live-verified on `codex-cli 0.144.0` (issue #599, 2026-07-18):**
/// - `OPENAI_API_KEY` **IS** honoured — codex's `auth.credentials` reports it
///   under `auth env vars present` and uses it for API-key auth. Keep.
/// - `OPENAI_BASE_URL` / `OPENAI_MODEL` are **NOT** honoured as env vars —
///   `codex doctor --json` reports `endpoint: wss://api.openai.com/v1/...`
///   regardless of what `OPENAI_BASE_URL` was exported. The Anthropic surface
///   doesn't have this trap (`claude` reads `ANTHROPIC_*` env vars natively).
///
/// The emitter therefore still **writes** all three vars (forward-compat for
/// any future codex release that picks them up) but the **effective routing**
/// happens one level out: `agent::spawn::build_spawn_command` derives a
/// per-pairing profile name from the same `OPENAI_*` pairs via
/// `crate::agent::provider::adapters::codex::proxy_pair`, idempotently writes
/// `$CODEX_HOME/<name>.config.toml` (a `[model_providers.<name>]` block Codex
/// actually consumes) via `codex::ensure_proxy_profile`, and passes `-p <name>`
/// to the codex CLI. Without that translation, a Proxied Provider Codex spawn
/// would silently target OpenAI's real endpoint on the user's own credentials
/// — the very leak issue #599 closes.
///
/// Like the Anthropic emitter, only non-empty fields emit a var, and **native
/// Codex (no pairing) injects nothing** — so the user's own `OPENAI_API_KEY`
/// / `codex login` is untouched (regression-pinned by
/// `codex::proxy_pair_none_for_native_codex`).
fn openai_surface_env(
    base_url: Option<&str>,
    api_key: Option<&str>,
    tiers: &ModelTiers,
) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(base) = base_url.filter(|s| !s.is_empty()) {
        env.push(("OPENAI_BASE_URL".to_string(), base.to_string()));
    }
    if let Some(key) = api_key.filter(|s| !s.is_empty()) {
        env.push(("OPENAI_API_KEY".to_string(), key.to_string()));
    }
    if let Some(model) = tiers.default.as_deref().filter(|s| !s.is_empty()) {
        env.push(("OPENAI_MODEL".to_string(), model.to_string()));
    }
    env
}

/// Pure precedence resolver — kept separate from `load()` so it can be
/// unit-tested without touching disk. The order is:
///   1. `explicit` (e.g. caller-passed argument)
///   2. `per_mesh` (DB column on `meshes.default_provider`)
///   3. `app_wide` (buildmesh-wide preference)
///   4. `"claude"` hardcoded fallback (post-#538 unified harness id;
///
/// Empty strings are treated as absent at every layer so a blank entry in
/// the DB does not block lower layers from being consulted.
pub fn resolve_default_provider(
    explicit: Option<String>,
    per_mesh: Option<String>,
    app_wide: Option<String>,
) -> String {
    fn non_empty(s: Option<String>) -> Option<String> {
        s.filter(|v| !v.is_empty())
    }
    non_empty(explicit)
        .or_else(|| non_empty(per_mesh))
        .or_else(|| non_empty(app_wide))
        .unwrap_or_else(|| "claude".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as TestMutex;

    /// Tests in this module share `APP_DATA_DIR` and `CACHE` global state, so
    /// they must run serially.
    static TEST_LOCK: TestMutex<()> = TestMutex::new(());
    static TEST_DIR_COUNTER: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn test_dir() -> PathBuf {
        let id = TEST_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "buildmesh-prefs-test-{}-{id}",
            std::process::id()
        ))
    }

    fn with_temp_dir<F: FnOnce(&PathBuf)>(f: F) -> PathBuf {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = test_dir();
        std::fs::create_dir_all(&tmp).unwrap();

        init_for_tests(tmp.clone());

        f(&tmp);

        reset_for_tests();
        let _ = std::fs::remove_dir_all(&tmp);
        tmp
    }

    #[test]
    fn preference_files_are_isolated_between_temp_directories() {
        let first_dir = with_temp_dir(|_| {
            save(AppPreferences {
                default_provider: Some("minimax".to_string()),
                ..Default::default()
            })
            .unwrap();
        });

        let second_dir = with_temp_dir(|_| {
            assert_eq!(load().unwrap(), AppPreferences::default());
        });

        assert_ne!(first_dir, second_dir);
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        with_temp_dir(|_| {
            let prefs = load().unwrap();
            assert_eq!(prefs, AppPreferences::default());
            assert_eq!(prefs.default_provider, None);
        });
    }

    #[test]
    fn save_then_load_round_trip() {
        with_temp_dir(|_| {
            let prefs = AppPreferences {
                default_provider: Some("minimax".to_string()),
                ..Default::default()
            };
            save(prefs.clone()).unwrap();
            // Clear cache to force a disk read.
            *CACHE.lock().unwrap() = None;
            let loaded = load().unwrap();
            assert_eq!(loaded, prefs);
        });
    }

    #[test]
    fn default_provider_helper_strips_empty_strings() {
        with_temp_dir(|_| {
            save(AppPreferences { default_provider: Some(String::new()), ..Default::default() }).unwrap();
            assert_eq!(default_provider(), None);

            save(AppPreferences { default_provider: Some("agy".to_string()), ..Default::default() }).unwrap();
            assert_eq!(default_provider(), Some("agy".to_string()));
        });
    }

    #[test]
    fn resolve_precedence_explicit_wins() {
        let got = resolve_default_provider(
            Some("codex".into()),
            Some("minimax".into()),
            Some("agy".into()),
        );
        assert_eq!(got, "codex");
    }

    #[test]
    fn resolve_precedence_falls_through_to_per_mesh() {
        let got = resolve_default_provider(None, Some("minimax".into()), Some("gemini".into()));
        assert_eq!(got, "minimax");
    }

    #[test]
    fn resolve_precedence_falls_through_to_app_wide() {
        let got = resolve_default_provider(None, None, Some("gemini".into()));
        assert_eq!(got, "gemini");
    }

    #[test]
    fn resolve_precedence_falls_through_to_claude() {
        let got = resolve_default_provider(None, None, None);
        // Post-#538 the unified Claude harness id is "claude", not the
        // legacy provider id "anthropic". A fresh install with no stored
        // preferences must resolve to a real harness id, not a dead one.
        assert_eq!(got, "claude");
    }

    #[test]
    fn resolve_precedence_treats_empty_strings_as_absent() {
        // Empty per-mesh value should not block the app-wide setting.
        let got = resolve_default_provider(
            Some(String::new()),
            Some(String::new()),
            Some("minimax".into()),
        );
        assert_eq!(got, "minimax");

        // All-empty everywhere collapses to the claude fallback.
        let got = resolve_default_provider(
            Some(String::new()),
            Some(String::new()),
            Some(String::new()),
        );
        assert_eq!(got, "claude");
    }

    #[test]
    fn malformed_json_falls_back_to_default() {
        with_temp_dir(|dir| {
            std::fs::write(dir.join("preferences.json"), "{not json").unwrap();
            *CACHE.lock().unwrap() = None;
            let prefs = load().unwrap();
            assert_eq!(prefs, AppPreferences::default());
        });
    }

    /// Issue: v19 Spawn Option composite-id migration rewrote
    /// `agent_nodes.provider` from bare → composite (e.g. `minimax` →
    /// `claude:minimax`), but never touched `preferences.json::default_provider`.
    /// A user whose app-wide default was set before #575 lands kept the
    /// legacy bare form in their preferences.json. Without normalization,
    /// `resolve_default_provider` returns that bare form, and
    /// `resolve_provider_env("minimax")` happily maps it to the keyed
    /// MiniMax account — spawning a Claude CLI session against MiniMax's
    /// endpoint even though the user never picked MiniMax-via-Claude.
    ///
    /// Regression guard: a bare `"minimax"` value gets rewritten to
    /// `"claude:minimax"` on the first run after upgrade. Idempotent —
    /// a second call is a no-op.
    #[test]
    fn ensure_default_provider_normalized_rewrites_legacy_bare_to_composite() {
        with_temp_dir(|_| {
            // Seed preferences.json with the legacy bare form, mirroring
            // the on-disk state Adam's machine has today.
            save(AppPreferences {
                default_provider: Some("minimax".to_string()),
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;

            ensure_default_provider_normalized().unwrap();

            *CACHE.lock().unwrap() = None;
            let loaded = load().unwrap();
            assert_eq!(
                loaded.default_provider,
                Some("claude:minimax".to_string()),
                "bare 'minimax' should be normalized to 'claude:minimax'"
            );
        });
    }

    /// Post-#918: bare `"kimi"` resolves to the native Kimi Code harness via
    /// `Provider::from_db_str` — no rewrite to `claude:kimi` (which would
    /// land the user in a state with no matching Proxied row, since the
    /// reserved `"kimi"` id is self_auth). The normalizer must leave
    /// legacy bare `"kimi"` preferences untouched.
    #[test]
    fn ensure_default_provider_normalized_leaves_legacy_kimi_alone() {
        with_temp_dir(|_| {
            save(AppPreferences {
                default_provider: Some("kimi".to_string()),
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;

            ensure_default_provider_normalized().unwrap();

            *CACHE.lock().unwrap() = None;
            let loaded = load().unwrap();
            assert_eq!(
                loaded.default_provider,
                Some("kimi".to_string()),
                "bare 'kimi' must be left alone post-#918 — it resolves to native Kimi Code"
            );
        });
    }

    #[test]
    fn ensure_default_provider_normalized_leaves_already_composite_unchanged() {
        with_temp_dir(|_| {
            // Already-migrated value — the safety net must not rewrite it.
            save(AppPreferences {
                default_provider: Some("claude:minimax".to_string()),
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;

            ensure_default_provider_normalized().unwrap();

            *CACHE.lock().unwrap() = None;
            let loaded = load().unwrap();
            assert_eq!(
                loaded.default_provider,
                Some("claude:minimax".to_string()),
                "already-composite values must not be rewritten"
            );
        });
    }

    #[test]
    fn ensure_default_provider_normalized_leaves_native_harness_unchanged() {
        with_temp_dir(|_| {
            // Native Claude harness id — not a proxied-provider bare id.
            save(AppPreferences {
                default_provider: Some("claude".to_string()),
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;

            ensure_default_provider_normalized().unwrap();

            *CACHE.lock().unwrap() = None;
            let loaded = load().unwrap();
            assert_eq!(
                loaded.default_provider,
                Some("claude".to_string()),
                "native harness id 'claude' must not be rewritten"
            );
        });
    }

    #[test]
    fn ensure_default_provider_normalized_leaves_none_unchanged() {
        with_temp_dir(|_| {
            // No app-wide override — the normalizer must be a no-op.
            save(AppPreferences {
                default_provider: None,
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;

            ensure_default_provider_normalized().unwrap();

            *CACHE.lock().unwrap() = None;
            let loaded = load().unwrap();
            assert_eq!(
                loaded.default_provider, None,
                "None must not be rewritten"
            );
        });
    }

    #[test]
    fn default_harness_profiles_include_terminal() {
        let defaults = default_harness_profiles();
        let terminal = defaults.iter().find(|p| p.id == "terminal").unwrap();
        assert_eq!(terminal.name, "Terminal");
        assert_eq!(terminal.harness, "terminal");
    }

    #[test]
    fn harness_profiles_always_contains_terminal_with_none_stored() {
        with_temp_dir(|_| {
            // No preferences.json on disk → only the code-defined defaults.
            let profiles = harness_profiles();
            assert!(profiles.iter().any(|p| p.id == "terminal"));
        });
    }

    #[test]
    fn harness_profiles_round_trips_a_stored_user_profile() {
        with_temp_dir(|_| {
            // Post-#538 shape: a "Kimi via Claude" profile is the `anthropic`
            // executor with a custom provider account supplying the endpoint.
            let custom = HarnessProfile {
                id: "kimi-via-claude".to_string(),
                name: "Kimi (via Claude)".to_string(),
                harness: "anthropic".to_string(),
            };
            save(AppPreferences {
                harness_profiles: vec![custom.clone()],
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;
            let profiles = harness_profiles();
            // Default Terminal plus the appended user profile.
            assert!(profiles.iter().any(|p| p.id == "terminal"));
            assert!(profiles.contains(&custom));
        });
    }

    #[test]
    fn harness_profiles_user_overrides_default_by_id() {
        with_temp_dir(|_| {
            // A stored profile with id "terminal" replaces the default label,
            // but Terminal is still present (override, not append).
            save(AppPreferences {
                harness_profiles: vec![HarnessProfile {
                    id: "terminal".to_string(),
                    name: "My Shell".to_string(),
                    harness: "terminal".to_string(),
                }],
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;
            let profiles = harness_profiles();
            let terminals: Vec<_> = profiles.iter().filter(|p| p.id == "terminal").collect();
            assert_eq!(terminals.len(), 1, "override by id, not append");
            assert_eq!(terminals[0].name, "My Shell");
        });
    }

    #[test]
    fn harness_profiles_new_id_appends() {
        with_temp_dir(|_| {
            save(AppPreferences {
                harness_profiles: vec![HarnessProfile {
                    id: "codex-fast".to_string(),
                    name: "Codex (fast)".to_string(),
                    harness: "codex".to_string(),
                }],
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;
            let profiles = harness_profiles();
            assert!(profiles.iter().any(|p| p.id == "terminal"));
            assert!(profiles.iter().any(|p| p.id == "codex-fast"));
        });
    }

    #[test]
    fn resolve_harness_provider_maps_terminal_profile() {
        with_temp_dir(|_| {
            assert_eq!(resolve_harness_provider("terminal"), Provider::Terminal);
        });
    }

    #[test]
    fn resolve_harness_provider_uses_profile_harness_field() {
        with_temp_dir(|_| {
            // A profile whose harness is "anthropic" resolves to Anthropic,
            // even though its id ("claude-profile") is not a legacy enum value.
            save(AppPreferences {
                harness_profiles: vec![HarnessProfile {
                    id: "claude-profile".to_string(),
                    name: "Claude Profile".to_string(),
                    harness: "anthropic".to_string(),
                }],
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(resolve_harness_provider("claude-profile"), Provider::Anthropic);
        });
    }

    #[test]
    fn resolve_harness_provider_falls_back_through_from_db_str() {
        with_temp_dir(|_| {
            // A legacy enum id with no matching profile resolves directly.
            assert_eq!(resolve_harness_provider("codex"), Provider::Codex);
            // Kimi Code (#918) is now a first-class native executor — a bare
            // `"kimi"` harness id resolves to `Provider::Kimi` directly.
            assert_eq!(resolve_harness_provider("kimi"), Provider::Kimi);
            // The still-retired "minimax" legacy id (issue #538) and any
            // truly-unknown id fall through to the Anthropic executor default.
            assert_eq!(resolve_harness_provider("minimax"), Provider::Anthropic);
            assert_eq!(resolve_harness_provider("totally-unknown"), Provider::Anthropic);
        });
    }

    #[test]
    fn merge_detected_profiles_appends_new_and_reports_count() {
        with_temp_dir(|_| {
            let detected = vec![
                HarnessProfile { id: "claude".into(), name: "Claude Code".into(), harness: "anthropic".into() },
                HarnessProfile { id: "codex".into(), name: "Codex".into(), harness: "codex".into() },
            ];
            let added = merge_detected_profiles(detected).unwrap();
            assert_eq!(added, 2);
            *CACHE.lock().unwrap() = None; // force a disk read
            let profiles = harness_profiles();
            assert!(profiles.iter().any(|p| p.id == "claude"));
            assert!(profiles.iter().any(|p| p.id == "codex"));
            // Terminal default is still present alongside the detected profiles.
            assert!(profiles.iter().any(|p| p.id == "terminal"));
        });
    }

    #[test]
    fn merge_detected_profiles_is_idempotent() {
        with_temp_dir(|_| {
            let detected = vec![HarnessProfile {
                id: "claude".into(),
                name: "Claude Code".into(),
                harness: "anthropic".into(),
            }];
            assert_eq!(merge_detected_profiles(detected.clone()).unwrap(), 1);
            // A second identical scan adds nothing.
            assert_eq!(merge_detected_profiles(detected).unwrap(), 0);
        });
    }

    #[test]
    fn merge_detected_profiles_never_overwrites_a_user_customized_entry() {
        with_temp_dir(|_| {
            // User renamed their Claude profile.
            save(AppPreferences {
                harness_profiles: vec![HarnessProfile {
                    id: "claude".into(),
                    name: "My Claude (subscription)".into(),
                    harness: "anthropic".into(),
                }],
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;
            // The scan re-detects "claude" with the default label — but the
            // user's name must win (id already present → skipped).
            let added = merge_detected_profiles(vec![HarnessProfile {
                id: "claude".into(),
                name: "Claude Code".into(),
                harness: "anthropic".into(),
            }])
            .unwrap();
            assert_eq!(added, 0);
            *CACHE.lock().unwrap() = None;
            let claude = harness_profiles().into_iter().find(|p| p.id == "claude").unwrap();
            assert_eq!(claude.name, "My Claude (subscription)");
        });
    }

    #[test]
    fn app_preferences_defaults_harness_profiles_to_empty_when_key_absent() {
        // Additive wire: an older preferences.json without the key deserializes
        // with an empty Vec rather than failing.
        let prefs: AppPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.harness_profiles, Vec::new());
    }

    // ─── Autopilot pool size ─────────────────────────────────────────────────

    #[test]
    fn app_preferences_defaults_autopilot_pool_size_to_none_when_key_absent() {
        // Additive wire: an older preferences.json loads with no global cap,
        // so upgrading changes no behaviour until the user sets a value.
        let prefs: AppPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.autopilot_pool_size, None);
    }

    #[test]
    fn autopilot_pool_size_round_trips_through_save_and_read() {
        with_temp_dir(|_| {
            assert_eq!(autopilot_pool_size(), None);
            let mut prefs = load().unwrap();
            prefs.autopilot_pool_size = Some(4);
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None; // force a disk read
            assert_eq!(autopilot_pool_size(), Some(4));
        });
    }

    // ─── Harness order (issue #573) ──────────────────────────────────────────

    #[test]
    fn app_preferences_defaults_harness_order_to_empty_when_key_absent() {
        let prefs: AppPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.harness_order, Vec::<String>::new());
    }

    #[test]
    fn harness_order_round_trips_through_set_and_read() {
        with_temp_dir(|_| {
            assert_eq!(harness_order(), Vec::<String>::new());
            set_harness_order(vec!["codex".into(), "claude".into()]).unwrap();
            *CACHE.lock().unwrap() = None; // force a disk read
            assert_eq!(harness_order(), vec!["codex".to_string(), "claude".to_string()]);
        });
    }

    /// Issue #573 regression: reordering while a harness is uninstalled must NOT
    /// evict its saved slot. The frontend only sends installed ids, so a plain
    /// overwrite would drop the absent one; the merge keeps it pinned.
    #[test]
    fn set_harness_order_preserves_uninstalled_harness_slot() {
        with_temp_dir(|_| {
            // Saved order had minimax between claude and codex.
            set_harness_order(vec!["claude".into(), "minimax".into(), "codex".into()]).unwrap();
            // minimax gets uninstalled; the user drags codex above claude. The UI
            // can only send the installed harnesses, so minimax is absent here.
            set_harness_order(vec!["codex".into(), "claude".into()]).unwrap();
            *CACHE.lock().unwrap() = None;
            // minimax keeps its middle slot; codex/claude swap around it.
            assert_eq!(
                harness_order(),
                vec!["codex".to_string(), "minimax".to_string(), "claude".to_string()],
            );
        });
    }

    #[test]
    fn merge_harness_order_refills_present_slots_keeps_dormant() {
        let stored = vec!["claude".to_string(), "minimax".to_string(), "codex".to_string()];
        let merged = merge_harness_order(&stored, vec!["codex".into(), "claude".into()]);
        assert_eq!(merged, vec!["codex", "minimax", "claude"]);
    }

    #[test]
    fn merge_harness_order_appends_brand_new_ids() {
        let stored = vec!["claude".to_string(), "codex".to_string()];
        let merged =
            merge_harness_order(&stored, vec!["codex".into(), "claude".into(), "newbie".into()]);
        assert_eq!(merged, vec!["codex", "claude", "newbie"]);
    }

    #[test]
    fn set_harness_order_drops_duplicate_ids() {
        with_temp_dir(|_| {
            set_harness_order(vec!["claude".into(), "claude".into(), "codex".into()]).unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(harness_order(), vec!["claude".to_string(), "codex".to_string()]);
        });
    }

    #[test]
    fn set_harness_order_filters_out_terminal() {
        with_temp_dir(|_| {
            // Terminal is forced last by the ordering logic, so it's never stored.
            set_harness_order(vec!["claude".into(), "terminal".into(), "codex".into()]).unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(harness_order(), vec!["claude".to_string(), "codex".to_string()]);
        });
    }

    // ─── Proxied Provider order (issue #577) ────────────────────────────────

    #[test]
    fn app_preferences_defaults_proxied_provider_order_to_empty_when_key_absent() {
        // Additive wire: an older preferences.json without the key deserializes
        // with an empty Vec rather than failing.
        let prefs: AppPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.proxied_provider_order, Vec::<ProxiedProviderOrder>::new());
    }

    #[test]
    fn proxied_order_for_returns_none_when_harness_unset() {
        with_temp_dir(|_| {
            // No stored order → the per-harness helper returns None so the
            // backend `order_proxied_children` falls through to natural order.
            assert_eq!(proxied_order_for("claude"), None);
        });
    }

    #[test]
    fn set_proxied_provider_order_round_trips_through_save_and_read() {
        with_temp_dir(|_| {
            set_proxied_provider_order("claude".into(), vec!["kimi".into(), "minimax".into()]).unwrap();
            *CACHE.lock().unwrap() = None; // force a disk read
            assert_eq!(
                proxied_order_for("claude"),
                Some(vec!["kimi".to_string(), "minimax".to_string()]),
            );
            assert_eq!(
                proxied_provider_order(),
                vec![ProxiedProviderOrder {
                    harness_id: "claude".to_string(),
                    provider_ids: vec!["kimi".to_string(), "minimax".to_string()],
                }]
            );
        });
    }

    #[test]
    fn set_proxied_provider_order_upserts_by_harness_id() {
        with_temp_dir(|_| {
            // First harness writes its own entry.
            set_proxied_provider_order("claude".into(), vec!["minimax".into(), "kimi".into()]).unwrap();
            // Second harness appends, first is untouched.
            set_proxied_provider_order("codex".into(), vec!["minimax".into()]).unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(
                proxied_order_for("claude"),
                Some(vec!["minimax".to_string(), "kimi".to_string()]),
            );
            assert_eq!(
                proxied_order_for("codex"),
                Some(vec!["minimax".to_string()]),
            );
            // Re-setting claude overwrites just that entry.
            set_proxied_provider_order("claude".into(), vec!["kimi".into(), "minimax".into()]).unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(
                proxied_order_for("claude"),
                Some(vec!["kimi".to_string(), "minimax".to_string()]),
            );
            assert_eq!(
                proxied_order_for("codex"),
                Some(vec!["minimax".to_string()]),
            );
            // Two harness entries persist in the underlying vector.
            assert_eq!(proxied_provider_order().len(), 2);
        });
    }

    #[test]
    fn set_proxied_provider_order_drops_duplicate_ids() {
        with_temp_dir(|_| {
            // A malformed UI send with duplicates: first occurrence wins.
            set_proxied_provider_order(
                "claude".into(),
                vec!["minimax".into(), "kimi".into(), "minimax".into()],
            )
            .unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(
                proxied_order_for("claude"),
                Some(vec!["minimax".to_string(), "kimi".to_string()]),
            );
        });
    }

    #[test]
    fn set_proxied_provider_order_drops_ids_not_currently_paired() {
        with_temp_dir(|_| {
            // Drop a stale UI send carrying an id whose pairing has been
            // detached (or was never attached). Defends the ordering seam
            // against silently persisting dead entries that would resurrect
            // on a later re-attach.
            set_proxied_provider_order(
                "claude".into(),
                vec!["minimax".into(), "ghost".into(), "kimi".into()],
            )
            .unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(
                proxied_order_for("claude"),
                Some(vec!["minimax".to_string(), "kimi".to_string()]),
            );
        });
    }

    #[test]
    fn set_proxied_provider_order_normalises_empty_to_drop_entry() {
        with_temp_dir(|_| {
            // An empty provider list is the user's signal "I have no order
            // preference" — drop the entry so a later attach re-derives
            // natural order rather than resurrecting an empty stored slot.
            set_proxied_provider_order("claude".into(), vec!["minimax".into()]).unwrap();
            set_proxied_provider_order("claude".into(), vec![]).unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(proxied_order_for("claude"), None);
            assert!(proxied_provider_order().is_empty());
        });
    }

    // ─── Provider accounts (issue #537) ──────────────────────────────────────

    fn custom_account(id: &str) -> ProviderAccount {
        ProviderAccount {
            id: id.to_string(),
            name: format!("Custom {id}"),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
            base_url: Some("https://example.com/v1".to_string()),
            model_tiers: ModelTiers::default(),
            models: vec!["model-a".to_string()],
        }
    }

    #[test]
    fn default_provider_accounts_cover_the_builtin_providers() {
        let ids: Vec<_> = default_provider_accounts().into_iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["anthropic", "codex", "agy", "grok", "kimi", "opencode", "minimax", "openrouter", "kimi-via-claude"]);
        // Pay-as-you-go First-class Model Providers (Claude-compatible) are
        // minimax / openrouter / kimi-via-claude. Self-auth Native Agent
        // Harnesses (anthropic / codex / agy / grok / kimi / opencode) hold
        // no creds in Buildmesh — their login lives in the harness's own
        // config dir (e.g. `~/.kimi/config.toml` for the Kimi Code CLI,
        // issue #918).
        let by_id = |id: &str| default_provider_accounts().into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(by_id("minimax").billing_mode, BillingMode::PayAsYouGo);
        assert!(by_id("minimax").claude_compatible);
        assert!(by_id("openrouter").claude_compatible);
        assert!(by_id("kimi-via-claude").claude_compatible);
        assert!(!by_id("anthropic").claude_compatible);
        assert!(!by_id("codex").claude_compatible);
        assert!(!by_id("agy").claude_compatible);
        assert!(!by_id("grok").claude_compatible);
        assert!(!by_id("kimi").claude_compatible);
        assert!(!by_id("opencode").claude_compatible);
        // MiniMax ships its absorbed-cwrap-parity base URL + tier map;
        // OpenRouter ships its API base URL with an empty tier map (the user
        // picks providers/models via the 5-tier UI fields); Kimi Code ships
        // no base URL (self-auth); kimi-via-claude ships the Moonshot
        // endpoint + the tier map defined in `kimi_default_tiers`.
        assert_eq!(by_id("minimax").base_url.as_deref(), Some("https://api.minimax.io/anthropic"));
        assert_eq!(by_id("openrouter").base_url.as_deref(), Some("https://openrouter.ai/api"));
        assert_eq!(by_id("kimi").base_url, None);
        assert_eq!(by_id("kimi-via-claude").base_url.as_deref(), Some("https://api.moonshot.ai/anthropic"));
        assert_eq!(by_id("minimax").model_tiers.default.as_deref(), Some("MiniMax-M3[1m]"));
        assert_eq!(by_id("openrouter").model_tiers, ModelTiers::default());
        assert_eq!(by_id("kimi-via-claude").model_tiers.default.as_deref(), Some("kimi-k3"));
        // All built-ins land enabled by default — opt-in for the keyed ones
        // is via missing API key rather than a flag.
        assert!(by_id("anthropic").enabled);
        assert!(by_id("codex").enabled);
        assert!(by_id("agy").enabled);
        assert!(by_id("grok").enabled);
        assert!(by_id("kimi").enabled);
        assert!(by_id("opencode").enabled);
        assert!(by_id("openrouter").enabled);
        assert!(by_id("kimi-via-claude").enabled);
    }

    #[test]
    fn app_preferences_defaults_provider_accounts_to_empty_when_key_absent() {
        let prefs: AppPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.provider_accounts, Vec::new());
    }

    #[test]
    fn billing_mode_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&BillingMode::Plan).unwrap(), "\"plan\"");
        assert_eq!(
            serde_json::to_string(&BillingMode::PayAsYouGo).unwrap(),
            "\"pay_as_you_go\""
        );
    }

    #[test]
    fn merge_provider_accounts_override_by_id_and_append() {
        let defaults = default_provider_accounts();
        let stored = vec![
            // Override the built-in minimax (disable it).
            ProviderAccount {
                id: "minimax".to_string(),
                name: "MiniMax".to_string(),
                enabled: false,
                billing_mode: BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: None,
                base_url: None,
                model_tiers: ModelTiers::default(),
                models: Vec::new(),
            },
            // Append a custom account.
            custom_account("deepseek"),
        ];
        let merged = merge_provider_accounts(defaults, stored);
        // Nine First-class Model Providers + Native Agent Harnesses (anthropic/
        // codex/agy/grok/opencode/minimax/kimi/openrouter/kimi-via-claude),
        // no duplicate minimax, plus the custom one.
        assert_eq!(merged.iter().filter(|a| a.id == "minimax").count(), 1);
        assert!(!merged.iter().find(|a| a.id == "minimax").unwrap().enabled);
        assert!(merged.iter().any(|a| a.id == "deepseek"));
        assert_eq!(merged.len(), 10);
    }

    #[test]
    fn merge_provider_accounts_rederives_claude_compatible_from_id() {
        // A stored override that lies about claude_compatible (e.g. an older
        // preferences.json, or a stale UI payload) is corrected on read.
        let stored = vec![
            // Self-auth built-in falsely flagged compatible → forced false.
            ProviderAccount { claude_compatible: true, ..custom_account("anthropic") },
            // Custom account with the default-false flag → forced true.
            ProviderAccount { claude_compatible: false, ..custom_account("deepseek") },
        ];
        let merged = merge_provider_accounts(default_provider_accounts(), stored);
        assert!(!merged.iter().find(|a| a.id == "anthropic").unwrap().claude_compatible);
        assert!(merged.iter().find(|a| a.id == "deepseek").unwrap().claude_compatible);
    }

    #[test]
    fn provider_accounts_round_trips_a_disabled_override_and_custom() {
        with_temp_dir(|_| {
            save(AppPreferences {
                provider_accounts: vec![
                    // Disable a built-in via stored override.
                    ProviderAccount {
                        id: "codex".to_string(),
                        name: "OpenAI / Codex".to_string(),
                        enabled: false,
                        billing_mode: BillingMode::Plan,
                        claude_compatible: false,
                        api_key: None,
                        base_url: None,
                        model_tiers: ModelTiers::default(),
                        models: Vec::new(),
                    },
                    custom_account("glm"),
                ],
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;

            let all = provider_accounts();
            // Built-ins always present; the codex override carries enabled=false;
            // the custom account is appended. (The enabled→poll filtering itself
            // lives in commands::usage::accounts_to_poll.)
            assert!(all.iter().any(|a| a.id == "glm"));
            assert!(all.iter().any(|a| a.id == "anthropic"));
            assert!(!all.iter().find(|a| a.id == "codex").unwrap().enabled);
        });
    }

    #[test]
    fn minimax_api_key_resolved_prefers_account_then_legacy_field() {
        with_temp_dir(|_| {
            // Legacy flat field only → read-through fallback.
            save(AppPreferences {
                minimax_api_key: Some("legacy-key".to_string()),
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(minimax_api_key_resolved(), Some("legacy-key".to_string()));

            // Account key present → wins over the legacy field.
            save(AppPreferences {
                minimax_api_key: Some("legacy-key".to_string()),
                provider_accounts: vec![ProviderAccount {
                    id: "minimax".to_string(),
                    name: "MiniMax".to_string(),
                    enabled: true,
                    billing_mode: BillingMode::PayAsYouGo,
                    claude_compatible: true,
                    api_key: Some("account-key".to_string()),
                    base_url: None,
                    model_tiers: ModelTiers::default(),
                    models: Vec::new(),
                }],
                ..Default::default()
            })
            .unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(minimax_api_key_resolved(), Some("account-key".to_string()));

            // Nothing set → None.
            save(AppPreferences::default()).unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(minimax_api_key_resolved(), None);
        });
    }

    #[test]
    fn upsert_account_stores_it_without_a_paired_profile() {
        // The spawn menu is derived from accounts now (see
        // commands::agent::compose_provider_menu), so upsert never materializes a
        // harness profile — for a custom account or a built-in alike (#568).
        let mut prefs = AppPreferences::default();
        upsert_provider_account(&mut prefs, custom_account("deepseek"));
        assert!(prefs.provider_accounts.iter().any(|a| a.id == "deepseek"));
        assert!(prefs.harness_profiles.is_empty(), "no paired profile is created");
    }

    #[test]
    fn upsert_existing_account_overrides_in_place() {
        let mut prefs = AppPreferences::default();
        upsert_provider_account(&mut prefs, custom_account("glm"));
        let mut updated = custom_account("glm");
        updated.enabled = false;
        upsert_provider_account(&mut prefs, updated);
        assert_eq!(prefs.provider_accounts.iter().filter(|a| a.id == "glm").count(), 1);
        assert!(!prefs.provider_accounts.iter().find(|a| a.id == "glm").unwrap().enabled);
    }

    #[test]
    fn remove_account_drops_it() {
        let mut prefs = AppPreferences::default();
        upsert_provider_account(&mut prefs, custom_account("deepseek"));
        remove_provider_account(&mut prefs, "deepseek");
        assert!(!prefs.provider_accounts.iter().any(|a| a.id == "deepseek"));
    }

    // ─── Spawn-time backend env injection (issue #538) ───────────────────────

    #[test]
    fn provider_account_env_injects_base_url_token_and_model_for_custom_account() {
        // custom_account: base_url=Some(.../v1), api_key=Some(sk-test), models=[model-a].
        let account = custom_account("deepseek");
        let env = provider_account_env(Some(&account));
        assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://example.com/v1".to_string())));
        assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-test".to_string())));
        assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "model-a".to_string())));
    }

    #[test]
    fn provider_account_env_pins_alias_models_and_timeout_for_custom_endpoint() {
        // A custom endpoint must map every claude model alias onto a configured
        // model (else Claude Code's built-in claude-* slugs 404 there) and use the
        // long timeout — the routing the deleted MiniMax/Kimi adapters provided.
        // With one model, the cheap "small/fast" alias maps to that same model.
        let account = custom_account("deepseek");
        let env = provider_account_env(Some(&account));
        for k in [
            "ANTHROPIC_SMALL_FAST_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ] {
            assert_eq!(
                env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str()),
                Some("model-a"),
                "{k} must be pinned to the configured model for a custom endpoint"
            );
        }
        assert!(env.contains(&("API_TIMEOUT_MS".to_string(), "3000000".to_string())));
    }

    #[test]
    fn provider_account_env_uses_second_model_as_small_fast_when_present() {
        let mut account = custom_account("glm");
        account.models = vec!["GLM-Big".to_string(), "GLM-Mini".to_string()];
        let env = provider_account_env(Some(&account));
        assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "GLM-Big".to_string())));
        assert!(env.contains(&("ANTHROPIC_SMALL_FAST_MODEL".to_string(), "GLM-Mini".to_string())));
        // The heavyweight aliases keep the primary model.
        assert!(env.contains(&("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), "GLM-Big".to_string())));
    }

    #[test]
    fn provider_account_env_no_alias_overrides_on_default_anthropic_endpoint() {
        // A custom account with a model but NO base_url targets the default
        // Anthropic endpoint, where claude-* slugs are valid — pin only the
        // primary model and leave Claude Code's own alias routing and timeout.
        let account = ProviderAccount {
            id: "my-claude".to_string(),
            name: "My Claude".to_string(),
            enabled: true,
            billing_mode: BillingMode::Plan,
            claude_compatible: true,
            api_key: Some("sk-ant".to_string()),
            base_url: None,
            model_tiers: ModelTiers::default(),
            models: vec!["claude-opus-4-8".to_string()],
        };
        let env = provider_account_env(Some(&account));
        assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "claude-opus-4-8".to_string())));
        assert!(!env.iter().any(|(k, _)| k == "ANTHROPIC_SMALL_FAST_MODEL"));
        assert!(!env.iter().any(|(k, _)| k == "API_TIMEOUT_MS"));
    }

    #[test]
    fn provider_account_env_empty_for_builtin_without_endpoint_or_absent() {
        // The built-in anthropic account carries no base_url/api_key → no overrides.
        let anthropic = default_provider_accounts().into_iter().find(|a| a.id == "anthropic").unwrap();
        assert!(provider_account_env(Some(&anthropic)).is_empty());
        // No matching account at all → empty.
        assert!(provider_account_env(None).is_empty());
    }

    #[test]
    fn provider_account_env_skips_blank_fields() {
        // A partially-filled account must not emit a blank ANTHROPIC_BASE_URL —
        // claude would treat "" as a real (broken) endpoint.
        let account = ProviderAccount {
            id: "x".to_string(),
            name: "X".to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some(String::new()),
            base_url: Some(String::new()),
            model_tiers: ModelTiers::default(),
            models: vec![String::new()],
        };
        assert!(provider_account_env(Some(&account)).is_empty());
    }

    #[test]
    fn provider_account_env_uses_model_tiers_over_flat_models() {
        // model_tiers wins when set; the deprecated flat list is ignored (#567).
        let mut account = custom_account("glm");
        account.models = vec!["IGNORED".to_string()];
        account.model_tiers = ModelTiers {
            default: Some("GLM-4.6".to_string()),
            small_fast: Some("GLM-4-Flash".to_string()),
            sonnet: Some("GLM-4.6".to_string()),
            opus: Some("GLM-4.6-Max".to_string()),
            fable: None,
            haiku: Some("GLM-4-Flash".to_string()),
        };
        let env = provider_account_env(Some(&account));
        assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "GLM-4.6".to_string())));
        assert!(env.contains(&("ANTHROPIC_SMALL_FAST_MODEL".to_string(), "GLM-4-Flash".to_string())));
        assert!(env.contains(&("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), "GLM-4.6-Max".to_string())));
        assert!(env.contains(&("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), "GLM-4.6".to_string())));
        assert!(env.contains(&("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), "GLM-4-Flash".to_string())));
        // Fable unset → falls back to the Opus pick, not the primary.
        assert!(env.contains(&("ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(), "GLM-4.6-Max".to_string())));
        assert!(!env.iter().any(|(_, v)| v == "IGNORED"), "flat models is superseded by tiers");
    }

    /// The Fable alias (Claude 5) is pinned for custom endpoints: an explicit
    /// `fable` tier wins; unset falls back to the opus pick (the nearest tier
    /// below); the built-in first-class maps default fable = opus.
    #[test]
    fn anthropic_surface_env_pins_fable_alias_with_opus_fallback() {
        let mut tiers = kimi_default_tiers();
        assert_eq!(tiers.fable, tiers.opus, "built-in kimi defaults fable to the opus pick");
        assert_eq!(
            minimax_default_tiers().fable,
            minimax_default_tiers().opus,
            "built-in minimax defaults fable to the opus pick"
        );

        tiers.fable = Some("kimi-fable-x".to_string());
        let env = anthropic_surface_env(Some("https://api.moonshot.ai/anthropic"), Some("sk"), &tiers);
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("ANTHROPIC_DEFAULT_FABLE_MODEL"), Some("kimi-fable-x"));

        tiers.fable = None;
        let env = anthropic_surface_env(Some("https://api.moonshot.ai/anthropic"), Some("sk"), &tiers);
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("ANTHROPIC_DEFAULT_FABLE_MODEL"),
            tiers.opus.as_deref(),
            "unset fable must fall back to the opus tier"
        );
    }

    #[test]
    fn builtin_minimax_account_reproduces_claude_code_model_routing() {
        // The regression that motivated this change: a keyed MiniMax must pin the
        // full per-tier model map (byte-for-byte cwrap parity) so Claude Code never
        // sends a `claude-opus-*` slug to MiniMax.
        let mut minimax = default_provider_accounts().into_iter().find(|a| a.id == "minimax").unwrap();
        minimax.api_key = Some("sk-mm".to_string());
        let env = provider_account_env(Some(&minimax));
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("ANTHROPIC_BASE_URL"), Some("https://api.minimax.io/anthropic"));
        assert_eq!(get("ANTHROPIC_AUTH_TOKEN"), Some("sk-mm"));
        assert_eq!(get("ANTHROPIC_MODEL"), Some("MiniMax-M3[1m]"));
        assert_eq!(get("ANTHROPIC_SMALL_FAST_MODEL"), Some("MiniMax-M2.7"));
        assert_eq!(get("ANTHROPIC_DEFAULT_SONNET_MODEL"), Some("MiniMax-M3[1m]"));
        assert_eq!(get("ANTHROPIC_DEFAULT_OPUS_MODEL"), Some("MiniMax-M3[1m]"));
        assert_eq!(get("ANTHROPIC_DEFAULT_FABLE_MODEL"), Some("MiniMax-M3[1m]"));
        assert_eq!(get("ANTHROPIC_DEFAULT_HAIKU_MODEL"), Some("MiniMax-M2.7"));
        assert_eq!(get("API_TIMEOUT_MS"), Some("3000000"));
    }

    #[test]
    fn builtin_kimi_account_is_self_auth_and_emits_no_env() {
        // Wayfinder #918 flipped the built-in `kimi` from a Claude-compatible
        // Moonshot LLM endpoint account (claude_compatible=true, base_url=
        // moonshot.ai/anthropic) to a self-auth native-harness account
        // (Kimi Code CLI handles auth via `~/.kimi/config.toml`). The
        // account therefore has no base_url and no api_key, so the spawn
        // path's `provider_account_env` produces no `ANTHROPIC_*` overrides
        // — the `kimi` binary is launched as-is and reads its own config.
        // This is what `provider_account_env_empty_for_builtin_without_endpoint_or_absent`
        // already pins for the Anthropic account; this test pins the same
        // invariant for Kimi specifically so a future refactor that
        // accidentally re-promotes Kimi to Claude-compatible trips it.
        let kimi = default_provider_accounts().into_iter().find(|a| a.id == "kimi").unwrap();
        assert!(!kimi.claude_compatible, "kimi built-in must be self_auth (wayfinder #918)");
        assert_eq!(kimi.base_url, None, "self-auth kimi must have no base_url");
        assert_eq!(kimi.api_key, None, "self-auth kimi must have no api_key");
        assert!(kimi.enabled, "self-auth kimi lands enabled by default (#918)");
        assert!(
            provider_account_env(Some(&kimi)).is_empty(),
            "self-auth kimi must inject no ANTHROPIC_* env — got {:?}",
            provider_account_env(Some(&kimi))
        );
    }

    #[test]
    fn resolve_provider_env_reads_the_account_by_profile_id() {
        // AC: ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN are injected for a
        // Claude-compatible provider. The node's stored provider id is the account
        // id, so resolving by that id finds the account's endpoint.
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, custom_account("deepseek"));
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;

            let env = resolve_provider_env("deepseek");
            assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://example.com/v1".to_string())));
            assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-test".to_string())));
        });
    }

    #[test]
    fn resolve_provider_env_for_keyed_builtin_minimax_injects_claude_code_env() {
        // The real spawn path: a node stored with provider="minimax" resolves the
        // built-in MiniMax account (merged with the user's stored key) and gets the
        // full Claude Code model routing — the end-to-end fix for "can't spawn MiniMax".
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            // Store only the key; base_url + tiers come from the code default.
            upsert_provider_account(
                &mut prefs,
                ProviderAccount {
                    api_key: Some("sk-mm".to_string()),
                    ..default_provider_accounts().into_iter().find(|a| a.id == "minimax").unwrap()
                },
            );
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;

            let env = resolve_provider_env("minimax");
            assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://api.minimax.io/anthropic".to_string())));
            assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-mm".to_string())));
            assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "MiniMax-M3[1m]".to_string())));
        });
    }

    #[test]
    fn resolve_provider_env_empty_for_anthropic_default_and_unknown() {
        with_temp_dir(|_| {
            // Built-in Anthropic subscription → no overrides → vanilla claude.
            assert!(resolve_provider_env("anthropic").is_empty());
            // An id with no account → empty (clean slate after the env reset).
            assert!(resolve_provider_env("totally-unknown").is_empty());
        });
    }

    // ─── Compatible API surfaces + per-pairing config (issue #576) ───────────

    #[test]
    fn api_surface_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&ApiSurface::Anthropic).unwrap(), "\"anthropic\"");
        assert_eq!(serde_json::to_string(&ApiSurface::OpenAI).unwrap(), "\"openai\"");
    }

    #[test]
    fn first_class_surfaces_publishes_both_surfaces_for_minimax() {
        let surfaces = first_class_surfaces("minimax");
        let by = |s: ApiSurface| surfaces.iter().find(|e| e.surface == s).unwrap();
        // Anthropic surface matches the long-standing Claude Code account default.
        assert_eq!(by(ApiSurface::Anthropic).base_url, "https://api.minimax.io/anthropic");
        assert_eq!(by(ApiSurface::Anthropic).model_tiers.default.as_deref(), Some("MiniMax-M3[1m]"));
        // OpenAI surface (best-effort) carries the v1 endpoint and a single model.
        assert_eq!(by(ApiSurface::OpenAI).base_url, "https://api.minimax.io/v1");
        assert_eq!(by(ApiSurface::OpenAI).model_tiers.default.as_deref(), Some("MiniMax-M3[1m]"));
        // A non-first-class id publishes nothing.
        assert!(first_class_surfaces("deepseek").is_empty());
        assert!(first_class_surfaces("anthropic").is_empty());
    }

    #[test]
    fn surface_for_executor_maps_only_proxy_capable_harnesses() {
        assert_eq!(surface_for_executor(Provider::Anthropic), Some(ApiSurface::Anthropic));
        assert_eq!(surface_for_executor(Provider::Codex), Some(ApiSurface::OpenAI));
        // Terminal/native-only harnesses speak no proxy surface.
        assert_eq!(surface_for_executor(Provider::Terminal), None);
    }

    #[test]
    fn provider_surfaces_first_class_vs_generic_vs_self_auth() {
        let by_id = |id: &str| default_provider_accounts().into_iter().find(|a| a.id == id).unwrap();
        // First-class → its published set (both surfaces).
        assert_eq!(provider_surfaces(&by_id("minimax")), vec![ApiSurface::Anthropic, ApiSurface::OpenAI]);
        // Generic Claude-compatible → exactly one Anthropic surface.
        assert_eq!(provider_surfaces(&custom_account("deepseek")), vec![ApiSurface::Anthropic]);
        // Self-auth built-in → never proxied.
        assert!(provider_surfaces(&by_id("anthropic")).is_empty());
    }

    #[test]
    fn openai_surface_env_emits_openai_vars_only() {
        let tiers = ModelTiers { default: Some("MiniMax-M3[1m]".into()), ..ModelTiers::default() };
        let env = openai_surface_env(Some("https://api.minimax.io/v1"), Some("sk-mm"), &tiers);
        assert!(env.contains(&("OPENAI_BASE_URL".to_string(), "https://api.minimax.io/v1".to_string())));
        assert!(env.contains(&("OPENAI_API_KEY".to_string(), "sk-mm".to_string())));
        assert!(env.contains(&("OPENAI_MODEL".to_string(), "MiniMax-M3[1m]".to_string())));
        // No ANTHROPIC_* leak onto the OpenAI surface, and no per-tier aliases.
        assert!(!env.iter().any(|(k, _)| k.starts_with("ANTHROPIC_")));
        assert!(!env.iter().any(|(k, _)| k == "OPENAI_SMALL_FAST_MODEL"));
    }

    #[test]
    fn surface_env_dispatches_by_surface() {
        let tiers = ModelTiers { default: Some("m".into()), ..ModelTiers::default() };
        let anth = surface_env(ApiSurface::Anthropic, Some("https://x/anthropic"), Some("k"), &tiers);
        assert!(anth.iter().any(|(k, _)| k == "ANTHROPIC_BASE_URL"));
        let oai = surface_env(ApiSurface::OpenAI, Some("https://x/v1"), Some("k"), &tiers);
        assert!(oai.iter().any(|(k, _)| k == "OPENAI_BASE_URL"));
    }

    /// Any Claude-compatible *custom* endpoint must force `ANTHROPIC_API_KEY=""`
    /// so a shell-set Anthropic key can't make Claude Code fall back to direct
    /// Anthropic auth. Required by OpenRouter's Anthropic Skin; defended for
    /// every custom endpoint (MiniMax, OpenRouter, future generic accounts
    /// alike) — matches `cwrap`'s behaviour. The default-Anthropic
    /// path (no base_url) must NOT emit the var, so a shell-set Anthropic
    /// key still flows through for direct Anthropic usage.
    #[test]
    fn anthropic_surface_env_force_blanks_api_key_for_custom_endpoints_only() {
        let tiers = ModelTiers { default: Some("m".into()), ..ModelTiers::default() };
        // First-class built-in: OpenRouter.
        let custom_first_class = anthropic_surface_env(Some("https://openrouter.ai/api"), Some("sk-or-x"), &tiers);
        assert!(
            custom_first_class.contains(&("ANTHROPIC_API_KEY".to_string(), String::new())),
            "OpenRouter (first-class Anthropic-skin endpoint) must force ANTHROPIC_API_KEY=\"\" so Claude Code doesn't fall back to direct Anthropic auth (got {:?})",
            custom_first_class
        );
        // Generic custom endpoint: same defence applies. A user with a
        // hand-rolled relay that expects to receive `ANTHROPIC_API_KEY` from
        // the user's shell would already be violating the Anthropic Skin
        // contract — cwrap already blanked it for every custom endpoint,
        // and breaking that compat is a deliberate behaviour shift.
        let custom_generic = anthropic_surface_env(Some("https://relay.example.com/anthropic"), Some("sk-relay"), &tiers);
        assert!(
            custom_generic.contains(&("ANTHROPIC_API_KEY".to_string(), String::new())),
            "Generic custom endpoint must ALSO force ANTHROPIC_API_KEY=\"\" — the defence is endpoint-scope, not id-scope (got {:?})",
            custom_generic
        );
        // Default-Anthropic path (no base_url): the var must NOT be emitted
        // so a shell-set Anthropic key still flows through for direct usage.
        let default_path = anthropic_surface_env(None, None, &tiers);
        assert!(
            !default_path.iter().any(|(k, _)| k == "ANTHROPIC_API_KEY"),
            "default-Anthropic path must NOT emit ANTHROPIC_API_KEY so a shell-set key still flows through"
        );
    }

    /// Spawn-time preflight: refuses to spawn against a custom Claude-compatible
    /// endpoint whose `model_tiers.default` is empty. Without this guard,
    /// `claude` sends its hardcoded `claude-3-5-sonnet-<date>` to OpenRouter
    /// and gets a 400 (OpenRouter expects `provider/model` slugs); the user
    /// would see only a server-side `tracing::warn`. Surface the error at the
    /// spawn path so the UI can prompt for the missing model field.
    ///
    /// The preflight runs BEFORE `resolve_provider_env` and reuses the same
    /// account lookup; mirroring the lookup keeps gate and builder in sync.
    #[test]
    fn preflight_account_env_fails_for_custom_endpoint_with_empty_default_tier() {
        let empty_tiers = ModelTiers::default();
        let account = ProviderAccount {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-or-x".to_string()),
            base_url: Some("https://openrouter.ai/api".to_string()),
            model_tiers: empty_tiers,
            models: Vec::new(),
        };
        let err = preflight_account_env(Some(&account)).unwrap_err();
        assert!(
            err.contains("Default model") && err.contains("openrouter"),
            "preflight should name the missing tier and the account id, got: {err}"
        );
    }

    /// Spawn-time preflight accepts a custom endpoint whose `default` tier is set.
    #[test]
    fn preflight_account_env_passes_for_custom_endpoint_with_default_tier_filled() {
        let tiers = ModelTiers {
            default: Some("anthropic/claude-3-5-sonnet-latest".to_string()),
            ..ModelTiers::default()
        };
        let account = ProviderAccount {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-or-x".to_string()),
            base_url: Some("https://openrouter.ai/api".to_string()),
            model_tiers: tiers,
            models: Vec::new(),
        };
        assert!(
            preflight_account_env(Some(&account)).is_ok(),
            "default-tier populated custom endpoint must pass preflight"
        );
    }

    /// Default-Anthropic (no `base_url`) path is unaffected — openrouter
    /// doesn't *appear* on this path, but `preflight(None)` mirrors what
    /// happens when `provider_accounts()` returns nothing for an unknown id.
    #[test]
    fn preflight_account_env_passes_for_no_account() {
        assert!(preflight_account_env(None).is_ok());
    }

    /// The extracted Anthropic-tier-source helper is the single source for the
    /// derived-pairing + spawn-preflight precedence. Account tiers win; the
    /// first-class published Anthropic endpoint fills a fully-cleared account;
    /// a Generic provider's account tiers are the only source.
    #[test]
    fn anthropic_tiers_for_precedence_is_account_then_published() {
        // Account set → account wins (Providers-page edit reaches the next spawn).
        let edited = ProviderAccount {
            model_tiers: ModelTiers {
                default: Some("edited".into()),
                ..ModelTiers::default()
            },
            ..custom_account("deepseek")
        };
        assert_eq!(anthropic_tiers_for(&edited).default.as_deref(), Some("edited"));

        // First-class account fully cleared by the user → published surface
        // fills defaults so ANTHROPIC_MODEL still emits (preflight gate relies
        // on this — see `preflight_account_env_passes_for_cleared_first_class_account`).
        let mut cleared = default_provider_accounts().into_iter().find(|a| a.id == "minimax").unwrap();
        cleared.model_tiers = ModelTiers::default();
        assert_eq!(anthropic_tiers_for(&cleared).default.as_deref(), Some("MiniMax-M3[1m]"));

        // Generic account cleared → empty (the pre-pairings behaviour).
        let generic_cleared = ProviderAccount {
            model_tiers: ModelTiers::default(),
            models: Vec::new(),
            ..custom_account("deepseek")
        };
        assert!(anthropic_tiers_for(&generic_cleared).is_empty());
    }

    /// First-class surfaces (minimax/kimi) supply their OWN populated
    /// `model_tiers` via `first_class_surfaces`. A user-cleared built-in
    /// account (`model_tiers == ModelTiers::default()`) must NOT false-
    /// positive — the env builder would still emit `ANTHROPIC_MODEL`
    /// from the surface's tier map, so the spawn should be permitted.
    #[test]
    fn preflight_account_env_passes_for_cleared_first_class_account() {
        let cleared = ModelTiers::default();
        let account = ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-mm".to_string()),
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            model_tiers: cleared,
            models: Vec::new(),
        };
        assert!(
            preflight_account_env(Some(&account)).is_ok(),
            "first-class surfaces supply populated tiers via `first_class_surfaces`; the spawn's env builder emits ANTHROPIC_MODEL — preflight must not double-gate"
        );
    }

    /// Surface lookup used by the pure pairing tests — mirrors `harness_surface`
    /// without touching disk (claude→Anthropic, codex→OpenAI, else None).
    fn test_surface_of(harness_id: &str) -> Option<ApiSurface> {
        match harness_id {
            "claude" => Some(ApiSurface::Anthropic),
            "codex" => Some(ApiSurface::OpenAI),
            _ => None,
        }
    }

    #[test]
    fn resolve_pairing_prefers_a_stored_pairing() {
        let account = custom_account("minimax");
        let stored = vec![ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://custom/v1".into()),
            model_tiers: ModelTiers { default: Some("override".into()), ..ModelTiers::default() },
        }];
        let got = resolve_pairing("codex", &account, &stored, test_surface_of).unwrap();
        assert_eq!(got.base_url.as_deref(), Some("https://custom/v1"));
        assert_eq!(got.model_tiers.default.as_deref(), Some("override"));
    }

    #[test]
    fn resolve_pairing_derives_first_class_openai_default() {
        // MiniMax via Codex with no stored pairing → derived from the published
        // OpenAI endpoint (the AC#1 multi-surface case).
        let account = default_provider_accounts().into_iter().find(|a| a.id == "minimax").unwrap();
        let got = resolve_pairing("codex", &account, &[], test_surface_of).unwrap();
        assert_eq!(got.surface, ApiSurface::OpenAI);
        assert_eq!(got.base_url.as_deref(), Some("https://api.minimax.io/v1"));
    }

    #[test]
    fn resolve_pairing_generic_provider_only_on_anthropic() {
        // A custom (Generic) provider derives the Anthropic default from its own
        // account URL, but has nothing for the OpenAI surface (never declared).
        let account = custom_account("deepseek"); // base_url https://example.com/v1
        let anth = resolve_pairing("claude", &account, &[], test_surface_of).unwrap();
        assert_eq!(anth.surface, ApiSurface::Anthropic);
        assert_eq!(anth.base_url.as_deref(), Some("https://example.com/v1"));
        assert!(resolve_pairing("codex", &account, &[], test_surface_of).is_none());
    }

    #[test]
    fn effective_pairings_derives_claude_default_and_overlays_stored() {
        let minimax = ProviderAccount {
            api_key: Some("sk-mm".into()),
            ..default_provider_accounts().into_iter().find(|a| a.id == "minimax").unwrap()
        };
        let accounts = vec![minimax];
        let stored = vec![ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://api.minimax.io/v1".into()),
            model_tiers: ModelTiers { default: Some("MiniMax-M3[1m]".into()), ..ModelTiers::default() },
        }];
        let pairings = effective_pairings(&accounts, &stored, "claude");
        // Default Claude/Anthropic pairing derived, plus the stored Codex one.
        assert!(pairings.iter().any(|p| p.harness_id == "claude" && p.surface == ApiSurface::Anthropic));
        assert!(pairings.iter().any(|p| p.harness_id == "codex" && p.surface == ApiSurface::OpenAI));
        assert_eq!(pairings.len(), 2);
    }

    #[test]
    fn effective_pairings_skips_unkeyed_disabled_or_self_auth_accounts() {
        let accounts = default_provider_accounts(); // minimax/kimi have no key; anthropic is self-auth
        // A stored pairing for an unkeyed account must not surface a row.
        let stored = vec![ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://api.minimax.io/v1".into()),
            model_tiers: ModelTiers::default(),
        }];
        assert!(effective_pairings(&accounts, &stored, "claude").is_empty());
    }

    #[test]
    fn resolve_provider_env_proxies_minimax_via_codex_with_openai_vars() {
        // AC#1: MiniMax attached to Codex spawns with the OpenAI base URL + model.
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(
                &mut prefs,
                ProviderAccount {
                    api_key: Some("sk-mm".into()),
                    ..default_provider_accounts().into_iter().find(|a| a.id == "minimax").unwrap()
                },
            );
            prefs.provider_pairings.push(ProviderPairing {
                harness_id: "codex".into(),
                provider_id: "minimax".into(),
                surface: ApiSurface::OpenAI,
                base_url: Some("https://api.minimax.io/v1".into()),
                model_tiers: ModelTiers { default: Some("MiniMax-M3[1m]".into()), ..ModelTiers::default() },
            });
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;

            let env = resolve_provider_env("codex:minimax");
            assert!(env.contains(&("OPENAI_BASE_URL".to_string(), "https://api.minimax.io/v1".to_string())));
            assert!(env.contains(&("OPENAI_API_KEY".to_string(), "sk-mm".to_string())));
            assert!(env.contains(&("OPENAI_MODEL".to_string(), "MiniMax-M3[1m]".to_string())));
            assert!(!env.iter().any(|(k, _)| k.starts_with("ANTHROPIC_")));
        });
    }

    #[test]
    fn resolve_provider_env_composite_claude_minimax_still_anthropic() {
        // The same provider via the Claude harness keeps the Anthropic surface —
        // the global key is reused, the surface differs by pairing (AC#1).
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(
                &mut prefs,
                ProviderAccount {
                    api_key: Some("sk-mm".into()),
                    ..default_provider_accounts().into_iter().find(|a| a.id == "minimax").unwrap()
                },
            );
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;

            let env = resolve_provider_env("claude:minimax");
            assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://api.minimax.io/anthropic".to_string())));
            assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-mm".to_string())));
            assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "MiniMax-M3[1m]".to_string())));
            assert!(!env.iter().any(|(k, _)| k.starts_with("OPENAI_")));
        });
    }

    /// Reproduces "edited Kimi settings don't reach a newly spawned session":
    /// the Providers page edits `ProviderAccount.model_tiers`/`base_url`, but a
    /// composite spawn id (`claude:kimi`) resolves a *pairing*, which used to
    /// take the hardcoded published tiers from `first_class_surfaces` and
    /// ignore the account's edits entirely.
    #[test]
    fn resolve_provider_env_composite_uses_edited_account_tiers() {
        with_temp_dir(|_| {
            // `"moonshot"` is the post-#918 stand-in for the (formerly first-
            // class) Kimi Moonshot LLM endpoint — the reserved `"kimi"` id is
            // now the self-auth native Kimi Code harness. A user wanting
            // Claude Code pointed at Moonshot creates a custom Claude-
            // compatible account under a non-reserved id; this test exercises
            // that path with `moonshot` as the representative proxy target.
            let mut moonshot = ProviderAccount {
                id: "moonshot".to_string(),
                name: "Moonshot Kimi".to_string(),
                enabled: true,
                billing_mode: BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("sk-moon".into()),
                base_url: Some("https://proxy.example.com/anthropic".into()),
                model_tiers: ModelTiers::default(),
                models: Vec::new(),
            };
            moonshot.model_tiers.default = Some("kimi-k2.6".into());
            moonshot.model_tiers.opus = Some("kimi-k3-preview".into());
            moonshot.model_tiers.fable = Some("kimi-k3-fable".into());
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, moonshot);
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;

            let env = resolve_provider_env("claude:moonshot");
            let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
            assert_eq!(
                get("ANTHROPIC_DEFAULT_OPUS_MODEL"),
                Some("kimi-k3-preview"),
                "a Providers-page tier edit must reach the composite spawn env"
            );
            assert_eq!(
                get("ANTHROPIC_DEFAULT_FABLE_MODEL"),
                Some("kimi-k3-fable"),
                "a Providers-page fable-tier edit must reach the composite spawn env"
            );
            assert_eq!(
                get("ANTHROPIC_BASE_URL"),
                Some("https://proxy.example.com/anthropic"),
                "a Providers-page base-URL edit must reach the composite spawn env"
            );
        });
    }

    /// Defaults: a keyed-but-untouched Kimi account emits the `fable` alias
    /// pinned to the first-class provider's opus pick (the spec's default rule).
    #[test]
    fn resolve_provider_env_composite_kimi_default_fable_matches_opus() {
        with_temp_dir(|_| {
            let mut kimi =
                default_provider_accounts().into_iter().find(|a| a.id == "kimi").unwrap();
            kimi.enabled = true;
            kimi.api_key = Some("sk-moon".into());
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, kimi);
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;

            let env = resolve_provider_env("claude:kimi");
            let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
            assert_eq!(
                get("ANTHROPIC_DEFAULT_FABLE_MODEL"),
                get("ANTHROPIC_DEFAULT_OPUS_MODEL"),
                "first-class providers must default fable to the opus pick"
            );
        });
    }

    /// A stored Anthropic-surface pairing is an attach-time snapshot (there is
    /// no UI to edit a pairing's tiers) — it must not shadow account edits made
    /// on the Providers page after the attach.
    ///
    /// Post-#918, the built-in `"kimi"` id is reserved for the self-auth
    /// native Kimi Code harness, so this test uses the stand-in `"moonshot"`
    /// id (a user-added Claude-compatible Moonshot LLM account) to exercise
    /// the same precedence rule. `kimi_default_tiers()` stays as a fixture
    /// helper for the pairing snapshot's stale tiers — it documents the
    /// Moonshot Claude-compat tier map the user would have been on.
    #[test]
    fn resolve_provider_env_composite_ignores_stale_stored_anthropic_pairing() {
        with_temp_dir(|_| {
            let mut moonshot = ProviderAccount {
                id: "moonshot".to_string(),
                name: "Moonshot Kimi".to_string(),
                enabled: true,
                billing_mode: BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("sk-moon".into()),
                base_url: Some("https://proxy.example.com/anthropic".into()),
                model_tiers: ModelTiers::default(),
                models: Vec::new(),
            };
            moonshot.model_tiers.default = Some("kimi-k2.6".into());
            moonshot.model_tiers.opus = Some("kimi-k3-preview".into());
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, moonshot);
            // What `attach_proxied_provider` froze before the account was edited.
            prefs.provider_pairings.push(ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "moonshot".into(),
                surface: ApiSurface::Anthropic,
                base_url: Some("https://api.moonshot.ai/anthropic".into()),
                model_tiers: kimi_default_tiers(),
            });
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;

            let env = resolve_provider_env("claude:moonshot");
            let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
            assert_eq!(
                get("ANTHROPIC_DEFAULT_OPUS_MODEL"),
                Some("kimi-k3-preview"),
                "a stale attach-time pairing snapshot must not shadow later account edits"
            );
        });
    }

    #[test]
    fn upsert_and_remove_provider_pairing_by_harness_provider_key() {
        let mut prefs = AppPreferences::default();
        let pairing = |harness: &str| ProviderPairing {
            harness_id: harness.into(),
            provider_id: "minimax".into(),
            surface: if harness == "codex" { ApiSurface::OpenAI } else { ApiSurface::Anthropic },
            base_url: Some("https://x".into()),
            model_tiers: ModelTiers::default(),
        };
        upsert_provider_pairing(&mut prefs, pairing("codex"));
        upsert_provider_pairing(&mut prefs, pairing("claude"));
        assert_eq!(prefs.provider_pairings.len(), 2);
        // Upsert same key overrides in place, not appends.
        let mut updated = pairing("codex");
        updated.base_url = Some("https://changed".into());
        upsert_provider_pairing(&mut prefs, updated);
        assert_eq!(prefs.provider_pairings.iter().filter(|p| p.harness_id == "codex").count(), 1);
        assert_eq!(
            prefs.provider_pairings.iter().find(|p| p.harness_id == "codex").unwrap().base_url.as_deref(),
            Some("https://changed")
        );
        remove_provider_pairing(&mut prefs, "codex", "minimax");
        assert!(!prefs.provider_pairings.iter().any(|p| p.harness_id == "codex"));
        assert!(prefs.provider_pairings.iter().any(|p| p.harness_id == "claude"));
    }

    #[test]
    fn set_account_key_if_absent_only_fills_an_empty_key() {
        with_temp_dir(|_| {
            // MiniMax built-in ships keyless → attach flow seeds a key.
            let mut prefs = AppPreferences::default();
            assert!(set_account_key_if_absent(&mut prefs, "minimax", "sk-mm"));
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;
            assert_eq!(minimax_api_key_resolved(), Some("sk-mm".to_string()));

            // A second attach must NOT overwrite the user's existing key.
            let mut prefs = load().unwrap();
            assert!(!set_account_key_if_absent(&mut prefs, "minimax", "sk-other"));
            // An empty key is always a no-op.
            assert!(!set_account_key_if_absent(&mut prefs, "kimi", ""));
        });
    }

    // ─── Issue #571 tidy-up: single source of truth for built-ins ───────────

    /// The built-in table is the single declaration site for which built-in
    /// accounts exist; `default_provider_accounts` materialises the full
    /// `ProviderAccount` from it. If a future contributor adds a row to one but
    /// not the other, this test fails immediately.
    #[test]
    fn built_in_provider_accounts_table_is_consistent_with_default_provider_accounts() {
        let table_ids: Vec<&str> = BUILTIN_PROVIDER_ACCOUNTS.iter().map(|b| b.id).collect();
        let default_ids: Vec<String> =
            default_provider_accounts().into_iter().map(|a| a.id).collect();
        assert_eq!(
            table_ids,
            default_ids.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }

    /// The `claude_compatible` classification for a built-in id is derived from
    /// the table's `self_auth` flag — the two cannot drift.
    #[test]
    fn is_claude_compatible_id_matches_table_self_auth_flag() {
        for b in BUILTIN_PROVIDER_ACCOUNTS {
            assert_eq!(
                is_claude_compatible_id(b.id),
                !b.self_auth,
                "is_claude_compatible_id({}) must equal !self_auth for the row in BUILTIN_PROVIDER_ACCOUNTS",
                b.id
            );
        }
    }

    /// The MiniMax model-tier map is defined once in `minimax_default_tiers()`
    /// and consumed by both the per-account `model_tiers` and the Anthropic
    /// surface's `model_tiers` in `first_class_surfaces("minimax")`. If a
    /// contributor re-hardcodes the strings in either consumer, this test
    /// catches the drift.
    #[test]
    fn minimax_default_tiers_is_the_source_for_minimax_account_and_surface() {
        let by_id = |id: &str| {
            default_provider_accounts()
                .into_iter()
                .find(|a| a.id == id)
                .unwrap()
        };
        assert_eq!(by_id("minimax").model_tiers, minimax_default_tiers());

        let surfaces = first_class_surfaces("minimax");
        let anthropic_surface = surfaces
            .iter()
            .find(|s| s.surface == ApiSurface::Anthropic)
            .expect("minimax must publish an Anthropic surface");
        assert_eq!(anthropic_surface.model_tiers, minimax_default_tiers());
    }

    /// Pins `kimi-via-claude` as a First-class Model Provider that the Claude
    /// Code attach picker will surface (Anthropic-compatible surface).
    #[test]
    fn kimi_via_claude_is_first_class_claude_compatible_for_claude_code() {
        let account = default_provider_accounts()
            .into_iter()
            .find(|a| a.id == "kimi-via-claude")
            .expect("kimi-via-claude must exist in the default accounts list");

        assert!(account.claude_compatible, "kimi-via-claude must be Claude-compatible");
        assert_eq!(account.billing_mode, BillingMode::PayAsYouGo);
        assert!(account.enabled, "kimi-via-claude lands enabled like the other Claude-compatible First-class Model Providers");
        assert_eq!(
            account.base_url.as_deref(),
            Some("https://api.moonshot.ai/anthropic"),
        );
        assert_eq!(account.model_tiers, kimi_default_tiers());

        let surfaces = first_class_surfaces("kimi-via-claude");
        let anthropic = surfaces
            .iter()
            .find(|s| s.surface == ApiSurface::Anthropic)
            .expect("kimi-via-claude must publish an Anthropic surface");
        assert_eq!(anthropic.base_url, "https://api.moonshot.ai/anthropic");
        assert_eq!(anthropic.model_tiers, kimi_default_tiers());

        // The surface-match half of the Claude Code attach picker must
        // include Anthropic for kimi-via-claude. We check the pure
        // `provider_surfaces` predicate directly so the assertion is
        // independent of stored preferences (a fresh-install default has
        // no `api_key` yet, so the keyed+enabled gate in
        // `compatible_providers_for_harness` would otherwise exclude it
        // even though the surface is right).
        assert!(
            provider_surfaces(&account).contains(&ApiSurface::Anthropic),
        );
    }

    /// A keyed kimi-via-claude account attached to the Claude Code harness
    /// emits the Moonshot Anthropic base URL + per-account tier env.
    #[test]
    fn resolve_provider_env_kimi_via_claude_attached_to_claude_emits_moonshot() {
        with_temp_dir(|_| {
            let mut kimi_via_claude = default_provider_accounts()
                .into_iter()
                .find(|a| a.id == "kimi-via-claude")
                .unwrap();
            kimi_via_claude.api_key = Some("sk-moon-123".into());
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, kimi_via_claude);
            save(prefs).unwrap();
            *CACHE.lock().unwrap() = None;

            let env = resolve_provider_env("claude:kimi-via-claude");
            let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
            assert_eq!(get("ANTHROPIC_BASE_URL"), Some("https://api.moonshot.ai/anthropic"));
            assert_eq!(get("ANTHROPIC_AUTH_TOKEN"), Some("sk-moon-123"));
            assert_eq!(get("ANTHROPIC_DEFAULT_FABLE_MODEL"), Some("kimi-k3"));
            assert_eq!(get("ANTHROPIC_DEFAULT_OPUS_MODEL"), Some("kimi-k3"));
            assert_eq!(get("ANTHROPIC_DEFAULT_SONNET_MODEL"), Some("kimi-2.7-code"));
            assert_eq!(get("ANTHROPIC_DEFAULT_HAIKU_MODEL"), Some("kimi-2.6"));
            assert_eq!(get("ANTHROPIC_SMALL_FAST_MODEL"), Some("kimi-2.6"));
        });
    }
}
