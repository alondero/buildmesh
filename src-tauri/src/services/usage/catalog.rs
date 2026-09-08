//! Canonical registry and dispatch for first-class Usage Meter integrations.
//!
//! Commands provide one effective account snapshot. This module owns provider
//! classification, native-harness gating, credential lookup, cache routing,
//! and fetch dispatch without reading preferences itself.
//!
//! Dispatch is through the [`crate::services::usage::adapter::UsageAdapter`]
//! seam only (issue #1657): the table holds drop-in adapters, never raw
//! fn pointers into the legacy fetcher module.

use super::adapters::{
    AgyAdapter, AnthropicAdapter, CodexAdapter, CommandcodeAdapter, CursorAdapter, DeepseekAdapter,
    FreebuffAdapter, GrokAdapter, KimiAdapter, MinimaxAdapter, OpencodeAdapter, OpenaiAdapter,
    OpenrouterAdapter,
};
use super::adapter::{api_key_for, UsageAdapter};
use super::types::ProviderUsage;
use crate::preferences::ProviderAccount;
use std::collections::HashSet;

static ANTHROPIC_ADAPTER: AnthropicAdapter = AnthropicAdapter;
static CODEX_ADAPTER: CodexAdapter = CodexAdapter;
static CURSOR_ADAPTER: CursorAdapter = CursorAdapter;
static MINIMAX_ADAPTER: MinimaxAdapter = MinimaxAdapter;
static AGY_ADAPTER: AgyAdapter = AgyAdapter;
static KIMI_ADAPTER: KimiAdapter = KimiAdapter;
static OPENROUTER_ADAPTER: OpenrouterAdapter = OpenrouterAdapter;
static GROK_ADAPTER: GrokAdapter = GrokAdapter;
static OPENCODE_ADAPTER: OpencodeAdapter = OpencodeAdapter;
static COMMANDCODE_ADAPTER: CommandcodeAdapter = CommandcodeAdapter;
static OPENAI_ADAPTER: OpenaiAdapter = OpenaiAdapter;
static DEEPSEEK_ADAPTER: DeepseekAdapter = DeepseekAdapter;
static FREEBUFF_ADAPTER: FreebuffAdapter = FreebuffAdapter;

static USAGE_METERS: &[&'static dyn UsageAdapter] = &[
    &ANTHROPIC_ADAPTER,
    &CODEX_ADAPTER,
    &CURSOR_ADAPTER,
    &MINIMAX_ADAPTER,
    &AGY_ADAPTER,
    // This is the Moonshot Model Provider, not the separately registered Kimi
    // Code Agent Harness. It is keyed and therefore has no detection gate.
    &KIMI_ADAPTER,
    &OPENROUTER_ADAPTER,
    &GROK_ADAPTER,
    &OPENCODE_ADAPTER,
    &COMMANDCODE_ADAPTER,
    // OpenAI's Organization Costs endpoint is admin-scoped; project keys
    // degrade through the fetcher's normal logged-in/detail envelope.
    &OPENAI_ADAPTER,
    // DeepSeek exposes a keyed cash-balance endpoint rather than plan windows.
    &DEEPSEEK_ADAPTER,
    // Freebuff self-authenticates through its CLI-managed credentials file.
    &FREEBUFF_ADAPTER,
];

/// Seam entry point: `catalog.dispatch(id).fetch(accounts)` proves the seam
/// — not the old module — is the dispatch and test surface.
pub(crate) fn dispatch(provider_id: &str) -> Option<&'static dyn UsageAdapter> {
    USAGE_METERS
        .iter()
        .copied()
        .find(|adapter| adapter.id() == provider_id)
}

pub(crate) fn contains(provider_id: &str) -> bool {
    dispatch(provider_id).is_some()
}

pub(crate) fn native_harness(provider_id: &str) -> Option<&'static str> {
    dispatch(provider_id).and_then(|adapter| adapter.native_harness())
}

pub(crate) fn configured_keyed_provider_ids(accounts: &[ProviderAccount]) -> HashSet<String> {
    USAGE_METERS
        .iter()
        .copied()
        .filter(|adapter| adapter.native_harness().is_none())
        .filter(|adapter| api_key_for(accounts, adapter.id()).is_some())
        .map(|adapter| adapter.id().to_string())
        .collect()
}

