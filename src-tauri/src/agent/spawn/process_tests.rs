use super::inject_attention_hook;
use tempfile::TempDir;

fn read_injected_settings(project: &std::path::Path) -> serde_json::Value {
    let path = project.join(".claude").join("settings.local.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("settings.local.json not written: {}", e));
    serde_json::from_str(&content).expect("settings.local.json is not valid JSON")
}

/// The canonical Buildmesh hook command template, mirrored from
/// `inject_attention_hook` so tests can compare URLs / command substrings
/// without depending on internal string literals.
const BUILDMESH_HOOK_COMMAND_MARKERS: &[&str] =
    &["BUILDMESH_PORT", "BUILDMESH_SESSION_ID", "/api/attention/"];

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

// =====================================================================
// Issue #1370 — preserve user-authored hooks and harden attention delivery
//
// The pre-#1370 implementation did `settings["hooks"] = expected_hooks`,
// which silently wiped any pre-existing matcher group the user had
// authored under `Notification` / `Stop` (or sibling events like
// `PreToolUse` / `UserPromptSubmit`). The tests below pin the new
// per-event additive merge: user entries survive byte-for-byte, the
// Buildmesh handler is appended as a sibling, repeat injections add no
// duplicates, writes are atomic (no .tmp residue), and malformed files
// refuse to overwrite.
//
// The attention route already classifies Claude `Notification` w/ message
// "needs your permission" → MarkInput, plain `Stop` → transcript-scan
// decision. The route tests in `http/routes/attention.rs` cover those;
// this module owns the hook WRITER invariants.
// =====================================================================

/// A pre-existing user-authored `Notification` matcher group is preserved
/// byte-for-byte and the Buildmesh handler is appended as a sibling group.
/// Regression guard for the pre-#1370 wholesale overwrite.
#[test]
fn attention_hook_preserves_user_notification_handler() {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let user_handler = serde_json::json!({
        "type": "command",
        "command": "/usr/local/bin/user-hook.sh",
        "timeout": 30,
    });
    let existing = serde_json::json!({
        "hooks": {
            "Notification": [
                { "matcher": "idle_prompt", "hooks": [user_handler] }
            ]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();

    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    let notification = settings["hooks"]["Notification"]
        .as_array()
        .expect("Notification must be an array");
    assert_eq!(
        notification.len(),
        2,
        "Notification must carry BOTH user and Buildmesh matcher groups; got {settings:#}"
    );
    // User matcher group at index 0 — preserved byte-for-byte.
    assert_eq!(notification[0]["matcher"], "idle_prompt");
    assert_eq!(
        notification[0]["hooks"][0]["command"],
        "/usr/local/bin/user-hook.sh"
    );
    assert_eq!(
        notification[0]["hooks"][0]["timeout"], 30,
        "user-authored sibling field (timeout) must survive the merge"
    );
    // Buildmesh matcher group appended at index 1.
    assert_eq!(
        notification[1]["matcher"], "",
        "Buildmesh Notification entry must carry the documented catch-all matcher"
    );
    let buildmesh_command = notification[1]["hooks"][0]["command"]
        .as_str()
        .expect("Buildmesh handler appended");
    for marker in BUILDMESH_HOOK_COMMAND_MARKERS {
        assert!(
            buildmesh_command.contains(marker),
            "Buildmesh command must carry `{marker}`; got {buildmesh_command}"
        );
    }
}

/// Unrelated Claude Code hook events the Buildmesh integration doesn't
/// touch (`PreToolUse`, `UserPromptSubmit`, `SessionStart`, …) survive
/// the merge byte-for-byte. Pin a representative pair.
#[test]
fn attention_hook_preserves_unrelated_events() {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo blocked" }] }
            ],
            "UserPromptSubmit": [
                { "hooks": [{ "type": "command", "command": "/opt/user-prompt.sh" }] }
            ],
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();

    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    assert_eq!(
        settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "echo blocked",
        "user-authored PreToolUse hook must survive"
    );
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "/opt/user-prompt.sh",
        "user-authored UserPromptSubmit hook must survive"
    );
    // Notification / Stop are now populated by the merge.
    assert!(settings["hooks"]["Notification"].is_array());
    assert!(settings["hooks"]["Stop"].is_array());
}

/// Repeat injections add no duplicate Buildmesh matcher groups. The
/// marker-anchored merge must update in place — not append — when the
/// canonical command anchors are recognised.
#[test]
fn attention_hook_repeat_injection_creates_no_duplicates() {
    let temp = TempDir::new().unwrap();
    for _ in 0..3 {
        inject_attention_hook(temp.path()).unwrap();
    }
    let settings = read_injected_settings(temp.path());
    for event in ["Notification", "Stop"] {
        let groups = settings["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("{event} not an array: {settings:#}"));
        assert_eq!(
            groups.len(),
            1,
            "{event} must carry exactly one Buildmesh matcher group after 3 injects; got {groups:?}"
        );
    }
}

/// Atomic write leaves no `.tmp` residue in the `.claude/` directory.
/// Mirrors the Codex precedent (`codex.rs:998-1007`,
/// `inject_atomic_write_leaves_no_tmp_residue`) and the Grok pattern at
/// `grok.rs::inject_atomic_write_leaves_no_tmp_residue`.
#[test]
fn attention_hook_atomic_write_leaves_no_tmp_residue() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let claude_dir = temp.path().join(".claude");
    let entries = std::fs::read_dir(&claude_dir).unwrap();
    let tmp_files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(
        tmp_files.is_empty(),
        "atomic write must not leave .tmp residue; found {tmp_files:?}"
    );
}

/// When the existing Buildmesh handlers are already present, a second
/// injection is a no-op — neither bytes nor `mtime` change. We sleep
/// briefly between writes so the mtime check is deterministic across
/// filesystems with low-resolution mtime (Windows FAT32 = 2s).
#[test]
fn attention_hook_idempotent_rerun_does_not_rewrite_when_already_wired() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();
    let path = temp.path().join(".claude").join("settings.local.json");
    let first_bytes = std::fs::read(&path).unwrap();
    let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    inject_attention_hook(temp.path()).unwrap();
    let second_bytes = std::fs::read(&path).unwrap();
    let second_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

    assert_eq!(first_bytes, second_bytes, "byte-identical re-run");
    assert_eq!(
        first_mtime, second_mtime,
        "idempotent re-run must NOT rewrite the file (no mtime change)"
    );
}

/// Issue #1370 review fix: refuse to silently wipe a malformed
/// user-authored file. A trailing comma, partial edit, or syntax error
/// must NOT cause `ensure_hooks_json` to fall back to `{}` and overwrite
/// the user's data. The function returns `Err`, the spawn path surfaces
/// it as a provision failure (`SignalHealth::Unavailable` lifecycle
/// event), and the user's content survives intact.
#[test]
fn attention_hook_refuses_to_overwrite_malformed_user_file() {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let path = claude_dir.join("settings.local.json");
    // Deliberately malformed: trailing comma + unmatched brace.
    let malformed = "{ \"hooks\": { \"Notification\": [],, }";
    std::fs::write(&path, malformed).unwrap();

    let result = inject_attention_hook(temp.path());
    let err = result
        .err()
        .expect("provision must refuse a malformed existing file");
    assert!(
        err.contains("malformed"),
        "Err message must explain the refusal; got {err}"
    );

    // The user's malformed content must survive intact — the previous
    // behaviour (silently overwriting with `{}`) clobbered user data;
    // the new behaviour leaves it for the user to repair.
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        on_disk, malformed,
        "malformed file content must NOT be overwritten"
    );
}

/// The positive case for the malformed-file pin: a missing file is
/// treated as `{}` (fresh install) and written normally. Locks the
/// happy path so a future refactor that conflates "missing" with
/// "malformed" trips here.
#[test]
fn attention_hook_treats_missing_file_as_empty_settings() {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    // Note: no file at settings.local.json — fresh install.
    assert!(!claude_dir.join("settings.local.json").exists());

    inject_attention_hook(temp.path()).unwrap();
    let written = std::fs::read_to_string(claude_dir.join("settings.local.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(value["hooks"]["Notification"][0]["matcher"], "");
    assert!(value["hooks"]["Stop"].is_array());
}

/// A pre-existing user-authored matcher group with a non-catch-all
/// matcher (e.g. `matcher: "permission_prompt"`) survives AND the
/// Buildmesh catch-all matcher is appended. The pre-#1370 implementation
/// wiped both, breaking any user who narrowed Notification to a
/// specific event type.
#[test]
fn attention_hook_user_narrow_matcher_survives_and_buildmesh_is_appended() {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let user_handler = serde_json::json!({
        "type": "command",
        "command": "/usr/local/bin/user-permission-hook.sh",
    });
    let existing = serde_json::json!({
        "hooks": {
            "Notification": [
                { "matcher": "permission_prompt", "hooks": [user_handler] }
            ]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();

    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    let notification = settings["hooks"]["Notification"]
        .as_array()
        .expect("Notification must be an array");
    assert_eq!(notification.len(), 2);
    // User's narrow matcher preserved at index 0.
    assert_eq!(notification[0]["matcher"], "permission_prompt");
    assert_eq!(
        notification[0]["hooks"][0]["command"],
        "/usr/local/bin/user-permission-hook.sh"
    );
    // Buildmesh catch-all appended at index 1.
    assert_eq!(notification[1]["matcher"], "");
}

/// The marker predicate (`is_buildmesh_handler`) must recognise a
/// handler by the canonical command anchors alone — future URL refactors
/// (adding `?token=…`, swapping `localhost` for `127.0.0.1`, etc.) must
/// keep the merge stable.
#[test]
fn attention_hook_marker_predicate_recognises_canonical_command() {
    use super::process::is_buildmesh_handler;
    let buildmesh = serde_json::json!({
        "type": "command",
        "command": "curl ... http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true",
    });
    assert!(is_buildmesh_handler(&buildmesh));

    let user = serde_json::json!({
        "type": "command",
        "command": "/usr/local/bin/user-hook.sh",
    });
    assert!(
        !is_buildmesh_handler(&user),
        "user-authored handlers must NOT match the Buildmesh marker"
    );

    let bare = serde_json::json!({"type": "command"});
    assert!(!is_buildmesh_handler(&bare));
}

/// Stop entries must NOT carry a `matcher` field. Claude Code's docs
/// warn that matchers on `Stop` are ignored with a warning; omitting the
/// field keeps the on-disk shape minimal.
#[test]
fn attention_hook_stop_entry_omits_matcher() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();
    let settings = read_injected_settings(temp.path());
    let stop_group = &settings["hooks"]["Stop"][0];
    assert!(
        stop_group.get("matcher").is_none(),
        "Stop matcher group must not carry a matcher field (Claude Code \
         ignores matchers on Stop with a warning); got {stop_group:#}"
    );
}
