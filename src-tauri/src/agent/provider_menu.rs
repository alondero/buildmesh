//! Spawn Menu composition — derive the `ProviderInfo` rows the desktop and
//! mobile Spawn Option pickers render.
//!
//! This is the deep module for the Spawn Menu derivation. Pre-#1052 the
//! derivation lived in `commands::agent::available_providers` — a 2,400+ line
//! file mixing process-lifecycle, spawn orchestration, and this menu logic.
//! Issue #1052 split the file so this module owns the menu logic next to its
//! unit tests, `agent::process` owns the process-lifecycle Tauri commands,
//! and `commands::agent` is left with thin spawn-orchestration adapters.
//! The Tauri command here is [`list_providers`]; the pure helpers
//! (`compose_provider_menu`, `order_providers`, `order_proxied_children`,
//! `provider_info_for`, `provider_info_for_pairing`) are the unit-test seam.

use crate::agent::provider::{Platform, ProviderInfo};
use tauri::command;

/// Compose the `ProviderInfo` (Spawn Option) row for a single harness profile on the
/// current host platform. Pure (no disk / DB / globals) so unit tests can
/// exercise the per-profile derivation — including the `resumable` flag —
/// without touching the preferences module's `APP_DATA_DIR` / `CACHE`
/// shared state (which is global per-process and would otherwise race
/// against other tests).
///
/// Extracted from `available_providers` so the per-profile logic can be
/// pinned without driving `harness_profiles()` (which reads from disk and
/// shares a `OnceLock`). Resolves the executor via `profile.harness` (the
/// stored profile field that names the backing [`Provider`]) rather than
/// `preferences::resolve_harness_provider(&profile.id)` — that helper
/// reads disk via `harness_profiles()` to look up an id, which would
/// defeat the test isolation. For the `available_providers` call site
/// the two paths are equivalent (every profile iterated here comes from
/// `harness_profiles()` and so its id→harness lookup is a no-op).
///
/// The row is a **native Spawn Option**: `id == profile.id` (no `:`), no
/// `provider_id`, `is_proxied = false`. `harness_id == profile.id` is the
/// grouping key the frontend uses to bucket rows under their harness header
/// (issue #575 / ADR-0016).
pub(super) fn provider_info_for(profile: &crate::preferences::HarnessProfile, host: Platform) -> Option<ProviderInfo> {
    let adapter = crate::models::Provider::from_db_str(&profile.harness).adapter();
    if !adapter.available_on().contains(&host) {
        return None;
    }
    let ui = adapter.ui();
    // Backend-derived answer to "can this provider resume an archived
    // session in place?" — both flags must be true: supports_resume()
    // gates the CLI flag, produces_readable_transcript() gates the
    // coordinator read API that rehydrates the session. The archived-node
    // resume picker consumes this so a custom Claude-compatible profile
    // (e.g. "DeepSeek via Claude") shows up without the old hardcoded id
    // allow-list (#550 follow-up).
    let resumable = adapter.supports_resume() && adapter.produces_readable_transcript();
    Some(ProviderInfo {
        id: profile.id.clone(),
        label: profile.name.clone(),
        color: ui.color,
        icon: ui.icon,
        resumable,
        harness_id: profile.id.clone(),
        provider_id: None,
        is_proxied: false,
        group_key: profile.id.clone(),
        capabilities: crate::agent::capabilities::capabilities_for(adapter),
    })
}

/// Compose the `ProviderInfo` (Spawn Option) row for one **Proxied Provider**
/// pairing — a [`crate::preferences::ProviderPairing`] attaching an account to a
/// harness over a chosen **Compatible API surface** (issue #576, generalises the
/// #575 account-only row). The endpoint URL + model map travel with the pairing
/// (resolved at spawn by [`crate::preferences::resolve_provider_env`]); the brand
/// label comes from the account and the row's colour/icon from the *executor*
/// adapter, so a MiniMax-via-Codex row reads as a Codex-family row while the
/// frontend `ProviderIcon` (keyed off the composite `id`) still renders the
/// MiniMax brand mark.
///
/// The composite `id` is `<harness_id>:<provider_id>` (e.g. `claude:minimax`,
/// `codex:minimax`) and `harness_id`/`group_key` cluster the row under its
/// harness header in the rendered Spawn Menu. The executor is resolved from the
/// pairing's harness *profile* (its `harness` field), falling back to parsing the
/// `harness_id` directly when no matching profile is present (a stored pairing
/// for an undetected harness, or a bare test env) — the same fallback chain the
/// resolver uses. Returns `None` only if that executor isn't available on this
/// host.
pub(super) fn provider_info_for_pairing(
    pairing: &crate::preferences::ProviderPairing,
    account: &crate::preferences::ProviderAccount,
    profiles: &[crate::preferences::HarnessProfile],
    host: Platform,
) -> Option<ProviderInfo> {
    let executor = profiles
        .iter()
        .find(|p| p.id == pairing.harness_id)
        .map(|p| crate::models::Provider::from_db_str(&p.harness))
        .unwrap_or_else(|| crate::models::Provider::from_db_str(&pairing.harness_id));
    let adapter = executor.adapter();
    if !adapter.available_on().contains(&host) {
        return None;
    }
    let ui = adapter.ui();
    Some(ProviderInfo {
        id: format!("{}:{}", pairing.harness_id, pairing.provider_id),
        label: account.name.clone(),
        color: ui.color,
        icon: ui.icon,
        resumable: adapter.supports_resume() && adapter.produces_readable_transcript(),
        harness_id: pairing.harness_id.clone(),
        provider_id: Some(pairing.provider_id.clone()),
        is_proxied: true,
        group_key: pairing.harness_id.clone(),
        capabilities: crate::agent::capabilities::capabilities_for(adapter),
    })
}

