//! Buildmesh-wide preferences, persisted as JSON in `app_data_dir/preferences.json`.
//!
//! This is the **application-level** layer of configuration, distinct from:
//!   - `meshes` DB columns — per-mesh overrides (e.g. `mesh.default_provider`)
//!   - `.claude/settings.json` — per-mesh Claude Code config (worktree.baseRef etc.)
//!
//! Precedence is applied at the call site: per-mesh value → app pref → hardcoded
//! fallback (`anthropic` for providers).

use crate::models::Provider;
use crate::agent::capabilities::{
    capabilities_for, EffortControlKind, HarnessCapabilities,
};
use crate::agent::provider::compatibility::{
    self, CompatibilityDecision, EndpointModelDescriptor, ProviderAuthMode, WireApi,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// True when no tier is set.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
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
/// §4 / ADR-0025, issue #576).
///
/// This is the *per-pairing* half of the config split: the **API key is global**
/// to the [`ProviderAccount`] (entered once on the Providers page, reused across
/// pairings), while the **Compatible API surface + endpoint URL + model-tier
/// remap are per pairing** and live here. One [`ProviderAccount`] can have
/// several `ProviderPairing`s — e.g. MiniMax paired with both Claude Code
/// (`surface = Anthropic`) and Codex (`surface = OpenAI`), each with its own
/// `base_url` and `model_tiers`.
///
/// Only **stored** pairings exist (ADR-0025) — there is no derived default
/// Anthropic pairing on key alone. Attach (Harnesses page) materialises a row
/// in [`AppPreferences::provider_pairings`]; [`effective_pairings`] returns
/// those stored rows for proxiable accounts.
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "PairingVerificationStatus.ts")]
pub enum PairingVerificationStatus {
    #[default]
    Pending,
    Verified,
    Failed,
    Stale,
    Unsupported,
}

/// Non-secret proof that an exact pairing/runtime/Codex installation passed
/// the Responses agent-loop verification (issue #1098).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "PairingVerification.ts")]
pub struct PairingVerification {
    pub harness_id: String,
    pub provider_id: String,
    pub pairing_signature: String,
    pub endpoint: String,
    pub model_id: String,
    pub auth_mode: ProviderAuthMode,
    pub runtime: String,
    pub executable: String,
    pub codex_version: String,
    #[serde(default)]
    pub capability_result: CompatibilityDecision,
    pub status: PairingVerificationStatus,
    pub verified_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
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

/// A user-configurable **Model Provider account** (ADR-0014 / ADR-0025 / PRD #534).
///
/// Credentials and billing only: a [`HarnessProfile`] names the executor, a
/// `ProviderAccount` names the credential identity. Endpoint URL and model-tier
/// remap live on [`ProviderPairing`] (Harnesses page), not here.
///
/// Self-auth built-ins (anthropic, codex, agy, grok, opencode) always appear via
/// [`default_provider_accounts`]. Keyed first-class providers (minimax, kimi,
/// openrouter) live in [`BUILTIN_PROVIDER_ACCOUNTS`] but are only materialised
/// when the user adds them from [`keyed_first_class_catalog`]. Users may also
/// add custom Claude-compatible accounts (name + API key).
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
    /// Whether this is a Claude-compatible keyed provider (MiniMax, custom
    /// accounts). When true a keyed+enabled account can be attached under a
    /// proxy-capable harness (#568). False for self-authenticating built-ins
    /// (anthropic/codex/agy), which hold no creds in Buildmesh.
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
}

/// The configurable **values** an Agent Harness can accept (issue #1148).
///
/// This is the wire-level "configurable harness value type" referenced by
/// #1149 step 2 — the shared shape every layer of the cascade (Agent Node
/// argument, Mesh override, application default) feeds into the resolver.
/// The `model` / `effort` fields are optional individually so a user can set
/// just one of them; an instance with both fields `None` is the empty
/// representation (the cascade treats it as absent).
///
/// Whitespace-only inputs are collapsed to `None` by [`normalize_harness_default`]
/// before the resolver ever sees them — issue #1148 acceptance criteria 32.
///
/// Generated to `src/types/generated/HarnessConfigValue.ts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "HarnessConfigValue.ts")]
pub struct HarnessConfigValue {
    /// Optional primary model id for this harness (e.g. `"MiniMax-M3[1m]"`,
    /// `"opus-4-1"`). `None` means "no model override at this layer". The
    /// resolver's capability mask drops this field when the selected harness
    /// does not declare `supports_model_override`, regardless of what layer
    /// supplied it (issue #1148 acceptance criteria 5).
    #[serde(default)]
    pub model: Option<String>,
    /// Optional effort / reasoning value for this harness (e.g. `"high"` for
    /// Claude Code, `"xhigh"` for Codex). `None` means "no effort override at
    /// this layer". The resolver drops the value when the selected harness
    /// advertises `EffortControlKind::None` OR when the value isn't in the
    /// harness's `effort_control.allowed` vocabulary.
    #[serde(default)]
    pub effort: Option<String>,
}

impl HarnessConfigValue {
    /// True when no field carries a non-blank value. A save that would
    /// leave the entry fully empty must remove the sparse map entry entirely
    /// — issue #1148 acceptance criteria 6 ("an empty harness configuration
    /// removes its sparse entry").
    pub fn is_empty(&self) -> bool {
        self.model.as_deref().is_none_or(str::is_empty)
            && self.effort.as_deref().is_none_or(str::is_empty)
    }
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
    /// the bottom by `agent::provider_menu::order_providers`. A row whose id isn't
    /// listed (a newly-detected harness) appends after the listed ones; an
    /// uninstalled harness keeps its saved slot here until it reappears.
    #[serde(default)]
    pub harness_order: Vec<String>,
    /// Stored **Proxied Provider** pairings (ADR-0016 §4 / ADR-0025, issue #576).
    /// Each entry attaches a [`ProviderAccount`] to a harness over a chosen
    /// **Compatible API surface** with its own endpoint URL + model-tier remap.
    /// Only stored pairings exist — there is no derived default on key alone
    /// (ADR-0025). An additive field — an older `preferences.json` without it
    /// loads with an empty list (legacy account endpoint fields are migrated
    /// into Claude Anthropic pairings on read — see [`migrate_prefs_json`]).
    #[serde(default)]
    pub provider_pairings: Vec<ProviderPairing>,
    /// Reconstructable, non-secret verification results for exact proxied
    /// endpoint/model/runtime/Codex combinations (issue #1098).
    #[serde(default)]
    pub pairing_verifications: Vec<PairingVerification>,
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
    /// One-shot migration gate (ADR-0025). Set after the legacy
    /// `base_url`/`model_tiers`/`models` fields are stripped from accounts and
    /// their values materialised into Claude Anthropic pairings, so future
    /// prefs loads never auto-pair a freshly-keyed account — attach is explicit.
    #[serde(default)]
    pub ad0025_account_pairings_migrated: bool,
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
    /// **Application-level harness defaults** (issue #1148 / #1150) — a
    /// sparse map keyed by stable harness profile id (the id the Spawn Menu
    /// uses, e.g. `"claude"`, `"codex"`, `"agy"`, plus any user-defined
    /// custom profile id). A present entry supplies a per-harness model
    /// and/or effort value; the resolver consumes them through the
    /// `application` slot of [`crate::agent::capabilities::FieldInputs`].
    /// A missing entry means "Buildmesh supplies no application override —
    /// the harness runs with its native behaviour".
    ///
    /// The map is **sparse**: an entry whose every field collapses to absent
    /// (blank after trimming) is removed entirely by [`upsert_harness_default`]
    /// / [`remove_harness_default`], so a stored empty `{}` is unreachable.
    /// Additive on disk — an older `preferences.json` without this field
    /// loads as an empty `HashMap` (issue #1148 acceptance criteria 1) via
    /// `#[serde(default)]`.
    #[serde(default)]
    pub harness_defaults: HashMap<String, HarnessConfigValue>,
}

/// Set during Tauri `setup()` so callers don't need an `AppHandle`.
static APP_DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// In-process cache, refreshed on every write. Reads consult the file only if
/// the cache is empty (first read).
static CACHE: Mutex<Option<AppPreferences>> = Mutex::new(None);

/// Lock the in-process preferences cache, recovering from a poisoned
/// mutex instead of panicking (issue #1224).
///
/// `CACHE` is a process-wide static — a panic in any `load`/`save`/
/// test helper that holds the guard permanently bricks the entire
/// preferences surface for the rest of the process. The cache value
/// itself is a plain `Option<AppPreferences>`; the panic cannot
/// corrupt it (the guard drops on unwind, leaving the value in a
/// consistent None-or-fully-populated state). `into_inner()` extracts
/// the value and trusts the next caller to decide whether to refresh
/// from disk. Mirrors `db::lock_db()` and `services::autopilot::
/// lock_planner_set`.
///
/// Production callers use `CACHE.lock().unwrap_or_else(|p| p.into_inner())`
/// directly so the helper is gated to `#[cfg(test)]` — clippy's lib-build
/// dead-code check doesn't see across the test boundary, and the helper
/// only saves the test module from repeating the recover pattern in
/// every fixture. Mirrors the `#[allow(dead_code)]` pattern on
/// `db::is_initialized`.
#[cfg(test)]
fn lock_cache() -> std::sync::MutexGuard<'static, Option<AppPreferences>> {
    match CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                "preferences CACHE mutex was poisoned by a prior panic — recovering (issue #1224)"
            );
            poisoned.into_inner()
        }
    }
}
static WRITE_LOCK: Mutex<()> = Mutex::new(());

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

/// Serialises tests outside this module against [`TEST_LOCK`] while they use
/// the process-global `APP_DATA_DIR`/`CACHE` state via [`init_for_tests`] —
/// without it, a parallel test in another module can swap the globals mid-run.
#[cfg(test)]
pub(crate) fn test_state_guard() -> std::sync::MutexGuard<'static, ()> {
    tests::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
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
    let mut value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(AppPreferences::default()),
    };
    let changed = migrate_prefs_json(&mut value);
    // Round-trip the migrated JSON before persisting so a partially-unknown
    // payload (older field the Rust struct doesn't know about) doesn't get
    // overwritten by `AppPreferences::default()`. Issue #xxxx: silent
    // overwrite was a data-loss path.
    let prefs: AppPreferences = match serde_json::from_value(value.clone()) {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!("preferences::read_from_disk post-migration deserialization failed; skipping persist");
            return Ok(AppPreferences::default());
        }
    };
    if changed {
        if let Err(e) = write_to_disk(&prefs) {
            tracing::warn!("preferences::read_from_disk migration save failed: {}", e);
        }
    }
    Ok(prefs)
}

/// One-shot ADR-0025 prefs JSON migration (pure on a `serde_json::Value`):
/// 1. Fold legacy `kimi-via-claude` companion key into a `kimi` account row.
/// 2. **Once** (`ad0025_account_pairings_migrated` flag): for each enabled
///    keyed Claude-compatible account with no Claude pairing yet, materialise
///    one from legacy account endpoint fields / first-class defaults (preserves
///    pre-ADR auto-derived Claude pairings). After the flag is set, saving a
///    key never auto-attaches.
/// 3. Strip legacy `base_url` / `model_tiers` / `models` from every account.
///
/// Returns whether anything changed (caller persists when true).
fn migrate_prefs_json(value: &mut serde_json::Value) -> bool {
    let Some(root) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;

    let claude_harness = claude_harness_id_from_json(root.get("harness_profiles"));
    let already_migrated = root
        .get("ad0025_account_pairings_migrated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !root.contains_key("provider_accounts") {
        root.insert("provider_accounts".into(), serde_json::json!([]));
    }
    if !root.contains_key("provider_pairings") {
        root.insert("provider_pairings".into(), serde_json::json!([]));
    }

    // --- 1. kimi-via-claude companion → first-class kimi (key only) ----------
    if migrate_kimi_companion_json(root) {
        changed = true;
    }

    // --- 2. one-shot: materialise Claude pairings for pre-ADR keyed accounts -
    let existing_pairings = root
        .get("provider_pairings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut pairings_to_add: Vec<serde_json::Value> = Vec::new();

    if !already_migrated {
        if let Some(accounts) = root
            .get("provider_accounts")
            .and_then(|v| v.as_array())
        {
            for account in accounts {
                let Some(obj) = account.as_object() else {
                    continue;
                };
                let id = obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() || !is_claude_compatible_id(&id) {
                    continue;
                }
                let enabled = obj
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let api_key = obj
                    .get("api_key")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                if !enabled || api_key.is_none() {
                    continue;
                }
                let already = existing_pairings.iter().any(|p| {
                    p.get("harness_id").and_then(|v| v.as_str()) == Some(claude_harness.as_str())
                        && p.get("provider_id").and_then(|v| v.as_str()) == Some(id.as_str())
                });
                if already {
                    continue;
                }
                let legacy_base = obj
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let legacy_tiers = obj.get("model_tiers").cloned();
                let published = first_class_surfaces(&id)
                    .into_iter()
                    .find(|e| e.surface == ApiSurface::Anthropic);
                let base_url = legacy_base
                    .or_else(|| published.as_ref().map(|e| e.base_url.clone()))
                    .unwrap_or_default();
                // ADR-0025: never synthesise a stored pairing whose
                // `base_url` is null — the attach command requires a non-empty
                // URL, and a sourced spawn env would route to nowhere.
                if base_url.is_empty() {
                    continue;
                }
                let model_tiers = legacy_tiers
                    .filter(|t| t.as_object().is_some_and(|o| !o.is_empty()))
                    .unwrap_or_else(|| {
                        published
                            .as_ref()
                            .map(|e| serde_json::to_value(&e.model_tiers).unwrap_or_default())
                            .unwrap_or_else(|| serde_json::json!({}))
                    });
                pairings_to_add.push(serde_json::json!({
                    "harness_id": claude_harness,
                    "provider_id": id,
                    "surface": "anthropic",
                    "base_url": if base_url.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(base_url)
                    },
                    "model_tiers": model_tiers,
                }));
            }
        }
        root.insert(
            "ad0025_account_pairings_migrated".into(),
            serde_json::Value::Bool(true),
        );
        changed = true;
    }

    // --- 3. strip legacy endpoint fields from every account ------------------
    if let Some(accounts) = root
        .get_mut("provider_accounts")
        .and_then(|v| v.as_array_mut())
    {
        for account in accounts.iter_mut() {
            let Some(obj) = account.as_object_mut() else {
                continue;
            };
            let had_legacy = obj.contains_key("base_url")
                || obj.contains_key("model_tiers")
                || obj.contains_key("models");
            if had_legacy {
                obj.remove("base_url");
                obj.remove("model_tiers");
                obj.remove("models");
                changed = true;
            }
        }
    }

    if !pairings_to_add.is_empty() {
        if let Some(pairings) = root
            .get_mut("provider_pairings")
            .and_then(|v| v.as_array_mut())
        {
            pairings.extend(pairings_to_add);
            changed = true;
        }
    }

    changed
}

