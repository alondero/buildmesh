//! Wire types — preferences data model and ts-rs definitions.
//!
//! This module is the single dependency-free source for the
//! `src/types/generated/*.ts` files. Keeping it leaf-only (no imports from
//! the other `preferences::*` submodules) means ts-rs can scan it in
//! isolation without dragging the provider catalog, the resolver, or the
//! env builder along for the ride.
//!
//! See the [module-level docs](super) for what concerns each submodule owns.

use crate::agent::provider::compatibility::{CompatibilityDecision, ProviderAuthMode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ts_rs::TS;

/// A user-selectable **Agent Harness** profile (ADR-0014 / PRD #534).
///
/// The harness (the executor binary recipe) is being split out from the
/// **Model Provider** (credentials/endpoint). This struct is the first
/// concrete shape of that split: `id` is the value stored in the DB
/// `provider` column and on the wire, `name` is the menu label, and
/// `harness` names the backing executor — for now a legacy [`crate::models::Provider`]
/// id, resolved by [`super::resolve_harness_provider`]. Later slices will give
/// `harness` richer meaning (its own binary recipe) and retire the
/// duplicated legacy [`crate::models::Provider`] enum.
///
/// Generated to src/types/generated/HarnessProfile.ts (issue #535).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "HarnessProfile.ts")]
pub struct HarnessProfile {
    /// Stable id — stored in `agent_nodes.provider` and sent over the wire.
    pub id: String,
    /// Menu label shown in the launch dropdown.
    pub name: String,
    /// Backing executor; for this slice a legacy [`crate::models::Provider`] id.
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
/// `claude` binary reads (see [`super::compatibility::resolve_provider_env`]):
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
/// [`super::compatibility::resolve_provider_env`] emits at spawn (`ANTHROPIC_*` vs `OPENAI_*`).
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
/// in [`AppPreferences::provider_pairings`]; [`super::effective_pairings`] returns
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
    /// published (see [`super::first_class_surfaces`]); for a Generic provider it's
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
/// by [`super::first_class_surfaces`].
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
/// [`super::default_provider_accounts`]. Keyed first-class providers (minimax, kimi,
/// openrouter) live in [`super::BUILTIN_PROVIDER_ACCOUNTS`] but are only materialised
/// when the user adds them from [`super::keyed_first_class_catalog`]. Users may also
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
    /// **Derived from `id` on read** (`merge_provider_accounts` normalizes it) —
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
/// Whitespace-only inputs are collapsed to `None` by [`super::normalize_harness_default`]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "AppPreferences.ts")]
pub struct AppPreferences {
    /// Buildmesh-wide default provider id (e.g. "anthropic", "minimax").
    /// `None` means "no app-wide override — use the hardcoded fallback".
    #[serde(default)]
    pub default_provider: Option<String>,
    /// MiniMax API key for usage fetching. **Deprecated** by `provider_accounts`
    /// (#537) — kept so existing preferences.json files still load and the stored
    /// key survives via [`super::minimax_api_key_resolved`]'s read-through fallback.
    #[serde(default)]
    pub minimax_api_key: Option<String>,
    /// Google Cloud project for Antigravity/Gemini quota API. Defaults to "cloudshell-gca".
    #[serde(default)]
    pub google_cloud_project: Option<String>,
    /// User customizations to the code-defined default harness profiles.
    /// Merged over [`super::default_harness_profiles`] by `id` (user wins) in
    /// [`super::harness_profiles`]; the defaults are always present even when this
    /// is empty, so a built-in like Terminal can never go missing.
    #[serde(default)]
    pub harness_profiles: Vec<HarnessProfile>,
    /// User customizations to the code-defined default model-provider accounts.
    /// Merged over [`super::default_provider_accounts`] by `id` (user wins) in
    /// [`super::provider_accounts`]; the built-ins are always present even when this is
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
    /// into Claude Anthropic pairings on read — see [`super::migrate_prefs_json`]).
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
    /// (blank after trimming) is removed entirely by [`super::upsert_harness_default`]
    /// / [`super::remove_harness_default`], so a stored empty `{}` is unreachable.
    /// Additive on disk — an older `preferences.json` without this field
    /// loads as an empty `HashMap` (issue #1148 acceptance criteria 1) via
    /// `#[serde(default)]`.
    #[serde(default)]
    pub harness_defaults: HashMap<String, HarnessConfigValue>,
    /// Buildmesh-wide default Worktree Node directory (issue #1519).
    /// Optional raw user input — relative values resolve from the Mesh root,
    /// absolute values must be in the same host environment (native/Windows
    /// versus WSL) as the Mesh. Trimmed; blank collapses to `None`
    /// (inherit/default). Shell variables and `~` are NOT expanded.
    /// `None` means "no app-wide override — use `.claude/worktrees`
    /// under the Mesh root". A per-Mesh `worktree_directory` overrides this.
    /// Additive on disk — older `preferences.json` without it loads as `None`.
    #[serde(default)]
    pub worktree_directory: Option<String>,
    /// Confirm before quitting when agent sessions are active (issue #1501).
    /// When `true` (the default), a window close request with active agent
    /// nodes (`running`, `awaiting_input`, `spawning`, `ready`) surfaces an
    /// exit-confirmation modal instead of terminating immediately. When
    /// `false`, close requests proceed without friction.
    /// Additive on disk — older `preferences.json` without it loads as
    /// `true` via `#[serde(default = ...)]`.
    #[serde(default = "default_confirm_before_quit")]
    pub confirm_before_quit: bool,
}

/// Default for [`AppPreferences::confirm_before_quit`] (issue #1501).
/// `true` — a fresh install (or an older `preferences.json` without the
/// field) confirms before quitting with active sessions.
pub fn default_confirm_before_quit() -> bool {
    true
}

impl Default for AppPreferences {
    fn default() -> Self {
        // Derive defaults from the serde attributes (the single source of
        // truth) instead of a hand-written struct literal: every field
        // carries `#[serde(default)]` or `#[serde(default = "...")]`, so an
        // empty object deserializes to exactly the default value — including
        // `confirm_before_quit = true` (issue #1501), which a
        // `#[derive(Default)]` would wrongly give as `false`. A future field
        // without a serde default fails loudly here (and in the
        // `malformed_json_falls_back_to_default` test) instead of silently
        // compiling with a divergent default.
        serde_json::from_value(serde_json::json!({}))
            .expect("AppPreferences must deserialize from {}: every field needs a serde default")
    }
}
