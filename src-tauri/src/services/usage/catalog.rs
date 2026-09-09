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

static USAGE_METERS: [&'static dyn UsageAdapter; 13] = [
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

    let result = crate::services::usage::adapter::dispatch_fetch(adapter, accounts);
    super::set_cached_usage(provider_id, result.clone());
    Some(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::BillingMode;
    use crate::services::usage::UsageWindow;

    /// Serialises the dispatch.fetch contract tests so they cannot race
    /// each other through `FETCH_OVERRIDE` or the global `USAGE_CACHE`.
    /// `cargo test` runs tests in parallel by default; without this lock,
    /// `cached_or_fetch("anthropic", force_refresh=true)` could observe
    /// an empty override (another test restored `previous`) and fall
    /// through to the production `AnthropicAdapter::fetch`, which issues
    /// a real HTTP request and returns a 0-window envelope on success —
    /// false-failing the assertion. The lock is test-only and scoped to
    /// this module.
    static OVERRIDE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    #[test]
    fn dispatched_keyed_adapters_report_no_credential_without_network() {
        // Production-boundary contract through the seam: real keyed adapters
        // with an empty account snapshot must report the no-credential
        // envelope without touching the network (empty-key early return).
        // A miswired adapter (wrong fn, wrong provider id) fails here.
        for id in ["minimax", "kimi", "openrouter", "openai", "deepseek"] {
            let adapter = dispatch(id).unwrap_or_else(|| panic!("missing adapter: {id}"));
            let usage = adapter.fetch(&[]);
            assert_eq!(usage.provider, id, "adapter {id} must mint its own envelope");
            assert!(!usage.logged_in, "adapter {id} with no key must be logged out");
            let error = usage.error.as_deref().unwrap_or_default();
            assert!(
                error.contains("No API key"),
                "adapter {id} must report no-credential, got: {error:?}"
            );
        }
    }

    /// Drop the production `USAGE_CACHE` entry for `id` so `cached_or_fetch`
    /// in the contract tests does not short-circuit on a cached envelope
    /// from a prior test run. The tests run in a single process and the
    /// cache is process-wide.
    fn uncache(id: &str) {
        crate::services::usage::invalidate_provider_cache(id);
    }

    #[test]
    fn dispatch_fetch_returns_representative_success_envelope() {
        let _guard = OVERRIDE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Production-boundary contract through `dispatch(id)` →
        // `cached_or_fetch(id, force_refresh=true)` → `dispatch_fetch` →
        // override. The contract is the catalog's dispatch path; a future
        // change to caching, dispatch, or the override hook fails here.
        uncache("anthropic");
        let success = ProviderUsage {
            provider: "anthropic".to_string(),
            logged_in: true,
            windows: vec![UsageWindow {
                label: "5-hour".to_string(),
                used_percent: Some(41.0),
                resets_at: None,
            }],
            balance: None,
            detail: None,
            error: None,
        };
        let observed = crate::services::usage::adapter::with_fetch_override(
            "anthropic",
            success.clone(),
            || cached_or_fetch("anthropic", true, &[]).expect("dispatch anthropic"),
        );
        assert_eq!(observed.provider, "anthropic");
        assert!(observed.logged_in);
        assert_eq!(observed.windows.len(), 1);
        assert_eq!(observed.windows[0].used_percent, Some(41.0));
    }

    #[test]
    fn dispatch_fetch_returns_malformed_response_unavailable_envelope() {
        let _guard = OVERRIDE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Adapter envelopes a parse failure as `unavailable` (logged_in=true,
        // error carries the failure). The seam must propagate it untouched
        // so the UI's "Invalid response" branch fires.
        uncache("minimax");
        let malformed = ProviderUsage {
            provider: "minimax".to_string(),
            logged_in: true,
            windows: vec![],
            balance: None,
            detail: None,
            error: Some(
                "Failed to parse response: Unexpected response shape: expected value at line 1"
                    .to_string(),
            ),
        };
        let observed = crate::services::usage::adapter::with_fetch_override(
            "minimax",
            malformed.clone(),
            || {
                cached_or_fetch(
                    "minimax",
                    true,
                    &[account("minimax", Some("k"))],
                )
                .expect("dispatch minimax")
            },
        );
        assert_eq!(observed.provider, "minimax");
        assert!(observed.logged_in, "parse failure stays logged_in");
        assert!(
            observed
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Failed to parse response"),
            "error string must propagate, got: {:?}",
            observed.error
        );
    }

    #[test]
    fn dispatch_fetch_returns_auth_failure_logged_out_envelope() {
        let _guard = OVERRIDE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Rejected credentials surface as `logged_out` (logged_in=false,
        // error carries the re-entry prompt). The seam must propagate it
        // so `assemble_meters`'s no-credential gate can distinguish
        // "no key" from "key rejected" once #1657 step 5 lifts that
        // distinction out of the command layer.
        uncache("kimi");
        let rejected = ProviderUsage {
            provider: "kimi".to_string(),
            logged_in: false,
            windows: vec![],
            balance: None,
            detail: None,
            error: Some("Invalid API key".to_string()),
        };
        let observed = crate::services::usage::adapter::with_fetch_override(
            "kimi",
            rejected.clone(),
            || {
                cached_or_fetch(
                    "kimi",
                    true,
                    &[account("kimi", Some("k"))],
                )
                .expect("dispatch kimi")
            },
        );
        assert_eq!(observed.provider, "kimi");
        assert!(!observed.logged_in, "rejected key must be logged out");
        assert_eq!(observed.error.as_deref(), Some("Invalid API key"));
    }
}
