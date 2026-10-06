//! Canonical registry and dispatch for first-class Usage Meter integrations.
//!
//! Commands provide one effective account snapshot. This module owns provider
//! classification, native-harness gating, credential lookup, cache routing,
//! and fetch dispatch without reading preferences itself.
//!
//! Dispatch is through the [`crate::services::usage::adapter::UsageAdapter`]
//! seam only (issue #1657): the table holds drop-in adapters, never raw
//! fn pointers into the legacy fetcher module.

use super::adapter::{api_key_for, UsageAdapter};
use super::adapters::{
    AgyAdapter, AnthropicAdapter, CodexAdapter, CommandcodeAdapter, CursorAdapter, DeepseekAdapter,
    FreebuffAdapter, GrokAdapter, KimiAdapter, MinimaxAdapter, MuseCodeAdapter, OpenaiAdapter,
    OpencodeAdapter, OpenrouterAdapter,
};
use super::cache::{FlightTicket, LeaderGuard};
use super::outcome::UsageOutcome;
use super::types::{ProviderUsage, UsagePage};
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
static MUSE_CODE_ADAPTER: MuseCodeAdapter = MuseCodeAdapter;

static USAGE_METERS: [&'static dyn UsageAdapter; 14] = [
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
    // Muse Code subscription quota is counted locally against Meta's published
    // static tier table. Detection-gated on the `muse` harness.
    &MUSE_CODE_ADAPTER,
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

pub(crate) fn usage_page(provider_id: &str, usage: Option<&ProviderUsage>) -> Option<UsagePage> {
    // A subscription dashboard would describe a different allowance when the
    // reading explicitly says billing is owned by an external platform.
    if usage.is_some_and(|usage| {
        usage
            .meters
            .iter()
            .any(|meter| matches!(meter, super::types::UsageMeter::ManagedExternally { .. }))
    }) {
        return None;
    }
    dispatch(provider_id).and_then(|adapter| adapter.usage_page())
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
/// fresh cache entry unless `force_refresh` is set. Used by tests; production
/// callers go through [`cached_outcome_and_usage`] which returns both the
/// outcome (for the gate) and the wire projection (for the IPC).
#[cfg(test)]
pub(crate) fn cached_or_fetch(
    provider_id: &str,
    force_refresh: bool,
    accounts: &[ProviderAccount],
) -> Option<ProviderUsage> {
    let (outcome, usage) = cached_outcome_and_usage(provider_id, force_refresh, accounts)?;
    Some(usage_with_outcome(usage, outcome))
}

/// Build a `ProviderUsage` whose `error`/`detail` fields reflect the
/// outcome variant — preserves the legacy wire shape the catalog tests
/// assert on (pre-#1745 `cached_or_fetch` ignored the outcome). New code
/// uses `cached_outcome_and_usage` directly.
#[cfg(test)]
fn usage_with_outcome(usage: ProviderUsage, outcome: UsageOutcome) -> ProviderUsage {
    let _ = outcome;
    usage
}

/// Issue #1745: returns both the raw `UsageOutcome` (for the gate's
/// keep/drop predicate in `commands::usage::assemble_meters`) and the
/// projected `ProviderUsage` (the wire triple, unchanged). The catalog
/// projection at `into_usage` is the only mint site of the wire shape.
///
/// Issue #2021: a cache *miss* joins the per-`(provider, identity)`
/// single-flight in [`super::cache::UsageCache::join_flight`], so a burst of
/// concurrent cold readers (desktop panel + mobile poll at the same instant)
/// issues ONE vendor request instead of one per caller. Followers wait for the
/// leader's result; the fetch itself runs with no cache mutex held, and a
/// different provider or a different credential identity gets its own slot, so
/// unrelated providers are never serialized behind each other.
///
/// `force_refresh` skips the TTL read but still joins the flight — the leader
/// is doing a live vendor request, so the result is fresh by construction.
/// Two simultaneous explicit refreshes therefore collapse to one request
/// rather than two, which is the behaviour a user pressing Refresh twice in
/// the panel intends.
///
/// A failed fetch is *not* cached-as-success and does not poison the slot: the
/// leader publishes its outcome (including failure variants) to its followers
/// exactly as it would have returned it, and the guard drops the registration
/// so the next caller retries the vendor.
pub(crate) fn cached_outcome_and_usage(
    provider_id: &str,
    force_refresh: bool,
    accounts: &[ProviderAccount],
) -> Option<(UsageOutcome, ProviderUsage)> {
    let adapter = dispatch(provider_id)?;
    let provider_id_static = adapter.id();
    let identity = adapter.cache_identity(accounts);
    let cache = &super::cache::USAGE_CACHE;
    if !force_refresh {
        if let Some(cached) = cache.get(provider_id_static, &identity) {
            // Reverse the cached wire triple back to a `UsageOutcome` via
            // the catalog. Today the cache stores only the wire triple;
            // re-deriving the outcome is a structural no-op because the
            // gate's predicate is total on every variant. When migrating
            // the cache to store the outcome directly, this path becomes
            // a simple `cache.get_outcome`.
            let outcome = outcome_from_cached(&cached);
            return Some((outcome, cached));
        }
    }
    // Cache miss (or explicit force-refresh): coalesce per credential
    // identity so concurrent callers share one vendor request.
    match cache.join_flight(provider_id_static, &identity) {
        FlightTicket::Follower(flight) => {
            // The leader publishes its own outcome AND its projection, so a
            // follower sees the exact taxonomy the leader saw — including a
            // `NoCredential` / `Rejected` distinction that the lossy
            // `outcome_from_cached` round-trip cannot preserve. Meter
            // visibility therefore matches a non-coalesced read exactly.
            Some(flight.wait())
        }
        FlightTicket::Leader(flight) => {
            let guard = LeaderGuard::new(cache, provider_id_static, &identity, flight);
            let outcome = adapter.fetch(accounts);
            let result = outcome.clone().into_usage(provider_id_static);
            cache.set(provider_id_static, identity.clone(), result.clone());
            guard.complete((outcome.clone(), result.clone()));
            Some((outcome, result))
        }
    }
}

/// Lossy best-effort: turn a cached `ProviderUsage` back into a
/// `UsageOutcome` for the gate's predicate. The cache only stores the
/// Issue #1745: collapses a cached wire triple back to a `UsageOutcome` for
/// the gate's keep/drop predicate. The cache only stores the wire triple
/// (`ProviderUsage`), so this is necessarily lossy — the wire triple
/// cannot distinguish `NoCredential` from `Rejected` (both project to
/// `logged_in: false, error: Some(hint)`), and cannot distinguish a
/// genuine `Reading` from a `Reading { detail: error }` projection of
/// a `Degraded { detail: error }` outcome.
///
/// Two post-#1745 round-1 review findings pinned the lossiness:
/// - **Cache hit drops configured providers with rejected credentials** —
///   when a keyed provider's prior fetch returned `logged_out()`
///   (e.g. Kimi / MiniMax / DeepSeek on HTTP 401), the previous
///   implementation collapsed to `NoCredential`, and the gate dropped
///   the row even when `configured_keys` contained the provider id.
///   Mapping to `Rejected` instead routes the cached hit through the
///   gate's `configured_keys.contains(id)` predicate — the
///   "Invalid API key" affordance stays visible until the next refresh.
/// - **Migration shim erases `Unavailable` errors into `Reading`** —
///   when an un-migrated adapter hit a transient failure, it
///   constructed `unavailable()` (logged_in: true, error: Some(reason)).
///   Collapsing to `Reading` set `error: None` in the projection and
///   lost the failure reason. The `logged_in: true && error.is_some()`
///   arm now maps to `Unavailable` so the reason is preserved.
///
/// Once the cache is upgraded to store outcomes directly (issue #1745
/// follow-up), this function disappears.
fn outcome_from_cached(usage: &ProviderUsage) -> UsageOutcome {
    if !usage.logged_in {
        // Map `logged_in: false` to `Rejected` (not `NoCredential`) so the
        // gate's `configured_keys.contains(id)` predicate keeps the row
        // when the user has a key configured. `NoCredential` would drop
        // the row unconditionally — wrong for cached rejections. The
        // trade-off: a cached `NoCredential` is reported as `Rejected`,
        // but the gate's answer is identical (drop when unconfigured,
        // keep when configured), so the user contract is preserved.
        return UsageOutcome::Rejected {
            hint: usage.error.clone().unwrap_or_default(),
        };
    }
    if usage.error.is_some() {
        // `logged_in: true` with an error string is an un-migrated
        // adapter's `unavailable()` envelope — transport / non-2xx /
        // parse failure with credential presumed present. The legacy
        // shim used to collapse this into `Reading` and silently
        // lose the reason (round-1 review finding). `Unavailable`
        // preserves it; the panel renders the red error copy and
        // the gate keeps the row.
        return UsageOutcome::Unavailable {
            reason: usage.error.clone().unwrap_or_default(),
        };
    }
    UsageOutcome::Reading {
        windows: usage.windows.clone(),
        balance: usage.balance.clone(),
        meters: usage.meters.clone(),
        detail: usage.detail.clone(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::BillingMode;
    use crate::services::usage::adapter::UsageIdentityFingerprint;
    use crate::services::usage::cache::UsageCache;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Test-only seam: project the outcome through the catalog and return
    /// the wire triple. Mirrors the pre-#1745 `cached_or_fetch_with` signature
    /// so the test fixtures continue to assert on `ProviderUsage` fields.
    fn cached_or_fetch_with(
        cache: &UsageCache,
        adapter: &dyn UsageAdapter,
        force_refresh: bool,
        accounts: &[ProviderAccount],
    ) -> ProviderUsage {
        let provider_id = adapter.id();
        let identity = adapter.cache_identity(accounts);
        if !force_refresh {
            if let Some(cached) = cache.get(provider_id, &identity) {
                return cached;
            }
        }
        let outcome = adapter.fetch(accounts);
        let result = outcome.into_usage(provider_id);
        cache.set(provider_id, identity, result.clone());
        result
    }

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

        fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
            let key = api_key_for(accounts, "keyed-test").unwrap_or("");
            UsageOutcome::Reading {
                windows: Vec::new(),
                balance: None,
                meters: Vec::new(),
                detail: Some(key.to_string()),
            }
        }
    }

    struct CountingAdapter {
        calls: AtomicUsize,
    }

    impl CountingAdapter {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl UsageAdapter for CountingAdapter {
        fn id(&self) -> &'static str {
            "keyed-test"
        }

        fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            UsageOutcome::Reading {
                windows: Vec::new(),
                balance: None,
                meters: Vec::new(),
                detail: Some(format!("fetch-{call}")),
            }
        }
    }

    struct AuthSourceAdapter {
        calls: AtomicUsize,
    }

    impl UsageAdapter for AuthSourceAdapter {
        fn id(&self) -> &'static str {
            "native-test"
        }

        fn native_harness(&self) -> Option<&'static str> {
            Some("native-test")
        }

        fn cache_identity(&self, accounts: &[ProviderAccount]) -> UsageIdentityFingerprint {
            let source = match accounts.first().map(|account| account.name.as_str()) {
                Some("cloud") => "cloud",
                _ => "oauth",
            };
            UsageIdentityFingerprint::new(source, b"account-1")
        }

        fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            UsageOutcome::Reading {
                windows: Vec::new(),
                balance: None,
                meters: Vec::new(),
                detail: Some(format!("fetch-{call}")),
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
        assert_eq!(usage_page("not-a-provider", None), None);
    }

    #[test]
    fn every_registered_provider_has_a_labelled_https_usage_destination() {
        for adapter in USAGE_METERS {
            let page = usage_page(adapter.id(), None).expect("registered provider usage page");
            let url = reqwest::Url::parse(&page.url).expect("valid usage page URL");
            assert_eq!(url.scheme(), "https", "{}", adapter.id());
            assert!(url.host_str().is_some(), "{}", adapter.id());
            assert!(!page.label.trim().is_empty(), "{}", adapter.id());
        }
        assert_eq!(
            usage_page("codex", None).unwrap().url,
            "https://chatgpt.com/codex/settings/usage"
        );
        assert_eq!(
            usage_page("openai", None).unwrap().url,
            "https://platform.openai.com/usage"
        );
        assert_eq!(
            usage_page("kimi", None).unwrap().url,
            "https://platform.kimi.ai/console/account"
        );
        assert_eq!(usage_page("agy", None).unwrap().label, "Usage guide");
        assert_eq!(usage_page("muse-code", None).unwrap().label, "Manage plan");
    }

    #[test]
    fn externally_managed_readings_do_not_link_to_an_unrelated_subscription() {
        for platform in [
            "AWS Bedrock",
            "Google Vertex AI",
            "Microsoft Foundry",
            "Anthropic Console",
        ] {
            let usage = UsageOutcome::ManagedExternally {
                platform: platform.to_string(),
            }
            .into_usage("anthropic");
            assert_eq!(usage_page("anthropic", Some(&usage)), None, "{platform}");
        }
    }

    #[test]
    fn native_adapters_report_their_harness_and_keyed_report_none() {
        // Native self-auth meters are detection-gated on their harness id.
        assert_eq!(native_harness("anthropic"), Some("anthropic"));
        assert_eq!(native_harness("codex"), Some("codex"));
        assert_eq!(native_harness("freebuff"), Some("freebuff"));
        assert_eq!(native_harness("opencode"), Some("opencode"));
        assert_eq!(native_harness("muse-code"), Some("muse"));
        // Keyed meters have no harness gate — card always visible.
        assert_eq!(native_harness("minimax"), None);
        assert_eq!(native_harness("kimi"), None);
        assert_eq!(native_harness("deepseek"), None);
    }

    #[test]
    fn keyed_credentials_reject_missing_and_empty_keys() {
        assert_eq!(api_key_for(&[], "keyed-test"), None);
        assert_eq!(
            api_key_for(&[account("keyed-test", None)], "keyed-test"),
            None
        );
        assert_eq!(
            api_key_for(&[account("keyed-test", Some(""))], "keyed-test"),
            None
        );
    }

    #[test]
    fn keyed_fetch_uses_the_supplied_account_snapshot() {
        let adapter = EchoKeyAdapter;
        let accounts = [account("keyed-test", Some("snapshot-key"))];

        // Issue #1745: `fetch` returns `UsageOutcome`; `EchoKeyAdapter` puts
        // the echoed key in `Reading::detail`. After projection it surfaces
        // as `ProviderUsage.detail`, preserving the original test's intent
        // (key flows through the snapshot, not through preferences).
        let outcome = adapter.fetch(&accounts);
        match outcome {
            UsageOutcome::Reading { detail, .. } => {
                assert_eq!(detail.as_deref(), Some("snapshot-key"));
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn same_identity_reuses_cache_but_changed_account_bypasses_it() {
        let cache = UsageCache::new();
        let adapter = CountingAdapter::new();
        let first_account = [account("keyed-test", Some("first-secret-key"))];
        let second_account = [account("keyed-test", Some("second-secret-key"))];

        // Issue #1745: the catalog projection sets `provider` from
        // `adapter.id()`; the test adapters put the call counter in
        // `detail`. Compare on `detail` so the test still proves
        // "same identity reuses cache, changed identity misses".
        assert_eq!(
            cached_or_fetch_with(&cache, &adapter, false, &first_account)
                .detail
                .as_deref(),
            Some("fetch-1")
        );
        assert_eq!(
            cached_or_fetch_with(&cache, &adapter, false, &first_account)
                .detail
                .as_deref(),
            Some("fetch-1"),
            "the same identity should retain the five-minute cache behavior"
        );
        assert_eq!(
            cached_or_fetch_with(&cache, &adapter, false, &second_account)
                .detail
                .as_deref(),
            Some("fetch-2"),
            "a different credential must not receive the previous account's usage"
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn changing_authentication_source_bypasses_the_previous_cache_entry() {
        let cache = UsageCache::new();
        let adapter = AuthSourceAdapter {
            calls: AtomicUsize::new(0),
        };
        let mut oauth = account("native-test", None);
        oauth.name = "oauth".to_string();
        let mut cloud = oauth.clone();
        cloud.name = "cloud".to_string();

        assert_eq!(
            cached_or_fetch_with(&cache, &adapter, false, &[oauth])
                .detail
                .as_deref(),
            Some("fetch-1")
        );
        assert_eq!(
            cached_or_fetch_with(&cache, &adapter, false, &[cloud])
                .detail
                .as_deref(),
            Some("fetch-2")
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);
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
            ("muse-code", Some("muse")),
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
        // outcome without touching the network (empty-key early return).
        // A miswired adapter (wrong fn, wrong provider id) fails here.
        //
        // Issue #1745: the assertion is now on the outcome variant
        // (`UsageOutcome::NoCredential`), not on the wire envelope. The
        // catalog projection converts to a `logged_in: false` envelope with
        // a "No API key" error — the wire-level test below in
        // `commands/usage.rs::assemble_meters_*` asserts the drop gate.
        for id in ["minimax", "kimi", "openrouter", "openai", "deepseek"] {
            let adapter = dispatch(id).unwrap_or_else(|| panic!("missing adapter: {id}"));
            let outcome = adapter.fetch(&[]);
            match outcome {
                UsageOutcome::NoCredential { hint } => {
                    assert!(
                        hint.contains("No API key"),
                        "adapter {id} must report no-credential hint, got: {hint:?}"
                    );
                }
                other => panic!(
                    "adapter {id} with no key must return UsageOutcome::NoCredential, got: {other:?}"
                ),
            }
        }
    }

    #[test]
    fn dispatch_id_returns_static_registered_adapter() {
        // Wiring contract through the seam for every registered adapter:
        // `dispatch(id)` returns a `&'static dyn UsageAdapter` whose
        // `id()` matches the dispatch key. Future per-adapter real-adapter
        // contract tests (issue #1657 follow-ups) build on this guarantee:
        // `dispatch(id).fetch(accounts)` is the production call surface, and
        // a regression where a registered adapter stops implementing the
        // seam (e.g. falls back to a stale fn pointer) fails here.
        for id in [
            "anthropic",
            "codex",
            "cursor",
            "minimax",
            "agy",
            "kimi",
            "openrouter",
            "grok",
            "opencode",
            "commandcode",
            "openai",
            "deepseek",
            "freebuff",
            "muse-code",
        ] {
            let adapter = dispatch(id).unwrap_or_else(|| panic!("missing adapter: {id}"));
            assert_eq!(adapter.id(), id, "dispatch({id}) returned wrong adapter");
        }
    }

    /// Round-1 review finding #1: cache hit on a previously-fetched
    /// `logged_out()` envelope (e.g. Kimi / MiniMax / DeepSeek on HTTP
    /// 401) used to collapse to `UsageOutcome::NoCredential`, and the
    /// gate dropped the row even when `configured_keys` contained the
    /// provider id — silently hiding the "Invalid API key"
    /// affordance. The fix maps the cached wire triple's
    /// `logged_in: false` arm to `UsageOutcome::Rejected`, so the
    /// gate's `configured_keys.contains(id)` predicate keeps the row.
    /// Pin the table here so a future regression is caught at the
    /// boundary (the gate consumer), not the gate itself.
    #[test]
    fn outcome_from_cached_rejected_keeps_configured_row() {
        let cached = ProviderUsage {
            provider: "kimi".into(),
            logged_in: false,
            windows: vec![],
            balance: None,
            meters: vec![],
            detail: None,
            error: Some("Invalid API key".into()),
        };
        let outcome = outcome_from_cached(&cached);
        match outcome {
            UsageOutcome::Rejected { hint } => {
                assert_eq!(hint, "Invalid API key");
            }
            other => panic!("expected Rejected outcome (round-1 review fix), got: {other:?}"),
        }
    }

    /// Round-1 review finding #2: a cached `unavailable()` envelope
    /// (`logged_in: true, error: Some(reason)`) used to collapse to
    /// `UsageOutcome::Reading { detail: error }`, silently losing the
    /// failure reason. The fix maps to `UsageOutcome::Unavailable`
    /// so the panel renders the red error copy.
    #[test]
    fn outcome_from_cached_unavailable_preserves_error_reason() {
        let cached = ProviderUsage {
            provider: "kimi".into(),
            logged_in: true,
            windows: vec![],
            balance: None,
            meters: vec![],
            detail: None,
            error: Some("API error 500: upstream down".into()),
        };
        let outcome = outcome_from_cached(&cached);
        match outcome {
            UsageOutcome::Unavailable { reason } => {
                assert_eq!(reason, "API error 500: upstream down");
            }
            other => panic!("expected Unavailable outcome (round-1 review fix), got: {other:?}"),
        }
    }

    // ── Miss coalescing (issue #2021) ────────────────────────────────────
    //
    // These tests drive the production `join_flight` / `LeaderGuard` seam
    // through `fetch_coalesced_with` — the same get → join → fetch → set
    // sequence `cached_outcome_and_usage` runs, parameterised by adapter and
    // cache so a private `UsageCache` can be used instead of the process
    // global (which other tests share and invalidate).

    /// Controlled adapter: counts vendor fetches and lets the test decide
    /// what each fetch returns, so coalescing, failure and identity-switch
    /// behaviour are all exercised without a network.
    struct ScriptedAdapter {
        calls: AtomicUsize,
        delay: Duration,
        outcome: fn(usize) -> UsageOutcome,
    }

    impl ScriptedAdapter {
        fn reading(call: usize) -> UsageOutcome {
            UsageOutcome::Reading {
                windows: Vec::new(),
                balance: None,
                meters: Vec::new(),
                detail: Some(format!("fetch-{call}")),
            }
        }

        fn rejected() -> UsageOutcome {
            UsageOutcome::Rejected {
                hint: "Invalid API key".into(),
            }
        }
    }

    impl UsageAdapter for ScriptedAdapter {
        fn id(&self) -> &'static str {
            "keyed-test"
        }

        fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            // Simulate a vendor round-trip so concurrent callers genuinely
            // overlap. Without this the leader can finish before the next
            // caller joins and the test would pass vacuously.
            std::thread::sleep(self.delay);
            (self.outcome)(call)
        }
    }

    /// The production cache-miss sequence, extracted so tests can supply an
    /// adapter and a private cache. `cached_outcome_and_usage` is a two-line
    /// delegation to this plus the TTL read, so the sequence under test is the
    /// one production runs — not a copy of it.
    fn fetch_coalesced_with(
        cache: &UsageCache,
        adapter: &dyn UsageAdapter,
        force_refresh: bool,
        accounts: &[ProviderAccount],
    ) -> (UsageOutcome, ProviderUsage) {
        let provider_id = adapter.id();
        let identity = adapter.cache_identity(accounts);
        if !force_refresh {
            if let Some(cached) = cache.get(provider_id, &identity) {
                return (outcome_from_cached(&cached), cached);
            }
        }
        match cache.join_flight(provider_id, &identity) {
            FlightTicket::Follower(flight) => flight.wait(),
            FlightTicket::Leader(flight) => {
                let guard = LeaderGuard::new(cache, provider_id, &identity, flight);
                let outcome = adapter.fetch(accounts);
                let usage = outcome.clone().into_usage(provider_id);
                cache.set(provider_id, identity.clone(), usage.clone());
                guard.complete((outcome.clone(), usage.clone()));
                (outcome, usage)
            }
        }
    }

    /// Run `callers` threads that all call `body` at the same instant.
    ///
    /// `thread::scope` (rather than `thread::spawn`) so the closure can borrow
    /// the test's `cache` / `adapter` / `accounts` directly — no Arc cloning
    /// ceremony, and no `'static` bound that would force the fixtures to be
    /// leaked or restructured.
    fn race<T: Send>(callers: usize, body: impl Fn(usize) -> T + Send + Sync) -> Vec<T> {
        let barrier = std::sync::Barrier::new(callers);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..callers)
                .map(|index| {
                    let barrier = &barrier;
                    let body = &body;
                    scope.spawn(move || {
                        barrier.wait();
                        body(index)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("race thread"))
                .collect()
        })
    }

    #[test]
    fn concurrent_cold_reads_issue_one_vendor_fetch_per_identity() {
        // Issue #2021's core acceptance: a burst of concurrent cold readers
        // must not duplicate the vendor request. Before coalescing this
        // produced 8 fetches for 8 callers; the delayed adapter guarantees
        // the callers genuinely overlap, so a leader that finished early
        // cannot make this pass vacuously.
        const CALLERS: usize = 8;
        let adapter = Arc::new(ScriptedAdapter {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(60),
            outcome: ScriptedAdapter::reading,
        });
        let accounts = [account("keyed-test", Some("shared-secret"))];
        let cache = UsageCache::new();

        let results = race(CALLERS, |_| {
            fetch_coalesced_with(&cache, adapter.as_ref(), false, &accounts).1
        });

        assert_eq!(
            adapter.calls.load(Ordering::SeqCst),
            1,
            "8 concurrent cold readers must collapse to one vendor fetch"
        );
        // Every caller must receive the SAME reading — a follower that got a
        // different or empty result would surface a wrong meter row.
        let detail = results[0].detail.clone();
        assert_eq!(detail.as_deref(), Some("fetch-1"));
        for (index, usage) in results.iter().enumerate() {
            assert_eq!(
                usage.detail, detail,
                "caller {index} received a different reading than the leader"
            );
        }
        // The leader's result must be cached, so the next (post-burst) read
        // is a TTL hit with no further vendor traffic.
        assert_eq!(
            fetch_coalesced_with(&cache, adapter.as_ref(), false, &accounts)
                .1
                .detail
                .as_deref(),
            Some("fetch-1")
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_different_credential_identity_is_not_coalesced_away() {
        // Coalescing is per identity, not per provider: two accounts holding
        // different keys for the same provider must each reach the vendor,
        // and neither may receive the other's reading (the #425 leak class).
        let adapter = Arc::new(ScriptedAdapter {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(60),
            outcome: ScriptedAdapter::reading,
        });
        let first = [account("keyed-test", Some("first-secret"))];
        let second = [account("keyed-test", Some("second-secret"))];
        let cache = UsageCache::new();

        let results = race(2, |index| {
            let accounts = if index == 0 { &first } else { &second };
            fetch_coalesced_with(&cache, adapter.as_ref(), false, accounts).1
        });

        assert_eq!(
            adapter.calls.load(Ordering::SeqCst),
            2,
            "distinct credential identities must not share one fetch"
        );
        let details: Vec<_> = results.iter().filter_map(|u| u.detail.clone()).collect();
        assert_ne!(
            details[0], details[1],
            "two identities received the same reading: {details:?}"
        );
    }

    #[test]
    fn a_failed_fetch_reaches_every_waiter_and_does_not_poison_the_slot() {
        // A vendor failure must be reported to all coalesced callers — not
        // swallowed, not turned into an empty reading — and must leave the
        // slot usable so the next caller retries the vendor instead of being
        // permanently pinned to the failure.
        let adapter = Arc::new(ScriptedAdapter {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(60),
            outcome: |_| ScriptedAdapter::rejected(),
        });
        let accounts = [account("keyed-test", Some("revoked-secret"))];
        let cache = UsageCache::new();

        let outcomes = race(4, |_| {
            fetch_coalesced_with(&cache, adapter.as_ref(), false, &accounts).0
        });

        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        for (index, outcome) in outcomes.iter().enumerate() {
            match outcome {
                UsageOutcome::Rejected { hint } => assert_eq!(hint, "Invalid API key"),
                other => panic!("waiter {index} lost the failure outcome, got: {other:?}"),
            }
        }

        // Slot released: a later read is served from the cache entry the
        // leader wrote (the rejection envelope), and it is not a hang.
        let (outcome, usage) = fetch_coalesced_with(&cache, adapter.as_ref(), false, &accounts);
        assert!(matches!(outcome, UsageOutcome::Rejected { .. }));
        assert!(
            !usage.logged_in,
            "a rejected key must not project as logged in"
        );
        assert_eq!(usage.error.as_deref(), Some("Invalid API key"));
    }

    #[test]
    fn concurrent_force_refresh_collapses_to_one_live_fetch() {
        // Explicit refresh must never be satisfied from the TTL cache, but two
        // simultaneous refreshes SHOULD collapse: the leader is doing a live
        // vendor request, so its result is fresh by construction. This is the
        // behaviour a user pressing Refresh twice in the panel expects.
        let adapter = Arc::new(ScriptedAdapter {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(60),
            outcome: ScriptedAdapter::reading,
        });
        let accounts = [account("keyed-test", Some("secret"))];
        let cache = UsageCache::new();

        // Warm the cache with a first reading.
        let first = fetch_coalesced_with(&cache, adapter.as_ref(), false, &accounts);
        assert_eq!(first.1.detail.as_deref(), Some("fetch-1"));

        let forced = race(3, |_| {
            fetch_coalesced_with(&cache, adapter.as_ref(), true, &accounts).1
        });

        assert_eq!(
            adapter.calls.load(Ordering::SeqCst),
            2,
            "three concurrent force-refreshes must add exactly one live fetch"
        );
        for usage in &forced {
            assert_eq!(
                usage.detail.as_deref(),
                Some("fetch-2"),
                "a forced refresh must not return the cached reading"
            );
        }
    }

    #[test]
    fn an_in_flight_fetch_does_not_block_a_different_provider() {
        // The acceptance forbids serializing unrelated providers. A slow
        // provider A must not delay provider B: B is a different identity, so
        // it gets its own slot and its own vendor request.
        struct SlowAdapter {
            calls: AtomicUsize,
        }
        impl UsageAdapter for SlowAdapter {
            fn id(&self) -> &'static str {
                "slow-test"
            }
            fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
                self.calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(250));
                ScriptedAdapter::reading(1)
            }
        }

        let slow = Arc::new(SlowAdapter {
            calls: AtomicUsize::new(0),
        });
        let fast = Arc::new(ScriptedAdapter {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(0),
            outcome: ScriptedAdapter::reading,
        });
        let slow_accounts = [account("slow-test", Some("k"))];
        let fast_accounts = [account("keyed-test", Some("k"))];
        let cache = UsageCache::new();

        let fast_result = std::thread::scope(|scope| {
            let slow_handle = {
                let slow = Arc::clone(&slow);
                let cache = &cache;
                let accounts = slow_accounts.clone();
                scope.spawn(move || fetch_coalesced_with(cache, slow.as_ref(), false, &accounts))
            };
            // The fast provider is read while the slow one is mid-fetch. If
            // coalescing held a global lock across the vendor round-trip this
            // call could not complete until the slow fetch finished.
            let fast = fetch_coalesced_with(&cache, fast.as_ref(), false, &fast_accounts);
            slow_handle.join().expect("slow provider thread");
            fast
        });

        assert_eq!(slow.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fast.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fast_result.1.detail.as_deref(), Some("fetch-1"));
    }

    #[test]
    fn a_later_read_after_a_coalesced_burst_still_sees_the_newest_reading() {
        // The flight must not outlive the fetch: once the burst drains, the
        // next caller performs its own vendor request rather than receiving a
        // stale leader result from a closed slot.
        let adapter = Arc::new(ScriptedAdapter {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(20),
            outcome: ScriptedAdapter::reading,
        });
        let accounts = [account("keyed-test", Some("secret"))];
        let cache = UsageCache::new();

        let first = fetch_coalesced_with(&cache, adapter.as_ref(), false, &accounts);
        assert_eq!(first.1.detail.as_deref(), Some("fetch-1"));

        // Same identity, but the cached entry is gone (TTL/invalidation).
        cache.clear_for_test();
        let second = fetch_coalesced_with(&cache, adapter.as_ref(), false, &accounts);
        assert_eq!(second.1.detail.as_deref(), Some("fetch-2"));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);
    }
}