/// Resolve the Claude harness id from a prefs JSON `harness_profiles` value —
/// first profile with `harness == "anthropic"`, else `"claude"`.
fn claude_harness_id_from_json(profiles: Option<&serde_json::Value>) -> String {
    profiles
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|p| {
                if p.get("harness").and_then(|h| h.as_str()) == Some("anthropic") {
                    p.get("id").and_then(|id| id.as_str()).map(str::to_string)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "claude".to_string())
}

/// Fold a stored `kimi-via-claude` companion into the first-class `kimi` row
/// (key only — endpoint fields are handled by the pairing migration). Mutates
/// the prefs root object; returns whether anything changed.
fn migrate_kimi_companion_json(root: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(accounts) = root
        .get_mut("provider_accounts")
        .and_then(|v| v.as_array_mut())
    else {
        return false;
    };
    let Some(companion_idx) = accounts.iter().position(|a| {
        a.get("id").and_then(|v| v.as_str()) == Some("kimi-via-claude")
    }) else {
        return false;
    };
    let companion = accounts[companion_idx].clone();
    let companion_key = companion
        .get("api_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if let Some(kimi) = accounts.iter_mut().find(|a| {
        a.get("id").and_then(|v| v.as_str()) == Some("kimi")
    }) {
        if let Some(obj) = kimi.as_object_mut() {
            let empty = obj
                .get("api_key")
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.is_empty());
            if empty {
                if let Some(key) = companion_key {
                    obj.insert("api_key".into(), serde_json::Value::String(key));
                }
            }
        }
    } else {
        // Materialise a first-class kimi row from the catalog template.
        let mut kimi = serde_json::json!({
            "id": "kimi",
            "name": "Moonshot / Kimi",
            "enabled": true,
            "billing_mode": "pay_as_you_go",
            "claude_compatible": true,
            "api_key": null,
        });
        if let Some(key) = companion_key {
            if let Some(obj) = kimi.as_object_mut() {
                obj.insert("api_key".into(), serde_json::Value::String(key));
            }
        }
        // Carry companion endpoint fields so step 2 can turn them into a pairing.
        if let (Some(src), Some(dst)) = (companion.as_object(), kimi.as_object_mut()) {
            for field in ["base_url", "model_tiers", "models", "enabled"] {
                if let Some(v) = src.get(field) {
                    dst.insert(field.to_string(), v.clone());
                }
            }
        }
        accounts.push(kimi);
    }
    accounts.remove(companion_idx);
    true
}

fn write_to_disk(prefs: &AppPreferences) -> Result<(), String> {
    let _write_guard = WRITE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = preferences_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create app data dir: {}", e))?;
    }
    let json = serde_json::to_string_pretty(prefs)
        .map_err(|e| format!("failed to serialize preferences: {}", e))?;
    let parent = path
        .parent()
        .ok_or_else(|| "preferences path has no parent directory".to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("failed to create temporary preferences file: {e}"))?;
    use std::io::Write;
    temporary
        .write_all(json.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|e| format!("failed to write temporary preferences file: {e}"))?;
    temporary
        .persist(&path)
        .map_err(|e| format!("failed to atomically replace preferences.json: {}", e.error))?;
    Ok(())
}

/// Load preferences, populating the in-process cache on first call.
///
/// Recovers from a poisoned `CACHE` mutex instead of panicking (issue
/// #1224). The cache is a plain `Mutex<Option<AppPreferences>>` —
/// a panic in a previous holder would have released the guard on
/// unwind, leaving the inner value in a consistent (None-or-fully-
/// populated) state. `.unwrap()` on `PoisonError` would brick every
/// subsequent `load`/`save` and freeze the whole preferences surface;
/// `into_inner()` lets the next caller decide whether to refresh
/// from disk.
pub fn load() -> Result<AppPreferences, String> {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::warn!(
                "preferences CACHE mutex was poisoned by a prior panic — recovering (issue #1224)"
            );
            poisoned.into_inner()
        }
    };
    if let Some(cached) = guard.as_ref() {
        return Ok(cached.clone());
    }
    let prefs = read_from_disk()?;
    *guard = Some(prefs.clone());
    Ok(prefs)
}

/// Persist preferences to disk and refresh the cache.
pub fn save(prefs: AppPreferences) -> Result<(), String> {
    let mut guard = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    write_to_disk(&prefs)?;
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
/// `agent::provider_menu::order_providers` (issue #573). Empty when never set, in
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
    // Kimi (Moonshot) — keyed via the user's Moonshot API key. The string
    // `kimi` also names the Kimi Code CLI Agent Harness in a different
    // namespace (`HarnessProfile.harness`); see CONTEXT.md "First-class
    // Model Provider" + "Usage follows the credential, not the pairing".
    BuiltInProviderAccount { id: "kimi",      name: "Moonshot / Kimi",       self_auth: false },
    BuiltInProviderAccount { id: "opencode",  name: "OpenCode",             self_auth: true  },
    BuiltInProviderAccount { id: "minimax",   name: "MiniMax",               self_auth: false },
    BuiltInProviderAccount { id: "openrouter",name: "OpenRouter",            self_auth: false },
    BuiltInProviderAccount { id: "cursor",    name: "Cursor",               self_auth: true  },
    // DeepSeek Platform API — keyed by the user's DeepSeek API key.
    // Publishes both Anthropic- and OpenAI-compatible surfaces (DeepSeek
    // runs an OpenAI-compatible endpoint at https://api.deepseek.com/v1
    // and an Anthropic-compatible Claude Code surface). Models: `deepseek-chat`
    // (V3.x — fast chat / mid-tier code) and `deepseek-reasoner` (R1 — the
    // strongest reasoning tier). See issue #1127.
    BuiltInProviderAccount { id: "deepseek",  name: "DeepSeek",              self_auth: false },
    // OpenAI Platform API — keyed by an `sk-admin-…` (org spend) or
    // `sk-proj-…` (graceful degradation: org costs 401, project keys still
    // work for inference). See ADR-0026 / issue #1109. No first-class
    // inference surface here — the row exists for the Usage Meter only;
    // OpenAI inference goes through the Codex harness.
    BuiltInProviderAccount { id: "openai",    name: "OpenAI Platform",       self_auth: false },
];

// One row per credential/billing identity. Pairings live in the Spawn Menu
// as composite ids (`claude:kimi`), not as additional rows here. See the
// "First-class Model Providers and the single-meter invariant" section
// in docs/knowledge-primer.md for the full rationale.