/// Build the spawn menu from the configuration lists. Pure (no disk/globals) so
/// the derivation is the unit-test seam.
///
/// Harness profiles (Terminal + startup-detected Claude/Codex/Antigravity/OpenCode)
/// come first as native rows, then one **Proxied Provider** row per *stored*
/// pairing for a proxiable account (ADR-0025 / issue #576,
/// [`crate::preferences::effective_pairings`]). Clearing the key or disabling
/// the account drops every stored row that depends on it.
///
/// **Dedup semantics** (issue #575 / ADR-0016): the composite id
/// `<harness>:<provider>` is unique per (harness profile id, account id) pair, so
/// the duplicate-row check by `info.id` only fires when the same (harness,
/// account) pair was produced twice. `effective_pairings` already dedups by
/// `(harness_id, provider_id)`, so this guard is a belt-and-braces on the native
/// rows (a `claude:claude` custom account stays distinct from the native
/// `claude` harness row).
pub(super) fn compose_provider_menu(
    profiles: Vec<crate::preferences::HarnessProfile>,
    accounts: Vec<crate::preferences::ProviderAccount>,
    pairings: Vec<crate::preferences::ProviderPairing>,
    host: Platform,
    order: &[String],
    proxied_order: &[crate::preferences::ProxiedProviderOrder],
) -> Vec<ProviderInfo> {
    let mut rows: Vec<ProviderInfo> = profiles
        .iter()
        .filter_map(|profile| provider_info_for(profile, host))
        .collect();
    // The Claude Code harness header the derived default pairings group under
    // (shared rule — see `preferences::claude_harness_id_from`).
    let _claude_harness_id = crate::preferences::claude_harness_id_from(&profiles);
    let effective = crate::preferences::effective_pairings(&accounts, &pairings);
    for pairing in &effective {
        let Some(account) = accounts.iter().find(|a| a.id == pairing.provider_id) else {
            continue;
        };
        if let Some(info) = provider_info_for_pairing(pairing, account, &profiles, host) {
            if !rows.iter().any(|r| r.id == info.id) {
                rows.push(info);
            }
        }
    }
    order_proxied_children(order_providers(rows, order), proxied_order)
}

/// Returns the list of agent providers available on this host platform.
/// Each provider declares which platforms it runs on via `AgentProvider::available_on()`.
///
/// `pub(crate)` so the mobile HTTP route (`http/routes/providers.rs`) can
/// keep using it as its menu source — the route wraps this call in
/// `commands::run_blocking` already, so the menu derivation continues to
/// stay off the async worker pool (issue #634).
pub(crate) fn available_providers() -> Vec<ProviderInfo> {
    let accounts = crate::preferences::provider_accounts();
    let configured_pairings = crate::preferences::provider_pairings();
    let needs_codex = configured_pairings
        .iter()
        .any(|pairing| pairing.surface == crate::preferences::ApiSurface::OpenAI);
    let native_codex = needs_codex
        .then(|| {
            crate::agent::provider::adapters::codex::discover_supported_install(
                crate::models::EnvType::Windows,
            )
        })
        .and_then(Result::ok);
    let wsl_codex = needs_codex
        .then(|| {
            crate::agent::provider::adapters::codex::discover_supported_install(
                crate::models::EnvType::Wsl,
            )
        })
        .and_then(Result::ok);
    let pairings = configured_pairings
        .into_iter()
        .filter(|pairing| {
            accounts
                .iter()
                .find(|account| account.id == pairing.provider_id)
                .is_some_and(|account| {
                    crate::services::provider_verification::launchable_on_runtime(
                        pairing,
                        account,
                        crate::models::EnvType::Windows,
                        native_codex.as_ref(),
                    )
                        || crate::services::provider_verification::launchable_on_runtime(
                            pairing,
                            account,
                            crate::models::EnvType::Wsl,
                            wsl_codex.as_ref(),
                        )
                })
        })
        .collect();
    compose_provider_menu(
        crate::preferences::harness_profiles(),
        accounts,
        pairings,
        Platform::current(),
        &crate::preferences::harness_order(),
        &crate::preferences::proxied_provider_order(),
    )
}

/// Within each harness bucket, sort **Proxied Provider** children by the
/// user's stored per-harness order (issue #577). The harness-level rank is
/// untouched — this runs after [`order_providers`] and only re-orders the
/// within-bucket child sequence. A child present in the bucket but not in
/// the stored list appends at the end in its natural input order (stable
/// sort). Native harness headers are never reordered — they're not proxied
/// children; a stored entry that names a native id is silently ignored.
///
/// Pure (no disk / globals) so the ordering is the unit-test seam. The
/// bucketing is by `group_key == harness_id`, the same wire-shape field
/// the frontend `groupBy` uses (ADR-0016 §6); every Proxied row carries
/// its harness id via that field.
pub(super) fn order_proxied_children(
    mut rows: Vec<ProviderInfo>,
    proxied_order: &[crate::preferences::ProxiedProviderOrder],
) -> Vec<ProviderInfo> {
    if proxied_order.is_empty() {
        return rows;
    }
    let index_by_harness: std::collections::HashMap<&str, &[String]> = proxied_order
        .iter()
        .map(|o| (o.harness_id.as_str(), o.provider_ids.as_slice()))
        .collect();
    rows.sort_by(|a, b| {
        // Only proxied rows within the same harness bucket compete on the
        // stored order. Native rows and rows in different buckets fall
        // through to the stable sort, preserving the harness-level rank
        // established by `order_providers`.
        if !a.is_proxied || !b.is_proxied || a.harness_id != b.harness_id {
            return std::cmp::Ordering::Equal;
        }
        let Some(provider_ids) = index_by_harness.get(a.harness_id.as_str()) else {
            return std::cmp::Ordering::Equal;
        };
        let rank_a = provider_ids
            .iter()
            .position(|id| id == a.provider_id.as_deref().unwrap_or(""));
        let rank_b = provider_ids
            .iter()
            .position(|id| id == b.provider_id.as_deref().unwrap_or(""));
        match (rank_a, rank_b) {
            (Some(ra), Some(rb)) => ra.cmp(&rb),
            (Some(_), None) => std::cmp::Ordering::Less, // listed before unlisted
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal, // both unlisted → stable
        }
    });
    rows
}

