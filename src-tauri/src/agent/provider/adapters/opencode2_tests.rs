use super::*;
use crate::agent::{
    capabilities::ResolvedAgentConfig,
    launch::{default_prepare, HarnessLaunchInput},
    spawn::ResumeCause,
};

#[cfg(windows)]
#[test]
fn session_api_preserves_json_through_the_windows_cli_shim() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("api fixture.cjs");
    std::fs::write(
        &script,
        r#"const body = process.argv[process.argv.indexOf('--data') + 1];
process.stdout.write(JSON.stringify({ data: JSON.parse(body) }));"#,
    )
    .unwrap();
    let node = crate::agent::detection::resolve_spawn_binary("node")
        .expect("Node is required for the Windows CLI argument fixture");
    let executable = dir.path().join("opencode2.cmd");
    std::fs::write(
        &executable,
        format!(
            "@echo off\r\n\"{}\" \"{}\" %*\r\n",
            node.display(),
            script.display()
        ),
    )
    .unwrap();
    let body = serde_json::json!({
        "id": "ses_assigned",
        "model": {"providerID": "custom", "id": "vendor/model"},
        "oddQuote": "a single \" then (parentheses)",
        "title": "spaces, quotes \" and \\\"; slash \\; newline\r\n; it's `literal` $() & | < > ^ ! %COMSPEC%"
    });
    let response = api(
        &executable,
        dir.path(),
        &dir.path().join("opencode.json"),
        "POST",
        "/api/session",
        body.clone(),
    )
    .unwrap();
    assert_eq!(response["data"], body);
}

#[test]
fn sandboxed_preparation_is_refused_before_project_plugins_can_execute() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    let resolved = ResolvedPath {
        host_path: path.clone(),
        spawn_path: path.clone(),
        raw_path: path,
        env_type: EnvType::Windows,
    };
    let config = ResolvedAgentConfig::default();
    let prepare = || {
        OPENCODE2
            .prepare_session(SessionPreparation {
                resolved: &resolved,
                executable: Some(Path::new("missing")),
                node_id: 1,
                session: SessionIdModeRef::Assign("ses_sandboxed"),
                config: &config,
                sandbox: true,
            })
            .unwrap_err()
    };
    let error = crate::sandbox::test_support::with_dev_gate_result(Some("1"), prepare);
    assert!(error.contains("sandbox"));
    let error = crate::sandbox::test_support::with_dev_gate_result(None, prepare);
    assert!(
        !error.contains("sandbox"),
        "a saved mesh flag must not enable the hidden developer feature: {error}"
    );
    assert!(!dir.path().join(".opencode").exists());
}

#[test]
fn both_background_adapters_reject_a_mismatched_binary_major() {
    use crate::preferences::launch_configurations::{capture, LaunchOverrides};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(if cfg!(windows) {
        "opencode.cmd"
    } else {
        "opencode"
    });
    let prefs = crate::preferences::AppPreferences::default();
    for actual in [1, 2] {
        let script = if cfg!(windows) {
            format!("@echo off\r\necho opencode v{actual}.2.3\r\n")
        } else {
            format!("#!/bin/sh\nprintf 'opencode v{actual}.2.3\\n'\n")
        };
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        for (harness, expected) in [("opencode", 1), ("opencode2", 2)] {
            let mut plan = capture(&prefs, harness, &LaunchOverrides::default()).unwrap();
            plan.harness.executable = Some(path.clone());
            let launch = crate::agent::background::resolve_plan(plan, &prefs).unwrap();
            let command = launch.command(dir.path(), &dir.path().join("result.txt"), "hello");
            assert_eq!(
                command.is_ok(),
                actual == expected,
                "harness={harness}, major={actual}"
            );
        }
    }
}

