//! Tests for `agent::provider_menu`.
//!
//! Split out of `src-tauri/src/agent/provider_menu.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test agent::provider_menu::tests

use super::*;
use crate::preferences::ProxiedProviderOrder;

/// Issue #2151: the pure adapter-level core resolves the stored mode
/// when valid, falls back to the unattended default when absent,
/// blank, or stale — and reports `None` for harnesses with no modes.
#[test]
fn effective_permission_resolves_default_stored_and_stale() {
    use crate::agent::capabilities::{PERMISSION_MODE_PROMPT, PERMISSION_MODE_UNATTENDED};
    use crate::agent::provider::adapters::{ANTHROPIC, OPENCODE, TERMINAL};

    // No stored value: the harness unattended default, marked default.
    let mode = effective_permission_for_adapter(&ANTHROPIC, None).expect("claude has modes");
    assert_eq!(mode.mode_id, PERMISSION_MODE_UNATTENDED);
    assert!(mode.label.contains("--dangerously-skip-permissions"));
    assert!(mode.is_default);

    // Stored prompt: honored, marked custom.
    let mode = effective_permission_for_adapter(&ANTHROPIC, Some("prompt")).expect("prompt stored");
    assert_eq!(mode.mode_id, PERMISSION_MODE_PROMPT);
    assert!(!mode.is_default);

    // Stale value (no longer a known mode): falls back to default.
    let mode =
        effective_permission_for_adapter(&ANTHROPIC, Some("turbo")).expect("stale falls back");
    assert_eq!(mode.mode_id, PERMISSION_MODE_UNATTENDED);
    assert!(mode.is_default);

    // Blank stored value: same as absent.
    let mode = effective_permission_for_adapter(&OPENCODE, Some("   ")).expect("blank falls back");
    assert_eq!(mode.mode_id, PERMISSION_MODE_UNATTENDED);
    assert!(mode.label.contains("--auto"));

    // Modeless harness: always None, even with a stored value.
    assert!(effective_permission_for_adapter(&TERMINAL, None).is_none());
    assert!(effective_permission_for_adapter(&TERMINAL, Some("prompt")).is_none());
}

/// Issue #2151 review round 1: the Spawn Menu wiring itself. Stamping
/// a prefs snapshot onto rows must carry the stored choice, the
/// harness default when nothing is stored, and `None` for a
/// modeless harness — including on a saved-configuration row, which
/// shares its harness's defaults entry.
#[test]
fn stamp_effective_permission_uses_row_capabilities_and_prefs_snapshot() {
    use crate::agent::capabilities::{PERMISSION_MODE_PROMPT, PERMISSION_MODE_UNATTENDED};
    use crate::agent::provider::adapters::{ANTHROPIC, TERMINAL};
    use crate::preferences::{AppPreferences, HarnessConfigValue};

    fn row(harness: &str) -> ProviderInfo {
        let adapter: &dyn AgentProvider = match harness {
            "claude" => &ANTHROPIC,
            _ => &TERMINAL,
        };
        ProviderInfo {
            id: harness.to_string(),
            label: harness.to_string(),
            color: String::new(),
            icon: String::new(),
            resumable: false,
            harness_id: harness.to_string(),
            provider_id: None,
            is_proxied: false,
            group_key: harness.to_string(),
            capabilities: adapter.capabilities(),
            runtime: None,
            configurations: Vec::new(),
            configuration: None,
            unavailable_reason: None,
            effective_permission: None,
        }
    }

    let mut prefs = AppPreferences::default();
    prefs.harness_defaults.insert(
        "claude".to_string(),
        HarnessConfigValue {
            model: None,
            effort: None,
            permission_mode: Some(PERMISSION_MODE_PROMPT.to_string()),
        },
    );

    // Saved-configuration rows share the harness entry: a row with a
    // configuration attached still stamps from `harness_defaults`.
    let mut config_row = row("claude");
    config_row.configuration = Some(
        crate::preferences::spawn_configurations::SpawnConfiguration {
            ..Default::default()
        },
    );

    let stamped = stamp_effective_permission(vec![config_row, row("terminal")], &prefs);
    let claude = stamped.iter().find(|r| r.harness_id == "claude").unwrap();
    let mode = claude
        .effective_permission
        .as_ref()
        .expect("claude row stamped");
    assert_eq!(mode.mode_id, PERMISSION_MODE_PROMPT);
    assert!(!mode.is_default);
    let terminal = stamped.iter().find(|r| r.harness_id == "terminal").unwrap();
    assert!(terminal.effective_permission.is_none());

    // Nothing stored: the harness unattended default, marked default.
    let stamped = stamp_effective_permission(vec![row("claude")], &AppPreferences::default());
    let mode = stamped[0]
        .effective_permission
        .as_ref()
        .expect("default stamped");
    assert_eq!(mode.mode_id, PERMISSION_MODE_UNATTENDED);
    assert!(mode.is_default);
}

#[test]
fn provider_ipc_reports_failed_reads_while_internal_discovery_keeps_its_fallback() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::create_dir(scratch.path().join("preferences.json")).unwrap();
    crate::preferences::init_for_tests(scratch.path().into());
    let error = list_providers_blocking().unwrap_err();
    assert!(
        error.starts_with("failed to read preferences.json:"),
        "{error}"
    );
    assert!(available_providers().iter().any(|row| row.id == "terminal"));
    crate::preferences::reset_for_tests();
}

