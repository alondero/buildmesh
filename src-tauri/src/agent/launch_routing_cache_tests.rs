use super::*;

#[test]
fn cline_legacy_pairing_uses_cline_auth_environment() {
    // The credential-store double is per-thread: reset it so this test's
    // seeded key cannot reach another test on the same thread.
    crate::preferences::secrets::test_support::reset();
    let temp = tempfile::tempdir().unwrap();
    let store = preferences::PreferencesStore::new(temp.path().to_path_buf());
    store
        .save(preferences::AppPreferences {
            provider_accounts: vec![preferences::ProviderAccount {
                id: "minimax".into(),
                name: "MiniMax".into(),
                enabled: true,
                billing_mode: preferences::BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("test-key".into()),
            }],
            provider_pairings: vec![preferences::ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "minimax".into(),
                surface: preferences::ApiSurface::Anthropic,
                base_url: Some("https://example.invalid/anthropic".into()),
                model_tiers: preferences::ModelTiers {
                    default: Some("MiniMax-M3".into()),
                    ..Default::default()
                },
            }],
            ..Default::default()
        })
        .unwrap();

    let resolved = crate::env::ResolvedPath {
        host_path: "C:/work".into(),
        spawn_path: "C:/work".into(),
        raw_path: "C:/work".into(),
        env_type: crate::models::EnvType::Windows,
    };
    let snapshot = preferences::resolver::snapshot::snapshot_or_default_from(&store);
    let routing =
        resolve_routing(&snapshot, "cline:minimax", Provider::Cline, &resolved, None).unwrap();
    let PreparedLaunchRouting::Environment { values: env, .. } = routing else {
        panic!("expected Cline environment routing");
    };
    assert_eq!(
        env.iter()
            .find(|(key, _)| key == "ANTHROPIC_API_KEY")
            .map(|(_, value)| value.as_str()),
        Some("test-key")
    );
    assert!(!env
        .iter()
        .any(|(key, value)| key == "ANTHROPIC_API_KEY" && value.is_empty()));
    crate::preferences::secrets::test_support::reset();
}