#[test]
fn api_preparation_creates_with_model_and_updates_a_distinct_regenerate_fork() {
    use serde_json::json;
    let mut calls = Vec::new();
    let id = prepare_identity(
        SessionIdModeRef::Assign("ses_assigned"),
        Some("anthropic/sonnet"),
        &mut |_, path, body| {
            calls.push((path.to_string(), body));
            Ok(json!({"data":{"id":"ses_assigned"}}))
        },
    )
    .unwrap();
    assert_eq!(id, "ses_assigned");
    assert_eq!(
        calls,
        [(
            "/api/session".into(),
            json!({"id":"ses_assigned","model":{"providerID":"anthropic","id":"sonnet"}})
        )]
    );
    calls.clear();
    let id = prepare_identity(
        SessionIdModeRef::Resume {
            id: "ses_parent",
            cause: ResumeCause::Regenerate,
        },
        Some("openai/gpt"),
        &mut |_, path, body| {
            calls.push((path.to_string(), body));
            Ok(json!({"data":{"id":"ses_sibling"}}))
        },
    )
    .unwrap();
    assert_eq!(id, "ses_sibling");
    assert_eq!(
        calls,
        [
            ("/api/session/ses_parent/fork".into(), json!({})),
            (
                "/api/session/ses_sibling/model".into(),
                json!({"model":{"providerID":"openai","id":"gpt"}})
            )
        ]
    );
    for cause in [ResumeCause::Explicit, ResumeCause::Startup] {
        calls.clear();
        let id = prepare_identity(
            SessionIdModeRef::Resume {
                id: "ses_parent",
                cause,
            },
            Some("openai/gpt"),
            &mut |method, path, body| {
                calls.push((path.to_string(), body));
                Ok(if method == "GET" {
                    json!({"data":{"id":"ses_parent"}})
                } else {
                    json!(null)
                })
            },
        )
        .unwrap();
        assert_eq!(id, "ses_parent");
        assert_eq!(calls[0].0, "/api/session/ses_parent");
        assert_eq!(calls[1].0, "/api/session/ses_parent/model");
    }
    assert!(prepare_identity(
        SessionIdModeRef::Resume {
            id: "ses_parent",
            cause: ResumeCause::Regenerate
        },
        None,
        &mut |_, _, _| Ok(json!({"data":{"id":"ses_parent"}}))
    )
    .is_err());
    assert!(prepare_identity(
        SessionIdModeRef::Assign("ses_assigned"),
        None,
        &mut |_, _, _| Err(SessionApiError::Failed("API unavailable".into()))
    )
    .is_err());
}

#[test]
fn resume_recovers_a_persisted_identity_after_a_failed_first_create() {
    use serde_json::json;
    for model in [None, Some("openai/gpt")] {
        assert!(prepare_identity(
            SessionIdModeRef::Assign("ses_retry"),
            model,
            &mut |_, _, _| Err(SessionApiError::Failed("temporary failure".into()))
        )
        .is_err());
        let mut calls = Vec::new();
        let id = prepare_identity(
            SessionIdModeRef::Resume {
                id: "ses_retry",
                cause: ResumeCause::Startup,
            },
            model,
            &mut |method, path, body| {
                calls.push((method.to_string(), path.to_string(), body));
                if method == "GET" {
                    Err(SessionApiError::MissingSession)
                } else {
                    Ok(json!({"data":{"id":"ses_retry"}}))
                }
            },
        )
        .unwrap();
        assert_eq!(id, "ses_retry");
        let mut create = json!({"id":"ses_retry"});
        if model.is_some() {
            create["model"] = json!({"providerID":"openai","id":"gpt"});
        }
        assert_eq!(
            calls,
            [
                ("GET".into(), "/api/session/ses_retry".into(), json!(null)),
                ("POST".into(), "/api/session".into(), create)
            ]
        );
    }
    let mut calls = 0;
    let error = prepare_identity(
        SessionIdModeRef::Resume {
            id: "ses_retry",
            cause: ResumeCause::Explicit,
        },
        None,
        &mut |_, _, _| {
            calls += 1;
            Err(SessionApiError::Failed("server unavailable".into()))
        },
    )
    .unwrap_err();
    assert_eq!(error, "server unavailable");
    assert_eq!(
        calls, 1,
        "an arbitrary lookup failure must not create a replacement session"
    );
    assert!(prepare_identity(
        SessionIdModeRef::Resume {
            id: "ses_retry",
            cause: ResumeCause::Regenerate
        },
        None,
        &mut |_, _, _| Err(SessionApiError::MissingSession)
    )
    .is_err());
}