#[test]
fn routing_catalog_preserves_specific_configuration_failures() {
    for failure in ["credential", "disabled", "model"] {
        let mut prefs = crate::preferences::AppPreferences {
            harness_profiles: vec![profile("codex", "codex")],
            ..Default::default()
        };
        let mut account = acct("minimax", true, Some("fixture"));
        let mut pairing = claude_pairing("minimax");
        pairing.harness_id = "codex".into();
        pairing.surface = crate::preferences::ApiSurface::OpenAI;
        pairing.model_tiers.default = Some("MiniMax-M3".into());
        match failure {
            "credential" => account.api_key = None,
            "disabled" => account.enabled = false,
            _ => pairing.model_tiers.default = None,
        }
        prefs.provider_accounts = vec![account.clone()];
        prefs.provider_pairings = vec![pairing];
        prefs.spawn_configurations.push(
            crate::preferences::spawn_configurations::SpawnConfiguration {
                id: "launch/audit".into(),
                name: "Audit".into(),
                spawn_option_id: "codex:minimax".into(),
                ..Default::default()
            },
        );
        let expected = crate::preferences::launch_configurations::resolve(
            &prefs,
            "launch/audit",
            &Default::default(),
        )
        .unwrap_err();
        let menu = routing_options(
            &prefs,
            prefs.harness_profiles.clone(),
            vec![account],
            Platform::Windows,
            None,
        );
        let row = menu.iter().find(|row| row.id == "launch/audit").unwrap();
        assert_eq!(
            row.unavailable_reason.as_deref(),
            Some(expected.as_str()),
            "{failure} remediation was replaced"
        );
        assert!(!expected.contains("pending"));
    }
}

#[test]
fn routing_catalog_matches_anthropic_launchability_without_probes() {
    let mut prefs = crate::preferences::AppPreferences::default();
    let mut invalid = claude_pairing("moonshot");
    invalid.model_tiers.default = None;
    prefs.provider_pairings = vec![claude_pairing("minimax"), invalid, claude_pairing("custom")];
    let mut blank = acct("custom", true, Some("   "));
    blank.claude_compatible = true;
    let menu = routing_options(
        &prefs,
        vec![profile("claude", "anthropic")],
        vec![
            acct("minimax", true, Some("fixture")),
            acct("moonshot", true, Some("fixture")),
            blank,
        ],
        Platform::Windows,
        None,
    );
    assert!(menu
        .iter()
        .any(|row| row.id == "claude:minimax" && row.unavailable_reason.is_none()));
    assert!(!menu.iter().any(|row| row.id == "claude:moonshot"));
    assert!(!menu.iter().any(|row| row.id == "claude:custom"));
}

#[test]
fn routing_catalog_has_native_capabilities_and_explains_unverified_routes() {
    let mut prefs = crate::preferences::AppPreferences::default();
    prefs
        .provider_pairings
        .push(crate::preferences::ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "custom".into(),
            surface: crate::preferences::ApiSurface::OpenAI,
            base_url: Some("https://example.test/v1".into()),
            model_tiers: crate::preferences::ModelTiers::default(),
        });
    let account = crate::preferences::ProviderAccount {
        id: "custom".into(),
        name: "Custom".into(),
        enabled: true,
        billing_mode: crate::preferences::BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: Some("fixture".into()),
    };
    let menu = routing_options(
        &prefs,
        vec![profile("codex", "codex"), profile("terminal", "terminal")],
        vec![account],
        Platform::Windows,
        None,
    );
    let native = menu.iter().find(|row| row.id == "codex").unwrap();
    assert!(native.unavailable_reason.is_none());
    assert!(native.capabilities.supports_resume);
    let route = menu.iter().find(|row| row.id == "codex:custom").unwrap();
    assert_eq!(
        route.unavailable_reason.as_deref(),
        Some("Runtime verification pending; retry provider checks if needed")
    );
    assert!(menu.iter().any(|row| row.id == "terminal"));
}

/// Issue #1934: `available_providers` must discover the two runtimes
/// concurrently. The runtime chains are independent, so serializing them
/// made a cold Settings -> Providers open pay their sum.
///
/// Each closure blocks until the other has entered, so both can only
/// return if they really were in flight together - a sleep-based margin
/// would pass on a slow machine. A serial regression strands the first
/// closure until the bound fires, so this fails rather than hangs.
///
/// Limit worth stating: this pins the overlap mechanism, not the wiring.
/// The bare test env configures no OpenAI-surface pairing, so
/// `needs_codex` is false and `available_providers` never reaches this
/// helper here; proving that call site routes through it would need a
/// paired OpenAI account on disk plus real `codex`/`wsl.exe` spawns. The
/// separate-runtime cache test in `codex` covers the other half of the
/// claim (the two never contend for one cache entry).
#[test]
fn both_codex_runtime_discoveries_are_issued_concurrently() {
    use crate::agent::provider::adapters::codex::CodexInstall;
    use std::sync::{Condvar, Mutex};

    /// Blocks until the sibling runtime has also entered, so both can only
    /// return if they really were in flight together.
    struct BothInside {
        arrived: Mutex<usize>,
        released: Condvar,
    }

    impl BothInside {
        fn wait(&self, entered: &str) {
            let bound = std::time::Duration::from_secs(2);
            let mut count = self.arrived.lock().unwrap_or_else(|p| p.into_inner());
            *count += 1;
            // Predicate first, then wait - the same shape as the codex
            // rendezvous. Checking before waiting is what lets the last
            // arriver return at once instead of blocking out the bound
            // nobody will notify it out of, and looping is what stops a
            // spurious wakeup from passing as "overlapped" before the
            // sibling arrived.
            loop {
                if *count >= 2 {
                    self.released.notify_all();
                    return;
                }
                let (guard, timeout) = self
                    .released
                    .wait_timeout(count, bound)
                    .unwrap_or_else(|p| p.into_inner());
                count = guard;
                if timeout.timed_out() && *count < 2 {
                    panic!(
                            "{entered} runtime never overlapped its sibling within {bound:?} - the derivations ran serially"
                        );
                }
            }
        }
    }

    let both_inside = BothInside {
        arrived: Mutex::new(0),
        released: Condvar::new(),
    };
    let install = |runtime: &str| {
        Some(CodexInstall {
            executable: format!("/usr/bin/{runtime}"),
            version: "0.158.0".to_string(),
            runtime_identity: runtime.to_string(),
            codex_home: format!("/home/dev/.{runtime}"),
            wsl_distro: None,
        })
    };

    let (native, foreign) = discover_codex_runtimes_concurrently(
        || {
            both_inside.wait("native");
            install("codex-native")
        },
        || {
            both_inside.wait("foreign");
            install("codex-foreign")
        },
    );

    assert_eq!(
        native.map(|i| i.runtime_identity).as_deref(),
        Some("codex-native")
    );
    assert_eq!(
        foreign.map(|i| i.runtime_identity).as_deref(),
        Some("codex-foreign")
    );
}

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
        attention_capability: crate::agent::capabilities::AttentionCapability::None,
        background_inference: None,
        supports_passive_turn_watcher: false,
        produces_readable_transcript: false,
        supports_model_override: false,
        supports_effort_override: false,
        supports_prefill: false,
        is_plain_terminal: false,
        effort_control: crate::agent::capabilities::EffortControlKind::None,
        permission_modes: Vec::new(),
        default_permission_mode: None,
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
        runtime: None,
        configurations: Vec::new(),
        configuration: None,
        unavailable_reason: None,
        effective_permission: None,
    }
}

