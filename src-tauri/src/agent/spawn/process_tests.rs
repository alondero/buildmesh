#![allow(unused_imports)]

use super::*;
use tempfile::TempDir;

fn read_injected_settings(project: &std::path::Path) -> serde_json::Value {
    let path = project.join(".claude").join("settings.local.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("settings.local.json not written: {}", e));
    serde_json::from_str(&content).expect("settings.local.json is not valid JSON")
}

/// The Notification hook must fire on EVERY notification type, not just
/// `idle_prompt`. An empty matcher is Claude Code's "match all" — without it
/// the hook ignores `permission_prompt` notifications, so the user is never
/// alerted when an agent asks to run a tool or otherwise needs a decision.
/// Regression guard for the "only alerted after the agent finishes" gap.
#[test]
fn attention_hook_notification_matcher_is_catch_all() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    let notification = &settings["hooks"]["Notification"][0];
    assert_eq!(
        notification["matcher"], "",
        "Notification matcher must be empty (catch-all) so permission_prompt \
             notifications alert the user, not just idle_prompt"
    );
    let command = notification["hooks"][0]["command"]
        .as_str()
        .expect("notification hook command should be a string");
    assert!(
        command.contains("/api/attention/"),
        "notification hook should POST to the attention endpoint, got: {command}"
    );
}

/// A `Stop` hook fires the instant the agent finishes a turn, so the user is
/// alerted immediately rather than waiting for the `idle_prompt` idle timer.
#[test]
fn attention_hook_includes_stop_event() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .expect("Stop hook command should be present so turn-end alerts fire immediately");
    assert!(
        command.contains("/api/attention/"),
        "Stop hook should POST to the attention endpoint, got: {command}"
    );
}

/// Both hooks must forward the hook's stdin JSON as the POST body (issue
/// #878). Claude Code pipes `{hook_event_name, transcript_path, …}` into
/// the command; without `--data-binary @-` the backend gets an empty body
/// and cannot tell "turn ended, user needed" from "turn ended, waiting on
/// background tasks".
#[test]
fn attention_hook_forwards_stdin_payload() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    for (event, path) in [
        (
            "Notification",
            &settings["hooks"]["Notification"][0]["hooks"][0],
        ),
        ("Stop", &settings["hooks"]["Stop"][0]["hooks"][0]),
    ] {
        let command = path["command"].as_str().unwrap();
        assert!(
            command.contains("--data-binary @-"),
            "{event} hook must forward stdin as the POST body, got: {command}"
        );
        assert!(
            command.contains("Content-Type: application/json"),
            "{event} hook must declare a JSON body, got: {command}"
        );
    }
}

/// Injection is idempotent: a second call over an already-correct file must
/// not rewrite it (the early-return guard) and must leave it parseable.
#[test]
fn attention_hook_injection_is_idempotent() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();
    let first = read_injected_settings(temp.path());
    inject_attention_hook(temp.path()).unwrap();
    let second = read_injected_settings(temp.path());
    assert_eq!(first, second, "second injection should be a no-op");
}

/// Injection must preserve unrelated keys already present in the user's
/// settings.local.json (e.g. `permissions`) — it only owns `hooks`.
#[test]
fn attention_hook_preserves_other_settings() {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.local.json"),
        r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#,
    )
    .unwrap();

    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    assert_eq!(
        settings["permissions"]["allow"][0], "Bash(ls:*)",
        "pre-existing permissions must survive hook injection"
    );
    assert_eq!(settings["hooks"]["Notification"][0]["matcher"], "");
}
