use super::*;
use crate::agent::provider::{UiMeta, WindowsShell};

struct CatalogTestAdapter;

impl AgentProvider for CatalogTestAdapter {
    fn id(&self) -> &'static str {
        "catalog-test"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Catalog Test".into(),
            color: "#000000".into(),
            icon: "test".into(),
        }
    }

    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        crate::models::Provider::Terminal.adapter().descriptor()
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "catalog-probe",
            base_args: Vec::new(),
            trailing_args: Vec::new(),
            windows_shell: if platform == Platform::Windows {
                WindowsShell::Cmd
            } else {
                WindowsShell::Direct
            },
        }
    }

    fn model_catalog_probe(&self) -> Option<ModelCatalogProbe> {
        Some(ModelCatalogProbe {
            binary: None,
            args: vec!["probe".into()],
            stdin: None,
            effort_storage: ModelEffortStorage::Separate,
            allow_unlisted_models: false,
            login_home_env: Some("BM_CATALOG_HOME"),
            env_remove: &[],
        })
    }

    fn parse_model_catalog(&self, stdout: &str) -> Result<Vec<HarnessCatalogModel>, String> {
        Ok(stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| HarnessCatalogModel {
                id: line.into(),
                name: line.into(),
                description: None,
                efforts: Vec::new(),
            })
            .collect())
    }
}

static CATALOG_TEST_ADAPTER: CatalogTestAdapter = CatalogTestAdapter;

fn write_probe(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let executable = dir.join(if cfg!(windows) {
        "catalog-probe.cmd"
    } else {
        "catalog-probe"
    });
    let script = if cfg!(windows) {
        "@echo off\r\ntype \"%~dp0catalog-output.txt\"\r\n".to_string()
    } else {
        "#!/bin/sh\ncat \"$(dirname \"$0\")/catalog-output.txt\"\n".to_string()
    };
    std::fs::write(&executable, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(dir.join("catalog-output.txt"), body).unwrap();
    executable
}

#[test]
fn catalog_results_are_cached_by_executable_until_their_ttl_expires() {
    let dir = tempfile::tempdir().unwrap();
    let executable = write_probe(dir.path(), "alpha\n");
    let target = ModelCatalogTarget {
        executable: Some(&executable),
        runtime: None,
        wsl_distro: None,
        login_home: None,
    };

    let first =
        discover_with_cache_ttl(&CATALOG_TEST_ADAPTER, target, Duration::from_secs(60)).unwrap();
    assert_eq!(first.models[0].id, "alpha");

    std::fs::write(dir.path().join("catalog-output.txt"), "beta\n").unwrap();
    let cached =
        discover_with_cache_ttl(&CATALOG_TEST_ADAPTER, target, Duration::from_secs(60)).unwrap();
    assert_eq!(cached.models[0].id, "alpha");

    let refreshed = discover_with_cache_ttl(&CATALOG_TEST_ADAPTER, target, Duration::ZERO).unwrap();
    assert_eq!(refreshed.models[0].id, "beta");
}

#[test]
fn a_failed_catalog_probe_can_recover_on_the_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let executable = write_probe(dir.path(), "");
    let target = ModelCatalogTarget {
        executable: Some(&executable),
        ..Default::default()
    };
    assert!(discover(&CATALOG_TEST_ADAPTER, target).is_err());

    std::fs::write(dir.path().join("catalog-output.txt"), "recovered\n").unwrap();
    let recovered = discover(&CATALOG_TEST_ADAPTER, target).unwrap();
    assert_eq!(recovered.models[0].id, "recovered");
}