#[test]
fn launch_configurations_keep_harness_parents_for_spawn_submenus() {
    let prefs = crate::preferences::AppPreferences {
        spawn_configurations: vec![
            crate::preferences::spawn_configurations::SpawnConfiguration {
                id: "launch/codex-sol".into(),
                name: "Sol".into(),
                spawn_option_id: "codex".into(),
                ..Default::default()
            },
            crate::preferences::spawn_configurations::SpawnConfiguration {
                id: "launch/claude:minimax".into(),
                name: "MiniMax".into(),
                spawn_option_id: "claude:minimax".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let menu = configuration_menu(
        vec![
            row_native("claude"),
            row_proxied("claude", "minimax"),
            row_native("codex"),
        ],
        &prefs,
        Platform::Windows,
    );
    let ids: Vec<_> = menu.iter().map(|row| row.id.as_str()).collect();
    assert!(
        ids.contains(&"claude"),
        "Claude Code must remain a submenu parent: {ids:?}"
    );
    assert!(
        ids.contains(&"codex"),
        "Codex must remain a submenu parent: {ids:?}"
    );
    assert!(ids.contains(&"launch/codex-sol"));
    assert!(ids.contains(&"launch/claude:minimax"));

    let unavailable = configuration_menu(Vec::new(), &prefs, Platform::Windows);
    let codex = unavailable
        .iter()
        .find(|row| row.id == "codex")
        .expect("saved Codex recipe keeps an unavailable harness parent");
    assert!(codex.unavailable_reason.is_some());
}

#[test]
fn windows_spawn_menu_omits_saved_wsl_configuration_for_windows_harness() {
    let mut prefs = crate::preferences::AppPreferences::default();
    prefs
        .harness_profiles
        .push(crate::preferences::HarnessProfile {
            id: "codex-wsl-test".into(),
            name: "Codex (WSL: Test)".into(),
            harness: "codex".into(),
            runtime: Some(crate::models::EnvType::Wsl),
            wsl_distro: Some("Test".into()),
            executable: None,
        });
    prefs.spawn_configurations.push(
        crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/codex-wsl-test".into(),
            name: "Codex (WSL: Test)".into(),
            spawn_option_id: "codex-wsl-test".into(),
            ..Default::default()
        },
    );
    prefs.spawn_configurations.push(
        crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/claude-wsl-old".into(),
            name: "Claude Code (WSL: Old)".into(),
            spawn_option_id: "claude-wsl-old".into(),
            ..Default::default()
        },
    );
    let menu = configuration_menu(vec![row_native("codex")], &prefs, Platform::Windows);
    assert_eq!(
        menu.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["codex"]
    );
}

#[test]
fn windows_spawn_menu_omits_routes_for_hidden_wsl_profiles() {
    let mut wsl = profile("claude-wsl-test", "anthropic");
    wsl.runtime = Some(crate::models::EnvType::Wsl);
    let mut route = claude_pairing("minimax");
    route.harness_id = wsl.id.clone();
    let mut old_route = claude_pairing("minimax");
    old_route.harness_id = "claude-wsl-old".into();
    let mut custom_wsl = profile("my-claude-guest", "anthropic");
    custom_wsl.runtime = Some(crate::models::EnvType::Wsl);
    let mut custom_route = claude_pairing("minimax");
    custom_route.harness_id = custom_wsl.id.clone();
    let menu = compose_provider_menu(
        vec![profile("claude", "anthropic"), wsl, custom_wsl],
        vec![acct("minimax", true, Some("sk-mm"))],
        vec![route, old_route, custom_route],
        Platform::Windows,
        None,
        &[],
        &[],
    );
    assert!(
        !menu
            .iter()
            .any(|row| row.harness_id.starts_with("claude-wsl-")
                || row.harness_id == "my-claude-guest"),
        "hidden WSL routes must not leak into the backend menu: {menu:?}"
    );
}

#[test]
fn windows_spawn_menu_omits_wsl_only_install_of_windows_capable_codex() {
    let mut wsl = profile("codex-wsl-test", "codex");
    wsl.runtime = Some(crate::models::EnvType::Wsl);
    let menu = compose_provider_menu(
        vec![wsl],
        Vec::new(),
        Vec::new(),
        Platform::Windows,
        Some("Test"),
        &[],
        &[],
    );
    assert!(
        menu.is_empty(),
        "Codex supports Windows, so its WSL install is not a spawn choice: {menu:?}"
    );
}

/// Issue #1864 follow-up — the Launch Configuration `reconcile` seeds for the
/// `-windows` mirror of a native harness must not resurrect a duplicate
/// "Claude Code (Windows)" harness group beside the canonical `claude` row.
#[test]
fn windows_spawn_menu_omits_saved_windows_mirror_configuration() {
    let mut prefs = crate::preferences::AppPreferences::default();
    prefs
        .harness_profiles
        .push(crate::preferences::HarnessProfile {
            id: "claude-windows".into(),
            name: "Claude Code (Windows)".into(),
            harness: "anthropic".into(),
            runtime: Some(crate::models::EnvType::Windows),
            wsl_distro: None,
            executable: None,
        });
    prefs.spawn_configurations.push(
        crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/claude-windows".into(),
            name: "Claude Code (Windows)".into(),
            spawn_option_id: "claude-windows".into(),
            ..Default::default()
        },
    );
    let menu = configuration_menu(vec![row_native("claude")], &prefs, Platform::Windows);
    assert_eq!(
        menu.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["claude"]
    );
}