/// Order the Spawn Menu by the user's stored harness order, with the plain
/// `terminal` row always pinned to the bottom (issue #534 / #573).
///
/// `order` is the persisted list of **harness profile ids** (Terminal excluded
/// — it's forced last regardless of where it appears). Each row's rank is
/// derived from its `harness_id` (the grouping key) — not its composite `id`
/// — so a Proxied Provider row like `claude:minimax` clusters under its
/// `claude` harness header instead of ranking at the bottom. A row whose
/// `harness_id` isn't in the list (a newly-detected harness) ranks just below
/// `usize::MAX` so it appends at the end *above* Terminal. An uninstalled
/// harness simply isn't among `providers`, so its id sits dormant in `order`
/// and its slot is restored verbatim when it reappears.
///
/// Pure and stable (no disk / globals) so the ordering is the unit-test seam:
/// the `(is_terminal, rank, harness_id)` tuple key sorts Terminal last via the
/// bool, ranks the rest by stored harness order, and the `harness_id`
/// tiebreak pins multiple *newcomers* — all sharing `rank = usize::MAX - 1` —
/// into a deterministic alphabetical order rather than relying on the input
/// order from `harness_profiles()` (issue #581). Listed harnesses always have
/// distinct ranks so the tiebreak is moot for them; Proxied rows share their
/// parent's `harness_id` and the stable sort keeps the native header ahead
/// of its children (the native row is built first in `compose_provider_menu`).
pub(super) fn order_providers(mut providers: Vec<ProviderInfo>, order: &[String]) -> Vec<ProviderInfo> {
    providers.sort_by(|a, b| {
        let key_a = (
            a.harness_id == "terminal",
            order
                .iter()
                .position(|id| *id == a.harness_id)
                .unwrap_or(usize::MAX - 1),
            a.harness_id.as_str(),
        );
        let key_b = (
            b.harness_id == "terminal",
            order
                .iter()
                .position(|id| *id == b.harness_id)
                .unwrap_or(usize::MAX - 1),
            b.harness_id.as_str(),
        );
        key_a.cmp(&key_b)
    });
    providers
}

