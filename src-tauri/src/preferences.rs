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
/// (issue #567 — restores the cwrap capability).
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
pub fn resolve_harness_provider(profile_id: &str) -> Provider {
    match harness_profiles().into_iter().find(|p| p.id == profile_id) {
        Some(profile) => Provider::from_db_str(&profile.harness),
        None => Provider::from_db_str(profile_id),
    }
}

/// Built-in accounts that authenticate via their own CLI (`~/.claude`,
/// `~/.codex`, …) and therefore hold no credentials in Buildmesh. Everything
/// else — MiniMax, Kimi, and any custom account — is a Claude-compatible keyed
/// provider (PRD #534 limits custom endpoints to Claude-compatible in V1).
const SELF_AUTH_BUILTIN_IDS: [&str; 3] = ["anthropic", "codex", "agy"];

/// Whether `id` names a Claude-compatible keyed provider — one that carries an
/// API key/endpoint, shows credential + model-tier fields in the UI, and can
/// appear in the spawn menu once configured. Self-authenticating built-ins are
/// the only exceptions (issue #568).
pub fn is_claude_compatible_id(id: &str) -> bool {
    !SELF_AUTH_BUILTIN_IDS.contains(&id)
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
pub fn default_provider_accounts() -> Vec<ProviderAccount> {
    let self_auth = |id: &str, name: &str| ProviderAccount {
        id: id.to_string(),
        name: name.to_string(),
        enabled: true,
        billing_mode: BillingMode::Plan,
        claude_compatible: false,
        api_key: None,
        base_url: None,
        model_tiers: ModelTiers::default(),
        models: Vec::new(),
    };
    let tiers = |default: &str, fast: &str| ModelTiers {
        default: Some(default.to_string()),
        small_fast: Some(fast.to_string()),
        sonnet: Some(default.to_string()),
        opus: Some(default.to_string()),
        haiku: Some(fast.to_string()),
    };
    vec![
        self_auth("anthropic", "Anthropic / Claude"),
        self_auth("codex", "OpenAI / Codex"),
        self_auth("agy", "Google / Antigravity"),
        ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: None,
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            // cwrap parity: M3 primary on Sonnet/Opus, M2.7 on small-fast/Haiku.
            model_tiers: tiers("MiniMax-M3[1m]", "MiniMax-M2.7"),
            models: Vec::new(),
        },
        ProviderAccount {
            id: "kimi".to_string(),
            name: "Kimi".to_string(),
            enabled: false,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: None,
            base_url: Some("https://api.moonshot.ai/anthropic".to_string()),
            // cwrap parity: k2.6 primary on Opus, k2.5 on small-fast/Sonnet/Haiku.
            model_tiers: ModelTiers {
                default: Some("kimi-k2.6".to_string()),
                small_fast: Some("kimi-k2.5".to_string()),
                sonnet: Some("kimi-k2.5".to_string()),
                opus: Some("kimi-k2.6".to_string()),
                haiku: Some("kimi-k2.5".to_string()),
            },
            models: Vec::new(),
        },
    ]
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

/// Build the backend-selecting `ANTHROPIC_*` environment for a claude-backed
/// harness profile from its paired model-provider account (issue #538).
///
/// A custom provider account pairs a [`HarnessProfile`] by **shared `id`** (see
/// [`upsert_provider_account`]), so the node's stored profile id is also the
/// account id. We look the account up and translate its `base_url` / `api_key` /
/// first `models` entry into the `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` /
/// `ANTHROPIC_MODEL` vars the `claude` binary reads to target a Claude-compatible
/// endpoint (MiniMax, Kimi, DeepSeek, …). This is the dynamic replacement for the
/// deleted `minimax.rs` / `kimi.rs` adapters' hardcoded `provider_env()`.
///
/// Returns empty when no account matches or it carries no custom endpoint — the
/// built-in Anthropic subscription needs no overrides, so it spawns vanilla
/// `claude`. The spawn path still resets the inherited backend env first (see
/// [`crate::agent::provider::AgentProvider::resets_backend_env`]), so an empty
/// result means a clean slate, not a leaked override.
pub fn resolve_provider_env(profile_id: &str) -> Vec<(String, String)> {
    provider_account_env(provider_accounts().iter().find(|a| a.id == profile_id))
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
    let base_url = account.base_url.as_deref().filter(|s| !s.is_empty());
    let mut env = Vec::new();
    if let Some(base) = base_url {
        env.push(("ANTHROPIC_BASE_URL".to_string(), base.to_string()));
    }
    if let Some(key) = account.api_key.as_deref().filter(|s| !s.is_empty()) {
        env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), key.to_string()));
    }
    let tiers = effective_tiers(account);
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
            "provider account '{}' sets a custom base_url but no models — claude will send its default model id to the custom endpoint and likely be rejected",
            account.id
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

/// Pure precedence resolver — kept separate from `load()` so it can be
/// unit-tested without touching disk. The order is:
///   1. `explicit` (e.g. caller-passed argument)
///   2. `per_mesh` (DB column on `meshes.default_provider`)
///   3. `app_wide` (buildmesh-wide preference)
///   4. `"anthropic"` hardcoded fallback
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
        .unwrap_or_else(|| "anthropic".to_string())
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
    fn resolve_precedence_falls_through_to_anthropic() {
        let got = resolve_default_provider(None, None, None);
        assert_eq!(got, "anthropic");
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

        // All-empty everywhere collapses to the anthropic fallback.
        let got = resolve_default_provider(
            Some(String::new()),
            Some(String::new()),
            Some(String::new()),
        );
        assert_eq!(got, "anthropic");
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
        assert_eq!(ids, vec!["anthropic", "codex", "agy", "minimax", "kimi"]);
        // MiniMax + Kimi are the pay-as-you-go, Claude-compatible exemplars; the
        // self-auth built-ins are plans and not Claude-compatible.
        let by_id = |id: &str| default_provider_accounts().into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(by_id("minimax").billing_mode, BillingMode::PayAsYouGo);
        assert!(by_id("minimax").claude_compatible);
        assert!(by_id("kimi").claude_compatible);
        assert!(!by_id("anthropic").claude_compatible);
        assert!(!by_id("codex").claude_compatible);
        // MiniMax + Kimi ship the cwrap base URL + per-tier map so a key is all
        // the user needs to add.
        assert_eq!(by_id("minimax").base_url.as_deref(), Some("https://api.minimax.io/anthropic"));
        assert_eq!(by_id("kimi").base_url.as_deref(), Some("https://api.moonshot.ai/anthropic"));
        assert_eq!(by_id("minimax").model_tiers.default.as_deref(), Some("MiniMax-M3[1m]"));
        // Kimi has no usage fetcher yet, so it's the one built-in disabled by default.
        assert!(!by_id("kimi").enabled);
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
        // Five built-ins, no duplicate minimax, plus the custom one.
        assert_eq!(merged.iter().filter(|a| a.id == "minimax").count(), 1);
        assert!(!merged.iter().find(|a| a.id == "minimax").unwrap().enabled);
        assert!(merged.iter().any(|a| a.id == "deepseek"));
        assert_eq!(merged.len(), 6);
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
    fn builtin_minimax_account_reproduces_cwrap_model_routing() {
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
    fn builtin_kimi_account_reproduces_cwrap_model_routing() {
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
    fn resolve_provider_env_for_keyed_builtin_minimax_injects_cwrap_env() {
        // The real spawn path: a node stored with provider="minimax" resolves the
        // built-in MiniMax account (merged with the user's stored key) and gets the
        // full cwrap model routing — the end-to-end fix for "can't spawn MiniMax".
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
}