/// The mirror's Provider Route leaks through the same door: once the
/// `-windows` profile is collapsed, `claude-windows:minimax` has no harness
/// group to belong to and must be dropped with it.
#[test]
fn windows_spawn_menu_omits_routes_for_windows_mirror_profiles() {
    let mut mirror = profile("claude-windows", "anthropic");
    mirror.runtime = Some(crate::models::EnvType::Windows);
    let mut route = claude_pairing("minimax");
    route.harness_id = mirror.id.clone();
    let menu = compose_provider_menu(
        vec![profile("claude", "anthropic"), mirror],
        vec![acct("minimax", true, Some("sk-mm"))],
        vec![route],
        Platform::Windows,
        None,
        &[],
        &[],
    );
    assert_eq!(
        menu.iter()
            .map(|row| row.harness_id.as_str())
            .collect::<Vec<_>>(),
        ["claude"],
        "the mirror's route must not create a second harness group: {menu:?}",
    );
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
        vec![
            row_native("terminal"),
            row_native("claude"),
            row_native("codex"),
        ],
        &[],
    );
    let ids: Vec<_> = ordered.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, vec!["claude", "codex", "terminal"]);
}

/// Issue #573: the stored harness order drives the row order, Terminal still
/// pinned last even if it appears mid-list in the stored order.
#[test]
fn order_providers_applies_stored_order() {
    let order = vec![
        "codex".to_string(),
        "terminal".to_string(),
        "claude".to_string(),
    ];
    let ordered = order_providers(
        vec![
            row_native("claude"),
            row_native("terminal"),
            row_native("codex"),
        ],
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
// 3. `SpawnOptionId::from_str` parses a composite id once at the
//    entry seam, splitting on the first `:` so the resolver chain
//    can pick the executor from the harness part and the credentials
//    from the provider part (issue #1659 item 1).

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
        runtime: None,
        wsl_distro: None,
        executable: None,
    };
    let info = provider_info_for(&claude, Platform::Windows)
        .expect("claude profile is available on Windows");
    assert_eq!(info.id, "claude");
    assert_eq!(info.harness_id, "claude");
    assert!(
        info.provider_id.is_none(),
        "native row must have no provider_id"
    );
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
        runtime: None,
        wsl_distro: None,
        executable: None,
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
            runtime: None,
            wsl_distro: None,
            executable: None,
        },
        crate::preferences::HarnessProfile {
            id: "codex".to_string(),
            name: "OpenAI Codex".to_string(),
            harness: "codex".to_string(),
            runtime: None,
            wsl_distro: None,
            executable: None,
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
        runtime: None,
        configurations: Vec::new(),
        configuration: None,
        unavailable_reason: None,
        effective_permission: None,
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
    assert_eq!(
        ids,
        vec![
            "claude",
            "claude:kimi",
            "claude:openrouter",
            "claude:minimax"
        ]
    );
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
    assert_eq!(
        ids,
        vec!["claude", "claude:kimi", "claude:minimax", "claude:deepseek"]
    );
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
        vec![
            profile("claude", "anthropic"),
            profile("terminal", "terminal"),
        ],
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
        None,
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

// ----- SpawnOptionId resolver (issue #575 → #1659) ------------------
//
// The composite id format `<harness>` (native) or `<harness>:<provider>`
// (proxied) is parsed once at the entry seam into
// [`crate::agent::provider::SpawnOptionId`]. A provider id containing
// `:` (a theoretical edge case — the id is user-chosen) is preserved
// intact on the right side. The tests below pin the parse contract so
// a future refactor that drops the first-`:` split trips review.

#[test]
fn parse_spawn_option_id_splits_bare_into_native() {
    let id = crate::agent::provider::SpawnOptionId::from("claude");
    assert_eq!(id.harness_id(), "claude");
    assert!(id.provider_id().is_none());
    assert!(!id.is_proxied());
}

#[test]
fn parse_spawn_option_id_splits_composite_into_harness_and_provider() {
    let id = crate::agent::provider::SpawnOptionId::from("claude:minimax");
    assert_eq!(id.harness_id(), "claude");
    assert_eq!(id.provider_id(), Some("minimax"));
    assert!(id.is_proxied());
}

#[test]
fn parse_spawn_option_id_keeps_provider_part_intact_on_duplicate_colons() {
    // A provider id with its own `:` (theoretical today, but the
    // id is user-chosen so we can't rule it out) lands entirely in
    // the provider slot. The first `:` is the split, not the last.
    let id = crate::agent::provider::SpawnOptionId::from("claude:weird:id");
    assert_eq!(id.harness_id(), "claude");
    assert_eq!(id.provider_id(), Some("weird:id"));
    assert_eq!(id.to_string(), "claude:weird:id");
}

/// The resolver chain (`resolve_harness_provider`) splits a composite
/// id on the first `:` and uses only the harness part to pick the
/// executor. `claude:minimax` resolves to the Anthropic executor
/// (Claude Code), not to a nonexistent `claude:minimax` Provider.
/// The composite-id path is exercised through
/// `Provider::from_db_str` in `models::tests`; the split logic
/// itself is the `SpawnOptionId` test above. Here we pin
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
        runtime: None,
        wsl_distro: None,
        executable: None,
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
        vec![
            profile("claude", "anthropic"),
            profile("terminal", "terminal"),
        ],
        vec![
            acct("minimax", true, Some("sk-mm")),
            acct("moonshot", true, Some("sk-moon")),
        ],
        vec![claude_pairing("minimax"), claude_pairing("moonshot")],
        Platform::Windows,
        None,
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
        None,
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
        None,
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
        runtime: None,
        wsl_distro: None,
        executable: None,
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
        runtime: None,
        wsl_distro: None,
        executable: None,
    };
    let info =
        provider_info_for(&cursor, Platform::Windows).expect("Cursor is available on Windows");
    assert!(
        info.resumable,
        "Cursor's workspace JSONL transcript must enable archive resume"
    );
    assert!(info.capabilities.produces_readable_transcript);
}

