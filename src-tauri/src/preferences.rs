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
use std::sync::{OnceLock, Mutex};
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
///   - `haiku`      → `ANTHROPIC_DEFAULT_HAIKU_MODEL`
///
/// Only meaningful for Claude-compatible providers (MiniMax, Kimi, custom) — it's
/// irrelevant for Antigravity / Codex, which is why the UI shows these fields only
/// for Claude-compatible accounts. Built-in MiniMax/Kimi ship these pre-filled
/// with the values the absorbed `cwrap` launcher used (byte-for-byte parity).
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
    /// key/endpoint (MiniMax, Kimi, custom). When true the UI shows credential +
    /// model-tier fields, and a keyed+enabled account appears in the spawn menu as
    /// a Claude-Code-backed provider (#568). False for self-authenticating
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
}

/// Set during Tauri `setup()` so callers don't need an `AppHandle`.
static APP_DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// In-process cache, refreshed on every write. Reads consult the file only if
/// the cache is empty (first read).
static CACHE: Mutex<Option<AppPreferences>> = Mutex::new(None);

pub fn init(app_data_dir: PathBuf) {
    // Safe to ignore: setup runs once, so OnceLock::set never realistically fails.
    let _ = APP_DATA_DIR.set(app_data_dir);
}

fn preferences_path() -> Result<PathBuf, String> {
    APP_DATA_DIR
        .get()
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

/// One-shot normalization of legacy bare `default_provider` values to
/// the post-#575 composite form (`minimax` → `claude:minimax`,
/// `kimi` → `claude:kimi`).
///
/// The v19 Spawn Option composite-id migration rewrote
/// `agent_nodes.provider` but never touched `preferences.json::default_provider`
/// (issue #575 / ADR-0016 §6). A user whose app-wide default was set
/// before #575 lands keeps the legacy bare form in their preferences.json
/// — and the bare form routes through `resolve_provider_env` to the keyed
/// **account** instead of the post-#575 proxied pairing, which silently
/// spawns Claude-CLI sessions against the wrong endpoint.
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
        "kimi" => Some("claude:kimi"),
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
    /// for Claude-compatible keyed providers (MiniMax, Kimi) that ship a base
    /// URL + per-tier model map (issue #568).
    self_auth: bool,
}

