//! Compatibility layer — spawn-env translation and harness-default validation.
//!
//! This module owns two flavours of compatibility:
//! * **Env translation** ([`resolve_provider_env`], [`surface_env`],
//!   [`anthropic_surface_env`], [`openai_surface_env`], [`preflight_resolve_provider_env`]):
//!   turns a `ProviderPairing` into the exact `ANTHROPIC_*` / `OPENAI_*` env vars
//!   a binary expects, and refuses to spawn against a custom endpoint whose
//!   primary model is unset.
//! * **Harness-default validation** ([`normalize_harness_default`],
//!   [`validate_harness_default`], [`upsert_harness_default`],
//!   [`remove_harness_default`], [`harness_default_for`]): gates user
//!   application-level harness overrides against each harness's capability
//!   contract (issue #1148).
//!
//! See the [module-level docs](super) for what concerns each submodule owns.

use super::model::{
    ApiSurface, AppPreferences, HarnessConfigValue, ModelTiers, ProviderAccount, ProviderPairing,
};
use super::resolver::{
    claude_harness_id, harness_capabilities_for, provider_accounts, provider_pairings,
};
use crate::agent::capabilities::EffortControlKind;

// ----- Harness default validation ----------------------------------------

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
    s.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
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
/// Pure mutator — the caller wraps it in [`super::storage::update`] (or
/// loads + saves) so the in-process cache refreshes on a successful persist.
pub fn upsert_harness_default(
    prefs: &mut AppPreferences,
    profile_id: &str,
    raw: HarnessConfigValue,
) -> Result<(), String> {
    let validated = validate_harness_default(profile_id, raw)?;
    if validated.is_empty() {
        prefs.harness_defaults.remove(profile_id);
    } else {
        prefs
            .harness_defaults
            .insert(profile_id.to_string(), validated);
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

// ----- Spawn env translation --------------------------------------------

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
    let (harness_id, provider_id) = crate::agent::provider::parse_spawn_option_id(spawn_option_id);
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
    let (harness_id, provider_id) = crate::agent::provider::parse_spawn_option_id(spawn_option_id);
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
            let pairing =
                stored_claude_anthropic_pairing(account, &pairings, &claude_harness_id()).cloned();
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
    if pairing
        .model_tiers
        .default
        .as_deref()
        .is_none_or(|s| s.is_empty())
    {
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
                (
                    "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    model(&tiers.sonnet).unwrap_or_else(|| primary.clone()),
                ),
                ("ANTHROPIC_DEFAULT_OPUS_MODEL", opus.clone()),
                (
                    "ANTHROPIC_DEFAULT_FABLE_MODEL",
                    model(&tiers.fable).unwrap_or(opus),
                ),
                (
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                    model(&tiers.haiku).unwrap_or(fast),
                ),
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