#[test]
fn provider_info_carries_runtime_without_a_resolved_launch_plan() {
    use crate::models::EnvType;
    for (runtime, host, wire) in [
        (EnvType::Wsl, Platform::Windows, "wsl"),
        (EnvType::WindowsInterop, Platform::Linux, "windowsinterop"),
        (EnvType::Windows, Platform::Windows, "windows"),
    ] {
        let mut profile = profile("custom-codex", "codex");
        profile.runtime = Some(runtime);
        let row = provider_info_for(&profile, host).unwrap();
        assert!(row.configuration.is_none());
        assert!(row.capabilities.background_inference.is_some());
        assert_eq!(serde_json::to_value(row).unwrap()["runtime"], wire);
    }
}

/// Negative companion to the previous test: the `harness` field must
/// actually drive the executor resolution. If a profile pins
/// `harness: "opencode"`, `resumable` must reflect OpenCode's actual
/// capability (true as of #1296: `supports_resume && produces_readable_transcript`)
/// rather than silently collapsing to Anthropic on the id. The test
/// pins the harness-driven lookup — the resumable value is asserted
/// elsewhere (`models::tests::only_transcript_writing_providers_produce_a_readable_transcript`).
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
        runtime: None,
        wsl_distro: None,
        executable: None,
    };
    let info = provider_info_for(&custom_opencode, Platform::Windows)
        .expect("OpenCode-backed profile is available on Windows");
    assert!(
        info.capabilities.produces_readable_transcript,
        "OpenCode-backed profile must advertise produces_readable_transcript=true \
             (#1296); the harness field drives resolution, not the user-chosen id"
    );
    assert!(
        info.resumable,
        "OpenCode-backed profile is resumable=true since #1296: \
             supports_resume && produces_readable_transcript"
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
        runtime: None,
        wsl_distro: None,
        executable: None,
    };
    let info =
        provider_info_for(&terminal, Platform::Windows).expect("Terminal is available on Windows");
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
        runtime: None,
        wsl_distro: None,
        executable: None,
    };
    let info = provider_info_for(&profile, Platform::Windows)
        .expect("minimax resolves to Anthropic and must be available on Windows");
    assert!(
        info.resumable,
        "legacy minimax id resolves to Anthropic (issue #538) and must be resumable"
    );
}

