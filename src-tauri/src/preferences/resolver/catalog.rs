//! Provider catalog — built-in classification, default tiers, surface mapping.

use super::super::model::{ApiSurface, BillingMode, ModelTiers, ProviderAccount, SurfaceEndpoint};
use crate::models::Provider;

use super::harness::{harness_profiles, resolve_harness_provider};

/// A row in the code-defined built-in Model Provider account list (issue
/// #571). The **single source of truth** for which built-ins exist and which
/// are self-authenticating — [`default_provider_accounts`] materialises the
/// full [`ProviderAccount`] from it, and [`is_claude_compatible_id`]
/// classifies an id by looking it up here. Adding a new built-in means adding
/// one row; the classification then follows automatically.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BuiltInProviderAccount {
    pub id: &'static str,
    pub name: &'static str,
    /// True for harnesses that authenticate via their own CLI (`~/.claude`,
    /// `~/.codex`, …) and therefore hold no credentials in Buildmesh. False
    /// for Claude-compatible keyed providers (MiniMax, custom) that ship a
    /// base URL + per-tier model map (issue #568). `kimi` is self_auth post-
    /// #918 (Kimi Code CLI handles auth via `~/.kimi/config.toml`).
    pub self_auth: bool,
}

pub(crate) const BUILTIN_PROVIDER_ACCOUNTS: &[BuiltInProviderAccount] = &[
    BuiltInProviderAccount {
        id: "anthropic",
        name: "Anthropic / Claude",
        self_auth: true,
    },
    BuiltInProviderAccount {
        id: "codex",
        name: "OpenAI / Codex",
        self_auth: true,
    },
    BuiltInProviderAccount {
        id: "agy",
        name: "Google / Antigravity",
        self_auth: true,
    },
    BuiltInProviderAccount {
        id: "grok",
        name: "xAI / Grok",
        self_auth: true,
    },
    // Kimi (Moonshot) — keyed via the user's Moonshot API key. The string
    // `kimi` also names the Kimi Code CLI Agent Harness in a different
    // namespace (`HarnessProfile.harness`); see CONTEXT.md "First-class
    // Model Provider" + "Usage follows the credential, not the pairing".
    BuiltInProviderAccount {
        id: "kimi",
        name: "Moonshot / Kimi",
        self_auth: false,
    },
    BuiltInProviderAccount {
        id: "opencode",
        name: "OpenCode",
        self_auth: true,
    },
    // Command Code owns its `~/.commandcode/auth.json` credential. The same
    // id names its native Agent Harness in the separate harness namespace.
    BuiltInProviderAccount {
        id: "commandcode",
        name: "Command Code",
        self_auth: true,
    },
    // Freebuff is an AI-coding CLI built on Codebuff (issue #1437). Its
    // CLI-managed credential lives at `~/.config/manicode/credentials.json`
    // (XDG-style, used on every platform), so the provider self-authenticates
    // the same way Command Code / Cursor / Kimi Code do. The same `freebuff`
    // id names its native Agent Harness in the separate harness namespace;
    // the `Usage follows the credential, not the pairing` invariant from
    // CONTEXT.md keeps the two registrations aligned. See issue #1438 for
    // the credential-parser + quota-fetcher wiring.
    BuiltInProviderAccount {
        id: "freebuff",
        name: "Freebuff",
        self_auth: true,
    },
    BuiltInProviderAccount {
        id: "minimax",
        name: "MiniMax",
        self_auth: false,
    },
    BuiltInProviderAccount {
        id: "openrouter",
        name: "OpenRouter",
        self_auth: false,
    },
    BuiltInProviderAccount {
        id: "cursor",
        name: "Cursor",
        self_auth: true,
    },
    // DeepSeek Platform API — keyed by the user's DeepSeek API key.
    // Publishes both Anthropic- and OpenAI-compatible surfaces (DeepSeek
    // runs an OpenAI-compatible endpoint at https://api.deepseek.com/v1
    // and an Anthropic-compatible Claude Code surface). Models: `deepseek-chat`
    // (V3.x — fast chat / mid-tier code) and `deepseek-reasoner` (R1 — the
    // strongest reasoning tier). See issue #1127.
    BuiltInProviderAccount {
        id: "deepseek",
        name: "DeepSeek",
        self_auth: false,
    },
    // OpenAI Platform API — keyed by an `sk-admin-…` (org spend) or
    // `sk-proj-…` (graceful degradation: org costs 401, project keys still
    // work for inference). See ADR-0026 / issue #1109. No first-class
    // inference surface here — the row exists for the Usage Meter only;
    // OpenAI inference goes through the Codex harness.
    BuiltInProviderAccount {
        id: "openai",
        name: "OpenAI Platform",
        self_auth: false,
    },
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
/// surface+URL at creation, stored directly on its [`super::super::model::ProviderPairing`].
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
        "kimi" => vec![SurfaceEndpoint {
            surface: ApiSurface::Anthropic,
            base_url: "https://api.moonshot.ai/anthropic".to_string(),
            model_tiers: kimi_default_tiers(),
        }],
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
/// [`super::harness::resolve_harness_provider`]; the pure core is
/// [`surface_for_executor`].
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
pub(crate) fn keyed_first_class_template(id: &str) -> Option<ProviderAccount> {
    keyed_first_class_catalog().into_iter().find(|a| a.id == id)
}

/// MiniMax's Claude-Code-parity default tier map. **Single source** consumed by
/// (issue #571 / ADR-0025):
/// - [`first_class_surfaces`] for `"minimax"` — the Anthropic surface tier
/// - [`crate::agent::provider::provider_conf::minimax_backend_env`] — the
///   session-naming side-channel
/// - attach-time pairing defaults via [`super::pairings::resolve_pairing`]
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

/// Resolve the Claude harness id from a list of harness profiles (typed form
/// of the JSON `claude_harness_id_from_json` used by the migration) — first
/// profile with `harness == "anthropic"`, else `"claude"`.
pub(crate) fn claude_harness_id_from(profiles: &[super::super::model::HarnessProfile]) -> String {
    profiles
        .iter()
        .find(|p| p.harness == "anthropic")
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "claude".to_string())
}

/// Disk-reading wrapper over [`claude_harness_id_from`].
pub(crate) fn claude_harness_id() -> String {
    claude_harness_id_from(&harness_profiles())
}