/// Tauri command — returns the derived Spawn Menu to the desktop / mobile
/// frontend (issue #575 / ADR-0016). Wraps `available_providers` in
/// `commands::run_blocking` so the menu derivation stays off the async
/// worker pool (issue #634). The result is what the desktop Spawn Modal
/// and the mobile provider picker render — the single source of truth
/// for "what can I spawn?".
#[command]
pub async fn list_providers() -> Vec<ProviderInfo> {
    match crate::commands::run_blocking("list_providers", || Ok(available_providers())).await {
        Ok(providers) => providers,
        Err(error) => {
            tracing::warn!("failed to derive provider menu: {error}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::ProxiedProviderOrder;

    #[test]
    fn available_providers_lists_only_harness_profiles_with_no_legacy_rows() {
        // Issue #538: the list is purely the dynamic harness profiles — no
        // hardcoded enum rows. In a bare test env (no detection) that's just the
        // code-defined Terminal default, present exactly once (no duplicate
        // legacy Terminal).
        let providers = available_providers();
        let terminals: Vec<_> = providers.iter().filter(|p| p.id == "terminal").collect();
        assert_eq!(
            terminals.len(),
            1,
            "expected exactly one (profile-sourced) Terminal row, got {}",
            terminals.len()
        );
        assert_eq!(terminals[0].label, "Terminal");
        // The retired legacy-only enum rows (e.g. bare "anthropic") must NOT
        // appear without a matching harness profile.
        assert!(
            !providers.iter().any(|p| p.id == "anthropic"),
            "legacy enum rows must not be listed once the profile list is the sole source"
        );
    }

    /// Capability-contract fixture used by both `row_native` and
    /// `row_proxied`: every bool is `false`, every list is empty, effort
    /// control is `None`. The Spawn-Menu ordering tests don't depend on
    /// any capability flag, so an "all false" descriptor is the most
    /// honest fixture (an accidental `true` would silently bias a future
    /// test). One helper, two callers — keeps the test contract pinned.
    fn caps_all_false(id: &str) -> crate::agent::capabilities::HarnessCapabilities {
        crate::agent::capabilities::HarnessCapabilities {
            harness_id: id.to_string(),
            supports_resume: false,
            supports_extra_args: false,
            auto_resume_on_startup: false,
            requires_attention_hook: false,
            produces_readable_transcript: false,
            supports_model_override: false,
            supports_effort_override: false,
            supports_prefill: false,
            is_plain_terminal: false,
            effort_control: crate::agent::capabilities::EffortControlKind::None,
            available_on: Vec::new(),
        }
    }

    /// Native Spawn Option fixture for `order_providers` tests (issue #583
    /// cleanup — replaces four inline `|id| ProviderInfo { ... }` closures
    /// with one helper). A native row is the clickable harness header:
    /// `harness_id` mirrors the row id, no `provider_id`, `group_key`
    /// follows the harness (issue #575 / ADR-0016 §6).
    fn row_native(id: &str) -> ProviderInfo {
        ProviderInfo {
            id: id.to_string(),
            label: id.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            harness_id: id.to_string(),
            provider_id: None,
            is_proxied: false,
            group_key: id.to_string(),
            capabilities: caps_all_false(id),
        }
    }

    /// Issue #534: Terminal is the least-common pick, so it must sort to the
    /// bottom of the provider menu while every real harness keeps its relative
    /// order. `order_providers` is the pure seam (no disk / globals) so the
    /// ordering can be pinned without driving `harness_profiles()`.
    #[test]
    fn order_providers_sorts_terminal_to_the_bottom() {
        // With no stored order, the two real harnesses keep their input order
        // and Terminal sorts last.
        let ordered = order_providers(
            vec![row_native("terminal"), row_native("claude"), row_native("codex")],
            &[],
        );
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "codex", "terminal"]);
    }

    /// Issue #573: the stored harness order drives the row order, Terminal still
    /// pinned last even if it appears mid-list in the stored order.
    #[test]
    fn order_providers_applies_stored_order() {
        let order = vec!["codex".to_string(), "terminal".to_string(), "claude".to_string()];
        let ordered = order_providers(
            vec![row_native("claude"), row_native("terminal"), row_native("codex")],
            &order,
        );
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        // codex before claude per the stored order; terminal forced last
        // despite sitting in the middle of `order`.
        assert_eq!(ids, vec!["codex", "claude", "terminal"]);
    }

    /// Issue #573 AC: a newly-detected harness (not yet in the stored order)
    /// appends at the end of the real harnesses, above Terminal.
    #[test]
    fn order_providers_new_harness_appends_above_terminal() {
        let order = vec!["claude".to_string(), "codex".to_string()];
        // "newbie" was just detected and isn't in the saved order.
        let ordered = order_providers(
            vec![
                row_native("terminal"),
                row_native("newbie"),
                row_native("codex"),
                row_native("claude"),
            ],
            &order,
        );
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "codex", "newbie", "terminal"]);
    }

    /// Issue #581: multiple *newcomers* (harnesses not yet in the stored
    /// order) all share `rank = usize::MAX - 1`. Without a tiebreak the
    /// relative order between them depends on the input order from
    /// `harness_profiles()` — which is deterministic today but is a
    /// hidden coupling. The `harness_id` tiebreak pins them into a
    /// deterministic alphabetical order, independent of how the upstream
    /// row derivation is implemented.
    #[test]
    fn order_providers_multiple_newcomers_sort_alphabetically() {
        // No stored order — every real harness is a newcomer.
        let order: Vec<String> = vec![];
        // The input is in *detection* order (claude, codex, agy, opencode),
        // NOT alphabetical. The assertion pins the alphabetical tiebreak,
        // not the input order.
        let ordered = order_providers(
            vec![
                row_native("terminal"),
                row_native("claude"),
                row_native("codex"),
                row_native("agy"),
                row_native("opencode"),
            ],
            &order,
        );
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["agy", "claude", "codex", "opencode", "terminal"],
            "newcomers must sort alphabetically by harness_id, not by input order"
        );
    }

    /// Issue #573 AC: an uninstalled harness keeps its saved slot — when it
    /// reappears among the rows it lands back in its stored position rather than
    /// being appended.
    #[test]
    fn order_providers_uninstalled_keeps_slot() {
        // Saved order had minimax between claude and codex; it was uninstalled
        // (absent from rows) for a while, now it's back.
        let order = vec![
            "claude".to_string(),
            "minimax".to_string(),
            "codex".to_string(),
        ];
        let ordered = order_providers(
            vec![
                row_native("codex"),
                row_native("claude"),
                row_native("minimax"),
                row_native("terminal"),
            ],
            &order,
        );
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "minimax", "codex", "terminal"]);
    }

    // ----- Spawn Option wire shape (issue #575 / ADR-0016) ---------------
    //
    // `compose_provider_menu` is the pure seam for the Spawn Menu derivation.
    // The grouped render (harness header + Proxied children) needs three
    // things to be true:
    //
    // 1. Each row carries `harness_id`, `provider_id`, `is_proxied`, and
    //    `group_key` (the frontend `groupBy(opt => opt.group_key)`).
    // 2. `order_providers` ranks by `harness_id` so a Proxied child
    //    clusters under its native harness header, not at the bottom of
    //    the stored order.
    // 3. `parse_spawn_option_id` splits a composite id on the first `:`
    //    so the resolver chain can pick the executor from the harness
    //    part and the credentials from the provider part.

    /// The native row for a harness profile has `provider_id = None`,
    /// `is_proxied = false`, and `group_key == harness_id == id`. A
    /// detected Claude Code profile is the canonical "clickable harness
    /// header" row.
    #[test]
    fn provider_info_for_marks_native_row_with_no_provider_id() {
        let claude = crate::preferences::HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "anthropic".to_string(),
        };
        let info = provider_info_for(&claude, Platform::Windows)
            .expect("claude profile is available on Windows");
        assert_eq!(info.id, "claude");
        assert_eq!(info.harness_id, "claude");
        assert!(info.provider_id.is_none(), "native row must have no provider_id");
        assert!(!info.is_proxied);
        assert_eq!(info.group_key, "claude");
    }

    /// A pairing surfaces as a Proxied Provider row with the composite id
    /// `<harness>:<provider>`, and `harness_id` / `group_key` follow the
    /// pairing's harness. The frontend uses these to bucket the row under the
    /// right harness header.
    #[test]
    fn provider_info_for_pairing_marks_proxied_row_with_composite_id() {
        let profiles = vec![crate::preferences::HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "anthropic".to_string(),
        }];
        let mm = crate::preferences::ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-mm".to_string()),
        };
        let pairing = crate::preferences::ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "minimax".to_string(),
            surface: crate::preferences::ApiSurface::Anthropic,
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            model_tiers: crate::preferences::ModelTiers::default(),
        };
        let info = provider_info_for_pairing(&pairing, &mm, &profiles, Platform::Windows)
            .expect("claude executor is available on Windows");
        assert_eq!(info.id, "claude:minimax");
        assert_eq!(info.harness_id, "claude");
        assert_eq!(info.provider_id.as_deref(), Some("minimax"));
        assert!(info.is_proxied);
        assert_eq!(info.group_key, "claude");
    }

    /// A second pairing of the same provider under a different harness/surface
    /// (MiniMax via Codex over OpenAI) yields a distinct composite id grouped
    /// under the Codex header — the multi-harness attach the issue is about
    /// (AC#1). The executor resolves from the codex profile, so the row is
    /// resumable (Codex supports resume and its rollout transcript is
    /// readable since #887).
    #[test]
    fn provider_info_for_pairing_supports_a_second_harness() {
        let profiles = vec![
            crate::preferences::HarnessProfile {
                id: "claude".to_string(),
                name: "Claude Code".to_string(),
                harness: "anthropic".to_string(),
            },
            crate::preferences::HarnessProfile {
                id: "codex".to_string(),
                name: "OpenAI Codex".to_string(),
                harness: "codex".to_string(),
            },
        ];
        let mm = crate::preferences::ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-mm".to_string()),
        };
        let pairing = crate::preferences::ProviderPairing {
            harness_id: "codex".to_string(),
            provider_id: "minimax".to_string(),
            surface: crate::preferences::ApiSurface::OpenAI,
            base_url: Some("https://api.minimax.io/v1".to_string()),
            model_tiers: crate::preferences::ModelTiers::default(),
        };
        let info = provider_info_for_pairing(&pairing, &mm, &profiles, Platform::Windows).unwrap();
        assert_eq!(info.id, "codex:minimax");
        assert_eq!(info.harness_id, "codex");
        assert_eq!(info.group_key, "codex");
        assert!(info.is_proxied);
        assert!(
            info.resumable,
            "Codex resumes and its rollout transcript is readable (#887)"
        );
    }

    /// A pairing whose harness has no detected profile falls back to parsing the
    /// `harness_id` directly. The resolver still maps a bare `"claude"` to the
    /// Anthropic executor, so the grouped render stays correct.
    #[test]
    fn provider_info_for_pairing_falls_back_to_parsing_harness_id() {
        let profiles: Vec<crate::preferences::HarnessProfile> = vec![];
        let mm = crate::preferences::ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-minimax".to_string()),
        };
        let pairing = crate::preferences::ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "minimax".to_string(),
            surface: crate::preferences::ApiSurface::Anthropic,
            base_url: None,
            model_tiers: crate::preferences::ModelTiers::default(),
        };
        let info = provider_info_for_pairing(&pairing, &mm, &profiles, Platform::Windows).unwrap();
        assert_eq!(info.harness_id, "claude");
        assert_eq!(info.group_key, "claude");
        assert_eq!(info.id, "claude:minimax");
    }

    /// Proxied Spawn Option fixture paired with `row_native` (issue #583
    /// cleanup — replaces an inline `|harness_id, provider_id|` closure with
    /// a named helper). A Proxied row carries the composite id
    /// `<harness>:<provider>` but `harness_id` / `group_key` follow the
    /// harness so the stable sort clusters the child under its native header.
    fn row_proxied(harness_id: &str, provider_id: &str) -> ProviderInfo {
        ProviderInfo {
            id: format!("{}:{}", harness_id, provider_id),
            label: provider_id.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            harness_id: harness_id.to_string(),
            provider_id: Some(provider_id.to_string()),
            is_proxied: true,
            group_key: harness_id.to_string(),
            capabilities: caps_all_false(harness_id),
        }
    }

    /// A Proxied Provider row clusters under its harness header
    /// (`harness_id` rank), not under its composite `id` (which isn't in
    /// the stored order). A naive `position(p.id)` would push the child
    /// to `usize::MAX - 1`; ranking by `harness_id` keeps it next to
    /// its native header so the frontend's `groupBy` groups them
    /// together.
    #[test]
    fn order_providers_ranks_proxied_rows_by_harness_id_not_composite_id() {
        let order = vec!["claude".to_string(), "codex".to_string()];
        let rows = vec![
            row_proxied("claude", "minimax"),
            row_native("terminal"),
            row_native("codex"),
            row_native("claude"),
        ];
        let ordered = order_providers(rows, &order);
        // All four rows have the same `harness_id` group ("claude",
        // "codex", or "terminal"), so the rank sort is by harness_id:
        // rank 0 = claude, rank 1 = codex, terminal = usize::MAX
        // (last). Within the same rank, the stable sort preserves the
        // input order, so the Proxied child (first input row, rank 0)
        // stays ahead of the native claude row. The frontend's
        // `groupBy(group_key)` then buckets them into the same
        // "Claude Code" group regardless of which row is first.
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["claude:minimax", "claude", "codex", "terminal"],
            "Proxied child must cluster under its harness header (stable sort keeps equal-rank input order)"
        );
    }

    // ----- order_proxied_children (issue #577) ------------------------
    //
    // Within each harness bucket, the user-chosen order of the **Proxied
    // Provider** children is applied AFTER `order_providers` so the harness-
    // level rank is untouched. Native harness headers (always the first
    // row in their bucket) are not orderable — only proxied children.
    // `order_proxied_children` is the pure seam so the per-harness sort
    // can be pinned without touching disk / globals.

    /// Empty / unset `proxied_order` is a no-op — natural input order wins.
    #[test]
    fn order_proxied_children_keeps_natural_order_when_unset() {
        let rows = vec![
            row_native("claude"),
            row_proxied("claude", "minimax"),
            row_proxied("claude", "kimi"),
        ];
        let ordered = order_proxied_children(rows.clone(), &[]);
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["claude", "claude:minimax", "claude:kimi"],
            "no stored order → preserve input order"
        );
    }

    /// The stored per-harness order is applied to children of that harness.
    /// Children present in the bucket but absent from the stored order
    /// append in their natural (input) order at the end (stable sort).
    #[test]
    fn order_proxied_children_applies_per_harness_order() {
        let rows = vec![
            row_native("claude"),
            row_proxied("claude", "minimax"),
            row_proxied("claude", "kimi"),
            row_proxied("claude", "openrouter"),
        ];
        let order = vec![ProxiedProviderOrder {
            harness_id: "claude".into(),
            provider_ids: vec!["kimi".into(), "openrouter".into(), "minimax".into()],
        }];
        let ordered = order_proxied_children(rows, &order);
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        // Native header first, then children in stored order.
        assert_eq!(ids, vec!["claude", "claude:kimi", "claude:openrouter", "claude:minimax"]);
    }

    /// A stored order applies ONLY to the children of the named harness.
    /// Other harnesses' children keep their natural order — the per-harness
    /// scoping is the entire point (cross-harness drag is disallowed by
    /// the UI; the backend enforces the same scope).
    #[test]
    fn order_proxied_children_is_scoped_per_harness() {
        let rows = vec![
            row_native("claude"),
            row_proxied("claude", "minimax"),
            row_proxied("claude", "kimi"),
            row_native("codex"),
            row_proxied("codex", "minimax"),
            row_proxied("codex", "kimi"),
        ];
        // Reorder Claude children only; Codex untouched.
        let order = vec![ProxiedProviderOrder {
            harness_id: "claude".into(),
            provider_ids: vec!["kimi".into(), "minimax".into()],
        }];
        let ordered = order_proxied_children(rows, &order);
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "claude",
                "claude:kimi",
                "claude:minimax",
                "codex",
                "codex:minimax",
                "codex:kimi",
            ],
            "Claude children reorder; Codex children keep natural order"
        );
    }

    /// Children that aren't in the stored order (newly attached, or stored
    /// on a different harness) append at the end of their bucket. The
    /// stable sort keeps their relative natural order intact.
    #[test]
    fn order_proxied_children_appends_unknown_ids_in_natural_order() {
        let rows = vec![
            row_native("claude"),
            row_proxied("claude", "minimax"),
            row_proxied("claude", "kimi"),
            row_proxied("claude", "deepseek"),
        ];
        let order = vec![ProxiedProviderOrder {
            harness_id: "claude".into(),
            provider_ids: vec!["kimi".into()],
        }];
        let ordered = order_proxied_children(rows, &order);
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        // kimi first (listed), then minimax and deepseek in input order.
        assert_eq!(ids, vec!["claude", "claude:kimi", "claude:minimax", "claude:deepseek"]);
    }

    /// Native harness rows are never reordered — they're not proxied
    /// children. A spurious entry in `proxied_order` that names a native
    /// id is silently ignored.
    #[test]
    fn order_proxied_children_does_not_move_native_harness_header() {
        // The natural input from `compose_provider_menu`: native first, then
        // children. A stored order that puts the native header second is
        // impossible to honor — the native row stays first; the proxied
        // children sort by the stored order within the bucket.
        let rows = vec![
            row_native("claude"),
            row_proxied("claude", "minimax"),
            row_proxied("claude", "kimi"),
        ];
        let order = vec![ProxiedProviderOrder {
            harness_id: "claude".into(),
            provider_ids: vec!["kimi".into(), "minimax".into()],
        }];
        let ordered = order_proxied_children(rows, &order);
        let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
        // Native header stays first; children sorted by stored order.
        assert_eq!(ids, vec!["claude", "claude:kimi", "claude:minimax"]);
    }

    /// End-to-end: `compose_provider_menu` runs `order_proxied_children`
    /// after `order_providers`, so the Spawn Menu applies the per-harness
    /// child order on top of the harness-level order. The native harness
    /// header is always first inside its bucket (the renderer puts the
    /// header above its children), and Terminal stays pinned last.
    #[test]
    fn compose_provider_menu_propagates_proxied_order() {
        let menu = compose_provider_menu(
            vec![profile("claude", "anthropic"), profile("terminal", "terminal")],
            vec![
                acct("minimax", true, Some("sk-mm")),
                // `"moonshot"` stands in for the (no-longer-first-class) Kimi
                // Moonshot LLM endpoint account; users who want Claude Code
                // pointed at Moonshot now create a custom Claude-compatible
                // account under a non-reserved id (#918 — the reserved `"kimi"`
                // id is the native Kimi Code harness, self_auth only).
                acct("moonshot", true, Some("sk-moon")),
            ],
            // ADR-0025: menu rows come from stored pairings only.
            vec![claude_pairing("minimax"), claude_pairing("moonshot")],
            Platform::Windows,
            &[],
            // User dragged Moonshot above MiniMax under Claude.
            &[ProxiedProviderOrder {
                harness_id: "claude".into(),
                provider_ids: vec!["moonshot".into(), "minimax".into()],
            }],
        );
        let ids: Vec<_> = menu.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["claude", "claude:moonshot", "claude:minimax", "terminal"],
            "native header first inside bucket; children in stored order; Terminal still pinned last",
        );
    }

    // ----- parse_spawn_option_id resolver (issue #575) ------------------
    //
    // The composite id format `<harness>` (native) or `<harness>:<provider>`
    // (proxied) is split on the first `:` by the resolver chain
    // (`preferences::resolve_harness_provider` and
    // `preferences::resolve_provider_env`). A provider id containing `:`
    // (a theoretical edge case — the id is user-chosen) is preserved
    // intact on the right side.

    #[test]
    fn parse_spawn_option_id_splits_bare_into_native() {
        let (harness, provider) =
            crate::agent::provider::parse_spawn_option_id("claude");
        assert_eq!(harness, "claude");
        assert!(provider.is_none());
    }

    #[test]
    fn parse_spawn_option_id_splits_composite_into_harness_and_provider() {
        let (harness, provider) =
            crate::agent::provider::parse_spawn_option_id("claude:minimax");
        assert_eq!(harness, "claude");
        assert_eq!(provider, Some("minimax"));
    }

    #[test]
    fn parse_spawn_option_id_keeps_provider_part_intact_on_duplicate_colons() {
        // A provider id with its own `:` (theoretical today, but the
        // id is user-chosen so we can't rule it out) lands entirely in
        // the provider slot. The first `:` is the split, not the last.
        let (harness, provider) =
            crate::agent::provider::parse_spawn_option_id("claude:weird:id");
        assert_eq!(harness, "claude");
        assert_eq!(provider, Some("weird:id"));
    }

    /// The resolver chain (`resolve_harness_provider`) splits a composite
    /// id on the first `:` and uses only the harness part to pick the
    /// executor. `claude:minimax` resolves to the Anthropic executor
    /// (Claude Code), not to a nonexistent `claude:minimax` Provider.
    /// The composite-id path is exercised through
    /// `Provider::from_db_str` in `models::tests`; the split logic
    /// itself is the `parse_spawn_option_id` test above. Here we pin
    /// the post-#538 legacy fallback for a bare `minimax` id so a
    /// pre-migration archived node still resolves correctly.
    #[test]
    fn resolve_harness_provider_legacy_minimax_id_falls_through_to_anthropic() {
        // `Provider::from_db_str("minimax")` is a static lookup —
        // doesn't touch the preferences cache or APP_DATA_DIR. So we
        // can verify the legacy fallback (issue #538 cutover) here
        // without driving the temp-dir helper.
        //
        // `"kimi"` USED to fall through here too — Kimi Code (wayfinder
        // #918) is now a first-class native executor, so it resolves to
        // `Provider::Kimi` directly. The dedicated test
        // `resolve_provider_env_kimi_id_resolves_to_native_harness` in
        // models::tests pins that path.
        use crate::models::Provider;
        assert_eq!(Provider::from_db_str("minimax"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str("kimi"), Provider::Kimi);
    }

    // ----- v19 migration (issue #575) -----------------------------------
    //
    // The `migrate_agent_node_provider_id_to_composite` rewrite
    // (src-tauri/src/db/mod.rs) is exercised by the integration tests
    // in `db::migration_tests`. The unit tests below pin the pure
    // helpers (id format + the first-class/custom id classification
    // rule) so a refactor that drops a category surfaces here.

    /// The legacy `minimax` / `kimi` ids are always Proxied Provider
    /// rows — they're the two first-class built-ins (issue #566) and
    /// always pair with Claude Code. The migration always rewrites
    /// them. (Pin the static list so adding a new first-class provider
    /// in the future requires a paired test update.)
    #[test]
    fn first_class_legacy_ids_are_known_proxied() {
        // The migration's first block rewrites exactly this single id.
        // ("kimi" USED to be in this list — Kimi Code is now a native
        // self-auth harness (wayfinder #918), so `is_claude_compatible_id`
        // returns false and the migration leaves its nodes alone.)
        let id = "minimax";
        assert!(
            crate::preferences::is_claude_compatible_id(id),
            "{id} must be classified as claude_compatible so the migration picks it up"
        );
    }

    /// A user-typed custom account id (e.g. "deepseek") is also
    /// Proxied — the migration's second block catches it as long as
    /// the live `provider_accounts()` read surfaces it as
    /// `claude_compatible`. A disabled or non-claude_compatible
    /// account is left alone (its nodes fall through to the
    /// Anthropic default at spawn time, which is the legacy
    /// behaviour).
    #[test]
    fn custom_claude_compatible_accounts_are_known_proxied() {
        // `is_claude_compatible_id` is the public classification — a
        // custom id is Proxied iff it isn't in the self-auth set.
        assert!(crate::preferences::is_claude_compatible_id("deepseek"));
        assert!(!crate::preferences::is_claude_compatible_id("anthropic"));
        assert!(!crate::preferences::is_claude_compatible_id("codex"));
    }

    // ----- compose_provider_menu (issue #568) ----------------------------
    //
    // The spawn menu is derived: harness profiles + configured Claude-compatible
    // accounts. `compose_provider_menu` is the pure seam so account-inclusion can
    // be pinned without driving `provider_accounts()` (disk + globals).

    fn profile(id: &str, harness: &str) -> crate::preferences::HarnessProfile {
        crate::preferences::HarnessProfile {
            id: id.to_string(),
            name: id.to_string(),
            harness: harness.to_string(),
        }
    }

    fn acct(id: &str, enabled: bool, key: Option<&str>) -> crate::preferences::ProviderAccount {
        crate::preferences::ProviderAccount {
            id: id.to_string(),
            name: format!("{id} acct"),
            enabled,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: crate::preferences::is_claude_compatible_id(id),
            api_key: key.map(str::to_string),
        }
    }

    fn claude_pairing(provider_id: &str) -> crate::preferences::ProviderPairing {
        crate::preferences::ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: provider_id.to_string(),
            surface: crate::preferences::ApiSurface::Anthropic,
            base_url: Some("https://api.example.com/anthropic".to_string()),
            model_tiers: crate::preferences::ModelTiers {
                default: Some("model-a".to_string()),
                ..crate::preferences::ModelTiers::default()
            },
        }
    }

    #[test]
    fn compose_menu_adds_enabled_keyed_claude_compatible_accounts() {
        // ADR-0025: a keyed account surfaces as a Proxied Provider row only
        // when a stored pairing exists (no auto-derived default on key alone).
        let menu = compose_provider_menu(
            vec![profile("claude", "anthropic"), profile("terminal", "terminal")],
            vec![
                acct("minimax", true, Some("sk-mm")),
                acct("moonshot", true, Some("sk-moon")),
            ],
            vec![claude_pairing("minimax"), claude_pairing("moonshot")],
            Platform::Windows,
            &[],
            // No stored per-harness child order — natural insertion order applies.
            &[],
        );
        let ids: Vec<_> = menu.iter().map(|p| p.id.as_str()).collect();
        // Composite ids — resolver splits on ':' to get (executor, creds).
        assert!(
            ids.contains(&"claude:minimax"),
            "keyed MiniMax must appear in the menu as `claude:minimax` (Proxied Provider), got {ids:?}"
        );
        assert!(
            ids.contains(&"claude:moonshot"),
            "keyed Moonshot (Kimi LLM via Claude Code, custom id post-#918) must appear in the menu as `claude:moonshot`, got {ids:?}"
        );
        // The MiniMax row carries its own composite id (and brand label) so
        // the frontend renders the brand icon keyed off the provider half.
        let mm = menu.iter().find(|p| p.id == "claude:minimax").unwrap();
        assert_eq!(mm.label, "minimax acct");
        assert_eq!(mm.harness_id, "claude");
        assert_eq!(mm.provider_id.as_deref(), Some("minimax"));
        assert!(mm.is_proxied);
        assert_eq!(mm.group_key, "claude");
        // Terminal still sorts last.
        assert_eq!(ids.last(), Some(&"terminal"));
    }

    #[test]
    fn compose_menu_excludes_unconfigured_or_disabled_accounts() {
        let menu = compose_provider_menu(
            vec![profile("terminal", "terminal")],
            vec![
                acct("minimax", true, None),          // enabled but no key
                acct("kimi", false, Some("sk-moon")), // keyed but disabled
                acct("anthropic", true, Some("x")),   // self-auth → not claude_compatible
            ],
            vec![],
            Platform::Windows,
            &[],
            &[],
        );
        let ids: Vec<_> = menu.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["terminal"], "none of these should reach the menu");
    }

    #[test]
    fn compose_menu_does_not_duplicate_a_detected_profile() {
        // A detected "claude" harness profile and a (hypothetical) same-id account
        // must not both appear — the profile wins, no duplicate row.
        let menu = compose_provider_menu(
            vec![profile("claude", "anthropic")],
            vec![acct("claude", true, Some("k"))],
            vec![],
            Platform::Windows,
            &[],
            &[],
        );
        assert_eq!(menu.iter().filter(|p| p.id == "claude").count(), 1);
    }

    // -----------------------------------------------------------------------
    // ProviderInfo.resumable flag (issue #550 follow-up)
    //
    // The frontend's archived-node resume picker used to filter providers via
    // a hardcoded `['anthropic','minimax','kimi']` allow-list, which silently
    // hid custom Claude-compatible harness profiles (e.g. "DeepSeek via
    // Claude") from the picker. The fix is a backend-supplied `resumable` flag
    // on ProviderInfo, derived from the resolved adapter's
    // `supports_resume() && produces_readable_transcript()` so every dynamic
    // Claude-compatible profile (which all share the `anthropic` executor,
    // see `preferences::resolve_harness_provider`) advertises itself correctly.
    // -----------------------------------------------------------------------

    /// Pin the negative case for `resumable`: Terminal is always present
    /// (code-defined default), and a plain shell neither supports resume nor
    /// produces a readable transcript, so the flag must be `false`. This
    /// test runs in the bare-test env (no `with_temp_dir`), so the only row
    /// `available_providers()` sees is Terminal.
    #[test]
    fn available_providers_marks_terminal_as_not_resumable() {
        let providers = available_providers();
        let terminal = providers
            .iter()
            .find(|p| p.id == "terminal")
            .expect("Terminal profile always present");
        assert!(
            !terminal.resumable,
            "Terminal is a plain shell — resumable must be false"
        );
    }

    /// Pin the positive case: a stored harness profile whose backing executor
    /// is the Claude-backed `anthropic` adapter must advertise `resumable=true`
    /// so it shows up in the archived-node resume picker. Mirrors the
    /// "DeepSeek via Claude" / "Kimi via Claude" pattern from issue #537 —
    /// any id, any user-chosen name; the resumability is purely a property of
    /// the resolved adapter, not the stored id.
    ///
    /// Tested against `provider_info_for` (the pure helper) rather than
    /// `available_providers` so this test doesn't drive the preferences
    /// module's `APP_DATA_DIR` / `CACHE` globals — which are shared across
    /// the `preferences::tests` module's `with_temp_dir` and would race.
    #[test]
    fn provider_info_marks_claude_backed_profile_as_resumable() {
        use crate::preferences::HarnessProfile;
        let deepseek = HarnessProfile {
            // Custom id + user-chosen name — the exact pattern that the old
            // allow-list silently filtered out.
            id: "deepseek-via-claude".to_string(),
            name: "DeepSeek (via Claude)".to_string(),
            harness: "anthropic".to_string(),
        };
        let info = provider_info_for(&deepseek, Platform::Windows)
            .expect("anthropic-backed profile is available on Windows");
        assert!(
            info.resumable,
            "claude-backed profile must be resumable=true so it shows up in the resume picker"
        );
    }

    #[test]
    fn provider_info_marks_cursor_profile_as_resumable() {
        use crate::preferences::HarnessProfile;
        let cursor = HarnessProfile {
            id: "cursor".to_string(),
            name: "Cursor Agent".to_string(),
            harness: "cursor".to_string(),
        };
        let info = provider_info_for(&cursor, Platform::Windows)
            .expect("Cursor is available on Windows");
        assert!(
            info.resumable,
            "Cursor's workspace JSONL transcript must enable archive resume"
        );
        assert!(info.capabilities.produces_readable_transcript);
    }

    /// Negative companion to the previous test: the `harness` field must
    /// actually drive the executor resolution. If a profile pins
    /// `harness: "opencode"`, `resumable` must be `false` even though the
    /// id is user-chosen and the test env has no stored profiles. This
    /// pins that `provider_info_for` consults `profile.harness` directly
    /// (via `Provider::from_db_str`) rather than silently falling through
    /// to Anthropic on the id.
    #[test]
    fn provider_info_consults_harness_field_not_id_fallback() {
        use crate::preferences::HarnessProfile;
        let custom_opencode = HarnessProfile {
            // Custom id, but `harness: "opencode"` — must NOT collapse to
            // Anthropic. (The previous version of `provider_info_for`
            // resolved via `resolve_harness_provider(&profile.id)`, whose
            // fallback path returned Anthropic for unknown ids and would
            // have silently made this test pass for the wrong reason.)
            id: "custom-opencode-flavor".to_string(),
            name: "Custom OpenCode".to_string(),
            harness: "opencode".to_string(),
        };
        let info = provider_info_for(&custom_opencode, Platform::Windows)
            .expect("OpenCode-backed profile is available on Windows");
        assert!(
            !info.resumable,
            "OpenCode-backed profile must be resumable=false; the harness field drives resolution"
        );
    }

    /// Pin that the same pure helper marks a non-resumable profile correctly.
    /// `Terminal`'s adapter (`is_plain_terminal`) returns false for
    /// `produces_readable_transcript` and false for `supports_resume`, so
    /// the derived flag must be false regardless of host.
    #[test]
    fn provider_info_marks_terminal_as_not_resumable() {
        use crate::preferences::HarnessProfile;
        let terminal = HarnessProfile {
            id: "terminal".to_string(),
            name: "Terminal".to_string(),
            harness: "terminal".to_string(),
        };
        let info = provider_info_for(&terminal, Platform::Windows)
            .expect("Terminal is available on Windows");
        assert!(
            !info.resumable,
            "Terminal is plain shell — resumable must be false"
        );
    }

    /// Pin the legacy-id contract: the `minimax`/`kimi` ids that archived
    /// nodes still carry on disk resolve to the Anthropic executor (per
    /// `Provider::from_db_str`) and therefore advertise `resumable=true`.
    /// This is the regression case the frontend's hardcoded
    /// `['anthropic','minimax','kimi']` allow-list used to encode as a
    /// stringly-typed list — now it's a single derivation rule.
    #[test]
    fn provider_info_marks_legacy_minimax_id_as_resumable() {
        // Pin the post-#538 cutover: bare `minimax` falls through to the
        // Anthropic executor (Claude-Code-backed, resumable). `kimi` USED
        // to fall through here too — wayfinder #918 promoted it to a
        // first-class native executor (`Provider::Kimi`). Kimi Code's
        // `resumable` flag depends on `supports_resume() &&
        // produces_readable_transcript()`; the reader wiring for
        // `~/.kimi/sessions/wire.jsonl` is a follow-up, so Kimi is
        // currently NOT marked resumable until the reader ships.
        use crate::preferences::HarnessProfile;
        let profile = HarnessProfile {
            id: "minimax".to_string(),
            name: "Minimax".to_string(),
            harness: "minimax".to_string(),
        };
        let info = provider_info_for(&profile, Platform::Windows)
            .expect("minimax resolves to Anthropic and must be available on Windows");
        assert!(
            info.resumable,
            "legacy minimax id resolves to Anthropic (issue #538) and must be resumable"
        );
    }
}