#[test]
fn unsupported_and_failed_catalog_probes_fail_closed() {
    let terminal = crate::models::Provider::Terminal.adapter();
    assert!(discover(terminal, ModelCatalogTarget::default()).is_err());

    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join(if cfg!(windows) {
        "failing-probe.cmd"
    } else {
        "failing-probe"
    });
    let script = if cfg!(windows) {
        "@echo off\r\nexit /b 1\r\n"
    } else {
        "#!/bin/sh\nexit 1\n"
    };
    std::fs::write(&executable, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let error = discover(
        &CATALOG_TEST_ADAPTER,
        ModelCatalogTarget {
            executable: Some(&executable),
            runtime: None,
            wsl_distro: None,
            login_home: None,
        },
    )
    .unwrap_err();
    assert!(error.contains("Harness model catalog failed"), "{error}");
}

#[test]
fn model_only_probes_keep_interactive_ids_without_inventing_effort() {
    let cases = [
        (
            crate::models::Provider::Grok,
            "Default model: grok-4.7\n\nAvailable models:\n  * grok-4.7 (default)\n  * custom/model\n",
            ["grok-4.7", "custom/model"],
        ),
        (
            crate::models::Provider::CommandCode,
            "Available models  ·  2 models\n\nAnthropic\nclaude-sonnet-5  fast\n\nOpen Source\nexample/model  reasoning\n\nPass the full id:\n\nDecision models (headless only)\ntypesafe/jev  headless only\n",
            ["claude-sonnet-5", "example/model"],
        ),
    ];
    for (provider, stdout, expected_ids) in cases {
        let adapter = provider.adapter();
        let dir = tempfile::tempdir().unwrap();
        let executable = write_probe(dir.path(), stdout);
        let target = ModelCatalogTarget {
            executable: Some(&executable),
            ..Default::default()
        };
        let catalog = discover(adapter, target).unwrap();
        assert_eq!(catalog.effort_storage, ModelEffortStorage::Undiscovered);
        assert_eq!(
            catalog
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert!(catalog.models.iter().all(|model| model.efforts.is_empty()));
    }
}

#[test]
fn claude_catalog_reads_the_matching_control_response() {
    let adapter = crate::models::Provider::Anthropic.adapter();
    let probe = adapter.model_catalog_probe().unwrap();
    let request: serde_json::Value = serde_json::from_str(probe.stdin.unwrap()).unwrap();
    assert_eq!(request["type"], "control_request");
    assert_eq!(request["request"]["subtype"], "initialize");
    assert_eq!(probe.effort_storage, ModelEffortStorage::Separate);
    assert!(probe.allow_unlisted_models);

    let models = adapter.parse_model_catalog(concat!(
        "{\"type\":\"system\",\"subtype\":\"init\"}\n",
        "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"other\"}}\n",
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"model-catalog","response":{"models":["#,
        r#"{"value":"default","displayName":"Default"},"#,
        r#"{"value":"sonnet","resolvedModel":"claude-sonnet-5","displayName":"Sonnet","supportedEffortLevels":["low","high","future"]},"#,
        r#"{"value":"claude-haiku-4-5-20251001"}]}}}"#,
    )).unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "sonnet");
    assert_eq!(models[0].name, "Sonnet");
    assert_eq!(
        models[0]
            .efforts
            .iter()
            .map(|effort| effort.id.as_str())
            .collect::<Vec<_>>(),
        ["low", "high"]
    );
    assert!(models[1].efforts.is_empty());
}

#[test]
fn catalog_probes_use_and_cache_by_the_selected_login_home() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join(if cfg!(windows) {
        "catalog-probe.cmd"
    } else {
        "catalog-probe"
    });
    let script = if cfg!(windows) {
        "@echo off
echo %BM_CATALOG_HOME%
"
    } else {
        "#!/bin/sh
printf '%s\n' \"$BM_CATALOG_HOME\"
"
    };
    std::fs::write(&executable, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let target = |login_home| ModelCatalogTarget {
        executable: Some(&executable),
        runtime: None,
        wsl_distro: None,
        login_home: Some(login_home),
    };

    let first = discover_with_cache_ttl(
        &CATALOG_TEST_ADAPTER,
        target("home-a"),
        Duration::from_secs(60),
    )
    .unwrap();
    let second = discover_with_cache_ttl(
        &CATALOG_TEST_ADAPTER,
        target("home-b"),
        Duration::from_secs(60),
    )
    .unwrap();

    assert_eq!(first.models[0].id, "home-a");
    assert_eq!(second.models[0].id, "home-b");
}

#[test]
fn codex_catalog_probes_remove_inherited_credential_overrides() {
    let _env = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let keys = [
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
    ];
    let previous: Vec<_> = keys
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    for key in keys {
        std::env::set_var(key, "inherited-override");
    }

    let dir = tempfile::tempdir().unwrap();
    let catalog = r#"{"models":[{"slug":"gpt-test","display_name":"GPT Test","visibility":"list","supported_reasoning_levels":[{"effort":"high"}]}]}"#;
    let executable = write_probe(dir.path(), catalog);
    let script = if cfg!(windows) {
        "@echo off\r\nif defined OPENAI_API_KEY exit /b 42\r\nif defined OPENAI_BASE_URL exit /b 43\r\nif defined CODEX_API_KEY exit /b 44\r\nif defined CODEX_ACCESS_TOKEN exit /b 45\r\ntype \"%~dp0catalog-output.txt\"\r\n"
    } else {
        "#!/bin/sh\n[ -z \"$OPENAI_API_KEY\" ] || exit 42\n[ -z \"$OPENAI_BASE_URL\" ] || exit 43\n[ -z \"$CODEX_API_KEY\" ] || exit 44\n[ -z \"$CODEX_ACCESS_TOKEN\" ] || exit 45\ncat \"$(dirname \"$0\")/catalog-output.txt\"\n"
    };
    std::fs::write(&executable, script).unwrap();

    let result = discover_with_cache_ttl(
        &crate::agent::provider::adapters::CODEX,
        ModelCatalogTarget {
            executable: Some(&executable),
            runtime: None,
            wsl_distro: None,
            login_home: None,
        },
        Duration::ZERO,
    );

    for (key, value) in previous {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    let catalog = result.unwrap();
    assert_eq!(catalog.models[0].id, "gpt-test");
}

#[test]
fn opencode_catalog_probes_remove_inherited_config_overrides() {
    let _env = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let keys = [
        "OPENCODE_CONFIG",
        "OPENCODE_CONFIG_CONTENT",
        "OPENCODE_CONFIG_DIR",
    ];
    let previous: Vec<_> = keys
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    for key in keys {
        std::env::set_var(key, "inherited-override");
    }

    let dir = tempfile::tempdir().unwrap();
    let catalog = r#"{"data":[{"providerID":"openai","id":"gpt","name":"GPT","status":"active","enabled":true,"variants":[{"id":"high","settings":{"reasoningEffort":"high"}}]}]}"#;
    let executable = write_probe(dir.path(), catalog);
    let script = if cfg!(windows) {
        "@echo off\r\nif defined OPENCODE_CONFIG exit /b 42\r\nif defined OPENCODE_CONFIG_CONTENT exit /b 43\r\nif defined OPENCODE_CONFIG_DIR exit /b 44\r\ntype \"%~dp0catalog-output.txt\"\r\n"
    } else {
        "#!/bin/sh\n[ -z \"$OPENCODE_CONFIG\" ] || exit 42\n[ -z \"$OPENCODE_CONFIG_CONTENT\" ] || exit 43\n[ -z \"$OPENCODE_CONFIG_DIR\" ] || exit 44\ncat \"$(dirname \"$0\")/catalog-output.txt\"\n"
    };
    std::fs::write(&executable, script).unwrap();

    let result = discover_with_cache_ttl(
        &crate::agent::provider::adapters::OPENCODE2,
        ModelCatalogTarget {
            executable: Some(&executable),
            runtime: None,
            wsl_distro: None,
            login_home: None,
        },
        Duration::ZERO,
    );

    for (key, value) in previous {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    let catalog = result.unwrap();
    assert_eq!(catalog.models[0].id, "openai/gpt");
}
