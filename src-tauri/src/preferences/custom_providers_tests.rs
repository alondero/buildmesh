use super::*;
use crate::preferences::tests::with_store;

fn create(store: &PreferencesStore, surface: ApiSurface) -> Result<SpawnConfiguration, String> {
    create_from(
        store,
        " My Custom API ".into(),
        " test-credential ".into(),
        surface,
        " https://provider.example/v1/ ".into(),
        " vendor/model-1 ".into(),
    )
}

#[test]
fn creates_resolvable_configuration_and_route_for_both_surfaces() {
    for surface in [ApiSurface::OpenAI, ApiSurface::Anthropic] {
        with_store(|store| {
            let configuration = create(store, surface).unwrap();
            let prefs = store.load().unwrap();
            let plan = crate::preferences::launch_configurations::resolve_for_edit(
                &prefs,
                &configuration.id,
            )
            .unwrap();
            let route = plan.route.unwrap();
            assert_eq!(route.surface, surface);
            assert_eq!(
                route.base_url.as_deref(),
                Some("https://provider.example/v1")
            );
            assert_eq!(route.provider_id, "my-custom-api");
            assert_eq!(
                route.harness_id,
                if surface == ApiSurface::OpenAI {
                    "codex"
                } else {
                    "claude"
                }
            );
            assert_eq!(route.model_tiers.default.as_deref(), Some("vendor/model-1"));
            if surface == ApiSurface::Anthropic {
                assert_eq!(route.model_tiers.haiku, route.model_tiers.default);
                assert_eq!(route.model_tiers.fable, route.model_tiers.default);
                assert_eq!(route.model_tiers.small_fast, route.model_tiers.default);
            }
            let account = prefs
                .provider_accounts
                .iter()
                .find(|a| a.id == "my-custom-api")
                .unwrap();
            assert_eq!(account.api_key.as_deref(), Some("test-credential"));
            assert_eq!(configuration.name, "My Custom API");
            assert!(configuration.id.starts_with("launch/"));
            assert!(configuration.generated.is_none());
            // Reopen through production storage, including credential hydration.
            let reopened = PreferencesStore::new(store.profile_dir().to_path_buf())
                .load()
                .unwrap();
            assert!(reopened
                .spawn_configurations
                .iter()
                .any(|c| c.id == configuration.id));
            assert_eq!(reopened.provider_pairings, prefs.provider_pairings);
            assert_eq!(
                reopened
                    .provider_accounts
                    .iter()
                    .find(|a| a.id == account.id)
                    .unwrap()
                    .api_key,
                account.api_key
            );
        });
    }
}

#[test]
fn duplicate_setup_cannot_overwrite_a_credential_route_or_configuration() {
    with_store(|store| {
        create(store, ApiSurface::OpenAI).unwrap();
        let before = std::fs::read(store.preferences_path()).unwrap();
        let revision = store.revision();
        let error = create(store, ApiSurface::Anthropic).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(store.revision(), revision);
        assert_eq!(std::fs::read(store.preferences_path()).unwrap(), before);
        assert_eq!(store.load().unwrap().spawn_configurations.len(), 1);
    });
}

#[test]
fn readding_a_removed_account_restores_it_and_preserves_saved_recipes() {
    with_store(|store| {
        let original = create(store, ApiSurface::OpenAI).unwrap();
        store
            .update(|prefs| super::super::remove_provider_account(prefs, "my-custom-api"))
            .unwrap();
        assert!(!store
            .load()
            .unwrap()
            .provider_accounts
            .iter()
            .any(|a| a.id == "my-custom-api"));
        let restored = create_from(
            store,
            "My Custom API".into(),
            "replacement-key".into(),
            ApiSurface::OpenAI,
            "https://replacement.example/v1".into(),
            "replacement-model".into(),
        )
        .unwrap();
        let prefs = store.load().unwrap();
        assert_eq!(
            prefs
                .provider_pairings
                .iter()
                .filter(|p| p.provider_id == "my-custom-api")
                .count(),
            1
        );
        assert!(prefs
            .spawn_configurations
            .iter()
            .any(|c| c.id == original.id));
        assert!(prefs
            .spawn_configurations
            .iter()
            .any(|c| c.id == restored.id));
        assert_eq!(
            prefs
                .provider_accounts
                .iter()
                .find(|a| a.id == "my-custom-api")
                .unwrap()
                .api_key
                .as_deref(),
            Some("replacement-key")
        );
        let plan =
            crate::preferences::launch_configurations::resolve_for_edit(&prefs, &restored.id)
                .unwrap();
        assert_eq!(
            plan.route.unwrap().base_url.as_deref(),
            Some("https://replacement.example/v1")
        );
    });
}

#[test]
fn malformed_setup_and_reserved_ids_leave_preferences_untouched() {
    with_store(|store| {
        create(store, ApiSurface::OpenAI).unwrap();
        let before = std::fs::read(store.preferences_path()).unwrap();
        for (name, key, endpoint, model) in [
            ("!!!", "key", "https://example.com/v1", "model"),
            ("Codex", "key", "https://example.com/v1", "model"),
            ("Kimi", "key", "https://example.com/v1", "model"),
            ("Custom", " ", "https://example.com/v1", "model"),
            ("Custom", "key", "https://example.com/v1", " "),
            ("Custom", "key", "not a URL", "model"),
            ("Custom", "key", "http://example.com/v1", "model"),
            ("Custom", "key", "https://user:pass@example.com/v1", "model"),
            (
                "Custom",
                "key",
                "https://example.com/v1?key=secret",
                "model",
            ),
            ("Custom", "key", "https://example.com/v1#fragment", "model"),
            (
                "Custom",
                "key",
                "https://example.com/v1/chat/completions",
                "model",
            ),
            ("Custom", "key", "https://example.com/v1/responses", "model"),
            ("Custom", "key", "https://example.com/v1/messages", "model"),
        ] {
            assert!(
                create_from(
                    store,
                    name.into(),
                    key.into(),
                    ApiSurface::OpenAI,
                    endpoint.into(),
                    model.into()
                )
                .is_err(),
                "{name} {endpoint}"
            );
            assert_eq!(std::fs::read(store.preferences_path()).unwrap(), before);
        }
    });
}

#[test]
fn loopback_http_is_supported_for_local_compatible_servers() {
    for base in [
        "http://localhost:11434/v1",
        "http://127.0.0.1:1234/v1",
        "http://[::1]:8080/v1",
    ] {
        with_store(|store| {
            let value = create_from(
                store,
                "Local API".into(),
                "local-key".into(),
                ApiSurface::OpenAI,
                base.into(),
                "local-model".into(),
            )
            .unwrap();
            let prefs = store.load().unwrap();
            let plan =
                crate::preferences::launch_configurations::resolve_for_edit(&prefs, &value.id)
                    .unwrap();
            assert_eq!(plan.route.unwrap().base_url.as_deref(), Some(base));
        });
    }
}

#[test]
fn corrupt_preferences_refuse_setup_without_replacing_the_file() {
    with_store(|store| {
        std::fs::write(store.preferences_path(), "{broken-json").unwrap();
        assert!(create(store, ApiSurface::OpenAI).is_err());
        assert_eq!(
            std::fs::read_to_string(store.preferences_path()).unwrap(),
            "{broken-json"
        );
    });
}