/// Issue #1937: every `available_providers` derivation must emit exactly
/// one greppable `info` line carrying the total wall-clock, the
/// per-runtime Codex probe cost, and the `needs_codex` gate state - the
/// acceptance test for the Settings -> Providers load work. The field
/// allow-list pins the redaction discipline: only runtime identities and
/// CLI versions travel, never credentials, keys, or endpoint URLs.
/// Issue #1948 extends the line with the compose-phase wall-clock and
/// the per-runtime cache-reuse bits, so a slow save-refresh reads as
/// cold discovery, warm discovery, or menu composition.
///
/// A hand-rolled capturing subscriber (scoped via `with_default`, so no
/// global state) keeps this parallel-safe without new dev-dependencies.
#[test]
fn available_providers_emits_one_greppable_derivation_log_line() {
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata};

    struct Recorder {
        fields: Vec<(String, String)>,
    }
    impl Visit for Recorder {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            // `record_debug` is the `Visit` funnel: every typed
            // `record_*` defaults through it, so one arm captures all
            // fields. String values arrive Debug-quoted - strip the
            // quotes for assertion readability.
            let rendered = format!("{value:?}");
            let unquoted = rendered
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(&rendered)
                .to_string();
            self.fields.push((field.name().to_string(), unquoted));
        }
    }
    type CapturedEvents = Arc<Mutex<Vec<(tracing::Level, Vec<(String, String)>)>>>;
    struct Capture {
        events: CapturedEvents,
    }
    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut recorder = Recorder { fields: Vec::new() };
            event.record(&mut recorder);
            let level = *event.metadata().level();
            self.events.lock().unwrap().push((level, recorder.fields));
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    let events: CapturedEvents = Arc::new(Mutex::new(Vec::new()));
    // `tracing` caches per-callsite interest globally: a first hit under
    // the default no-op dispatcher pins the site as disabled, and a
    // rebuild only reaches already-registered sites - so a sibling test
    // hitting the derivation first (or between rebuild and emit)
    // would silently swallow the line under parallelism. Drive one
    // throwaway derivation first (registering the site under the
    // capturing dispatcher), rebuild, discard its line, and assert on
    // the second derivation. Whatever the initial cache state, the
    // second derivation contributes exactly one line - which also
    // proves the "one line per derivation" budget.
    //
    // The derivation itself, not `available_providers`: the memo
    // (issue #2349) turns a second call in the same preferences
    // generation into a cache hit, and a hit logs nothing. The line
    // belongs to the derivation, so drive that directly.
    let run_once = || {
        tracing::dispatcher::with_default(
            &tracing::dispatcher::Dispatch::new(Capture {
                events: events.clone(),
            }),
            || {
                tracing::callsite::rebuild_interest_cache();
                available_providers_with_preferences(
                    crate::preferences::snapshot_or_default(),
                    &crate::agent::detection::detection_snapshot(),
                )
            },
        )
    };
    run_once();
    tracing::callsite::rebuild_interest_cache();
    events.lock().unwrap().clear();
    let providers = run_once();

    let events = events.lock().unwrap();
    let derivations: Vec<_> = events
        .iter()
        .filter(|(_, fields)| {
            fields.iter().any(|(name, value)| {
                name == "message" && value == "provider menu derivation completed"
            })
        })
        .collect();
    assert_eq!(
        derivations.len(),
        1,
        "expected exactly one provider menu derivation log line, got {}",
        derivations.len()
    );
    // Issue #2349: the line is `debug`. The default filter already admits
    // this crate at `debug`, so the level is not what stops an idle window
    // writing it (the memo is) - but the issue asked for it, and `info`
    // ranked a per-poll line above the lifecycle lines it buried.
    assert_eq!(
        derivations[0].0,
        tracing::Level::DEBUG,
        "the derivation line must not be logged at info"
    );
    let fields = &derivations[0].1;
    let mut names: Vec<&str> = fields
        .iter()
        .filter(|(name, _)| name != "message")
        .map(|(name, _)| name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
            names,
            [
                "codex_probe_duration_ms",
                "foreign_codex_cached",
                "foreign_codex_version",
                "foreign_env",
                "foreign_runtime_identity",
                "menu_compose_duration_ms",
                "menu_rows",
                "native_codex_cached",
                "native_codex_version",
                "native_runtime_identity",
                "needs_codex",
                "total_duration_ms",
            ],
            "the derivation log field set is the redaction contract - adding a field is a deliberate change"
        );
    let value = |name: &str| {
        fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("derivation log must carry `{name}`"))
    };
    // The timings must flow from the real derivation, not hardcode a
    // shape: the total parses as a millisecond count and the row count
    // matches the returned menu.
    value("total_duration_ms")
        .parse::<u128>()
        .expect("total_duration_ms must be a millisecond count");
    value("codex_probe_duration_ms")
        .parse::<u128>()
        .expect("codex_probe_duration_ms must be a millisecond count");
    // The phases must partition the derivation (issue #1948): the two
    // fields must always sum within the total, so a future phase cannot
    // be added without updating this budget. Limit of this guard: the bare
    // test env skips both probes (needs_codex is false), so a `compose_started`
    // misplaced above zero-length probes still passes here - clock placement
    // is pinned by inspection; real cold/warm splits come from the log line.
    // Issue #1934: the two runtimes are discovered concurrently, so their
    // costs share one `codex_probe_duration_ms` window. Summing a
    // per-runtime duration each would double-count that overlap and break
    // this partition, which is why the field is gone rather than renamed.
    // Millis truncation only rounds each phase down, so the sum of floors
    // still cannot exceed the total.
    let phase_total = value("codex_probe_duration_ms")
        .parse::<u128>()
        .expect("codex_probe_duration_ms must be a millisecond count")
        + value("menu_compose_duration_ms")
            .parse::<u128>()
            .expect("menu_compose_duration_ms must be a millisecond count");
    assert!(
        phase_total
            <= value("total_duration_ms")
                .parse::<u128>()
                .expect("total_duration_ms must be a millisecond count"),
        "phase timings must partition the derivation: probes + compose cannot exceed the total"
    );
    assert_eq!(
        value("menu_rows")
            .parse::<usize>()
            .expect("menu_rows must parse"),
        providers.len(),
        "logged menu_rows must match the derived menu"
    );
    // A bare test env configures no OpenAI-surface pairings, so the
    // Codex probes are skipped - and the line must say so explicitly
    // rather than reading as a genuinely fast probe (issue #1937).
    assert_eq!(value("needs_codex"), "false");
    assert_eq!(value("native_codex_cached"), "false");
    assert_eq!(value("foreign_codex_cached"), "false");
    assert_eq!(value("native_runtime_identity"), "none");
    assert_eq!(value("foreign_runtime_identity"), "none");
}

// -----------------------------------------------------------------------
// Issue #2349 — the derived-menu memo. The regression it fixes is a
// *rate*: the Android client re-reads `/api/providers` every 5s, so an
// idle paired phone re-derived the menu (Codex probe included) twelve
// times a minute. These pin the memo that makes a repeat read free and
// the invalidation that keeps it honest.
// -----------------------------------------------------------------------

/// A stand-in derivation result, shaped like a real menu row so a memo
/// hit can be compared against what the derivation returned.
fn memo_row() -> ProviderInfo {
    use crate::agent::provider::adapters::TERMINAL;
    ProviderInfo {
        id: "terminal".to_string(),
        label: "Terminal".to_string(),
        color: String::new(),
        icon: String::new(),
        resumable: false,
        harness_id: "terminal".to_string(),
        provider_id: None,
        is_proxied: false,
        group_key: "terminal".to_string(),
        capabilities: TERMINAL.capabilities(),
        runtime: None,
        configurations: Vec::new(),
        configuration: None,
        unavailable_reason: None,
        effective_permission: None,
    }
}

/// The memo key a test pins: the preferences generation plus the detection
/// epoch (issue #2532).
fn memo_key(generation: u64, detection_epoch: u64) -> MenuKey {
    MenuKey {
        generation,
        detection_epoch,
    }
}