#[test]
fn only_a_structured_missing_session_error_allows_recovery() {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    let response = |stdout: &str| std::process::Output {
        status: std::process::ExitStatus::from_raw(256),
        stdout: stdout.as_bytes().to_vec(),
        stderr: b"HTTP 404 Not Found".to_vec(),
    };
    assert!(matches!(
        decode_api_output(response(
            r#"{"_tag":"SessionNotFoundError","sessionID":"ses_retry"}"#
        )),
        Err(SessionApiError::MissingSession)
    ));
    for body in [
        "Session not found",
        r#"{"_tag":"ServerError","message":"Session not found"}"#,
        "{broken",
    ] {
        assert!(matches!(
            decode_api_output(response(body)),
            Err(SessionApiError::Failed(_))
        ));
    }
}

#[test]
fn v2_prefill_trims_lines_and_drops_blank_segments() {
    assert_eq!(
        OPENCODE2.prefill_args(" first \r\n\r\n second \n "),
        ["--prompt", "first second"]
    );
}

#[test]
fn provisioning_keeps_one_external_deny_when_rules_follow_it() {
    let dir = tempfile::tempdir().unwrap();
    config::provision(dir.path(), 3, None).unwrap();
    let path = config::config_path(dir.path(), 3);
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["permissions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"action":"shell","resource":"*","effect":"ask"}));
    std::fs::write(&path, doc.to_string()).unwrap();
    config::provision(dir.path(), 3, None).unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    config::provision(dir.path(), 3, None).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    let doc: serde_json::Value = serde_json::from_str(&first).unwrap();
    let rules = doc["permissions"].as_array().unwrap();
    assert_eq!(
        rules
            .iter()
            .filter(|r| r["action"] == "external_directory")
            .count(),
        1
    );
    assert_eq!(rules.last().unwrap()["action"], "external_directory");
}

#[test]
fn fresh_and_resume_use_standalone_assigned_identity_without_v1_flags() {
    let id = OPENCODE2.new_session_id();
    assert_eq!(
        crate::http::request::parse_session_id_for_provider("opencode2", &id),
        Some(id.clone())
    );
    assert!(!OPENCODE2.self_assigns_session_id());
    assert!(!OPENCODE2.captures_session_id_from_pty());
    let config = ResolvedAgentConfig {
        model: Some("anthropic/claude-sonnet-5-5".into()),
        ..Default::default()
    };
    for session in [
        SessionIdModeRef::Assign(&id),
        SessionIdModeRef::Resume {
            id: &id,
            cause: ResumeCause::Explicit,
        },
        SessionIdModeRef::Resume {
            id: &id,
            cause: ResumeCause::Regenerate,
        },
    ] {
        let prepared = default_prepare(
            &OPENCODE2,
            HarnessLaunchInput {
                platform: Platform::Linux,
                runtime: EnvType::Windows,
                session,
                config: &config,
                prefill: Some("hello"),
                sandbox: false,
            },
        );
        assert_eq!(
            prepared.recipe.base_args,
            [
                "--auto",
                "--standalone",
                "--session",
                &id,
                "--prompt",
                "hello"
            ]
        );
        assert!(!prepared
            .recipe
            .base_args
            .iter()
            .any(|a| a == "--model" || a == "--fork"));
        assert!(prepared.capabilities.supports_model_override);
    }
    assert!(model_ref("claude-sonnet-5-5").is_err());
    assert!(model_ref("anthropic/model#variant").is_err());
    assert_eq!(
        model_ref("custom/vendor/model").unwrap(),
        serde_json::json!({"providerID":"custom","id":"vendor/model"})
    );
}

#[test]
fn project_options_preserve_users_and_isolate_two_node_routes_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let path = config::config_path(dir.path(), 1);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path,r#"{"plugins":[{"package":"user-plugin","options":{"keep":true}}],"permissions":[{"action":"shell","resource":"git push *","effect":"deny"}],"username":"keep"}"#).unwrap();
    config::provision(dir.path(), 1, Some(("ses_nodeOne", 41234))).unwrap();
    config::provision(dir.path(), 2, Some(("ses_nodeTwo", 41234))).unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    config::provision(dir.path(), 1, Some(("ses_nodeOne", 41234))).unwrap();
    assert_eq!(first, std::fs::read_to_string(&path).unwrap());
    let doc: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(doc["username"], "keep");
    assert_eq!(doc["plugins"][0]["package"], "user-plugin");
    assert_eq!(
        doc["plugins"][1]["options"]["sessions"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(doc["permissions"][1]["action"], "external_directory");
    let second_before = std::fs::read(config::config_path(dir.path(), 2)).unwrap();
    config::provision(dir.path(), 1, Some(("ses_restarted", 41234))).unwrap();
    assert_eq!(
        second_before,
        std::fs::read(config::config_path(dir.path(), 2)).unwrap()
    );
    let doc = crate::agent::managed_config::read_document(&path)
        .unwrap()
        .root;
    assert!(doc["plugins"][1]["options"]["sessions"]["ses_nodeOne"].is_null());
    assert_eq!(
        doc["plugins"][1]["options"]["sessions"]["ses_restarted"]["nodeId"],
        1
    );
}

#[test]
fn malformed_managed_config_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = config::config_path(dir.path(), 1);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{broken").unwrap();
    assert!(config::provision(dir.path(), 1, None).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{broken");
}

#[test]
fn user_project_jsonc_is_left_to_the_cli_and_never_shadowed_for_v1() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".opencode")).unwrap();
    let path = dir.path().join(".opencode/opencode.jsonc");
    let original = "{ // user config\n\"plugins\":[\"user-plugin\",],\"username\":\"keep\",}\n";
    std::fs::write(&path, original).unwrap();
    config::provision(dir.path(), 12, Some(("ses_jsoncSession", 2992))).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    assert!(!dir.path().join(".opencode/opencode.json").exists());
    assert!(config::config_path(dir.path(), 12).exists());
}

#[test]
fn background_has_project_deny_rules_model_and_unchanged_json_decoder() {
    use crate::preferences::launch_configurations::{capture, LaunchOverrides};
    let prefs = crate::preferences::AppPreferences::default();
    let mut plan = capture(&prefs, "opencode2", &LaunchOverrides::default()).unwrap();
    plan.model = Some("anthropic/claude-sonnet-5-5".into());
    let launch = crate::agent::background::resolve_plan(plan, &prefs).unwrap();
    assert_eq!(launch.args, ["--model", "anthropic/claude-sonnet-5-5"]);
    let recipe = OPENCODE2.background_recipe(Platform::Linux).unwrap();
    assert_eq!(
        recipe.spawn.base_args,
        ["run", "--standalone", "--format", "json"]
    );
    assert!(recipe
        .env
        .iter()
        .all(|(key, _)| key != "OPENCODE_CONFIG_CONTENT"));
    let config: serde_json::Value = serde_json::from_str(&recipe.project_files[0].1).unwrap();
    assert_eq!(config["permissions"][0]["effect"], "deny");
    assert_eq!(config["permissions"][1]["action"], "external_directory");
    assert_eq!(
        launch
            .final_output(
                "{\"type\":\"text\",\"part\":{\"type\":\"text\",\"text\":\"Done\"}}\n".into(),
                Path::new("unused")
            )
            .unwrap(),
        "Done"
    );
}