#[test]
fn cline_launch_configuration_resolves_fallback_pairings_for_both_surfaces() {
    // The credential-store double is per-thread: reset it so this test's
    // seeded key cannot reach another test on the same thread.
    crate::preferences::secrets::test_support::reset();
    let temp = tempfile::tempdir().unwrap();
    let store = preferences::PreferencesStore::new(temp.path().to_path_buf());
    let resolved = crate::env::ResolvedPath {
        host_path: "C:/work".into(),
        spawn_path: "C:/work".into(),
        raw_path: "C:/work".into(),
        env_type: crate::models::EnvType::Windows,
    };
    for (surface, source_harness, base_url) in [
        (
            preferences::ApiSurface::Anthropic,
            "claude",
            "https://example.invalid/anthropic",
        ),
        (
            preferences::ApiSurface::OpenAI,
            "codex",
            "https://example.invalid/v1",
        ),
    ] {
        let prefs = preferences::AppPreferences {
            harness_profiles: vec![preferences::HarnessProfile {
                id: "cline".into(),
                name: "Cline".into(),
                harness: "cline".into(),
                runtime: None,
                wsl_distro: None,
                executable: None,
            }],
            provider_accounts: vec![preferences::ProviderAccount {
                id: "custom".into(),
                name: "Custom".into(),
                enabled: true,
                billing_mode: preferences::BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("test-key".into()),
            }],
            provider_pairings: vec![preferences::ProviderPairing {
                harness_id: source_harness.into(),
                provider_id: "custom".into(),
                surface,
                base_url: Some(base_url.into()),
                model_tiers: preferences::ModelTiers {
                    default: Some("test-model".into()),
                    ..Default::default()
                },
            }],
            spawn_configurations: vec![preferences::spawn_configurations::SpawnConfiguration {
                id: "launch/cline:custom".into(),
                name: "Cline custom".into(),
                spawn_option_id: "cline:custom".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        store.save(prefs.clone()).unwrap();
        let plan = preferences::launch_configurations::resolve(
            &prefs,
            "launch/cline:custom",
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            plan.route.as_ref().map(|route| route.surface),
            Some(surface)
        );
        let routing = prepare_snapshot_from(&store, &plan, &resolved).unwrap();
        let PreparedLaunchRouting::Environment { values: env, .. } = routing else {
            panic!("expected Cline environment routing");
        };
        let key_name = match surface {
            preferences::ApiSurface::Anthropic => "ANTHROPIC_API_KEY",
            preferences::ApiSurface::OpenAI => "OPENAI_API_KEY",
        };
        assert_eq!(
            env.iter()
                .find(|(key, _)| key == key_name)
                .map(|(_, value)| value.as_str()),
            Some("test-key")
        );
        let resumed = preferences::launch_configurations::resolve_snapshot_from(
            &store,
            &plan,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            resumed.route.as_ref().map(|route| route.surface),
            Some(surface)
        );
    }
    crate::preferences::secrets::test_support::reset();
}

/// Picker-visible-but-spawn-fails regression: a paired Claude launch must
/// carry the detection-resolved executable on its `Environment` routing.
/// The variant previously dropped it, so the spawn fell back to a bare
/// stem the GUI process `PATH` cannot resolve. The profile path here is
/// never touched on disk — the `or_else` short-circuits — so the test is
/// hermetic on machines with and without the CLI installed.
#[test]
fn prepare_threads_profile_executable_into_environment_routing() {
    // The credential-store double is per-thread: reset it so this test's
    // seeded key cannot reach another test on the same thread.
    crate::preferences::secrets::test_support::reset();
    let temp = tempfile::tempdir().unwrap();
    let store = preferences::PreferencesStore::new(temp.path().to_path_buf());
    let resolved_exe = PathBuf::from(if cfg!(windows) {
        "C:/Users/me/.local/bin/claude.exe"
    } else {
        "/home/me/.local/bin/claude"
    });
    store
        .save(preferences::AppPreferences {
            harness_profiles: vec![preferences::HarnessProfile {
                id: "claude".into(),
                name: "Claude Code".into(),
                harness: "anthropic".into(),
                runtime: None,
                wsl_distro: None,
                executable: Some(resolved_exe.clone()),
            }],
            provider_accounts: vec![preferences::ProviderAccount {
                id: "minimax".into(),
                name: "MiniMax".into(),
                enabled: true,
                billing_mode: preferences::BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("test-key".into()),
            }],
            provider_pairings: vec![preferences::ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "minimax".into(),
                surface: preferences::ApiSurface::Anthropic,
                base_url: Some("https://example.invalid/anthropic".into()),
                model_tiers: preferences::ModelTiers {
                    default: Some("MiniMax-M3".into()),
                    ..Default::default()
                },
            }],
            ..Default::default()
        })
        .unwrap();
    let resolved = crate::env::ResolvedPath {
        host_path: "C:/work".into(),
        spawn_path: "C:/work".into(),
        raw_path: "C:/work".into(),
        env_type: crate::models::EnvType::Windows,
    };
    let routing = prepare_from(&store, "claude:minimax", Provider::Anthropic, &resolved).unwrap();
    let PreparedLaunchRouting::Environment {
        values: env,
        executable,
    } = routing
    else {
        panic!("expected Claude environment routing");
    };
    assert_eq!(executable.as_deref(), Some(resolved_exe.as_path()));
    assert!(
        env.iter().any(|(key, _)| key == "ANTHROPIC_API_KEY"),
        "backend env must still flow alongside the executable"
    );
    crate::preferences::secrets::test_support::reset();
}

/// `recipe_binary_for` must name the same stem the spawn invokes, per
/// platform — the spawn-time re-resolution consults this name, so a
/// mismatch would resolve (and launch) a different program than routing
/// selected. A native (non-`Wsl`, non-interop) env resolves through the
/// host recipe by design, so the Anthropic expectation follows the test
/// runner's own platform; the `Wsl` arm is host-independent.
#[test]
fn recipe_binary_for_returns_the_stem_the_spawn_invokes() {
    use crate::models::EnvType;
    let native_claude = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    assert_eq!(
        super::recipe_binary_for(Provider::Anthropic, EnvType::Windows),
        native_claude
    );
    assert_eq!(
        super::recipe_binary_for(Provider::Anthropic, EnvType::Wsl),
        "claude"
    );
    assert_eq!(
        super::recipe_binary_for(Provider::Codex, EnvType::Windows),
        "codex"
    );
    assert_eq!(
        super::recipe_binary_for(Provider::Cline, EnvType::Windows),
        "cline"
    );
}

/// Review round 1 — a `WindowsInterop` spawn must never carry a
/// host-resolved executable: the Linux-side absolute path is not a valid
/// Windows path and PowerShell cannot run it. The profile path here is
/// never touched on disk — the interop arm returns before any lookup —
/// so the test is hermetic on every host.
#[test]
fn windows_interop_never_carries_a_host_resolved_executable() {
    use crate::models::EnvType;
    let profile = Some(PathBuf::from("/mnt/c/Users/me/.local/bin/claude.exe"));
    assert_eq!(
        super::spawn_time_executable(
            profile.clone(),
            Provider::Anthropic,
            EnvType::WindowsInterop
        ),
        None,
    );
    assert_eq!(
        super::spawn_time_executable(profile.clone(), Provider::Anthropic, EnvType::Wsl),
        profile,
    );
    assert_eq!(
        super::spawn_time_executable(profile.clone(), Provider::Anthropic, EnvType::Windows),
        profile,
    );
}

fn key(id: &str) -> RoutingCacheKey {
    (id.to_string(), "anthropic", "windows")
}

fn native(path: &str) -> PreparedLaunchRouting {
    PreparedLaunchRouting::Native {
        executable: Some(PathBuf::from(path)),
        harness_home: None,
    }
}

/// A distinct preference source, as a derived cache sees it.
///
/// The identity comes from a real store — it is what the cache keys on, so a
/// test must not invent one. The directory is returned with it so the caller
/// can keep it alive.
fn source() -> (tempfile::TempDir, SourceIdentity) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::preferences::PreferencesStore::new(dir.path().to_path_buf());
    (dir, store.source().clone())
}

/// Store `routing` at `revision`, going through `get` first exactly as
/// `prepare` does — `put` only accepts the source's current revision, so a
/// bare `put` would be dropped.
fn seed(
    cache: &mut RoutingCache,
    source: &SourceIdentity,
    revision: u64,
    key: RoutingCacheKey,
    routing: PreparedLaunchRouting,
) {
    assert!(
        cache.get(source, revision, &key).is_none(),
        "seed expects a cold key"
    );
    cache.put(source, revision, key, routing);
}

/// A hit returns the stored value; a repeated read at the same revision
/// keeps hitting (the memo's whole point).
#[test]
fn get_returns_the_value_stored_at_the_current_revision() {
    let (_dir, source) = source();
    let mut cache = RoutingCache::default();
    assert!(
        cache.get(&source, 7, &key("claude")).is_none(),
        "cold cache misses"
    );
    cache.put(&source, 7, key("claude"), native("/usr/bin/claude"));
    match cache.get(&source, 7, &key("claude")) {
        Some(PreparedLaunchRouting::Native { executable, .. }) => {
            assert_eq!(
                executable.as_deref(),
                Some(std::path::Path::new("/usr/bin/claude"))
            );
        }
        other => panic!("expected the memoised Native routing, got {other:?}"),
    }
}

/// A revision bump invalidates the map — the settings-change path.
#[test]
fn a_revision_bump_invalidates_every_entry() {
    let (_dir, source) = source();
    let mut cache = RoutingCache::default();
    seed(
        &mut cache,
        &source,
        7,
        key("claude"),
        native("/usr/bin/claude"),
    );
    assert!(
        cache.get(&source, 8, &key("claude")).is_none(),
        "a bumped revision must drop entries computed before it"
    );
}

/// The write-side race guard: a slow spawn that sampled revision 7 must
/// not be able to publish its result once the map has moved to 8. Without
/// this `put` a rotated endpoint/key could become serveable state.
#[test]
fn put_drops_an_entry_computed_before_a_bump() {
    let (_dir, source) = source();
    let mut cache = RoutingCache::default();
    // The map was already advanced to revision 8 by another caller...
    assert!(cache.get(&source, 8, &key("claude")).is_none());
    // ...then a slow spawn tries to insert its revision-7 result.
    cache.put(&source, 7, key("claude"), native("/usr/bin/claude"));
    assert!(
        cache.get(&source, 8, &key("claude")).is_none(),
        "a put stamped with a stale revision must be dropped, not stored"
    );
}

/// A caller holding an older revision must not evict the newer entries.
#[test]
fn a_stale_reader_does_not_evict_newer_entries() {
    let (_dir, source) = source();
    let mut cache = RoutingCache::default();
    seed(
        &mut cache,
        &source,
        8,
        key("claude"),
        native("/usr/bin/claude"),
    );
    // A slow reader that sampled revision 7 must miss, not clear the map.
    assert!(cache.get(&source, 7, &key("claude")).is_none());
    assert!(
        cache.get(&source, 8, &key("claude")).is_some(),
        "the revision-8 entry must survive a revision-7 reader"
    );
}

/// Different runtimes are different fingerprints — a WSL resolution must
/// not satisfy a Windows lookup of the same spawn option.
#[test]
fn the_runtime_is_part_of_the_fingerprint() {
    let (_dir, source) = source();
    let mut cache = RoutingCache::default();
    seed(
        &mut cache,
        &source,
        1,
        ("claude".to_string(), "anthropic", "wsl"),
        native("/home/u/bin/claude"),
    );
    assert!(
        cache
            .get(&source, 1, &("claude".to_string(), "anthropic", "windows"))
            .is_none(),
        "a Windows lookup must not reuse the WSL entry"
    );
}

/// The harness executor is part of the fingerprint — the same spawn option
/// string resolved under a different executor must not reuse the entry.
#[test]
fn the_provider_is_part_of_the_fingerprint() {
    let (_dir, source) = source();
    let mut cache = RoutingCache::default();
    seed(
        &mut cache,
        &source,
        1,
        ("claude".to_string(), "anthropic", "windows"),
        native("/usr/bin/claude"),
    );
    assert!(
        cache
            .get(&source, 1, &("claude".to_string(), "codex", "windows"))
            .is_none(),
        "a different harness must not reuse the entry"
    );
}

/// Two stores can hold the **same revision number** while carrying different
/// preferences — revisions are per store, so they are not comparable across
/// stores. The source identity is what keeps their entries apart
/// (issue #1482).
#[test]
fn two_sources_at_the_same_revision_do_not_share_entries() {
    let (_first_dir, first) = source();
    let (_second_dir, second) = source();
    let mut cache = RoutingCache::default();

    seed(
        &mut cache,
        &first,
        4,
        key("claude"),
        native("/first/claude"),
    );
    assert!(
        cache.get(&second, 4, &key("claude")).is_none(),
        "an entry computed against another profile must not be a hit"
    );
    assert!(
        cache.get(&first, 4, &key("claude")).is_some(),
        "the entry must still serve its own profile"
    );
}

/// A revision bump in one profile must not disturb another profile's
/// entries — the rotation is scoped to the source that moved.
#[test]
fn a_revision_bump_in_one_source_leaves_another_source_alone() {
    let (_first_dir, first) = source();
    let (_second_dir, second) = source();
    let mut cache = RoutingCache::default();

    seed(
        &mut cache,
        &first,
        4,
        key("claude"),
        native("/first/claude"),
    );
    seed(
        &mut cache,
        &second,
        4,
        key("claude"),
        native("/second/claude"),
    );

    // The first profile's preferences change.
    assert!(cache.get(&first, 5, &key("claude")).is_none());

    assert!(
        cache.get(&second, 4, &key("claude")).is_some(),
        "another profile's entry must survive this profile's revision bump"
    );
}

/// The routing and the revision it is memoised under must describe the **same**
/// preferences read (issue #1482).
///
/// The memo key names the revision the routing was computed from, so a write
/// that lands after the snapshot was taken must not change what that snapshot
/// derives: otherwise the routing would be published under a revision that
/// does not describe it, and a caller holding that revision would be served a
/// routing from a different read.
#[test]
fn a_routing_is_derived_from_the_snapshot_it_is_keyed_on() {
    // The credential-store double is per-thread: reset it so this test's
    // seeded key cannot reach another test on the same thread.
    crate::preferences::secrets::test_support::reset();
    let temp = tempfile::tempdir().unwrap();
    let store = preferences::PreferencesStore::new(temp.path().to_path_buf());
    let resolved = resolved_windows();
    let token = |routing: PreparedLaunchRouting| {
        let PreparedLaunchRouting::Environment { values: env, .. } = routing else {
            panic!("expected Claude environment routing");
        };
        env.iter()
            .find(|(key, _)| key == "ANTHROPIC_AUTH_TOKEN")
            .map(|(_, value)| value.clone())
    };

    store.save(proxied_prefs("key-one")).unwrap();
    // The read the memo key is built from.
    let snapshot = preferences::resolver::snapshot::snapshot_or_default_from(&store);
    // A write lands after that read.
    store.save(proxied_prefs("key-two")).unwrap();

    let routed = token(
        prepare_from_snapshot(&snapshot, "claude:minimax", Provider::Anthropic, &resolved).unwrap(),
    );
    assert_eq!(
        routed.as_deref(),
        Some("key-one"),
        "the routing must describe the snapshot it is keyed on"
    );

    // The later revision derives the later value: the two reads really differ.
    let fresh = preferences::resolver::snapshot::snapshot_or_default_from(&store);
    let routed = token(
        prepare_from_snapshot(&fresh, "claude:minimax", Provider::Anthropic, &resolved).unwrap(),
    );
    assert_eq!(routed.as_deref(), Some("key-two"));
    crate::preferences::secrets::test_support::reset();
}

/// The routing reads preferences **exactly once** (issue #1482).
///
/// The memo key names the revision the routing is derived from, so the key and
/// the derivation must come from the same read. A second read could answer
/// from a newer revision than the key names, and the routing would then be
/// published under a revision that does not describe it.
///
/// The counter is per-thread, like the preferences cache itself, so a parallel
/// test on another worker thread leaves this thread's count alone.
#[test]
fn a_routing_reads_preferences_exactly_once() {
    crate::preferences::secrets::test_support::reset();
    let temp = tempfile::tempdir().unwrap();
    let store = preferences::PreferencesStore::new(temp.path().to_path_buf());
    store.save(proxied_prefs("key-one")).unwrap();
    let resolved = resolved_windows();

    let before = crate::preferences::load_count();
    let _routing =
        prepare_from(&store, "claude:minimax", Provider::Anthropic, &resolved).expect("routing");
    assert_eq!(
        crate::preferences::load_count() - before,
        1,
        "the routing must read preferences exactly once — the memo key and the \
         derived routing must describe the same read"
    );
    crate::preferences::secrets::test_support::reset();
}

/// A proxied provider with one credential, for the routing tests above.
fn proxied_prefs(key: &str) -> preferences::AppPreferences {
    preferences::AppPreferences {
        provider_accounts: vec![preferences::ProviderAccount {
            id: "minimax".into(),
            name: "MiniMax".into(),
            enabled: true,
            billing_mode: preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some(key.into()),
        }],
        provider_pairings: vec![preferences::ProviderPairing {
            harness_id: "claude".into(),
            provider_id: "minimax".into(),
            surface: preferences::ApiSurface::Anthropic,
            base_url: Some("https://example.invalid/anthropic".into()),
            model_tiers: preferences::ModelTiers {
                default: Some("MiniMax-M3".into()),
                ..Default::default()
            },
        }],
        ..Default::default()
    }
}

/// The resolved path the routing tests spawn against.
fn resolved_windows() -> crate::env::ResolvedPath {
    crate::env::ResolvedPath {
        host_path: "C:/work".into(),
        spawn_path: "C:/work".into(),
        raw_path: "C:/work".into(),
        env_type: crate::models::EnvType::Windows,
    }
}