/// Whether `id` names a Claude-compatible keyed provider — one that holds a
/// global **credential** in Buildmesh and can be attached under a proxy-
/// capable harness once the user explicitly attaches a pairing (ADR-0025).
/// Self-authenticating built-ins are the only exceptions (issue #568). The
/// classification is **derived from [`BUILTIN_PROVIDER_ACCOUNTS`]** so a new
/// self-auth built-in can't drift out of sync with the account definition in
/// `default_provider_accounts`. Spawn-menu visibility still requires an
/// explicit attach — see [`effective_pairings`].
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
/// Catalog metadata is only an attach candidate. Codex pairings remain
/// unavailable until the exact endpoint/model/runtime context passes live
/// Responses verification.
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
                model_tiers: openai_tiers("MiniMax-M3"),
            },
        ],
        // Direct Kimi Open Platform is Chat Completions-only, so only its
        // Anthropic-compatible Claude Code surface is offered.
        "kimi" => vec![
            SurfaceEndpoint {
                surface: ApiSurface::Anthropic,
                base_url: "https://api.moonshot.ai/anthropic".to_string(),
                model_tiers: kimi_default_tiers(),
            },
        ],
        // OpenRouter Anthropic Skin — Anthropic-only by scope decision (empty
        // model_tiers; user picks provider/model slugs per tier on attach).
        "openrouter" => vec![SurfaceEndpoint {
            surface: ApiSurface::Anthropic,
            base_url: "https://openrouter.ai/api".to_string(),
            model_tiers: ModelTiers::default(),
        }],
        // DeepSeek publishes both surfaces (issue #1127):
        //   - Anthropic-compatible Claude Code backend (DeepSeek maintains
        //     an Anthropic Messages-compatible surface alongside the OpenAI
        //     endpoint — Claude Code routes through it without modification)
        //   - OpenAI-compatible `/v1` for Codex + any other OpenAI-compatible
        //     consumer (matches the documented `https://api.deepseek.com/v1`
        //     base URL). Tier defaults mirror the two-tier model lineup
        //     (`deepseek-chat` for the fast path, `deepseek-reasoner` for
        //     the strong-reasoning tiers).
        "deepseek" => vec![
            SurfaceEndpoint {
                surface: ApiSurface::Anthropic,
                base_url: "https://api.deepseek.com/anthropic".to_string(),
                model_tiers: deepseek_default_tiers(),
            },
            SurfaceEndpoint {
                surface: ApiSurface::OpenAI,
                base_url: "https://api.deepseek.com/v1".to_string(),
                model_tiers: openai_tiers("deepseek-chat"),
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
/// (custom) provider exposes **both** Anthropic and OpenAI — surface is chosen
/// per attach (ADR-0025). Self-auth built-ins expose none (they're never proxied).
pub fn provider_surfaces(account: &ProviderAccount) -> Vec<ApiSurface> {
    let first_class = first_class_surfaces(&account.id);
    if !first_class.is_empty() {
        return first_class.into_iter().map(|s| s.surface).collect();
    }
    if account.claude_compatible {
        // Generic credential — surface is per pairing (attach flow).
        vec![ApiSurface::Anthropic, ApiSurface::OpenAI]
    } else {
        Vec::new()
    }
}

/// Atomically mutate the latest cached preference value and persist it while
/// serialising competing read-modify-write operations.
pub fn update(mutator: impl FnOnce(&mut AppPreferences)) -> Result<AppPreferences, String> {
    let mut guard = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        *guard = Some(read_from_disk()?);
    }
    let mut candidate = guard
        .as_ref()
        .expect("preferences cache was initialized")
        .clone();
    mutator(&mut candidate);
    // Publish the new cached value only after the durable atomic replacement
    // succeeds. A failed write must not manufacture an in-memory verification
    // record that launch preflight could mistake for persisted proof.
    write_to_disk(&candidate)?;
    *guard = Some(candidate.clone());
    Ok(candidate)
}

/// Reconstruct the exact endpoint/model descriptor from persisted pairing
/// state. `ApiSurface::OpenAI` is deliberately not proof of Responses
/// compatibility: known Kimi rows are classified as Chat Completions, and
/// MiniMax's retired `[1m]` alias is left capability-incomplete.
pub fn endpoint_model_descriptor(pairing: &ProviderPairing) -> EndpointModelDescriptor {
    let endpoint = pairing.base_url.as_deref().unwrap_or("").trim().to_string();
    let model_id = pairing
        .model_tiers
        .default
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let (wire_api, capabilities) = match pairing.surface {
        ApiSurface::Anthropic => (
            WireApi::AnthropicMessages,
            compatibility::complete_agent_capabilities(),
        ),
        ApiSurface::OpenAI if pairing.provider_id == "kimi" => (
            WireApi::ChatCompletions,
            compatibility::complete_agent_capabilities(),
        ),
        ApiSurface::OpenAI
            if pairing.provider_id == "minimax" && model_id != "MiniMax-M3" =>
        {
            (WireApi::Responses, Default::default())
        }
        ApiSurface::OpenAI => (
            WireApi::Responses,
            compatibility::complete_agent_capabilities(),
        ),
    };
    EndpointModelDescriptor {
        provider_id: pairing.provider_id.clone(),
        endpoint,
        wire_api,
        model_id,
        capabilities,
        auth_modes: vec![ProviderAuthMode::BearerEnv],
        context_window: None,
        reasoning_effort: None,
    }
}

/// One matcher drives attach eligibility, verification, spawn-menu visibility,
/// and the final backend preflight.
pub fn pairing_compatibility(pairing: &ProviderPairing) -> CompatibilityDecision {
    let descriptor = endpoint_model_descriptor(pairing);
    let requirements = match pairing.surface {
        ApiSurface::Anthropic => compatibility::claude_requirements(),
        ApiSurface::OpenAI => compatibility::codex_requirements(),
    };
    let mut decision = compatibility::match_descriptor(&descriptor, &requirements);
    if decision.compatible && descriptor.endpoint.is_empty() {
        decision.compatible = false;
        decision.reason = Some("endpoint is required".into());
    }
    if decision.compatible && descriptor.model_id.is_empty() {
        decision.compatible = false;
        decision.reason = Some("explicit model is required".into());
    }
    if pairing.provider_id == "minimax"
        && pairing.surface == ApiSurface::OpenAI
        && descriptor.model_id != "MiniMax-M3"
    {
        decision.compatible = false;
        decision.reason = Some(
            "MiniMax Codex pairings require the current Responses model ID 'MiniMax-M3'"
                .into(),
        );
    }
    decision
}

fn pairing_can_potentially_match(pairing: &ProviderPairing) -> bool {
    let mut candidate = pairing.clone();
    if candidate.model_tiers.default.as_deref().is_none_or(str::is_empty) {
        candidate.model_tiers.default = Some("model-selected-during-attach".into());
    }
    pairing_compatibility(&candidate).compatible
}

/// The Claude Code harness id the derived default Anthropic pairings group under
/// — the first `anthropic`-backed profile, else the literal `"claude"` (which
/// still resolves to the Anthropic executor). Pure; the single source of this
/// rule so the menu (`agent::provider_menu::compose_provider_menu`) and the pairing
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

/// The full effective pairing set (stored only, ADR-0025) read from disk —
/// the harness-config page's "what's attached to each harness" source.
pub fn effective_provider_pairings() -> Vec<ProviderPairing> {
    effective_pairings(&provider_accounts(), &provider_pairings())
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
    let stored = provider_pairings();
    provider_accounts()
        .into_iter()
        .filter(|account| {
            if !provider_surfaces(account).contains(&surface) {
                return false;
            }
            attach_pairing_defaults(harness_id, account, &stored, |_| Some(surface))
                .is_some_and(|pairing| pairing_can_potentially_match(&pairing))
        })
        .collect()
}

/// The pairing the attach flow should store when proxying `provider_id` through
/// `harness_id` (issue #576 / ADR-0025): a stored pairing wins (idempotent
/// re-attach), else the published first-class default for the harness's surface.
/// Looks up effective accounts first, then the keyed first-class catalog so an
/// as-yet-unmaterialised MiniMax/Kimi/OpenRouter still prefills. `None` when
/// the provider is incompatible with the harness's surface.
pub fn pairing_for(harness_id: &str, provider_id: &str) -> Option<ProviderPairing> {
    let accounts = provider_accounts();
    let account = accounts
        .iter()
        .find(|a| a.id == provider_id)
        .cloned()
        .or_else(|| keyed_first_class_template(provider_id))?;
    attach_pairing_defaults(harness_id, &account, &provider_pairings(), harness_surface)
}

/// The code-defined model-provider accounts that always exist regardless of what
/// `preferences.json` stores (ADR-0025). **Self-auth built-ins only** —
/// anthropic, codex, agy, grok, opencode. Keyed first-class providers
/// (minimax, kimi, openrouter) are absent until the user adds them from
/// [`keyed_first_class_catalog`]; they still live in [`BUILTIN_PROVIDER_ACCOUNTS`]
/// with `self_auth: false` for classification and catalog materialisation.
///
/// Materialised from [`BUILTIN_PROVIDER_ACCOUNTS`], the single declaration
/// site for built-ins (issue #571).
pub fn default_provider_accounts() -> Vec<ProviderAccount> {
    BUILTIN_PROVIDER_ACCOUNTS
        .iter()
        .filter(|b| b.self_auth)
        .map(|b| ProviderAccount {
            id: b.id.to_string(),
            name: b.name.to_string(),
            enabled: true,
            billing_mode: BillingMode::Plan,
            claude_compatible: false,
            api_key: None,
        })
        .collect()
}

/// Catalog of keyed first-class provider templates for the UI "Add provider"
/// picker (ADR-0025): minimax, kimi, openrouter. Each row is credentials-only
/// (`api_key: None`, `enabled: true`, `claude_compatible: true`). The UI offers
/// only those not already present in the effective account list.
pub fn keyed_first_class_catalog() -> Vec<ProviderAccount> {
    BUILTIN_PROVIDER_ACCOUNTS
        .iter()
        .filter(|b| !b.self_auth)
        .map(|b| ProviderAccount {
            id: b.id.to_string(),
            name: b.name.to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: None,
        })
        .collect()
}

/// Template for a single keyed first-class id, if it exists in the catalog.
fn keyed_first_class_template(id: &str) -> Option<ProviderAccount> {
    keyed_first_class_catalog()
        .into_iter()
        .find(|a| a.id == id)
}

/// MiniMax's Claude-Code-parity default tier map. **Single source** consumed by
/// (issue #571 / ADR-0025):
/// - [`first_class_surfaces`] for `"minimax"` — the Anthropic surface tier
/// - [`crate::agent::provider::provider_conf::minimax_backend_env`] — the
///   session-naming side-channel
/// - attach-time pairing defaults via [`resolve_pairing`]
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

/// Kimi's default tier map for the **First-class Model Provider** `kimi`.
/// **Single source** consumed by the Anthropic surface's `model_tiers` in
/// `first_class_surfaces("kimi")` and attach-time pairing defaults (ADR-0025).
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

/// DeepSeek's default tier map for the **First-class Model Provider**
/// `deepseek` (issue #1127). DeepSeek's two production model lines:
///   - `deepseek-chat` (V3.x) — the general-purpose chat / mid-tier code model.
///   - `deepseek-reasoner` (R1) — the strong-reasoning model.
///
/// Per the first-class rule pinned in
/// `resolve_provider_env_composite_kimi_default_fable_matches_opus`,
/// `default` mirrors `opus` and `fable` falls back to `opus` when unset.
/// Here we explicitly set fable/opus/reasoner to `deepseek-reasoner` so the
/// Anthropic-compatible Claude Code backend gets a strong reasoning model for
/// the `Opus` / `Fable` aliases and the fast chat model for `Sonnet` /
/// `Haiku` / `small_fast`.
///
/// **Single source** consumed by the Anthropic surface's `model_tiers` in
/// `first_class_surfaces("deepseek")` and attach-time pairing defaults
/// (ADR-0025). The OpenAI surface passes only the `default` tier (Codex
/// takes a single model), which `first_class_surfaces` pins to
/// `deepseek-chat` directly.
pub(crate) fn deepseek_default_tiers() -> ModelTiers {
    ModelTiers {
        // `default` mirrors `opus` per the first-class rule.
        default: Some("deepseek-reasoner".to_string()),
        // Strong-reasoning tier: route Opus + Fable through R1.
        opus: Some("deepseek-reasoner".to_string()),
        fable: Some("deepseek-reasoner".to_string()),
        // Mid-tier code: deepseek-chat (V3.x) is the fastest code-friendly
        // model DeepSeek ships.
        sonnet: Some("deepseek-chat".to_string()),
        // Cheap / background: deepseek-chat covers the haiku + small_fast
        // slots so Claude Code's "small fast" alias doesn't 404 on the
        // custom endpoint.
        haiku: Some("deepseek-chat".to_string()),
        small_fast: Some("deepseek-chat".to_string()),
    }
}

/// The effective account list: code-defined defaults with the user's stored
/// `provider_accounts` merged over them by `id` (user wins / new ids append).
/// Mirrors [`harness_profiles`] so a built-in can never be removed by an empty
/// or partial `preferences.json`.
pub fn provider_accounts() -> Vec<ProviderAccount> {
    let mut prefs = match load() {
        Ok(prefs) => prefs,
        Err(e) => {
            tracing::warn!("preferences::provider_accounts load failed, using defaults: {}", e);
            return default_provider_accounts();
        }
    };
    let merged = merge_provider_accounts(default_provider_accounts(), prefs.provider_accounts.clone());
    let (migrated, changed) = migrate_kimi_companion(merged);
    // One-shot persistence: if a PR #1044 `kimi-via-claude` row was carried
    // over to the first-class `kimi` row, write the cleaned list back so the
    // stale row stops haunting subsequent reads. Subsequent reads see no
    // companion, so `changed` is false and the save is skipped.
    if changed {
        prefs.provider_accounts = migrated.clone();
        if let Err(e) = save(prefs) {
            tracing::warn!("preferences::provider_accounts migration save failed: {}", e);
        }
    }
    migrated
}

/// Migrate any stored `kimi-via-claude` companion (left over from PR #1044)
/// into the first-class `kimi` row and drop the companion. **Key only**
/// (ADR-0025 — endpoint fields live on pairings; JSON migration handles
/// legacy endpoint → pairing before this runs). Returns the migrated list
/// plus a `changed` flag so the caller can persist the result when the
/// migration actually moved state. Pure — no disk side effects.
fn migrate_kimi_companion(
    mut accounts: Vec<ProviderAccount>,
) -> (Vec<ProviderAccount>, bool) {
    let Some(companion_idx) = accounts.iter().position(|a| a.id == "kimi-via-claude") else {
        return (accounts, false);
    };
    let companion = accounts[companion_idx].clone();
    if let Some(kimi) = accounts.iter_mut().find(|a| a.id == "kimi") {
        if kimi.api_key.as_ref().is_none_or(|v| v.is_empty()) {
            kimi.api_key = companion.api_key.clone();
        }
    } else if let Some(mut kimi) = keyed_first_class_template("kimi") {
        kimi.api_key = companion.api_key.clone();
        if !companion.enabled {
            kimi.enabled = false;
        }
        accounts.push(kimi);
    }
    accounts.remove(companion_idx);
    (accounts, true)
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

/// Resolve the effective OpenAI Platform API key from the merged provider-
/// accounts list (issue #1109, ADR-0026). New keyed id — no legacy flat
/// field; identical lookup shape to [`kimi_api_key_resolved`] but kept
/// separate so the future legacy-fallback seam stays one symbol per
/// provider. Empty strings collapse to `None` so a half-cleared config
/// doesn't surface as a logged-out row.
pub fn openai_api_key_resolved() -> Option<String> {
    merge_provider_accounts(default_provider_accounts(), load().ok()?.provider_accounts)
        .into_iter()
        .find(|a| a.id == "openai")
        .and_then(|a| a.api_key)
        .filter(|v| !v.is_empty())
}

/// Resolve the effective DeepSeek API key from the merged provider-accounts
/// list (issue #1127). Brand-new keyed id — no legacy flat field; identical
/// lookup shape to [`openai_api_key_resolved`] so each provider's single seam
/// stays explicit. Empty strings collapse to `None` so a half-cleared config
/// doesn't surface as a logged-out row in the Usage panel.
pub fn deepseek_api_key_resolved() -> Option<String> {
    merge_provider_accounts(default_provider_accounts(), load().ok()?.provider_accounts)
        .into_iter()
        .find(|a| a.id == "deepseek")
        .and_then(|a| a.api_key)
        .filter(|v| !v.is_empty())
}

/// Upsert a provider account into `prefs` (by `id`). Pure: mutates the passed
/// `prefs` so the command layer stays a thin load→mutate→save.
///
/// No longer materializes a paired [`HarnessProfile`]: the spawn menu is now
/// *derived* from the accounts list (see `agent::provider_menu::compose_provider_menu`),
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
/// provider_id)` key (issue #576 / ADR-0025).
pub fn remove_provider_pairing(prefs: &mut AppPreferences, harness_id: &str, provider_id: &str) {
    prefs
        .provider_pairings
        .retain(|p| !(p.harness_id == harness_id && p.provider_id == provider_id));
}

/// Resolve the **capability descriptor** for a harness profile id, used by
/// the application-default validator (issue #1148 step 3). `None` when the
/// profile id doesn't name a known harness (built-in or user-added), so the
/// validator can reject unknown ids at the backend boundary — issue #1148
/// acceptance criteria 5 ("Unknown harness ids … are rejected at the backend
/// boundary").
///
/// Known = `harness_profiles()` carries the id, *or* `Provider::from_db_str`
/// parses it (built-in adapter ids, plus the legacy `"anthropic"` alias).
/// `resolve_harness_provider` already merges both sources and falls back to
/// the Anthropic executor on an unknown — we re-check by feeding the same
/// input into `harness_profiles()` + the built-in id whitelist so unknown
/// ids are refused.
pub fn harness_capabilities_for(profile_id: &str) -> Option<HarnessCapabilities> {
    if !is_known_harness_id(profile_id) {
        return None;
    }
    Some(capabilities_for(resolve_harness_provider(profile_id).adapter()))
}

/// True iff `profile_id` names a known Agent Harness — a built-in adapter id
/// or a stored `HarnessProfile`. Used by [`harness_capabilities_for`] to
/// reject unknown ids without leaking the silent `from_db_str → Anthropic`
/// fallback through the validator (issue #1148 AC #5).
///
/// Built-in ids are matched case-insensitively to mirror
/// [`crate::models::Provider::from_db_str`] (the executor resolver used by
/// `resolve_harness_provider`). Custom profile ids are matched case-
/// sensitively because they are user-defined strings — a user who names a
/// profile `"Claude"` keeps that exact id everywhere.
pub fn is_known_harness_id(profile_id: &str) -> bool {
    let trimmed = profile_id.trim();
    if trimmed.is_empty() {
        return false;
    }
    let normalized = trimmed.to_ascii_lowercase();
    // Built-ins (`BUILTIN_HARNESS_IDS` covers every adapter id plus the
    // legacy `"anthropic"` alias). `contains` short-circuits on the first
    // hit — same semantics as `iter().any()` but cheaper to read.
    if crate::agent::provider::BUILTIN_HARNESS_IDS
        .contains(&normalized.as_str())
    {
        return true;
    }
    // User-stored profiles (custom Claude-compatible profiles like
    // `"deepseek-via-claude"`). `harness_profiles()` is cached via the
    // preferences mutex so this is a single HashMap-style scan on the
    // post-init path.
    harness_profiles().iter().any(|p| p.id == profile_id)
}

/// Trim and collapse blank values on a single harness-default value (issue
/// #1148 acceptance criteria 6: "Blank values are normalized to absent").
/// Pure — the unit-test seam for "what does an empty input look like after
/// the boundary normalises it".
pub fn normalize_harness_default(raw: HarnessConfigValue) -> HarnessConfigValue {
    HarnessConfigValue {
        model: trim_to_none(raw.model.as_deref()),
        effort: trim_to_none(raw.effort.as_deref()),
    }
}

/// Trim a string slice and collapse empties to `None`. Local helper so
/// [`normalize_harness_default`] doesn't reach into
/// `agent::capabilities::normalize_non_empty` (a private seam there).
fn trim_to_none(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// Validate a harness default against the selected harness's capability
/// contract. Three rules (issue #1148 acceptance criteria 5):
///
/// * **Unknown harness id** → `Err`. The harness profile id must resolve to
///   a known adapter (built-in or user-added); an unrecognised id is refused
///   at the write boundary so corrupt config can't affect unrelated harnesses.
/// * **Effort on a harness without effort control** → `Err`. The harness's
///   `EffortControlKind` is the single contract; an effort value submitted
///   for a harness that advertises `None` is refused at the write boundary
///   (issue #1148 AC #5 "Accept effort only when the harness declares
///   effort support").
/// * **Effort value outside the harness's vocabulary** → `Err`. The
///   harness's `EffortControlKind::allowed` list is the single contract; a
///   value not in it is refused at the write boundary (issue #1148 AC #5
///   "Accept only values allowed by that harness's effort-control kind").
///
/// Model values pass through after trimming — there is no harness-side model
/// vocabulary; the harness either accepts the override or the resolver's
/// `supports_model_override` flag drops it on the spawn path. A blank model
/// collapses to `None` here so the storage-shape invariant
/// (`is_empty → no entry`) keeps working.
pub fn validate_harness_default(
    profile_id: &str,
    raw: HarnessConfigValue,
) -> Result<HarnessConfigValue, String> {
    let caps = harness_capabilities_for(profile_id)
        .ok_or_else(|| format!("unknown harness id '{profile_id}'"))?;
    let normalized = normalize_harness_default(raw);
    match &caps.effort_control {
        EffortControlKind::None => {
            if normalized.effort.is_some() {
                return Err(format!(
                    "harness '{profile_id}' does not support an effort override"
                ));
            }
        }
        EffortControlKind::Closed { allowed } | EffortControlKind::InlineConfig { allowed, .. } => {
            if let Some(value) = normalized.effort.as_deref() {
                if !allowed.iter().any(|a| a == value) {
                    return Err(format!(
                        "effort '{value}' is not allowed for harness '{profile_id}' \
                         (allowed: {allowed:?})"
                    ));
                }
            }
        }
    }
    Ok(normalized)
}

/// Upsert one harness's application default (issue #1148 / #1150).
/// Validates against the harness's capability descriptor; an empty
/// post-validation value **removes** the sparse map entry rather than
/// storing `{model: None, effort: None}` (issue #1148 acceptance criteria
/// 6 "an empty harness configuration removes its sparse entry").
///
/// Pure mutator — the caller wraps it in [`update`] (or loads + saves) so
/// the in-process cache refreshes on a successful persist.
pub fn upsert_harness_default(
    prefs: &mut AppPreferences,
    profile_id: &str,
    raw: HarnessConfigValue,
) -> Result<(), String> {
    let validated = validate_harness_default(profile_id, raw)?;
    if validated.is_empty() {
        prefs.harness_defaults.remove(profile_id);
    } else {
        prefs.harness_defaults.insert(profile_id.to_string(), validated);
    }
    Ok(())
}

/// Remove one harness's application default. Idempotent — calling on a
/// missing id is a no-op (so the UI's "Reset" affordance never errors on a
/// harness that was already cleared).
pub fn remove_harness_default(prefs: &mut AppPreferences, profile_id: &str) {
    prefs.harness_defaults.remove(profile_id);
}

/// The stored default for `profile_id`, if any. The caller passes the value
/// straight into the resolver's `application` slot — the resolver's
/// capability mask is what actually gates whether the value reaches the
/// harness process (issue #1148 acceptance criteria 6).
pub fn harness_default_for(prefs: &AppPreferences, profile_id: &str) -> Option<HarnessConfigValue> {
    prefs.harness_defaults.get(profile_id).cloned()
}

/// Set a provider account's **global** API key only if it currently has none
/// (the "set-if-absent from the attach flow" rule, ADR-0016 §4 / issue #576).
/// Returns whether a key was written. The canonical key editor stays on the
/// Providers page; this lets the harness-config attach flow seed a key for a
/// provider the user hasn't configured yet without ever *overwriting* one.
///
/// Looks up the effective account (defaults + stored). For a keyed first-class
/// id not yet materialised, seeds from [`keyed_first_class_catalog`].
pub fn set_account_key_if_absent(prefs: &mut AppPreferences, provider_id: &str, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let effective =
        merge_provider_accounts(default_provider_accounts(), prefs.provider_accounts.clone());
    let Some(account) = effective
        .into_iter()
        .find(|a| a.id == provider_id)
        .or_else(|| keyed_first_class_template(provider_id))
    else {
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

/// The stored **Proxied Provider** pairings in preferences (ADR-0025 / issue
/// #576). A load failure is logged and treated as "no stored pairings".
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
    // Known = effective accounts ∪ keyed first-class catalog (ADR-0025: keyed
    // rows may not be materialised yet but are still attachable / orderable).
    let mut known_ids: std::collections::HashSet<String> = provider_accounts()
        .into_iter()
        .map(|a| a.id)
        .collect();
    for a in keyed_first_class_catalog() {
        known_ids.insert(a.id);
    }
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

/// Resolve the **stored** pairing for spawn / env (ADR-0025). Spawn is
/// stored-only — a composite spawn id without a stored attach yields `None`
/// (empty env), so a keyless account never auto-spawns.
fn resolve_pairing(
    harness_id: &str,
    account: &ProviderAccount,
    stored: &[ProviderPairing],
) -> Option<ProviderPairing> {
    stored
        .iter()
        .find(|p| p.harness_id == harness_id && p.provider_id == account.id)
        .cloned()
}

pub fn resolve_stored_pairing_and_account(
    spawn_option_id: &str,
) -> Result<Option<(ProviderPairing, ProviderAccount)>, String> {
    let (harness_id, provider_id) =
        crate::agent::provider::parse_spawn_option_id(spawn_option_id);
    let Some(provider_id) = provider_id else {
        return Ok(None);
    };
    let accounts = provider_accounts();
    let account = accounts
        .into_iter()
        .find(|account| account.id == provider_id)
        .ok_or_else(|| format!("provider account '{provider_id}' is missing"))?;
    let pairing = resolve_pairing(harness_id, &account, &provider_pairings())
        .ok_or_else(|| format!("pairing '{harness_id}:{provider_id}' is missing"))?;
    Ok(Some((pairing, account)))
}

/// Attach-form defaults for `(harness, provider)`: stored pairing wins, else
/// first-class published endpoint for the harness surface. Generics without a
/// stored pairing return a surface-only shell (`base_url = None`) so the UI
/// can still show the form; the attach command requires a non-empty URL.
fn attach_pairing_defaults(
    harness_id: &str,
    account: &ProviderAccount,
    stored: &[ProviderPairing],
    surface_of: impl Fn(&str) -> Option<ApiSurface>,
) -> Option<ProviderPairing> {
    if let Some(p) = resolve_pairing(harness_id, account, stored) {
        return Some(p);
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
    // Generic: surface from harness, empty endpoint for the user to fill.
    if account.claude_compatible {
        return Some(ProviderPairing {
            harness_id: harness_id.to_string(),
            provider_id: account.id.clone(),
            surface,
            base_url: None,
            model_tiers: ModelTiers::default(),
        });
    }
    None
}

/// The full set of **Proxied Provider** pairings to render in the Spawn Menu
/// (ADR-0025 / issue #576): **stored pairings only**, filtered to proxiable
/// accounts. No derived default Anthropic pairing on key alone. Pure (no
/// disk/globals) — the unit-test seam for the menu derivation.
///
/// "Proxiable" = enabled, Claude-compatible, and keyed (non-empty API key).
pub(crate) fn effective_pairings(
    accounts: &[ProviderAccount],
    stored: &[ProviderPairing],
) -> Vec<ProviderPairing> {
    let keyed = |a: &ProviderAccount| a.api_key.as_deref().is_some_and(|k| !k.is_empty());
    let is_proxiable = |a: &ProviderAccount| a.enabled && a.claude_compatible && keyed(a);
    let proxiable_ids: std::collections::HashSet<&str> = accounts
        .iter()
        .filter(|a| is_proxiable(a))
        .map(|a| a.id.as_str())
        .collect();
    stored
        .iter()
        .filter(|p| proxiable_ids.contains(p.provider_id.as_str()))
        .cloned()
        .collect()
}

/// Build the backend-selecting environment for a **Spawn Option** (issue #576 /
/// ADR-0025) — the pairing-scoped, surface-aware successor to the #538
/// account-only resolver.
///
/// Resolution by id shape ([`crate::agent::provider::parse_spawn_option_id`]):
///   * **Composite proxied id** (`<harness>:<provider>`, e.g. `claude:minimax`,
///     `codex:minimax`): resolve the `(harness, provider)` **stored pairing**
///     and emit env for that pairing's surface — `ANTHROPIC_*` for `Anthropic`,
///     `OPENAI_*` for `OpenAI` — using the account's **global** API key.
///     Stored-only — no first-class synthesis without an explicit attach
///     (so a keyless account never auto-spawns).
///   * **Bare id** (`minimax`, a custom account id, or a native harness id):
///     look up the bare id as an account and resolve via its stored Claude
///     Anthropic pairing + account key (legacy pre-composite node ids).
///
/// Returns empty when no account matches or no stored pairing exists — the
/// spawn path resets inherited backend vars first (see
/// [`crate::agent::provider::AgentProvider::resets_backend_env`]), so empty
/// means a clean slate, not a leaked override.
pub fn resolve_provider_env(spawn_option_id: &str) -> Vec<(String, String)> {
    let (harness_id, provider_id) =
        crate::agent::provider::parse_spawn_option_id(spawn_option_id);
    let accounts = provider_accounts();
    let pairings = provider_pairings();
    match provider_id {
        Some(provider_id) => {
            let Some(account) = accounts.iter().find(|a| a.id == provider_id) else {
                return Vec::new();
            };
            let Some(pairing) = resolve_pairing(harness_id, account, &pairings) else {
                return Vec::new();
            };
            surface_env(
                pairing.surface,
                pairing.base_url.as_deref(),
                account.api_key.as_deref(),
                &pairing.model_tiers,
            )
        }
        None => {
            let Some(account) = accounts.iter().find(|a| a.id == spawn_option_id) else {
                return Vec::new();
            };
            provider_account_env(account, &pairings, &claude_harness_id())
        }
    }
}

/// Preflight gate run by `spawn_agent_inner` BEFORE [`resolve_provider_env`].
/// Catches the silent-fail trap where a Claude-compatible custom endpoint
/// (OpenRouter, or any Generic provider with a non-empty pairing `base_url`)
/// launches `claude` against a third-party backend without a primary model
/// pinned — `claude` then sends its hardcoded `claude-3-5-sonnet-<date>`
/// default, the third-party rejects it (OpenRouter expects `provider/model`
/// slugs), and the user only sees a server-side `tracing::warn` they can't
/// reach. Returning `Err` here surfaces the issue as the spawn result, so the
/// UI can prompt the user to fill the `Default model` tier on the **Harnesses**
/// page (ADR-0025).
pub fn preflight_resolve_provider_env(spawn_option_id: &str) -> Result<(), String> {
    let (harness_id, provider_id) =
        crate::agent::provider::parse_spawn_option_id(spawn_option_id);
    let accounts = provider_accounts();
    let pairings = provider_pairings();
    let (pairing_opt, account_id) = match provider_id {
        Some(pid) => {
            let Some(account) = accounts.iter().find(|a| a.id == pid) else {
                return Ok(());
            };
            let pairing = resolve_pairing(harness_id, account, &pairings);
            (pairing, account.id.clone())
        }
        None => {
            let Some(account) = accounts.iter().find(|a| a.id == spawn_option_id) else {
                return Ok(());
            };
            let pairing = stored_claude_anthropic_pairing(account, &pairings, &claude_harness_id())
                .cloned();
            (pairing, account.id.clone())
        }
    };
    preflight_pairing_env(pairing_opt.as_ref(), &account_id)
}

/// Pure helper shared with the unit tests — given the already-resolved pairing
/// (or `None` for the vanilla-Anthropic path), returns `Err` iff the pairing
/// routes through a non-empty `base_url` but has no primary model pinned.
/// Split out from the disk-reading wrapper so the rule is testable without
/// touching the global preferences cache.
fn preflight_pairing_env(
    pairing: Option<&ProviderPairing>,
    account_id: &str,
) -> Result<(), String> {
    let Some(pairing) = pairing else {
        return Ok(());
    };
    let base_url_is_set = pairing.base_url.as_deref().is_some_and(|s| !s.is_empty());
    if !base_url_is_set {
        return Ok(());
    }
    // OpenAI surface only needs a model when one is required by the consumer;
    // the OpenRouter-style 400 trap is Anthropic-surface specific (claude
    // sends a hardcoded claude-* slug). Gate Anthropic pairings only.
    if pairing.surface != ApiSurface::Anthropic {
        return Ok(());
    }
    if pairing.model_tiers.default.as_deref().is_none_or(|s| s.is_empty()) {
        return Err(format!(
            "Custom Claude-compatible endpoint '{account_id}' requires the 'Default model' tier to be set. Open the Harnesses page and configure it (e.g. 'anthropic/claude-3-5-sonnet-latest' for Claude via OpenRouter)."
        ));
    }
    Ok(())
}

/// Look up the stored Claude Anthropic pairing for a bare-id account, if any.
fn stored_claude_anthropic_pairing<'a>(
    account: &ProviderAccount,
    pairings: &'a [ProviderPairing],
    claude_harness_id: &str,
) -> Option<&'a ProviderPairing> {
    pairings.iter().find(|p| {
        p.provider_id == account.id
            && p.surface == ApiSurface::Anthropic
            && (p.harness_id == claude_harness_id || p.harness_id == "claude")
    })
}

/// Bare-id path for [`resolve_provider_env`]: resolve via the account's stored
/// Claude Anthropic pairing + global API key (ADR-0025 — no account endpoint).
fn provider_account_env(
    account: &ProviderAccount,
    pairings: &[ProviderPairing],
    claude_harness_id: &str,
) -> Vec<(String, String)> {
    let Some(pairing) = stored_claude_anthropic_pairing(account, pairings, claude_harness_id)
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
    /// they must run serially. Other modules reach this via
    /// [`test_state_guard`].
    pub(crate) static TEST_LOCK: TestMutex<()> = TestMutex::new(());
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
    fn failed_update_does_not_publish_candidate_to_cache() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let app_data_file = test_dir();
        std::fs::write(&app_data_file, "not a directory").unwrap();
        init_for_tests(app_data_file.clone());

        assert_eq!(load().unwrap().default_provider, None);
        let error = update(|prefs| prefs.default_provider = Some("must-not-leak".into()))
            .unwrap_err();
        assert!(error.contains("app data dir") || error.contains("temporary preferences"));
        assert_eq!(load().unwrap().default_provider, None);

        reset_for_tests();
        std::fs::remove_file(app_data_file).unwrap();
    }

    #[test]
    fn save_then_load_round_trip() {
        with_temp_dir(|_| {
            // Migration flag is set up front so a fresh prefs round-trip
            // doesn't flip it during the disk read.
            let prefs = AppPreferences {
                default_provider: Some("minimax".to_string()),
                ad0025_account_pairings_migrated: true,
                ..Default::default()
            };
            save(prefs.clone()).unwrap();
            // Clear cache to force a disk read.
            *lock_cache() = None;
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
            *lock_cache() = None;
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
            *lock_cache() = None;

            ensure_default_provider_normalized().unwrap();

            *lock_cache() = None;
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
            *lock_cache() = None;

            ensure_default_provider_normalized().unwrap();

            *lock_cache() = None;
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
            *lock_cache() = None;

            ensure_default_provider_normalized().unwrap();

            *lock_cache() = None;
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
            *lock_cache() = None;

            ensure_default_provider_normalized().unwrap();

            *lock_cache() = None;
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
            *lock_cache() = None;

            ensure_default_provider_normalized().unwrap();

            *lock_cache() = None;
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
            *lock_cache() = None;
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
            *lock_cache() = None;
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
            *lock_cache() = None;
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
            *lock_cache() = None;
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
            *lock_cache() = None; // force a disk read
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
            *lock_cache() = None;
            // The scan re-detects "claude" with the default label — but the
            // user's name must win (id already present → skipped).
            let added = merge_detected_profiles(vec![HarnessProfile {
                id: "claude".into(),
                name: "Claude Code".into(),
                harness: "anthropic".into(),
            }])
            .unwrap();
            assert_eq!(added, 0);
            *lock_cache() = None;
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
            *lock_cache() = None; // force a disk read
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
            *lock_cache() = None; // force a disk read
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
            *lock_cache() = None;
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
            *lock_cache() = None;
            assert_eq!(harness_order(), vec!["claude".to_string(), "codex".to_string()]);
        });
    }

    #[test]
    fn set_harness_order_filters_out_terminal() {
        with_temp_dir(|_| {
            // Terminal is forced last by the ordering logic, so it's never stored.
            set_harness_order(vec!["claude".into(), "terminal".into(), "codex".into()]).unwrap();
            *lock_cache() = None;
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
            *lock_cache() = None; // force a disk read
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
            *lock_cache() = None;
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
            *lock_cache() = None;
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
            *lock_cache() = None;
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
            *lock_cache() = None;
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
            *lock_cache() = None;
            assert_eq!(proxied_order_for("claude"), None);
            assert!(proxied_provider_order().is_empty());
        });
    }

    // ─── Provider accounts (issue #537 / ADR-0025) ───────────────────────────

    fn custom_account(id: &str) -> ProviderAccount {
        ProviderAccount {
            id: id.to_string(),
            name: format!("Custom {id}"),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        }
    }

    fn claude_pairing(provider_id: &str, base_url: &str, default_model: &str) -> ProviderPairing {
        ProviderPairing {
            harness_id: "claude".into(),
            provider_id: provider_id.into(),
            surface: ApiSurface::Anthropic,
            base_url: Some(base_url.into()),
            model_tiers: ModelTiers {
                default: Some(default_model.into()),
                ..ModelTiers::default()
            },
        }
    }

    fn keyed_minimax(key: &str) -> ProviderAccount {
        ProviderAccount {
            api_key: Some(key.into()),
            ..keyed_first_class_catalog()
                .into_iter()
                .find(|a| a.id == "minimax")
                .unwrap()
        }
    }

    fn keyed_kimi(key: &str) -> ProviderAccount {
        ProviderAccount {
            api_key: Some(key.into()),
            ..keyed_first_class_catalog()
                .into_iter()
                .find(|a| a.id == "kimi")
                .unwrap()
        }
    }

    #[test]
    fn default_provider_accounts_are_self_auth_only() {
        let ids: Vec<_> = default_provider_accounts().into_iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["anthropic", "codex", "agy", "grok", "opencode", "cursor"]);
        for a in default_provider_accounts() {
            assert!(!a.claude_compatible, "{} must be self-auth", a.id);
            assert_eq!(a.billing_mode, BillingMode::Plan);
            assert!(a.enabled);
            assert!(a.api_key.is_none());
        }
    }

    #[test]
    fn default_provider_accounts_cover_the_builtin_providers() {
        // Retained name; ADR-0025 defaults are self-auth only.
        let ids: Vec<_> = default_provider_accounts().into_iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["anthropic", "codex", "agy", "grok", "opencode", "cursor"]);
        let catalog_ids: Vec<_> = keyed_first_class_catalog().into_iter().map(|a| a.id).collect();
        // `openai` joins the keyed catalog in issue #1109 / ADR-0026 — its
        // /v1/organization/costs fetcher is admin-scoped, so the row is
        // keyed (user-supplied `sk-admin-…` or `sk-proj-…`) rather than
        // self-auth. PayAsYouGo because the costs API reports monthly spend.
        // `deepseek` joins in issue #1127 — keyed (user-supplied DeepSeek
        // API key), PayAsYouGo because the balance endpoint reports cash.
        // Order matches `BUILTIN_PROVIDER_ACCOUNTS` insertion order (the
        // catalog is `filter(!self_auth)` over the source list).
        assert_eq!(catalog_ids, vec!["kimi", "minimax", "openrouter", "deepseek", "openai"]);
        for id in &["kimi", "minimax", "openrouter", "deepseek", "openai"] {
            let a = keyed_first_class_catalog()
                .into_iter()
                .find(|a| a.id == *id)
                .unwrap();
            assert!(a.claude_compatible);
            assert_eq!(a.billing_mode, BillingMode::PayAsYouGo);
            assert!(a.enabled);
            assert!(a.api_key.is_none());
        }
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
        assert_eq!(defaults.len(), 6);
        let stored = vec![
            ProviderAccount {
                id: "minimax".to_string(),
                name: "MiniMax".to_string(),
                enabled: false,
                billing_mode: BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: None,
            },
            custom_account("deepseek"),
        ];
        let merged = merge_provider_accounts(defaults, stored);
        assert_eq!(merged.iter().filter(|a| a.id == "minimax").count(), 1);
        assert!(!merged.iter().find(|a| a.id == "minimax").unwrap().enabled);
        assert!(merged.iter().any(|a| a.id == "deepseek"));
        // 6 self-auth + minimax + deepseek
        assert_eq!(merged.len(), 8);
        // storing only a custom → 7
        let just_custom = merge_provider_accounts(default_provider_accounts(), vec![custom_account("x")]);
        assert_eq!(just_custom.len(), 7);
        // storing only minimax → 7
        let just_mm = merge_provider_accounts(default_provider_accounts(), vec![keyed_minimax("sk")]);
        assert_eq!(just_mm.len(), 7);
    }

    #[test]
    fn merge_provider_accounts_rederives_claude_compatible_from_id() {
        let stored = vec![
            ProviderAccount {
                claude_compatible: true,
                ..custom_account("anthropic")
            },
            ProviderAccount {
                claude_compatible: false,
                ..custom_account("deepseek")
            },
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
                    ProviderAccount {
                        id: "codex".to_string(),
                        name: "OpenAI / Codex".to_string(),
                        enabled: false,
                        billing_mode: BillingMode::Plan,
                        claude_compatible: false,
                        api_key: None,
                    },
                    custom_account("glm"),
                ],
                ..Default::default()
            })
            .unwrap();
            *lock_cache() = None;

            let all = provider_accounts();
            assert!(all.iter().any(|a| a.id == "glm"));
            assert!(all.iter().any(|a| a.id == "anthropic"));
            assert!(!all.iter().find(|a| a.id == "codex").unwrap().enabled);
        });
    }

    #[test]
    fn minimax_api_key_resolved_prefers_account_then_legacy_field() {
        with_temp_dir(|_| {
            save(AppPreferences {
                minimax_api_key: Some("legacy-key".to_string()),
                ..Default::default()
            })
            .unwrap();
            *lock_cache() = None;
            assert_eq!(minimax_api_key_resolved(), Some("legacy-key".to_string()));

            save(AppPreferences {
                minimax_api_key: Some("legacy-key".to_string()),
                provider_accounts: vec![ProviderAccount {
                    id: "minimax".to_string(),
                    name: "MiniMax".to_string(),
                    enabled: true,
                    billing_mode: BillingMode::PayAsYouGo,
                    claude_compatible: true,
                    api_key: Some("account-key".to_string()),
                }],
                ..Default::default()
            })
            .unwrap();
            *lock_cache() = None;
            assert_eq!(minimax_api_key_resolved(), Some("account-key".to_string()));

            save(AppPreferences::default()).unwrap();
            *lock_cache() = None;
            assert_eq!(minimax_api_key_resolved(), None);
        });
    }

    #[test]
    fn upsert_account_stores_it_without_a_paired_profile() {
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

    // ─── Spawn-time backend env injection (issue #538 / ADR-0025) ────────────

    #[test]
    fn provider_account_env_injects_from_stored_claude_pairing() {
        let account = custom_account("deepseek");
        let pairings = vec![claude_pairing("deepseek", "https://example.com/v1", "model-a")];
        let env = provider_account_env(&account, &pairings, "claude");
        assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://example.com/v1".to_string())));
        assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-test".to_string())));
        assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "model-a".to_string())));
    }

    #[test]
    fn provider_account_env_pins_alias_models_and_timeout_for_custom_endpoint() {
        let account = custom_account("deepseek");
        let pairings = vec![claude_pairing("deepseek", "https://example.com/v1", "model-a")];
        let env = provider_account_env(&account, &pairings, "claude");
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
    fn provider_account_env_empty_without_stored_pairing() {
        let account = custom_account("deepseek");
        assert!(provider_account_env(&account, &[], "claude").is_empty());
        let anthropic = default_provider_accounts()
            .into_iter()
            .find(|a| a.id == "anthropic")
            .unwrap();
        assert!(provider_account_env(&anthropic, &[], "claude").is_empty());
    }

    #[test]
    fn provider_account_env_uses_pairing_model_tiers() {
        let account = custom_account("glm");
        let mut pairing = claude_pairing("glm", "https://example.com/v1", "GLM-4.6");
        pairing.model_tiers = ModelTiers {
            default: Some("GLM-4.6".to_string()),
            small_fast: Some("GLM-4-Flash".to_string()),
            sonnet: Some("GLM-4.6".to_string()),
            opus: Some("GLM-4.6-Max".to_string()),
            fable: None,
            haiku: Some("GLM-4-Flash".to_string()),
        };
        let env = provider_account_env(&account, &[pairing], "claude");
        assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "GLM-4.6".to_string())));
        assert!(env.contains(&("ANTHROPIC_SMALL_FAST_MODEL".to_string(), "GLM-4-Flash".to_string())));
        assert!(env.contains(&("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), "GLM-4.6-Max".to_string())));
        assert!(env.contains(&("ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(), "GLM-4.6-Max".to_string())));
    }

    /// The Fable alias (Claude 5) is pinned for custom endpoints: an explicit
    /// `fable` tier wins; unset falls back to the opus pick.
    #[test]
    fn anthropic_surface_env_pins_fable_alias_with_opus_fallback() {
        let mut tiers = kimi_default_tiers();
        assert_eq!(tiers.fable, tiers.opus, "built-in kimi defaults fable to the opus pick");
        assert_eq!(
            minimax_default_tiers().fable,
            minimax_default_tiers().opus,
            "built-in minimax defaults fable to the opus pick"
        );
        assert_eq!(
            deepseek_default_tiers().fable,
            deepseek_default_tiers().opus,
            "built-in deepseek defaults fable to the opus pick"
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
    fn builtin_minimax_pairing_reproduces_claude_code_model_routing() {
        let account = keyed_minimax("sk-mm");
        let pairing = ProviderPairing {
            harness_id: "claude".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.minimax.io/anthropic".into()),
            model_tiers: minimax_default_tiers(),
        };
        let env = provider_account_env(&account, &[pairing], "claude");
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
    fn builtin_deepseek_pairing_reproduces_claude_code_model_routing() {
        // Issue #1127: DeepSeek's Anthropic-compatible Claude Code backend
        // receives the full per-tier alias map (issue #567). The default
        // tier map routes Opus/Fable to `deepseek-reasoner` (R1) and the
        // fast tiers (Sonnet/Haiku/small_fast) to `deepseek-chat` (V3.x).
        let account = ProviderAccount {
            api_key: Some("sk-deepseek".to_string()),
            ..keyed_first_class_catalog()
                .into_iter()
                .find(|a| a.id == "deepseek")
                .unwrap()
        };
        let pairing = ProviderPairing {
            harness_id: "claude".into(),
            provider_id: "deepseek".into(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.deepseek.com/anthropic".into()),
            model_tiers: deepseek_default_tiers(),
        };
        let env = provider_account_env(&account, &[pairing], "claude");
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("ANTHROPIC_BASE_URL"),
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(get("ANTHROPIC_AUTH_TOKEN"), Some("sk-deepseek"));
        assert_eq!(get("ANTHROPIC_MODEL"), Some("deepseek-reasoner"));
        assert_eq!(get("ANTHROPIC_SMALL_FAST_MODEL"), Some("deepseek-chat"));
        assert_eq!(
            get("ANTHROPIC_DEFAULT_SONNET_MODEL"),
            Some("deepseek-chat")
        );
        assert_eq!(
            get("ANTHROPIC_DEFAULT_OPUS_MODEL"),
            Some("deepseek-reasoner")
        );
        assert_eq!(
            get("ANTHROPIC_DEFAULT_FABLE_MODEL"),
            Some("deepseek-reasoner")
        );
        assert_eq!(get("ANTHROPIC_DEFAULT_HAIKU_MODEL"), Some("deepseek-chat"));
        assert_eq!(get("API_TIMEOUT_MS"), Some("3000000"));
    }

    #[test]
    fn resolve_provider_env_reads_pairing_for_bare_id() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, custom_account("deepseek"));
            prefs.provider_pairings.push(claude_pairing(
                "deepseek",
                "https://example.com/v1",
                "model-a",
            ));
            save(prefs).unwrap();
            *lock_cache() = None;

            let env = resolve_provider_env("deepseek");
            assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://example.com/v1".to_string())));
            assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-test".to_string())));
        });
    }

    #[test]
    fn resolve_provider_env_for_keyed_builtin_minimax_injects_claude_code_env() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, keyed_minimax("sk-mm"));
            prefs.provider_pairings.push(ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "minimax".into(),
                surface: ApiSurface::Anthropic,
                base_url: Some("https://api.minimax.io/anthropic".into()),
                model_tiers: minimax_default_tiers(),
            });
            save(prefs).unwrap();
            *lock_cache() = None;

            let env = resolve_provider_env("minimax");
            assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://api.minimax.io/anthropic".to_string())));
            assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-mm".to_string())));
            assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "MiniMax-M3[1m]".to_string())));
        });
    }

    #[test]
    fn resolve_provider_env_empty_for_anthropic_default_and_unknown() {
        with_temp_dir(|_| {
            assert!(resolve_provider_env("anthropic").is_empty());
            assert!(resolve_provider_env("totally-unknown").is_empty());
        });
    }

    // ─── Compatible API surfaces + per-pairing config (issue #576 / ADR-0025) ─

    #[test]
    fn api_surface_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&ApiSurface::Anthropic).unwrap(), "\"anthropic\"");
        assert_eq!(serde_json::to_string(&ApiSurface::OpenAI).unwrap(), "\"openai\"");
    }

    #[test]
    fn first_class_surfaces_publishes_both_surfaces_for_minimax() {
        let surfaces = first_class_surfaces("minimax");
        let by = |s: ApiSurface| surfaces.iter().find(|e| e.surface == s).unwrap();
        assert_eq!(by(ApiSurface::Anthropic).base_url, "https://api.minimax.io/anthropic");
        assert_eq!(by(ApiSurface::Anthropic).model_tiers.default.as_deref(), Some("MiniMax-M3[1m]"));
        assert_eq!(by(ApiSurface::OpenAI).base_url, "https://api.minimax.io/v1");
        assert_eq!(by(ApiSurface::OpenAI).model_tiers.default.as_deref(), Some("MiniMax-M3"));
        // DeepSeek (issue #1127) publishes both surfaces too: an
        // Anthropic-compatible Claude Code backend and an OpenAI-compatible
        // `/v1` endpoint. Tier defaults use deepseek-reasoner for the
        // strongest tier and deepseek-chat for the fast path.
        let ds_surfaces = first_class_surfaces("deepseek");
        assert_eq!(ds_surfaces.len(), 2);
        let ds_anthropic = ds_surfaces
            .iter()
            .find(|e| e.surface == ApiSurface::Anthropic)
            .unwrap();
        assert_eq!(ds_anthropic.base_url, "https://api.deepseek.com/anthropic");
        assert_eq!(
            ds_anthropic.model_tiers.default.as_deref(),
            Some("deepseek-reasoner")
        );
        let ds_openai = ds_surfaces
            .iter()
            .find(|e| e.surface == ApiSurface::OpenAI)
            .unwrap();
        assert_eq!(ds_openai.base_url, "https://api.deepseek.com/v1");
        assert_eq!(ds_openai.model_tiers.default.as_deref(), Some("deepseek-chat"));
        assert!(first_class_surfaces("anthropic").is_empty());
        // OpenRouter is Anthropic-only with empty tiers.
        let or_surfaces = first_class_surfaces("openrouter");
        assert_eq!(or_surfaces.len(), 1);
        assert_eq!(or_surfaces[0].surface, ApiSurface::Anthropic);
        assert_eq!(or_surfaces[0].base_url, "https://openrouter.ai/api");
        assert_eq!(or_surfaces[0].model_tiers, ModelTiers::default());
    }

    #[test]
    fn surface_for_executor_maps_only_proxy_capable_harnesses() {
        assert_eq!(surface_for_executor(Provider::Anthropic), Some(ApiSurface::Anthropic));
        assert_eq!(surface_for_executor(Provider::Codex), Some(ApiSurface::OpenAI));
        assert_eq!(surface_for_executor(Provider::Terminal), None);
    }

    #[test]
    fn provider_surfaces_first_class_vs_generic_vs_self_auth() {
        let mm = keyed_first_class_catalog()
            .into_iter()
            .find(|a| a.id == "minimax")
            .unwrap();
        assert_eq!(
            provider_surfaces(&mm),
            vec![ApiSurface::Anthropic, ApiSurface::OpenAI]
        );
        // DeepSeek (issue #1127) is a first-class keyed provider — its
        // surfaces come from `first_class_surfaces`, not from the Generic
        // "both surfaces" fallback. The published surface set is identical
        // to MiniMax's, so the harness picker offers both Anthropic and
        // OpenAI attachments.
        let ds = keyed_first_class_catalog()
            .into_iter()
            .find(|a| a.id == "deepseek")
            .unwrap();
        assert_eq!(
            provider_surfaces(&ds),
            vec![ApiSurface::Anthropic, ApiSurface::OpenAI]
        );
        // Generic → both surfaces (ADR-0025). A custom (non-built-in) id
        // keeps the Generic fallback path even though `deepseek` itself is
        // now first-class.
        assert_eq!(
            provider_surfaces(&custom_account("glm")),
            vec![ApiSurface::Anthropic, ApiSurface::OpenAI]
        );
        let anth = default_provider_accounts()
            .into_iter()
            .find(|a| a.id == "anthropic")
            .unwrap();
        assert!(provider_surfaces(&anth).is_empty());
    }

    #[test]
    fn openai_surface_env_emits_openai_vars_only() {
        let tiers = ModelTiers {
            default: Some("MiniMax-M3[1m]".into()),
            ..ModelTiers::default()
        };
        let env = openai_surface_env(Some("https://api.minimax.io/v1"), Some("sk-mm"), &tiers);
        assert!(env.contains(&("OPENAI_BASE_URL".to_string(), "https://api.minimax.io/v1".to_string())));
        assert!(env.contains(&("OPENAI_API_KEY".to_string(), "sk-mm".to_string())));
        assert!(env.contains(&("OPENAI_MODEL".to_string(), "MiniMax-M3[1m]".to_string())));
        assert!(!env.iter().any(|(k, _)| k.starts_with("ANTHROPIC_")));
        assert!(!env.iter().any(|(k, _)| k == "OPENAI_SMALL_FAST_MODEL"));
    }

    #[test]
    fn surface_env_dispatches_by_surface() {
        let tiers = ModelTiers {
            default: Some("m".into()),
            ..ModelTiers::default()
        };
        let anth = surface_env(ApiSurface::Anthropic, Some("https://x/anthropic"), Some("k"), &tiers);
        assert!(anth.iter().any(|(k, _)| k == "ANTHROPIC_BASE_URL"));
        let oai = surface_env(ApiSurface::OpenAI, Some("https://x/v1"), Some("k"), &tiers);
        assert!(oai.iter().any(|(k, _)| k == "OPENAI_BASE_URL"));
    }

    #[test]
    fn anthropic_surface_env_force_blanks_api_key_for_custom_endpoints_only() {
        let tiers = ModelTiers {
            default: Some("m".into()),
            ..ModelTiers::default()
        };
        let custom_first_class =
            anthropic_surface_env(Some("https://openrouter.ai/api"), Some("sk-or-x"), &tiers);
        assert!(custom_first_class.contains(&("ANTHROPIC_API_KEY".to_string(), String::new())));
        let custom_generic =
            anthropic_surface_env(Some("https://relay.example.com/anthropic"), Some("sk-relay"), &tiers);
        assert!(custom_generic.contains(&("ANTHROPIC_API_KEY".to_string(), String::new())));
        let default_path = anthropic_surface_env(None, None, &tiers);
        assert!(!default_path.iter().any(|(k, _)| k == "ANTHROPIC_API_KEY"));
    }

    #[test]
    fn preflight_pairing_env_fails_for_custom_endpoint_with_empty_default_tier() {
        let pairing = ProviderPairing {
            harness_id: "claude".into(),
            provider_id: "openrouter".into(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://openrouter.ai/api".into()),
            model_tiers: ModelTiers::default(),
        };
        let err = preflight_pairing_env(Some(&pairing), "openrouter").unwrap_err();
        assert!(
            err.contains("Default model") && err.contains("openrouter") && err.contains("Harnesses"),
            "preflight should name the missing tier, account id, and Harnesses page, got: {err}"
        );
    }

    #[test]
    fn preflight_pairing_env_passes_for_custom_endpoint_with_default_tier_filled() {
        let pairing = ProviderPairing {
            harness_id: "claude".into(),
            provider_id: "openrouter".into(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://openrouter.ai/api".into()),
            model_tiers: ModelTiers {
                default: Some("anthropic/claude-3-5-sonnet-latest".to_string()),
                ..ModelTiers::default()
            },
        };
        assert!(preflight_pairing_env(Some(&pairing), "openrouter").is_ok());
    }

    #[test]
    fn preflight_pairing_env_passes_for_no_pairing() {
        assert!(preflight_pairing_env(None, "x").is_ok());
    }

    #[test]
    fn preflight_pairing_env_passes_for_first_class_pairing_with_tiers() {
        let pairing = ProviderPairing {
            harness_id: "claude".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.minimax.io/anthropic".into()),
            model_tiers: minimax_default_tiers(),
        };
        assert!(preflight_pairing_env(Some(&pairing), "minimax").is_ok());
    }

    fn test_surface_of(harness_id: &str) -> Option<ApiSurface> {
        match harness_id {
            "claude" => Some(ApiSurface::Anthropic),
            "codex" => Some(ApiSurface::OpenAI),
            _ => None,
        }
    }

    #[test]
    fn resolve_pairing_prefers_a_stored_pairing() {
        let account = keyed_minimax("sk");
        let stored = vec![ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://custom/v1".into()),
            model_tiers: ModelTiers {
                default: Some("override".into()),
                ..ModelTiers::default()
            },
        }];
        let got = resolve_pairing("codex", &account, &stored).unwrap();
        assert_eq!(got.base_url.as_deref(), Some("https://custom/v1"));
        assert_eq!(got.model_tiers.default.as_deref(), Some("override"));
    }

    #[test]
    fn resolve_pairing_honours_stored_anthropic_payload() {
        // Stored Anthropic pairing is source of truth — not re-derived.
        let account = keyed_minimax("sk");
        let stored = vec![ProviderPairing {
            harness_id: "claude".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://custom-proxy.example/anthropic".into()),
            model_tiers: ModelTiers {
                default: Some("custom-model".into()),
                ..ModelTiers::default()
            },
        }];
        let got = resolve_pairing("claude", &account, &stored).unwrap();
        assert_eq!(got.base_url.as_deref(), Some("https://custom-proxy.example/anthropic"));
        assert_eq!(got.model_tiers.default.as_deref(), Some("custom-model"));
    }

    #[test]
    fn resolve_pairing_requires_stored_row_for_spawn() {
        // Spawn path: no stored pairing → None (no silent first-class fill).
        let account = keyed_minimax("sk");
        assert!(resolve_pairing("codex", &account, &[]).is_none());
    }

    #[test]
    fn attach_pairing_defaults_derives_first_class_openai() {
        let account = keyed_minimax("sk");
        let got = attach_pairing_defaults("codex", &account, &[], test_surface_of).unwrap();
        assert_eq!(got.surface, ApiSurface::OpenAI);
        assert_eq!(got.base_url.as_deref(), Some("https://api.minimax.io/v1"));
    }

    #[test]
    fn resolve_pairing_generic_provider_only_on_anthropic() {
        // Generics without stored pairing → None for both surfaces.
        // With a stored OpenAI pairing they can attach OpenAI.
        let account = custom_account("deepseek");
        assert!(resolve_pairing("claude", &account, &[]).is_none());
        assert!(resolve_pairing("codex", &account, &[]).is_none());
        let stored = vec![ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "deepseek".into(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://example.com/v1".into()),
            model_tiers: ModelTiers {
                default: Some("ds".into()),
                ..ModelTiers::default()
            },
        }];
        let got = resolve_pairing("codex", &account, &stored).unwrap();
        assert_eq!(got.surface, ApiSurface::OpenAI);
        assert_eq!(got.base_url.as_deref(), Some("https://example.com/v1"));
    }

    #[test]
    fn effective_pairings_stored_only_no_auto_derive() {
        let accounts = vec![keyed_minimax("sk-mm")];
        // Keyed, no stored pairing → empty effective.
        assert!(effective_pairings(&accounts, &[]).is_empty());
        let stored = vec![ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://api.minimax.io/v1".into()),
            model_tiers: ModelTiers {
                default: Some("MiniMax-M3[1m]".into()),
                ..ModelTiers::default()
            },
        }];
        let pairings = effective_pairings(&accounts, &stored);
        assert_eq!(pairings.len(), 1);
        assert_eq!(pairings[0].harness_id, "codex");
        assert!(!pairings.iter().any(|p| p.harness_id == "claude"));
    }

    #[test]
    fn effective_pairings_returns_only_stored_proxiable() {
        // Stored pairings for proxiable (keyed + enabled + claude_compatible)
        // accounts are returned as-is. Unkeyed accounts contribute no rows.
        let accounts = vec![keyed_minimax("sk-mm")];
        let stored = vec![
            claude_pairing("minimax", "https://api.minimax.io/anthropic", "MiniMax-M3[1m]"),
            ProviderPairing {
                harness_id: "codex".into(),
                provider_id: "minimax".into(),
                surface: ApiSurface::OpenAI,
                base_url: Some("https://api.minimax.io/v1".into()),
                model_tiers: ModelTiers {
                    default: Some("MiniMax-M3[1m]".into()),
                    ..ModelTiers::default()
                },
            },
        ];
        let pairings = effective_pairings(&accounts, &stored);
        assert!(pairings.iter().any(|p| p.harness_id == "claude" && p.surface == ApiSurface::Anthropic));
        assert!(pairings.iter().any(|p| p.harness_id == "codex" && p.surface == ApiSurface::OpenAI));
        assert_eq!(pairings.len(), 2);
        // A stored row for a missing/unkeyed account is filtered by
        // `proxiable_ids`, so an unkeyed account → no pairings.
        let unkeyed = vec![ProviderAccount {
            id: "minimax".into(),
            ..keyed_minimax("sk-mm")
        }];
        assert!(effective_pairings(&unkeyed, &[]).is_empty());
    }

    #[test]
    fn effective_pairings_skips_unkeyed_disabled_or_self_auth_accounts() {
        let accounts = default_provider_accounts();
        let stored = vec![ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://api.minimax.io/v1".into()),
            model_tiers: ModelTiers::default(),
        }];
        assert!(effective_pairings(&accounts, &stored).is_empty());
    }

    #[test]
    fn resolve_provider_env_proxies_minimax_via_codex_with_openai_vars() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, keyed_minimax("sk-mm"));
            prefs.provider_pairings.push(ProviderPairing {
                harness_id: "codex".into(),
                provider_id: "minimax".into(),
                surface: ApiSurface::OpenAI,
                base_url: Some("https://api.minimax.io/v1".into()),
                model_tiers: ModelTiers {
                    default: Some("MiniMax-M3[1m]".into()),
                    ..ModelTiers::default()
                },
            });
            save(prefs).unwrap();
            *lock_cache() = None;

            let env = resolve_provider_env("codex:minimax");
            assert!(env.contains(&("OPENAI_BASE_URL".to_string(), "https://api.minimax.io/v1".to_string())));
            assert!(env.contains(&("OPENAI_API_KEY".to_string(), "sk-mm".to_string())));
            assert!(env.contains(&("OPENAI_MODEL".to_string(), "MiniMax-M3[1m]".to_string())));
            assert!(!env.iter().any(|(k, _)| k.starts_with("ANTHROPIC_")));
        });
    }

    #[test]
    fn resolve_provider_env_composite_without_stored_pairing_is_empty() {
        // Post-migration: saving a key alone does not synthesise a spawn
        // pairing. The migration flag has to be set so the legacy migration
        // doesn't auto-pair (which is the only path that did, one-shot).
        with_temp_dir(|_| {
            let mut prefs = AppPreferences {
                ad0025_account_pairings_migrated: true,
                ..Default::default()
            };
            upsert_provider_account(&mut prefs, keyed_minimax("sk-mm"));
            save(prefs).unwrap();
            *lock_cache() = None;

            assert!(resolve_provider_env("claude:minimax").is_empty());
        });
    }

    #[test]
    fn resolve_provider_env_composite_claude_minimax_uses_stored_pairing() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, keyed_minimax("sk-mm"));
            prefs.provider_pairings.push(ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "minimax".into(),
                surface: ApiSurface::Anthropic,
                base_url: Some("https://api.minimax.io/anthropic".into()),
                model_tiers: minimax_default_tiers(),
            });
            save(prefs).unwrap();
            *lock_cache() = None;

            let env = resolve_provider_env("claude:minimax");
            assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://api.minimax.io/anthropic".to_string())));
            assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-mm".to_string())));
            assert!(env.contains(&("ANTHROPIC_MODEL".to_string(), "MiniMax-M3[1m]".to_string())));
            assert!(!env.iter().any(|(k, _)| k.starts_with("OPENAI_")));
        });
    }

    #[test]
    fn resolve_provider_env_composite_uses_stored_pairing_tiers() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(
                &mut prefs,
                ProviderAccount {
                    id: "moonshot".into(),
                    name: "Moonshot Kimi".into(),
                    enabled: true,
                    billing_mode: BillingMode::PayAsYouGo,
                    claude_compatible: true,
                    api_key: Some("sk-moon".into()),
                },
            );
            prefs.provider_pairings.push(ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "moonshot".into(),
                surface: ApiSurface::Anthropic,
                base_url: Some("https://proxy.example.com/anthropic".into()),
                model_tiers: ModelTiers {
                    default: Some("kimi-k2.6".into()),
                    opus: Some("kimi-k3-preview".into()),
                    fable: Some("kimi-k3-fable".into()),
                    ..ModelTiers::default()
                },
            });
            save(prefs).unwrap();
            *lock_cache() = None;

            let env = resolve_provider_env("claude:moonshot");
            let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
            assert_eq!(get("ANTHROPIC_DEFAULT_OPUS_MODEL"), Some("kimi-k3-preview"));
            assert_eq!(get("ANTHROPIC_DEFAULT_FABLE_MODEL"), Some("kimi-k3-fable"));
            assert_eq!(get("ANTHROPIC_BASE_URL"), Some("https://proxy.example.com/anthropic"));
        });
    }

    #[test]
    fn resolve_provider_env_composite_kimi_default_fable_matches_opus() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, keyed_kimi("sk-moon"));
            save(prefs).unwrap();
            *lock_cache() = None;

            let env = resolve_provider_env("claude:kimi");
            let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
            assert_eq!(
                get("ANTHROPIC_DEFAULT_FABLE_MODEL"),
                get("ANTHROPIC_DEFAULT_OPUS_MODEL"),
                "first-class providers must default fable to the opus pick"
            );
        });
    }

    #[test]
    fn resolve_provider_env_composite_honours_stored_anthropic_pairing() {
        // Opposite of the old "ignore stale pairing" rule — stored is SoT.
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(
                &mut prefs,
                ProviderAccount {
                    id: "moonshot".into(),
                    name: "Moonshot Kimi".into(),
                    enabled: true,
                    billing_mode: BillingMode::PayAsYouGo,
                    claude_compatible: true,
                    api_key: Some("sk-moon".into()),
                },
            );
            prefs.provider_pairings.push(ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "moonshot".into(),
                surface: ApiSurface::Anthropic,
                base_url: Some("https://api.moonshot.ai/anthropic".into()),
                model_tiers: kimi_default_tiers(),
            });
            save(prefs).unwrap();
            *lock_cache() = None;

            let env = resolve_provider_env("claude:moonshot");
            let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
            assert_eq!(get("ANTHROPIC_DEFAULT_OPUS_MODEL"), Some("kimi-k3"));
            assert_eq!(get("ANTHROPIC_BASE_URL"), Some("https://api.moonshot.ai/anthropic"));
        });
    }

    #[test]
    fn upsert_and_remove_provider_pairing_by_harness_provider_key() {
        let mut prefs = AppPreferences::default();
        let pairing = |harness: &str| ProviderPairing {
            harness_id: harness.into(),
            provider_id: "minimax".into(),
            surface: if harness == "codex" {
                ApiSurface::OpenAI
            } else {
                ApiSurface::Anthropic
            },
            base_url: Some("https://x".into()),
            model_tiers: ModelTiers::default(),
        };
        upsert_provider_pairing(&mut prefs, pairing("codex"));
        upsert_provider_pairing(&mut prefs, pairing("claude"));
        assert_eq!(prefs.provider_pairings.len(), 2);
        let mut updated = pairing("codex");
        updated.base_url = Some("https://changed".into());
        upsert_provider_pairing(&mut prefs, updated);
        assert_eq!(
            prefs
                .provider_pairings
                .iter()
                .filter(|p| p.harness_id == "codex")
                .count(),
            1
        );
        assert_eq!(
            prefs
                .provider_pairings
                .iter()
                .find(|p| p.harness_id == "codex")
                .unwrap()
                .base_url
                .as_deref(),
            Some("https://changed")
        );
        remove_provider_pairing(&mut prefs, "codex", "minimax");
        assert!(!prefs.provider_pairings.iter().any(|p| p.harness_id == "codex"));
        assert!(prefs.provider_pairings.iter().any(|p| p.harness_id == "claude"));
    }

    #[test]
    fn set_account_key_if_absent_only_fills_an_empty_key() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            // MiniMax not yet materialised — seeded from keyed catalog.
            assert!(set_account_key_if_absent(&mut prefs, "minimax", "sk-mm"));
            save(prefs).unwrap();
            *lock_cache() = None;
            assert_eq!(minimax_api_key_resolved(), Some("sk-mm".to_string()));

            let mut prefs = load().unwrap();
            assert!(!set_account_key_if_absent(&mut prefs, "minimax", "sk-other"));
            assert!(!set_account_key_if_absent(&mut prefs, "kimi", ""));
        });
    }

    // ─── Issue #571 tidy-up: single source of truth for built-ins ───────────

    #[test]
    fn built_in_provider_accounts_table_is_consistent_with_default_provider_accounts() {
        // Defaults are the self-auth subset of the full built-in table.
        let self_auth_ids: Vec<&str> = BUILTIN_PROVIDER_ACCOUNTS
            .iter()
            .filter(|b| b.self_auth)
            .map(|b| b.id)
            .collect();
        let default_ids: Vec<String> = default_provider_accounts().into_iter().map(|a| a.id).collect();
        assert_eq!(
            self_auth_ids,
            default_ids.iter().map(String::as_str).collect::<Vec<_>>()
        );
        let catalog_ids: Vec<&str> = BUILTIN_PROVIDER_ACCOUNTS
            .iter()
            .filter(|b| !b.self_auth)
            .map(|b| b.id)
            .collect();
        let keyed_ids: Vec<String> = keyed_first_class_catalog().into_iter().map(|a| a.id).collect();
        assert_eq!(
            catalog_ids,
            keyed_ids.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }

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

    #[test]
    fn minimax_default_tiers_is_the_source_for_minimax_surface() {
        let surfaces = first_class_surfaces("minimax");
        let anthropic_surface = surfaces
            .iter()
            .find(|s| s.surface == ApiSurface::Anthropic)
            .expect("minimax must publish an Anthropic surface");
        assert_eq!(anthropic_surface.model_tiers, minimax_default_tiers());
    }

    #[test]
    fn resolve_provider_env_kimi_attached_to_claude_emits_moonshot() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, keyed_kimi("sk-moon-123"));
            // Composite path can still fill from first_class_surfaces without
            // a stored pairing; pin a stored one to exercise the pairing path.
            prefs.provider_pairings.push(ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "kimi".into(),
                surface: ApiSurface::Anthropic,
                base_url: Some("https://api.moonshot.ai/anthropic".into()),
                model_tiers: kimi_default_tiers(),
            });
            save(prefs).unwrap();
            *lock_cache() = None;

            let env = resolve_provider_env("claude:kimi");
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

    #[test]
    fn builtin_provider_accounts_have_no_via_substring_in_id() {
        for b in BUILTIN_PROVIDER_ACCOUNTS {
            assert!(
                !b.id.contains("via"),
                "Built-in provider account id '{}' contains 'via'",
                b.id,
            );
        }
    }

    #[test]
    fn kimi_via_claude_id_does_not_exist_in_default_provider_accounts() {
        let exists = default_provider_accounts()
            .iter()
            .any(|a| a.id == "kimi-via-claude");
        assert!(!exists);
        let catalog_has = keyed_first_class_catalog()
            .iter()
            .any(|a| a.id == "kimi-via-claude");
        assert!(!catalog_has);
    }

    #[test]
    fn kimi_is_first_class_claude_compatible_with_moonshot_endpoint() {
        // Not in defaults; lives in catalog + first_class_surfaces.
        assert!(!default_provider_accounts().iter().any(|a| a.id == "kimi"));
        let kimi = keyed_first_class_catalog()
            .into_iter()
            .find(|a| a.id == "kimi")
            .expect("kimi must exist in keyed first-class catalog");
        assert!(kimi.claude_compatible);
        assert_eq!(kimi.billing_mode, BillingMode::PayAsYouGo);

        let surfaces = first_class_surfaces("kimi");
        let anthropic = surfaces
            .iter()
            .find(|s| s.surface == ApiSurface::Anthropic)
            .expect("kimi must publish an Anthropic-compatible surface");
        assert_eq!(anthropic.base_url, "https://api.moonshot.ai/anthropic");
        assert_eq!(anthropic.model_tiers, kimi_default_tiers());
        assert!(
            !surfaces.iter().any(|s| s.surface == ApiSurface::OpenAI),
            "direct Kimi is Chat Completions-only and must not be offered to Codex",
        );
    }

    #[test]
    fn concurrent_updates_do_not_lose_results_or_leave_temporary_files() {
        with_temp_dir(|directory| {
            save(AppPreferences::default()).unwrap();
            let workers = (0..16)
                .map(|index| {
                    std::thread::spawn(move || {
                        update(|prefs| prefs.harness_order.push(format!("worker-{index}")))
                            .unwrap();
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                worker.join().unwrap();
            }
            let prefs = load().unwrap();
            assert_eq!(prefs.harness_order.len(), 16);
            assert!((0..16).all(|index| prefs.harness_order.contains(&format!("worker-{index}"))));
            assert_eq!(
                std::fs::read_dir(directory)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp"))
                    .count(),
                0
            );
        });
    }

    #[test]
    fn codex_compatibility_rejects_kimi_chat_and_unverified_minimax_alias() {
        let mut pairing = ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "kimi".into(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://api.moonshot.ai/v1".into()),
            model_tiers: ModelTiers {
                default: Some("kimi-k3".into()),
                ..Default::default()
            },
        };
        assert!(!pairing_compatibility(&pairing).compatible);

        pairing.provider_id = "minimax".into();
        pairing.base_url = Some("https://api.minimax.io/v1".into());
        pairing.model_tiers.default = Some("MiniMax-M3[1m]".into());
        let decision = pairing_compatibility(&pairing);
        assert!(!decision.compatible);
        assert!(decision.reason.unwrap().contains("MiniMax-M3"));

        pairing.model_tiers.default = Some("MiniMax-M3".into());
        assert!(pairing_compatibility(&pairing).compatible);
    }

    #[test]
    fn provider_accounts_migrates_stored_kimi_via_claude_into_first_class_kimi() {
        with_temp_dir(|tmp| {
            let stored_companion = ProviderAccount {
                id: "kimi-via-claude".to_string(),
                name: "Kimi via Claude Code".to_string(),
                enabled: true,
                billing_mode: BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("sk-moon-test".to_string()),
            };
            let mut prefs = AppPreferences::default();
            upsert_provider_account(&mut prefs, stored_companion);
            save(prefs).unwrap();
            *lock_cache() = None;

            let accounts = provider_accounts();
            assert!(
                accounts.iter().all(|a| a.id != "kimi-via-claude"),
                "companion id `kimi-via-claude` must be migrated away",
            );
            let kimi = accounts
                .iter()
                .find(|a| a.id == "kimi")
                .expect("first-class `kimi` must be present after migration");
            assert_eq!(kimi.api_key.as_deref(), Some("sk-moon-test"));

            let raw = std::fs::read_to_string(tmp.join("preferences.json")).unwrap();
            assert!(
                !raw.contains("kimi-via-claude"),
                "migration must persist — preferences.json still carries the companion id: {raw}",
            );
        });
    }

    #[test]
    fn migrate_legacy_account_endpoint_into_claude_pairing() {
        with_temp_dir(|tmp| {
            // Write raw JSON with legacy account endpoint fields (pre-ADR-0025).
            let raw = r#"{
                "provider_accounts": [{
                    "id": "minimax",
                    "name": "MiniMax",
                    "enabled": true,
                    "billing_mode": "pay_as_you_go",
                    "claude_compatible": true,
                    "api_key": "sk-mm-legacy",
                    "base_url": "https://api.minimax.io/anthropic",
                    "model_tiers": {
                        "default": "MiniMax-M3[1m]",
                        "small_fast": "MiniMax-M2.7"
                    },
                    "models": []
                }],
                "provider_pairings": []
            }"#;
            std::fs::write(tmp.join("preferences.json"), raw).unwrap();
            *lock_cache() = None;

            let prefs = load().unwrap();
            // Legacy fields stripped from account.
            let mm = prefs
                .provider_accounts
                .iter()
                .find(|a| a.id == "minimax")
                .expect("minimax account retained");
            assert_eq!(mm.api_key.as_deref(), Some("sk-mm-legacy"));
            // Pairing materialised.
            let pairing = prefs
                .provider_pairings
                .iter()
                .find(|p| p.provider_id == "minimax" && p.harness_id == "claude")
                .expect("claude pairing materialised from legacy endpoint");
            assert_eq!(pairing.surface, ApiSurface::Anthropic);
            assert_eq!(
                pairing.base_url.as_deref(),
                Some("https://api.minimax.io/anthropic")
            );
            assert_eq!(pairing.model_tiers.default.as_deref(), Some("MiniMax-M3[1m]"));

            // Persisted — re-read from disk without legacy fields.
            let disk = std::fs::read_to_string(tmp.join("preferences.json")).unwrap();
            assert!(!disk.contains("\"base_url\"") || disk.contains("provider_pairings"));
            let disk_val: serde_json::Value = serde_json::from_str(&disk).unwrap();
            let acct = &disk_val["provider_accounts"][0];
            assert!(acct.get("base_url").is_none());
            assert!(acct.get("model_tiers").is_none());
            assert!(acct.get("models").is_none());
        });
    }

    #[test]
    fn migrate_prefs_json_pure_creates_pairing_and_strips_fields() {
        let mut value = serde_json::json!({
            "provider_accounts": [{
                "id": "openrouter",
                "name": "OpenRouter",
                "enabled": true,
                "billing_mode": "pay_as_you_go",
                "claude_compatible": true,
                "api_key": "sk-or",
                "base_url": "https://openrouter.ai/api",
                "model_tiers": {},
                "models": ["legacy-model"]
            }],
            "provider_pairings": []
        });
        assert!(migrate_prefs_json(&mut value));
        let acct = &value["provider_accounts"][0];
        assert!(acct.get("base_url").is_none());
        assert!(acct.get("model_tiers").is_none());
        assert!(acct.get("models").is_none());
        let pairings = value["provider_pairings"].as_array().unwrap();
        assert_eq!(pairings.len(), 1);
        assert_eq!(pairings[0]["provider_id"], "openrouter");
        assert_eq!(pairings[0]["harness_id"], "claude");
        assert_eq!(pairings[0]["base_url"], "https://openrouter.ai/api");
        assert_eq!(value["ad0025_account_pairings_migrated"], true);
        // Second run is a no-op (flag set; no leftover legacy fields).
        assert!(!migrate_prefs_json(&mut value));
    }

    #[test]
    fn migrate_prefs_json_does_not_auto_pair_after_flag_set() {
        // Post-migration: a new keyed account without a pairing must not get
        // one on the next prefs load (attach is explicit).
        let mut value = serde_json::json!({
            "ad0025_account_pairings_migrated": true,
            "provider_accounts": [{
                "id": "minimax",
                "name": "MiniMax",
                "enabled": true,
                "billing_mode": "pay_as_you_go",
                "claude_compatible": true,
                "api_key": "sk-new"
            }],
            "provider_pairings": []
        });
        assert!(!migrate_prefs_json(&mut value));
        assert!(value["provider_pairings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn migrate_prefs_json_skips_keyed_generic_without_endpoint() {
        // A keyed+enabled generic with no legacy URL and no first-class
        // surface must NOT get a null-base_url pairing (the attach command
        // would reject it as a no-op). `glm` is a true Generic (no entry in
        // `BUILTIN_PROVIDER_ACCOUNTS`); `deepseek` was a Generic before
        // issue #1127 promoted it to first-class keyed tracked, so we can't
        // reuse it here without breaking the test's intent.
        let mut value = serde_json::json!({
            "provider_accounts": [{
                "id": "glm",
                "name": "GLM",
                "enabled": true,
                "billing_mode": "pay_as_you_go",
                "claude_compatible": true,
                "api_key": "sk-glm"
            }],
            "provider_pairings": []
        });
        assert!(migrate_prefs_json(&mut value));
        assert!(
            value["provider_pairings"].as_array().unwrap().is_empty(),
            "keyed generic without an endpoint must not synthesise a pairing"
        );
    }

    // -----------------------------------------------------------------------
    // Harness defaults (issue #1150 / #1148)
    // -----------------------------------------------------------------------

    /// Older `preferences.json` files without a `harness_defaults` key load
    /// as an empty map (issue #1148 acceptance criteria 1: "Older preference
    /// files without harness defaults load as an empty defaults map").
    /// The `#[serde(default)]` annotation on the field is the contract; this
    /// test pins the on-disk shape so a future field-rename doesn't silently
    /// break compat with installed users.
    #[test]
    fn old_prefs_file_without_harness_defaults_loads_as_empty_map() {
        with_temp_dir(|dir| {
            // Hand-written legacy payload: every other field present, but no
            // `harness_defaults` key. Mirrors a `preferences.json` written
            // by a pre-#1150 build.
            let raw = serde_json::json!({
                "default_provider": "claude",
                "ad0025_account_pairings_migrated": true,
                "provider_accounts": [],
                "provider_pairings": [],
                "harness_profiles": [],
                "harness_order": [],
                "pairing_verifications": [],
                "proxied_provider_order": []
            });
            std::fs::write(dir.join("preferences.json"), raw.to_string()).unwrap();
            *lock_cache() = None;
            let prefs = load().unwrap();
            assert!(
                prefs.harness_defaults.is_empty(),
                "missing field must deserialise to empty map, got {:?}",
                prefs.harness_defaults
            );
        });
    }

    /// Sparse persistence: only harnesses the user has configured appear in
    /// the stored map. The validator removes entries whose every field
    /// collapsed to absent, so a stored empty value is unreachable
    /// (issue #1148 AC #6: "Blank values are normalized to absent, and an
    /// empty harness configuration removes its sparse entry").
    #[test]
    fn upsert_harness_default_persists_then_round_trips() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences {
                ad0025_account_pairings_migrated: true,
                ..Default::default()
            };
            upsert_harness_default(
                &mut prefs,
                "claude",
                HarnessConfigValue {
                    model: Some("opus-4-1".into()),
                    effort: Some("high".into()),
                },
            )
            .unwrap();
            save(prefs.clone()).unwrap();
            *lock_cache() = None;
            let loaded = load().unwrap();
            assert_eq!(loaded.harness_defaults.len(), 1);
            assert_eq!(
                loaded.harness_defaults.get("claude"),
                Some(&HarnessConfigValue {
                    model: Some("opus-4-1".into()),
                    effort: Some("high".into())
                })
            );
        });
    }

    /// Clearing the last field of a harness's default removes the sparse
    /// map entry entirely (issue #1148 AC #6).
    #[test]
    fn upsert_harness_default_removes_entry_when_all_fields_blank() {
        with_temp_dir(|_| {
            let mut prefs = AppPreferences {
                ad0025_account_pairings_migrated: true,
                ..Default::default()
            };
            // Seed: a stored default for `claude`.
            upsert_harness_default(
                &mut prefs,
                "claude",
                HarnessConfigValue {
                    model: Some("opus".into()),
                    effort: None,
                },
            )
            .unwrap();
            assert_eq!(prefs.harness_defaults.len(), 1);

            // Write an all-blank value: validator must remove the entry.
            upsert_harness_default(
                &mut prefs,
                "claude",
                HarnessConfigValue {
                    model: Some("   ".into()), // whitespace collapses to None
                    effort: None,
                },
            )
            .unwrap();
            assert!(
                prefs.harness_defaults.is_empty(),
                "all-blank value must remove the entry, got {:?}",
                prefs.harness_defaults
            );

            // Whitespace + None on effort: same path.
            upsert_harness_default(
                &mut prefs,
                "codex",
                HarnessConfigValue {
                    model: Some("\t".into()),
                    effort: Some(String::new()),
                },
            )
            .unwrap();
            assert!(prefs.harness_defaults.is_empty());
        });
    }

    /// Validator rejects unknown harness ids at the write boundary so corrupt
    /// configuration can't affect unrelated harnesses (issue #1148 AC #5:
    /// "Unknown harness ids … are rejected at the backend boundary").
    #[test]
    fn upsert_harness_default_rejects_unknown_harness_id() {
        let mut prefs = AppPreferences::default();
        let result = upsert_harness_default(
            &mut prefs,
            "definitely-not-a-harness",
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: None,
            },
        );
        assert!(result.is_err(), "must reject unknown id, got {:?}", result);
        assert!(prefs.harness_defaults.is_empty());
    }

    /// Validator rejects effort on a harness without effort control
    /// (OpenCode, Terminal, Kimi, Mcode) — the harness has no place
    /// for the value, so the write is refused at the boundary rather than
    /// silently storing a field the resolver would never forward (issue
    /// #1148 AC #5: "Accept effort only when the harness declares effort
    /// support").
    ///
    /// Note: Anthropic, Codex, Antigravity (#1286), and Grok (#1280)
    /// accept effort, so they must be excluded from this test set.
    #[test]
    fn upsert_harness_default_rejects_effort_on_harness_without_effort_control() {
        let mut prefs = AppPreferences::default();
        // Kimi supports model override but NOT effort (per capabilities);
        // issue #1286 switched this from "agy" because AGY now advertises
        // `--effort <low|medium|high>` and the validator must NOT reject
        // a valid effort value on it.
        let result = upsert_harness_default(
            &mut prefs,
            "kimi",
            HarnessConfigValue {
                model: Some("some-model".into()),
                effort: Some("high".into()),
            },
        );
        assert!(result.is_err(), "must reject effort on a non-effort harness");
        assert!(
            prefs.harness_defaults.is_empty(),
            "rejected write must not partially mutate the map"
        );
    }

    /// Validator accepts a model-only value for a non-effort harness (the
    /// model path is independent of effort — Kimi accepts `--model` but not
    /// `--effort`). The blank-after-trim effort is rejected here because the
    /// input carries an effort value at all; a pure model-only write
    /// succeeds.
    #[test]
    fn upsert_harness_default_accepts_model_only_on_non_effort_harness() {
        let mut prefs = AppPreferences::default();
        upsert_harness_default(
            &mut prefs,
            "kimi",
            HarnessConfigValue {
                model: Some("some-model".into()),
                effort: None,
            },
        )
        .unwrap();
        let stored = prefs.harness_defaults.get("kimi").unwrap();
        assert_eq!(stored.model.as_deref(), Some("some-model"));
        assert!(stored.effort.is_none());
    }

    /// Validator rejects effort values outside the harness's vocabulary
    /// (issue #1148 AC #5: "Accept only values allowed by that harness's
    /// effort-control kind"). Codex's closed vocabulary is `["none", "low",
    /// "medium", "high", "xhigh"]`; a clearly-bogus value is refused.
    #[test]
    fn upsert_harness_default_rejects_effort_outside_vocabulary() {
        let mut prefs = AppPreferences::default();
        let result = upsert_harness_default(
            &mut prefs,
            "codex",
            HarnessConfigValue {
                model: None,
                effort: Some("ultra-mega-high".into()),
            },
        );
        assert!(result.is_err(), "must reject out-of-vocabulary effort");
        assert!(prefs.harness_defaults.is_empty());
    }

    /// Codex accepts `xhigh` (superset of Claude's closed vocab); the
    /// capability mask must allow it through at write.
    #[test]
    fn upsert_harness_default_accepts_codex_xhigh() {
        let mut prefs = AppPreferences::default();
        upsert_harness_default(
            &mut prefs,
            "codex",
            HarnessConfigValue {
                model: None,
                effort: Some("xhigh".into()),
            },
        )
        .unwrap();
        assert_eq!(
            prefs.harness_defaults.get("codex").and_then(|v| v.effort.as_deref()),
            Some("xhigh")
        );
    }

    /// Validator is case-insensitive on built-in harness ids to mirror
    /// `Provider::from_db_str` (the executor resolver that
    /// `resolve_harness_provider` uses). A capitalised `"Claude"` is
    /// accepted as the Claude profile. The Spawn Menu emits lowercase ids
    /// anyway, so this only matters when a hand-typed or scripted write
    /// bypasses the canonical form.
    #[test]
    fn upsert_harness_default_accepts_canonical_lowercase_via_capitalisation() {
        let mut prefs = AppPreferences::default();
        upsert_harness_default(
            &mut prefs,
            "Claude",
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: Some("high".into()),
            },
        )
        .unwrap();
        // Stored under the canonical lowercase id (the validator doesn't
        // re-key the entry — it accepts the input as written — but the
        // harness profile list lookup canonicalises for built-ins).
        assert!(prefs.harness_defaults.contains_key("Claude"));
    }

    /// `remove_harness_default` is idempotent — clearing an already-cleared
    /// harness is a no-op (so the UI's "Reset" affordance never errors).
    #[test]
    fn remove_harness_default_is_idempotent() {
        let mut prefs = AppPreferences {
            ad0025_account_pairings_migrated: true,
            ..Default::default()
        };
        // Missing key: no-op, no error.
        remove_harness_default(&mut prefs, "claude");
        assert!(prefs.harness_defaults.is_empty());

        // Present key: removed.
        prefs.harness_defaults.insert(
            "claude".into(),
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: None,
            },
        );
        remove_harness_default(&mut prefs, "claude");
        assert!(prefs.harness_defaults.is_empty());

        // Missing again: still no-op.
        remove_harness_default(&mut prefs, "claude");
        assert!(prefs.harness_defaults.is_empty());
    }

    /// `normalize_harness_default` collapses whitespace-only inputs to `None`
    /// (issue #1148 AC #6: "Blank values are normalized to absent"). Trim
    /// the kept value so a user who pastes `" opus "` doesn't store the
    /// padding.
    #[test]
    fn normalize_harness_default_trims_blanks_and_whitespace() {
        let normalized = normalize_harness_default(HarnessConfigValue {
            model: Some("  opus  ".into()),
            effort: Some("   ".into()),
        });
        assert_eq!(normalized.model.as_deref(), Some("opus"));
        assert!(normalized.effort.is_none(), "blank effort must collapse to None");
    }

    /// The reader `harness_default_for` is the spawn-seam's source of truth
    /// for the application slot. A present entry round-trips; a missing key
    /// is `None`.
    #[test]
    fn harness_default_for_reads_back_stored_value() {
        let mut prefs = AppPreferences {
            ad0025_account_pairings_migrated: true,
            ..Default::default()
        };
        assert!(harness_default_for(&prefs, "claude").is_none());
        upsert_harness_default(
            &mut prefs,
            "claude",
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: Some("high".into()),
            },
        )
        .unwrap();
        let got = harness_default_for(&prefs, "claude").unwrap();
        assert_eq!(got.model.as_deref(), Some("opus"));
        assert_eq!(got.effort.as_deref(), Some("high"));
        assert!(harness_default_for(&prefs, "codex").is_none());
    }

    /// A failed save must not publish the candidate to the cache (mirrors
    /// `failed_update_does_not_publish_candidate_to_cache` for harness
    /// defaults). The point is: a write that the backend rejects at the
    /// boundary never poisons the in-process cache, so subsequent spawns
    /// see the previous good state.
    #[test]
    fn failed_upsert_harness_default_leaves_cache_unchanged() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = test_dir();
        std::fs::create_dir_all(&tmp).unwrap();
        init_for_tests(tmp.clone());

        // Seed: a valid default for `claude`.
        save(AppPreferences {
            harness_defaults: HashMap::from([(
                "claude".into(),
                HarnessConfigValue {
                    model: Some("opus".into()),
                    effort: None,
                },
            )]),
            ad0025_account_pairings_migrated: true,
            ..Default::default()
        })
        .unwrap();

        // Attempt an invalid upsert (unknown harness id). The command layer
        // returns Err without touching the cache; load() returns the seed.
        let mut prefs = load().unwrap();
        let err = upsert_harness_default(
            &mut prefs,
            "not-a-harness",
            HarnessConfigValue {
                model: Some("opus".into()),
                effort: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("unknown harness id"));

        // Cached prefs are unchanged (the mutator ran on a local clone that
        // was never written back).
        let reloaded = load().unwrap();
        assert_eq!(
            reloaded.harness_defaults.get("claude").and_then(|v| v.model.as_deref()),
            Some("opus")
        );
        assert!(!reloaded.harness_defaults.contains_key("not-a-harness"));

        reset_for_tests();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `is_known_harness_id` accepts every built-in adapter id (issue
    /// #1148 AC #5). The whitelist is the same one `BUILTIN_HARNESS_IDS`
    /// documents so a future adapter edit can't silently widen or shrink the
    /// acceptance surface.
    #[test]
    fn is_known_harness_id_accepts_every_builtin() {
        for id in crate::agent::provider::BUILTIN_HARNESS_IDS {
            assert!(
                is_known_harness_id(id),
                "built-in harness id '{id}' must be accepted"
            );
        }
    }

    /// `is_known_harness_id` rejects empty strings and truly-bogus ids.
    /// Empty strings would let a frontend typo silently fall through to the
    /// Anthropic executor via `from_db_str`'s fallback — exactly the leak
    /// the validator is meant to close. Built-in case variations
    /// (`"Claude"`, `"OPENCODE"`) are accepted — see
    /// [`is_known_harness_id_accepts_canonical_lowercase_via_capitalisation`]
    /// above.
    #[test]
    fn is_known_harness_id_rejects_unknown() {
        for id in ["", "definitely-not-a-harness", "  ", "claude-mini"] {
            assert!(
                !is_known_harness_id(id),
                "id '{id}' must be rejected"
            );
        }
    }

    /// Issue #1224 — poison-recovery regression for the preferences
    /// `CACHE` mutex. `preferences::load()` used to lock with `.unwrap()`
    /// — a single panic while a caller held the guard would permanently
    /// poison the cache and every subsequent `load`/`save` would
    /// re-panic, freezing the whole preferences surface. The new
    /// `load` body now calls `into_inner()` so reads keep returning
    /// `AppPreferences` after any one-off failure.
    ///
    /// Runs under `TEST_LOCK` (see module-level comment) because
    /// `CACHE` is process-wide global state.
    #[test]
    fn preferences_load_recovers_from_cache_poison() {
        let _guard = TEST_LOCK.lock().unwrap();
        // Reset to a known-empty cache so the test starts clean even if
        // a prior test left state behind.
        *super::lock_cache() = None;

        // Poison the cache mutex. `catch_unwind` keeps the test binary
        // alive; the panic payload is the assertion that the inner
        // path did panic.
        let poison_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = super::CACHE.lock().expect("first lock must succeed (test setup)");
            panic!("intentional preferences CACHE poison for issue #1224 regression test");
        }));
        assert!(
            poison_result.is_err(),
            "test fixture must panic to poison the CACHE mutex"
        );

        // After the panic, the OLD `.lock().unwrap()` form would now
        // return `Err(Poisoned)` for every subsequent caller until
        // process restart. The new `load` body must hand back the
        // current cached value (None here) without panicking. We do
        // not call `load()` directly because it would also hit disk
        // (`read_from_disk`); a cache-only assertion is sufficient to
        // pin the recovery shape, and disk-coupled tests need a
        // writable `APP_DATA_DIR` fixture.
        match super::CACHE.lock() {
            Ok(g) => assert!(g.is_none(), "cache must be None after manual reset"),
            Err(poisoned) => {
                // The poison itself: this is the exact failure the
                // helper in `load()` must turn into a usable guard.
                let recovered = poisoned.into_inner();
                assert!(
                    recovered.is_none(),
                    "recovered cache must observe the value the prior holder left behind"
                );
            }
        }
    }
}