/// A repeat read inside one generation must not re-derive. Driven
/// through the memo seam with a pinned generation and a counting closure
/// rather than through wall-clock: the count is the observable, and no
/// sleep (or racing preferences write) can make it pass for the wrong
/// reason.
#[test]
fn a_repeat_read_within_the_ttl_does_not_re_derive() {
    let mut derivations = 0;
    let mut derive = || {
        derivations += 1;
        vec![memo_row()]
    };
    let first = derived_menu_at(memo_key(7, 0), &mut derive);
    let second = derived_menu_at(memo_key(7, 0), &mut derive);
    assert_eq!(derivations, 1, "the second read must be a memo hit");
    let ids = |rows: &[ProviderInfo]| {
        rows.iter()
            .map(|row| row.id.clone())
            .collect::<Vec<String>>()
    };
    assert_eq!(
        ids(&second),
        ids(&first),
        "a memo hit must serve the rows the derivation produced"
    );
}

/// The invalidation that keeps the memo honest is the preferences
/// generation, which every preference write bumps. A read at the next
/// generation must derive again.
#[test]
fn a_newer_generation_re_derives_the_menu() {
    let mut derivations = 0;
    let mut derive = || {
        derivations += 1;
        vec![memo_row()]
    };
    derived_menu_at(memo_key(7, 0), &mut derive);
    derived_menu_at(memo_key(8, 0), &mut derive);
    assert_eq!(
        derivations, 2,
        "a settings write must invalidate the memoized menu"
    );
}

/// The detection epoch is the second half of the memo key (issue #2532). A
/// probe that observes a different set of installed harnesses bumps it, and
/// the memo must re-derive on that — inside the TTL, without waiting for the
/// timer.
#[test]
fn a_newer_detection_epoch_re_derives_the_menu() {
    let mut derivations = 0;
    let mut derive = || {
        derivations += 1;
        vec![memo_row()]
    };
    derived_menu_at(memo_key(7, 0), &mut derive);
    derived_menu_at(memo_key(7, 1), &mut derive);
    assert_eq!(
        derivations, 2,
        "a new detection epoch must invalidate the memoized menu"
    );
}

/// The TTL is the only thing bounding a change made *outside* Buildmesh
/// (a `npm i -g @openai/codex` while the window is open), and the
/// generation cannot see that. Pinned with a synthetic clock so the
/// bound is asserted, not slept through.
#[test]
fn a_memoized_menu_expires_after_the_ttl() {
    let mut cache = DerivedMenuCache::default();
    let derived_at = Instant::now();
    let menu = Arc::new(Vec::<ProviderInfo>::new());
    cache.put(memo_key(0, 0), derived_at, Arc::clone(&menu));
    assert!(
        cache
            .get(
                memo_key(0, 0),
                derived_at + DERIVED_MENU_TTL - Duration::from_nanos(1)
            )
            .is_some(),
        "a memoized menu is served until its TTL expires"
    );
    assert!(
        cache
            .get(memo_key(0, 0), derived_at + DERIVED_MENU_TTL)
            .is_none(),
        "an expired menu must re-derive so an out-of-process install shows up"
    );
}

/// The write-side race guard, mirrored from `agent::launch_routing`: a
/// derivation that sampled generation `G` cannot publish or evict once a
/// settings write has moved the slot to `G+1`.
#[test]
fn a_derivation_from_an_older_generation_cannot_publish() {
    let mut cache = DerivedMenuCache::default();
    let now = Instant::now();
    cache.get(memo_key(2, 0), now); // moves the slot to generation 2
    cache.put(memo_key(1, 0), now, Arc::new(Vec::new()));
    assert!(
        cache.get(memo_key(2, 0), now).is_none(),
        "a stale derivation must not become serveable state"
    );
    assert!(
        cache.get(memo_key(1, 0), now).is_none(),
        "an older reader must not evict the newer generation"
    );
}

/// The same race guard on the detection epoch: a derivation that observed
/// epoch `E` does not publish or evict once a probe has moved the slot to
/// `E+1`.
#[test]
fn a_derivation_from_an_older_detection_epoch_cannot_publish() {
    let mut cache = DerivedMenuCache::default();
    let now = Instant::now();
    cache.get(memo_key(2, 1), now); // moves the slot to epoch 1
    cache.put(memo_key(2, 0), now, Arc::new(Vec::new()));
    assert!(
        cache.get(memo_key(2, 1), now).is_none(),
        "a stale derivation must not become serveable state"
    );
    assert!(
        cache.get(memo_key(2, 0), now).is_none(),
        "an older reader must not evict the newer detection epoch"
    );
}

/// Both production entry points must route through the memo — that is the
/// wiring the rate fix depends on. A stored harness profile shows up in
/// the menu only after the write that adds it, whether the menu is read
/// through the mobile route's `available_providers` or the Tauri
/// command's `list_providers_blocking`.
#[test]
fn both_menu_entry_points_follow_a_preferences_write() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::write(
        scratch.path().join("preferences.json"),
        serde_json::to_vec_pretty(&crate::preferences::AppPreferences::default()).unwrap(),
    )
    .unwrap();
    crate::preferences::init_for_tests(scratch.path().into());

    let present = |rows: &[ProviderInfo]| rows.iter().any(|row| row.id == "second-terminal");
    assert!(!present(&available_providers()));
    assert!(!present(&list_providers_blocking().unwrap()));

    crate::preferences::update(|prefs| {
        prefs
            .harness_profiles
            .push(profile("second-terminal", "terminal"))
    })
    .expect("preferences write lands");

    assert!(
        present(&list_providers_blocking().unwrap()),
        "the command must serve the menu a write just changed"
    );
    assert!(
        present(&available_providers()),
        "the route must serve the menu a write just changed"
    );
    crate::preferences::reset_for_tests();
}

// -----------------------------------------------------------------------
// Issue #2532 — the derivation reads one preferences snapshot.
//
// The derivation used to reach preferences through several independent
// accessors, each with its own load, so a write landing between two of them
// could hand the derivation a torn view. These pin the replacement: one read
// per derivation, and a derivation a concurrent write leaves alone.
// -----------------------------------------------------------------------