/// Fetch a provider using the supplied effective account snapshot, serving a
/// fresh cache entry unless `force_refresh` is set.
pub(crate) fn cached_or_fetch(
    provider_id: &str,
    force_refresh: bool,
    accounts: &[ProviderAccount],
) -> Option<ProviderUsage> {
    let adapter = dispatch(provider_id)?;
    if !force_refresh {
        if let Some(cached) = super::get_cached_usage(provider_id) {
            return Some(cached);
        }
    }

    let result = adapter.fetch(accounts);
    super::set_cached_usage(provider_id, result.clone());
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::BillingMode;

    fn account(id: &str, api_key: Option<&str>) -> ProviderAccount {
        ProviderAccount {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: api_key.map(str::to_string),
        }
    }

    /// Test-only adapter proving the seam is the test surface: echoes the
    /// supplied key as the provider name so `fetch` can assert snapshot
    /// threading without touching the network.
    struct EchoKeyAdapter;

    impl UsageAdapter for EchoKeyAdapter {
        fn id(&self) -> &'static str {
            "keyed-test"
        }

        fn fetch(&self, accounts: &[ProviderAccount]) -> ProviderUsage {
            let key = api_key_for(accounts, "keyed-test").unwrap_or("");
            ProviderUsage {
                provider: key.to_string(),
                logged_in: true,
                windows: Vec::new(),
                balance: None,
                detail: None,
                error: None,
            }
        }
    }

    #[test]
    fn catalog_ids_are_non_empty_and_unique() {
        let mut ids = HashSet::new();
        for adapter in USAGE_METERS {
            assert!(!adapter.id().trim().is_empty());
            assert!(ids.insert(adapter.id()), "duplicate id: {}", adapter.id());
        }
    }

    #[test]
    fn unknown_provider_has_no_definition_or_native_harness() {
        assert!(dispatch("not-a-provider").is_none());
        assert!(!contains("not-a-provider"));
        assert_eq!(native_harness("not-a-provider"), None);
        assert!(cached_or_fetch("not-a-provider", true, &[]).is_none());
    }

    #[test]
    fn native_adapters_report_their_harness_and_keyed_report_none() {
        // Native self-auth meters are detection-gated on their harness id.
        assert_eq!(native_harness("anthropic"), Some("anthropic"));
        assert_eq!(native_harness("codex"), Some("codex"));
        assert_eq!(native_harness("freebuff"), Some("freebuff"));
        assert_eq!(native_harness("opencode"), Some("opencode"));
        // Keyed meters have no harness gate — card always visible.
        assert_eq!(native_harness("minimax"), None);
        assert_eq!(native_harness("kimi"), None);
        assert_eq!(native_harness("deepseek"), None);
    }

    #[test]
    fn keyed_credentials_reject_missing_and_empty_keys() {
        assert_eq!(api_key_for(&[], "keyed-test"), None);
        assert_eq!(api_key_for(&[account("keyed-test", None)], "keyed-test"), None);
        assert_eq!(
            api_key_for(&[account("keyed-test", Some(""))], "keyed-test"),
            None
        );
    }

    #[test]
    fn keyed_fetch_uses_the_supplied_account_snapshot() {
        let adapter = EchoKeyAdapter;
        let accounts = [account("keyed-test", Some("snapshot-key"))];

        assert_eq!(adapter.fetch(&accounts).provider, "snapshot-key");
    }

    #[test]
    fn dispatch_exposes_every_registered_adapter_through_the_seam() {
        // Contract: `dispatch(id).fetch` is the test surface, not the old
        // module. Pin the id/harness contract per adapter so a future
        // two-place edit (fetcher + catalog) fails here.
        let cases: &[(&str, Option<&str>)] = &[
            ("anthropic", Some("anthropic")),
            ("codex", Some("codex")),
            ("cursor", Some("cursor")),
            ("minimax", None),
            ("agy", Some("agy")),
            ("kimi", None),
            ("openrouter", None),
            ("grok", Some("grok")),
            ("opencode", Some("opencode")),
            ("commandcode", Some("commandcode")),
            ("openai", None),
            ("deepseek", None),
            ("freebuff", Some("freebuff")),
        ];
        for (id, harness) in cases {
            let adapter = dispatch(id).unwrap_or_else(|| panic!("missing adapter: {id}"));
            assert_eq!(adapter.id(), *id);
            assert_eq!(adapter.native_harness(), *harness);
            assert!(contains(id));
        }
    }

    #[test]
    fn configured_keyed_ids_come_from_the_account_snapshot() {
        let accounts = [
            account("minimax", Some("k")),
            account("kimi", None),
            account("openrouter", Some("")),
        ];
        let ids = configured_keyed_provider_ids(&accounts);
        assert!(ids.contains("minimax"));
        assert!(!ids.contains("kimi"));
        assert!(!ids.contains("openrouter"));
        // Native self-auth providers never appear in the keyed set.
        assert!(!ids.contains("anthropic"));
    }
}