const BUILTIN_PROVIDER_ACCOUNTS: &[BuiltInProviderAccount] = &[
    BuiltInProviderAccount { id: "anthropic", name: "Anthropic / Claude",   self_auth: true  },
    BuiltInProviderAccount { id: "codex",     name: "OpenAI / Codex",        self_auth: true  },
    BuiltInProviderAccount { id: "agy",       name: "Google / Antigravity",  self_auth: true  },
    BuiltInProviderAccount { id: "minimax",   name: "MiniMax",               self_auth: false },
    BuiltInProviderAccount { id: "kimi",      name: "Kimi",                  self_auth: false },
    BuiltInProviderAccount { id: "openrouter",name: "OpenRouter",            self_auth: false },
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
    let anthropic_tiers = |default: &str, fast: &str| ModelTiers {
        default: Some(default.to_string()),
        small_fast: Some(fast.to_string()),
        sonnet: Some(default.to_string()),
        opus: Some(default.to_string()),
        haiku: Some(fast.to_string()),
    };
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
        "kimi" => vec![
            SurfaceEndpoint {
                surface: ApiSurface::Anthropic,
                base_url: "https://api.moonshot.ai/anthropic".to_string(),
                model_tiers: anthropic_tiers("kimi-k2.6", "kimi-k2.5"),
            },
            SurfaceEndpoint {
                surface: ApiSurface::OpenAI,
                base_url: "https://api.moonshot.ai/v1".to_string(),
                model_tiers: openai_tiers("kimi-k2.6"),
            },
        ],
        // Deliberately no `openrouter` arm — the OpenRouter integration ships
        // Anthropic-skin ONLY by a conscious scope decision (paired with empty
        // `model_tiers`, hand-rolled per-tier picks). Codex-under-OpenRouter
        // works via a Generic custom-provider account (the Anthropic-surface
        // pairing path derives from `account.base_url`); the documented
        // OpenRouter OpenAI surface is reachable but not first-class here.
        // Documented in PR #702 body so the asymmetry with MiniMax/Kimi is
        // discoverable rather than silent.
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
/// MiniMax and Kimi are first-class Claude-compatible providers (issue #566):
/// they ship the base URL + per-tier model map the absorbed `cwrap` launcher used
/// (byte-for-byte parity), so the user only needs to add an API key. Kimi has no
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
                "kimi" => (
                    Some("https://api.moonshot.ai/anthropic".to_string()),
                    kimi_default_tiers(),
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
                _ => (None, ModelTiers::default()),
            };
            // Kimi is opt-in — historically because it had no usage fetcher,
            // now kept opt-in for back-compat with installed users (the
            // wallet meter landed in #615, but the opt-in step is part of
            // the product contract at this point).
            let enabled = !matches!(b.id, "kimi");
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
pub(crate) fn minimax_default_tiers() -> ModelTiers {
    ModelTiers {
        default: Some("MiniMax-M3[1m]".to_string()),
        small_fast: Some("MiniMax-M2.7".to_string()),
        sonnet: Some("MiniMax-M3[1m]".to_string()),
        opus: Some("MiniMax-M3[1m]".to_string()),
        haiku: Some("MiniMax-M2.7".to_string()),
    }
}

/// Kimi's Claude-Code-parity default tier map. **Single source** consumed by
/// [`default_provider_accounts`]. The Anthropic surface in
/// [`first_class_surfaces`] for `"kimi"` intentionally does NOT consume this
/// — the surface uses a different layout (k2.6 on sonnet vs the account's
/// k2.5 on sonnet) that pre-dates this tidy-up; surfacing the account's
/// k2.5-on-sonnet choice via new pairings is a separate behaviour change.
pub(crate) fn kimi_default_tiers() -> ModelTiers {
    ModelTiers {
        default: Some("kimi-k2.6".to_string()),
        small_fast: Some("kimi-k2.5".to_string()),
        sonnet: Some("kimi-k2.5".to_string()),
        opus: Some("kimi-k2.6".to_string()),
        haiku: Some("kimi-k2.5".to_string()),
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
/// so an enabled, keyed, Claude-compatible account — built-in MiniMax/Kimi or a
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

/// The default **Anthropic-surface** pairing derived for a keyed account under
/// the Claude Code harness (issue #576). This is the back-compat bridge: it
/// reproduces the pre-#576 "keyed account → MiniMax-via-Claude row" with no
/// migration. URL + model map come from the first-class published Anthropic
/// endpoint when the provider is first-class, else from the account's own
/// `base_url`/tiers (a custom Generic provider configured before pairings
/// existed).
fn default_anthropic_pairing(account: &ProviderAccount, claude_harness_id: &str) -> ProviderPairing {
    let (base_url, model_tiers) = first_class_surfaces(&account.id)
        .into_iter()
        .find(|e| e.surface == ApiSurface::Anthropic)
        .map(|e| (Some(e.base_url), e.model_tiers))
        .unwrap_or_else(|| (account.base_url.clone(), effective_tiers(account)));
    ProviderPairing {
        harness_id: claude_harness_id.to_string(),
        provider_id: account.id.clone(),
        surface: ApiSurface::Anthropic,
        base_url,
        model_tiers,
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
    if let Some(p) = stored
        .iter()
        .find(|p| p.harness_id == harness_id && p.provider_id == account.id)
    {
        return Some(p.clone());
    }
    let surface = surface_of(harness_id)?;
    if let Some(ep) = first_class_surfaces(&account.id)
        .into_iter()
        .find(|e| e.surface == surface)
    {
        return Some(ProviderPairing {
            harness_id: harness_id.to_string(),
            provider_id: account.id.clone(),
            surface,
            base_url: Some(ep.base_url),
            model_tiers: ep.model_tiers,
        });
    }
    // No published endpoint for this surface. Only the Anthropic surface has a
    // back-compat default (the account's own URL/tiers); a Generic provider on
    // any other surface has nothing to fall back to.
    (surface == ApiSurface::Anthropic && account.claude_compatible)
        .then(|| default_anthropic_pairing(account, harness_id))
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
        match out
            .iter_mut()
            .find(|o| o.harness_id == p.harness_id && o.provider_id == p.provider_id)
        {
            Some(existing) => *existing = p.clone(),
            None => out.push(p.clone()),
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
/// Tier source mirrors [`default_anthropic_pairing`]: first-class surfaces'
/// tiers win over the account's own (so a user-cleared MiniMax with empty
/// `model_tiers` doesn't false-positive — the surface still publishes
/// `minimax_default_tiers()` and the env builder emits `ANTHROPIC_MODEL`).
/// OpenRouter + Generic fall through to the account's effective tiers.
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
    let tiers = first_class_surfaces(&account.id)
        .into_iter()
        .find(|e| e.surface == ApiSurface::Anthropic)
        .map(|e| e.model_tiers)
        .unwrap_or_else(|| effective_tiers(account));
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
            // the primary so a partially-filled map never sends a `claude-*` slug.
            let fast = model(&tiers.small_fast).unwrap_or_else(|| primary.clone());
            for (k, v) in [
                ("ANTHROPIC_SMALL_FAST_MODEL", fast.clone()),
                ("ANTHROPIC_DEFAULT_SONNET_MODEL", model(&tiers.sonnet).unwrap_or_else(|| primary.clone())),
                ("ANTHROPIC_DEFAULT_OPUS_MODEL", model(&tiers.opus).unwrap_or_else(|| primary.clone())),
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
/// **Best-effort / unverified on this host.** `OPENAI_BASE_URL` /
/// `OPENAI_API_KEY` is the documented way to point an OpenAI-compatible client
/// at a custom endpoint, but the exact contract the installed `codex` build
/// honours (env vs `~/.codex/config.toml` `model_providers`) couldn't be
/// exercised here without a `codex` binary and a provider OpenAI key. Live Codex
/// verification is tracked as a #576 follow-up; the Anthropic surface is
/// exercised end-to-end. Like the Anthropic emitter, only non-empty fields emit
/// a var, and native Codex (no pairing) injects nothing — so the user's own
/// `OPENAI_API_KEY` / `codex login` is untouched.
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
    /// they must run serially. A real test crate would use `serial_test`, but
    /// a local Mutex is fine here.
    static TEST_LOCK: TestMutex<()> = TestMutex::new(());

    fn with_temp_dir<F: FnOnce(&PathBuf)>(f: F) {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = std::env::temp_dir().join(format!("buildmesh-prefs-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Reset globals for each test (OnceLock can't be reset, so we work
        // around it by checking whether the existing value already points at
        // a buildmesh-prefs-test dir — in which case we just reuse).
        let _ = APP_DATA_DIR.set(tmp.clone());
        *CACHE.lock().unwrap() = None;

        f(&tmp);

        let _ = std::fs::remove_dir_all(&tmp);
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

    #[test]
    fn ensure_default_provider_normalized_rewrites_legacy_kimi_to_composite() {
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
                Some("claude:kimi".to_string()),
                "bare 'kimi' should be normalized to 'claude:kimi'"
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
            // A retired legacy id ("minimax"/"kimi") and any unknown id fall
            // through to the Anthropic executor default (issue #538 cutover).
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
        assert_eq!(ids, vec!["anthropic", "codex", "agy", "minimax", "kimi", "openrouter"]);
        // MiniMax + Kimi + OpenRouter are the pay-as-you-go, Claude-compatible
        // exemplars; the self-auth built-ins are plans and not Claude-compatible.
        let by_id = |id: &str| default_provider_accounts().into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(by_id("minimax").billing_mode, BillingMode::PayAsYouGo);
        assert!(by_id("minimax").claude_compatible);
        assert!(by_id("kimi").claude_compatible);
        assert!(by_id("openrouter").claude_compatible);
        assert!(!by_id("anthropic").claude_compatible);
        assert!(!by_id("codex").claude_compatible);
        // MiniMax + Kimi ship the cwrap launcher's base URL + per-tier map so a key is all
        // the user needs to add. OpenRouter ships its API base URL + empty
        // per-tier map so the user picks provider/model per slot.
        assert_eq!(by_id("minimax").base_url.as_deref(), Some("https://api.minimax.io/anthropic"));
        assert_eq!(by_id("kimi").base_url.as_deref(), Some("https://api.moonshot.ai/anthropic"));
        assert_eq!(by_id("openrouter").base_url.as_deref(), Some("https://openrouter.ai/api"));
        assert_eq!(by_id("minimax").model_tiers.default.as_deref(), Some("MiniMax-M3[1m]"));
        assert_eq!(by_id("openrouter").model_tiers, ModelTiers::default());
        // Kimi ships disabled by default (opt-in via the Providers page) — wallet
        // meter fetcher is now wired up (see services::usage::kimi_usage), so the
        // "no fetcher" rationale that pre-dates this PR no longer applies.
        assert!(!by_id("kimi").enabled);
        // OpenRouter ships a fetcher on day 1, so it lands enabled by default
        // — same shape as MiniMax, opt-in via missing key rather than a flag.
        assert!(by_id("openrouter").enabled);
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
        // Six built-ins (anthropic/codex/agy/minimax/kimi/openrouter), no
        // duplicate minimax, plus the custom one.
        assert_eq!(merged.iter().filter(|a| a.id == "minimax").count(), 1);
        assert!(!merged.iter().find(|a| a.id == "minimax").unwrap().enabled);
        assert!(merged.iter().any(|a| a.id == "deepseek"));
        assert_eq!(merged.len(), 7);
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
            haiku: Some("GLM-4-Flash".to_string()),
        };
        let env = provider_account_env(Some(&account));
        assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "GLM-4.6".to_string())));
        assert!(env.contains(&("ANTHROPIC_SMALL_FAST_MODEL".to_string(), "GLM-4-Flash".to_string())));
        assert!(env.contains(&("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), "GLM-4.6-Max".to_string())));
        assert!(env.contains(&("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), "GLM-4.6".to_string())));
        assert!(env.contains(&("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), "GLM-4-Flash".to_string())));
        assert!(!env.iter().any(|(_, v)| v == "IGNORED"), "flat models is superseded by tiers");
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
        assert_eq!(get("ANTHROPIC_DEFAULT_HAIKU_MODEL"), Some("MiniMax-M2.7"));
        assert_eq!(get("API_TIMEOUT_MS"), Some("3000000"));
    }

    #[test]
    fn builtin_kimi_account_reproduces_claude_code_model_routing() {
        let mut kimi = default_provider_accounts().into_iter().find(|a| a.id == "kimi").unwrap();
        kimi.api_key = Some("sk-moon".to_string());
        let env = provider_account_env(Some(&kimi));
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("ANTHROPIC_BASE_URL"), Some("https://api.moonshot.ai/anthropic"));
        assert_eq!(get("ANTHROPIC_MODEL"), Some("kimi-k2.6"));
        assert_eq!(get("ANTHROPIC_SMALL_FAST_MODEL"), Some("kimi-k2.5"));
        assert_eq!(get("ANTHROPIC_DEFAULT_OPUS_MODEL"), Some("kimi-k2.6"));
        assert_eq!(get("ANTHROPIC_DEFAULT_SONNET_MODEL"), Some("kimi-k2.5"));
        assert_eq!(get("ANTHROPIC_DEFAULT_HAIKU_MODEL"), Some("kimi-k2.5"));
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
    /// every custom endpoint (MiniMax, Kimi, OpenRouter, future generic
    /// accounts alike) — matches `cwrap`'s behaviour. The default-Anthropic
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
}