/// Write an empty preferences file into a scratch directory and point the
/// preferences module at it, so a test owns its preferences.
fn scratch_preferences() -> tempfile::TempDir {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::write(
        scratch.path().join("preferences.json"),
        serde_json::to_vec_pretty(&crate::preferences::AppPreferences::default()).unwrap(),
    )
    .unwrap();
    crate::preferences::init_for_tests(scratch.path().into());
    scratch
}

/// The row ids of a menu, for order-independent comparison.
fn menu_ids(rows: &[ProviderInfo]) -> Vec<String> {
    rows.iter().map(|row| row.id.clone()).collect()
}

/// The derivation reads preferences **once** (issue #2532): the snapshot is
/// the single read, and the derivation that consumes it reads no preferences
/// of its own.
///
/// The counter is per-thread, like the preferences cache itself, so a
/// parallel test on another worker thread leaves this thread's count alone.
#[test]
fn a_derivation_reads_preferences_exactly_once() {
    let scratch = scratch_preferences();

    let before = crate::preferences::load_count();
    let snapshot = crate::preferences::load_snapshot().expect("preferences read");
    assert_eq!(
        crate::preferences::load_count() - before,
        1,
        "the snapshot must read preferences exactly once"
    );

    let before = crate::preferences::load_count();
    let _menu = available_providers_with_preferences(
        snapshot,
        &crate::agent::detection::detection_snapshot(),
    );
    assert_eq!(
        crate::preferences::load_count() - before,
        0,
        "the derivation must not read preferences at all — the snapshot is its \
         only source"
    );

    crate::preferences::reset_for_tests();
    drop(scratch);
}

/// A torn derivation is impossible (issue #2532): the rows are a pure
/// function of the snapshot the derivation was handed, so a write that lands
/// afterwards does not reach into a derivation already in flight.
///
/// The write is proven to have landed — a *fresh* snapshot sees it — so the
/// first assertion is about isolation, not about a write that failed to land.
#[test]
fn a_derivation_cannot_be_torn_by_a_concurrent_write() {
    let scratch = scratch_preferences();
    crate::preferences::update(|prefs| {
        prefs
            .harness_profiles
            .push(profile("first-terminal", "terminal"))
    })
    .expect("preferences write lands");

    let detection = crate::agent::detection::detection_snapshot();
    let snapshot = crate::preferences::load_snapshot().expect("preferences read");
    let before = available_providers_with_preferences(snapshot.clone(), &detection);

    // A write lands while the derivation is in flight.
    crate::preferences::update(|prefs| {
        prefs
            .harness_profiles
            .push(profile("second-terminal", "terminal"))
    })
    .expect("preferences write lands");

    let after_write = available_providers_with_preferences(snapshot.clone(), &detection);
    assert_eq!(
        menu_ids(&before),
        menu_ids(&after_write),
        "a write must not reach into a derivation that already holds its snapshot"
    );

    // The write really landed: a fresh snapshot sees it.
    let fresh = crate::preferences::load_snapshot().expect("preferences read");
    let fresh_menu = available_providers_with_preferences(fresh, &detection);
    assert!(
        menu_ids(&fresh_menu).contains(&"second-terminal".to_string()),
        "the write must reach a derivation that starts after it"
    );

    crate::preferences::reset_for_tests();
    drop(scratch);
}

/// The detection epoch is what makes a harness installed while the app runs
/// appear in the menu (issue #2532) — without waiting for the TTL.
///
/// The observation is the derivation's second input and the epoch the second
/// half of the memo key, so a probe that observes a new install invalidates
/// the memoized menu straight away.
#[test]
fn a_harness_installed_after_startup_appears_without_waiting_for_the_ttl() {
    let scratch = scratch_preferences();
    crate::preferences::update(|prefs| prefs.harness_profiles.push(profile("codex", "codex")))
        .expect("preferences write lands");

    let previous = crate::agent::detection::detection_snapshot();

    // Startup: the probe did not see `codex` installed.
    let startup = crate::agent::detection::observe_installations(Default::default());
    assert!(
        startup.observed(),
        "a probe that found nothing installed is still an observation — the set is \
         known to be empty, not unknown"
    );
    assert!(
        startup.epoch > previous.epoch,
        "the first observation must move the epoch"
    );
    let snapshot = crate::preferences::load_snapshot().expect("preferences read");
    let menu = derived_menu(snapshot, available_providers_with_preferences);
    assert!(
        !menu_ids(&menu).contains(&"codex".to_string()),
        "a harness the probe did not see installed must not be offered"
    );

    // A later probe observes the install.
    let after = crate::agent::detection::observe_installations(std::collections::HashSet::from([
        "codex".to_string(),
    ]));
    assert!(
        after.epoch > startup.epoch,
        "a probe that observes a different set must bump the epoch"
    );

    // The memo key changed with it, so the menu is derived again — inside the
    // TTL, without the timer being involved.
    let snapshot = crate::preferences::load_snapshot().expect("preferences read");
    let menu = derived_menu(snapshot, available_providers_with_preferences);
    assert!(
        menu_ids(&menu).contains(&"codex".to_string()),
        "the newly installed harness must appear without waiting for the TTL"
    );

    crate::agent::detection::restore_detection_state(&previous);
    crate::preferences::reset_for_tests();
    drop(scratch);
}

/// The epoch only moves on a real change: re-publishing the same observation
/// leaves it alone, so a repeated probe does not re-derive the menu.
#[test]
fn republishing_the_same_detection_observation_keeps_the_epoch() {
    let previous = crate::agent::detection::detection_snapshot();
    let first = crate::agent::detection::observe_installations(std::collections::HashSet::from([
        "codex".to_string(),
    ]));
    let second = crate::agent::detection::observe_installations(std::collections::HashSet::from([
        "codex".to_string(),
    ]));
    assert_eq!(
        first.epoch, second.epoch,
        "an unchanged observation must not move the epoch"
    );
    crate::agent::detection::restore_detection_state(&previous);
}
