//! Canonical registry and dispatch for first-class Usage Meter integrations.
//!
//! Commands provide one effective account snapshot. This module owns provider
//! classification, native-harness gating, credential lookup, cache routing,
//! and fetch dispatch without reading preferences itself.

use super::ProviderUsage;
use crate::preferences::ProviderAccount;
use std::collections::HashSet;

type FetchNativeUsage = fn() -> ProviderUsage;
type FetchKeyedUsage = fn(&str) -> ProviderUsage;

#[derive(Clone, Copy)]
enum MeterKind {
    Native {
        harness: &'static str,
        fetch: FetchNativeUsage,
    },
    ApiKey {
        fetch: FetchKeyedUsage,
    },
}

/// Everything the service layer needs to route one first-class Usage Meter.
struct UsageMeterDefinition {
    id: &'static str,
    kind: MeterKind,
}

impl UsageMeterDefinition {
    /// Native Usage Meters currently have a one-to-one provider/harness id.
    const fn native(id: &'static str, fetch: FetchNativeUsage) -> Self {
        Self {
            id,
            kind: MeterKind::Native { harness: id, fetch },
        }
    }

    const fn keyed(id: &'static str, fetch: FetchKeyedUsage) -> Self {
        Self {
            id,
            kind: MeterKind::ApiKey { fetch },
        }
    }

    fn native_harness(&self) -> Option<&'static str> {
        match self.kind {
            MeterKind::Native { harness, .. } => Some(harness),
            MeterKind::ApiKey { .. } => None,
        }
    }

    fn api_key<'a>(&self, accounts: &'a [ProviderAccount]) -> Option<&'a str> {
        match self.kind {
            MeterKind::Native { .. } => None,
            MeterKind::ApiKey { .. } => accounts
                .iter()
                .find(|account| account.id == self.id)
                .and_then(|account| account.api_key.as_deref())
                .filter(|key| !key.is_empty()),
        }
    }

    fn fetch(&self, accounts: &[ProviderAccount]) -> ProviderUsage {
        match self.kind {
            MeterKind::Native { fetch, .. } => fetch(),
            MeterKind::ApiKey { fetch } => fetch(self.api_key(accounts).unwrap_or("")),
        }
    }
}

static USAGE_METERS: &[UsageMeterDefinition] = &[
    UsageMeterDefinition::native("anthropic", super::anthropic_usage),
    UsageMeterDefinition::native("codex", super::codex_usage),
    UsageMeterDefinition::native("cursor", super::cursor_usage),
    UsageMeterDefinition::keyed("minimax", super::minimax_usage),
    UsageMeterDefinition::native("agy", super::agy_usage),
    // This is the Moonshot Model Provider, not the separately registered Kimi
    // Code Agent Harness. It is keyed and therefore has no detection gate.
    UsageMeterDefinition::keyed("kimi", super::kimi_usage),
    UsageMeterDefinition::keyed("openrouter", super::openrouter_usage),
    UsageMeterDefinition::native("grok", super::grok_usage),
    UsageMeterDefinition::native("opencode", super::opencode_usage),
    UsageMeterDefinition::native("commandcode", super::commandcode_usage),
    // OpenAI's Organization Costs endpoint is admin-scoped; project keys
    // degrade through the fetcher's normal logged-in/detail envelope.
    UsageMeterDefinition::keyed("openai", super::openai_usage),
    // DeepSeek exposes a keyed cash-balance endpoint rather than plan windows.
    UsageMeterDefinition::keyed("deepseek", super::deepseek_usage),
    // Freebuff self-authenticates through its CLI-managed credentials file.
    UsageMeterDefinition::native("freebuff", super::freebuff_usage),
];

fn find(provider_id: &str) -> Option<&'static UsageMeterDefinition> {
    USAGE_METERS
        .iter()
        .find(|definition| definition.id == provider_id)
}

pub(crate) fn contains(provider_id: &str) -> bool {
    find(provider_id).is_some()
}

pub(crate) fn native_harness(provider_id: &str) -> Option<&'static str> {
    find(provider_id).and_then(UsageMeterDefinition::native_harness)
}

pub(crate) fn configured_keyed_provider_ids(accounts: &[ProviderAccount]) -> HashSet<String> {
    USAGE_METERS
        .iter()
        .filter(|definition| definition.api_key(accounts).is_some())
        .map(|definition| definition.id.to_string())
        .collect()
}

/// Fetch a provider using the supplied effective account snapshot, serving a
/// fresh cache entry unless `force_refresh` is set.
pub(crate) fn cached_or_fetch(
    provider_id: &str,
    force_refresh: bool,
    accounts: &[ProviderAccount],
) -> Option<ProviderUsage> {
    let definition = find(provider_id)?;
    if !force_refresh {
        if let Some(cached) = super::get_cached_usage(provider_id) {
            return Some(cached);
        }
    }

    let result = definition.fetch(accounts);
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

    fn usage_echoing_key(key: &str) -> ProviderUsage {
        ProviderUsage {
            provider: key.to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: None,
            detail: None,
            error: None,
        }
    }

    #[test]
    fn catalog_ids_are_non_empty_and_unique() {
        let mut ids = HashSet::new();
        for definition in USAGE_METERS {
            assert!(!definition.id.trim().is_empty());
            assert!(ids.insert(definition.id), "duplicate id: {}", definition.id);
        }
    }

    #[test]
    fn unknown_provider_has_no_definition_or_native_harness() {
        assert!(find("not-a-provider").is_none());
        assert!(!contains("not-a-provider"));
        assert_eq!(native_harness("not-a-provider"), None);
        assert!(cached_or_fetch("not-a-provider", true, &[]).is_none());
    }

    #[test]
    fn native_constructor_uses_provider_id_as_harness_id() {
        let definition = UsageMeterDefinition::native("native-test", super::super::anthropic_usage);
        assert_eq!(definition.native_harness(), Some("native-test"));
    }

    #[test]
    fn keyed_credentials_reject_missing_and_empty_keys() {
        let definition = UsageMeterDefinition::keyed("keyed-test", usage_echoing_key);

        assert_eq!(definition.api_key(&[]), None);
        assert_eq!(definition.api_key(&[account("keyed-test", None)]), None);
        assert_eq!(definition.api_key(&[account("keyed-test", Some(""))]), None);
    }

    #[test]
    fn keyed_fetch_uses_the_supplied_account_snapshot() {
        let definition = UsageMeterDefinition::keyed("keyed-test", usage_echoing_key);
        let accounts = [account("keyed-test", Some("snapshot-key"))];

        assert_eq!(definition.fetch(&accounts).provider, "snapshot-key");
    }
}
