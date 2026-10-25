use super::*;

const ANCHORS: &[&str] = &["/api/attention/", "BUILDMESH_PORT"];

const OWNED_COMMAND: Ownership = Ownership {
    fields: &["command"],
    anchors: ANCHORS,
};

fn flat_json_shape() -> JsonHookShape {
    JsonHookShape {
        root_key: Some("hooks"),
        replace_root: false,
        required_root: &[],
        handler: HandlerShape::Command,
        ownership: OWNED_COMMAND,
        events: &[EventRule {
            name: "stop",
            optional: false,
            merge: EventMerge::Flat,
        }],
    }
}

fn provision_spec<'a>(format: HookFormat, command: &'a str) -> HookProvisioningSpec<'a> {
    HookProvisioningSpec {
        harness: "test",
        file: "hooks.json",
        label: "test hooks.json",
        command,
        event_commands: &[],
        format,
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Cursor's shape: one root object, one flat event, a required `version`.
#[test]
fn json_flat_handler_is_pinned_byte_for_byte() {
    let temp = tempfile::tempdir().unwrap();
    let format = HookFormat::Json(JsonHookShape {
        required_root: &[("version", RequiredRoot::Integer(1))],
        ..flat_json_shape()
    });

    let outcome =
        provision_attention_hooks(&provision_spec(format, "callback"), temp.path()).unwrap();

    assert!(outcome.written);
    assert_eq!(
        read(&outcome.path),
        "{\n  \"hooks\": {\n    \"stop\": [\n      {\n        \"command\": \"callback\"\n      }\n    ]\n  },\n  \"version\": 1\n}"
    );
}

/// Claude Code's shape: matcher groups, a typed handler, and a catch-all
/// `matcher: ""` on one event.
#[test]
fn json_matcher_groups_are_pinned_byte_for_byte() {
    let temp = tempfile::tempdir().unwrap();
    let format = HookFormat::Json(JsonHookShape {
        root_key: Some("hooks"),
        replace_root: false,
        required_root: &[],
        handler: HandlerShape::TypedCommand,
        ownership: OWNED_COMMAND,
        events: &[
            EventRule {
                name: "Notification",
                optional: false,
                merge: EventMerge::MatcherGroup { matcher: Some("") },
            },
            EventRule {
                name: "Stop",
                optional: false,
                merge: EventMerge::MatcherGroup { matcher: None },
            },
        ],
    });

    let outcome =
        provision_attention_hooks(&provision_spec(format, "callback"), temp.path()).unwrap();

    assert_eq!(
        read(&outcome.path),
        "{\n  \"hooks\": {\n    \"Notification\": [\n      {\n        \"hooks\": [\n          {\n            \"command\": \"callback\",\n            \"type\": \"command\"\n          }\n        ],\n        \"matcher\": \"\"\n      }\n    ],\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"command\": \"callback\",\n            \"type\": \"command\"\n          }\n        ]\n      }\n    ]\n  }\n}"
    );
}

/// AGY's shape: Buildmesh owns one whole namespace, so a re-provision
/// replaces it rather than merging event by event, and an omitted optional
/// event disappears.
#[test]
fn json_replace_root_namespace_is_pinned_and_drops_stale_keys() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hooks.json");
    std::fs::write(
        &path,
        "{\"buildmesh-attention\":{\"stale\":true,\"Stop\":[]},\"user-tool\":{\"keep\":1}}",
    )
    .unwrap();
    let format = HookFormat::Json(JsonHookShape {
        root_key: Some("buildmesh-attention"),
        replace_root: true,
        required_root: &[],
        handler: HandlerShape::TypedCommand,
        ownership: OWNED_COMMAND,
        events: &[
            EventRule {
                name: "Stop",
                optional: false,
                merge: EventMerge::Flat,
            },
            EventRule {
                name: "PreToolUse",
                optional: true,
                merge: EventMerge::MatcherGroup {
                    matcher: Some("ask_question"),
                },
            },
        ],
    });
    let overrides = [("PreToolUse", "pre")];
    let provision_spec = HookProvisioningSpec {
        event_commands: &overrides,
        ..provision_spec(format, "stop")
    };

    let outcome = provision_attention_hooks(&provision_spec, temp.path()).unwrap();

    assert_eq!(
        read(&outcome.path),
        "{\n  \"buildmesh-attention\": {\n    \"PreToolUse\": [\n      {\n        \"hooks\": [\n          {\n            \"command\": \"pre\",\n            \"type\": \"command\"\n          }\n        ],\n        \"matcher\": \"ask_question\"\n      }\n    ],\n    \"Stop\": [\n      {\n        \"command\": \"stop\",\n        \"type\": \"command\"\n      }\n    ]\n  },\n  \"user-tool\": {\n    \"keep\": 1\n  }\n}"
    );
}

/// Grok's shape: question events drop our handler from every group and
/// append one group of their own, keeping the user's siblings.
#[test]
fn json_question_event_replaces_only_our_group() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hooks.json");
    std::fs::write(
        &path,
        "{\"hooks\":{\"PreToolUse\":[{\"matcher\":\"Bash\",\"hooks\":[{\"type\":\"command\",\"command\":\"echo user\"}]},{\"matcher\":\"^ask_user_question$\",\"hooks\":[{\"type\":\"command\",\"command\":\"stale /api/attention/ BUILDMESH_PORT\"}]}]}}",
    )
    .unwrap();
    let format = HookFormat::Json(JsonHookShape {
        root_key: Some("hooks"),
        replace_root: false,
        required_root: &[],
        handler: HandlerShape::TypedCommand,
        ownership: Ownership {
            fields: &["url", "command"],
            ..OWNED_COMMAND
        },
        events: &[EventRule {
            name: "PreToolUse",
            optional: false,
            merge: EventMerge::Question {
                matcher: "^ask_user_question$",
            },
        }],
    });

    provision_attention_hooks(&provision_spec(format, "callback"), temp.path()).unwrap();

    let written: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
    let groups = written["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{groups:?}");
    assert_eq!(groups[0]["matcher"], "Bash");
    assert_eq!(groups[1]["matcher"], "^ask_user_question$");
    assert_eq!(groups[1]["hooks"][0]["command"], "callback");
    assert_eq!(groups[1]["hooks"].as_array().unwrap().len(), 1);
}

/// Every format: a second provision over an already-correct file is a
/// no-op, byte-identical and reported as not written.
#[test]
fn re_provision_is_byte_identical_and_reports_no_write() {
    let json_temp = tempfile::tempdir().unwrap();
    let command = "curl $BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID";
    let json_spec = provision_spec(HookFormat::Json(flat_json_shape()), command);
    let first = provision_attention_hooks(&json_spec, json_temp.path()).unwrap();
    let json_bytes = read(&first.path);
    let second = provision_attention_hooks(&json_spec, json_temp.path()).unwrap();
    assert!(
        !second.written,
        "an unchanged JSON file must not be rewritten"
    );
    assert_eq!(read(&second.path), json_bytes);

    let toml_temp = tempfile::tempdir().unwrap();
    let toml_format = HookFormat::Toml(TomlHookShape {
        root_key: "hooks",
        timeout: 10,
        ownership: toml_ownership(),
        events: &[TomlEventRule {
            name: "Stop",
            matcher: None,
        }],
    });
    let toml_spec = provision_spec(toml_format, command);
    let first = provision_attention_hooks(&toml_spec, toml_temp.path()).unwrap();
    let toml_bytes = read(&first.path);
    let second = provision_attention_hooks(&toml_spec, toml_temp.path()).unwrap();
    assert!(
        !second.written,
        "an unchanged TOML file must not be rewritten"
    );
    assert_eq!(read(&second.path), toml_bytes);
}

fn toml_ownership() -> Ownership {
    Ownership {
        fields: &["command"],
        anchors: &["BUILDMESH_SESSION_ID", "BUILDMESH_PORT"],
    }
}

/// Kimi's TOML shape, including the comment/formatting preservation the
/// `toml_edit` merge provides.
#[test]
fn toml_hook_array_is_pinned_byte_for_byte() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    std::fs::write(
        &path,
        "# keep my preferences\ndefault_model = 'custom-model'\n\n[[hooks]]\n# user hook\nevent = 'Stop'\ncommand = 'echo user'\ntimeout = 2\n",
    )
    .unwrap();
    let format = HookFormat::Toml(TomlHookShape {
        root_key: "hooks",
        timeout: 10,
        ownership: toml_ownership(),
        events: &[
            TomlEventRule {
                name: "Stop",
                matcher: None,
            },
            TomlEventRule {
                name: "PreToolUse",
                matcher: Some("AskUserQuestion|ExitPlanMode"),
            },
        ],
    });

    let spec = HookProvisioningSpec {
        file: "config.toml",
        ..provision_spec(format, "callback")
    };
    provision_attention_hooks(&spec, temp.path()).unwrap();

    assert_eq!(
        read(&path),
        "# keep my preferences\ndefault_model = 'custom-model'\n\n[[hooks]]\n# user hook\nevent = 'Stop'\ncommand = 'echo user'\ntimeout = 2\n\n[[hooks]]\nevent = \"Stop\"\ncommand = \"callback\"\ntimeout = 10\n\n[[hooks]]\nevent = \"PreToolUse\"\nmatcher = \"AskUserQuestion|ExitPlanMode\"\ncommand = \"callback\"\ntimeout = 10\n"
    );
}

/// Both formats resolve each event's command the same way: a declared
/// override wins, otherwise the spec's default callback is written.
#[test]
fn toml_events_honour_declared_command_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let format = HookFormat::Toml(TomlHookShape {
        root_key: "hooks",
        timeout: 10,
        ownership: toml_ownership(),
        events: &[
            TomlEventRule {
                name: "Stop",
                matcher: None,
            },
            TomlEventRule {
                name: "PreToolUse",
                matcher: Some("AskUserQuestion|ExitPlanMode"),
            },
        ],
    });
    let overrides = [("PreToolUse", "gate")];
    let spec = HookProvisioningSpec {
        file: "config.toml",
        event_commands: &overrides,
        ..provision_spec(format, "default")
    };

    let outcome = provision_attention_hooks(&spec, temp.path()).unwrap();

    let written: toml_edit::DocumentMut = read(&outcome.path).parse().unwrap();
    let commands: Vec<&str> = written["hooks"]
        .as_array_of_tables()
        .unwrap()
        .iter()
        .map(|hook| hook["command"].as_str().unwrap())
        .collect();
    assert_eq!(commands, vec!["default", "gate"]);
}

/// The malformed-file refusal is produced in one place, names the path,
/// and leaves the user's bytes untouched for every harness label.
#[test]
fn malformed_documents_are_refused_with_the_one_wording() {
    for label in [
        "agy hooks.json",
        "cursor hooks.json",
        "grok hooks.json",
        "settings.local.json",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hooks.json");
        let original = "{ \"hooks\": [],, }";
        std::fs::write(&path, original).unwrap();
        let spec = HookProvisioningSpec {
            label,
            ..provision_spec(HookFormat::Json(flat_json_shape()), "callback")
        };

        let error = provision_attention_hooks(&spec, temp.path()).unwrap_err();

        let message = error.to_string();
        assert!(
            message.starts_with(&format!("refusing to overwrite malformed {path:?}: ")),
            "one frozen refusal wording per harness: {message}"
        );
        assert!(
            message.ends_with(". Repair or remove the file and retry"),
            "the refusal must tell the user what to do: {message}"
        );
        assert_eq!(read(&path), original, "the user's bytes must survive");
    }

    // The TOML arm reaches the same one refusal, with its own label.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let original = "hooks = [";
    std::fs::write(&path, original).unwrap();
    let format = HookFormat::Toml(TomlHookShape {
        root_key: "hooks",
        timeout: 10,
        ownership: toml_ownership(),
        events: &[],
    });
    let spec = HookProvisioningSpec {
        file: "config.toml",
        label: "Kimi config",
        ..provision_spec(format, "callback")
    };

    let error = provision_attention_hooks(&spec, temp.path()).unwrap_err();

    let message = error.to_string();
    assert!(
        message.starts_with(&format!("refusing to overwrite malformed {path:?}: ")),
        "the TOML arm must use the same frozen refusal: {message}"
    );
    assert!(message.ends_with(". Repair or remove the file and retry"));
    assert_eq!(read(&path), original, "the user's bytes must survive");
}

/// An unreadable target is a different failure: the module names the
/// harness's own file rather than claiming it is malformed.
#[test]
fn unreadable_documents_name_the_harness_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hooks.json");
    std::fs::create_dir(&path).unwrap();
    let format = HookFormat::Json(flat_json_shape());

    let error =
        provision_attention_hooks(&provision_spec(format, "callback"), temp.path()).unwrap_err();

    let message = error.to_string();
    assert!(
        message.starts_with(&format!(
            "failed to read test hooks.json at {}: ",
            path.display()
        )),
        "{message}"
    );
    assert!(path.is_dir());
}

/// A required root entry that does not match is the user's schema
/// mismatch, refused with the exact field name.
#[test]
fn required_root_values_are_checked_before_any_write() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hooks.json");
    std::fs::write(&path, "{\"version\": 2, \"hooks\": {\"stop\": []}}").unwrap();
    let format = HookFormat::Json(JsonHookShape {
        required_root: &[("version", RequiredRoot::Integer(1))],
        ..flat_json_shape()
    });

    let error =
        provision_attention_hooks(&provision_spec(format, "callback"), temp.path()).unwrap_err();

    assert_eq!(
        error.to_string(),
        "test hooks.json `version` must be 1; got 2"
    );
    assert_eq!(read(&path), "{\"version\": 2, \"hooks\": {\"stop\": []}}");
}

/// A missing required root entry is a document change of its own: a file whose
/// hooks already match must still be rewritten so the schema reaches disk.
#[test]
fn a_missing_required_root_entry_is_persisted_even_when_hooks_match() {
    let temp = tempfile::tempdir().unwrap();
    let format = HookFormat::Json(JsonHookShape {
        required_root: &[("version", RequiredRoot::Integer(1))],
        ..flat_json_shape()
    });
    let command = "curl $BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID";
    let spec = provision_spec(format, command);
    assert!(
        provision_attention_hooks(&spec, temp.path())
            .unwrap()
            .written
    );

    // Drop only the required entry, leaving the provisioned hooks untouched.
    let path = temp.path().join("hooks.json");
    let mut document: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
    document.as_object_mut().unwrap().remove("version");
    std::fs::write(&path, serde_json::to_string(&document).unwrap()).unwrap();

    let repaired = provision_attention_hooks(&spec, temp.path()).unwrap();

    assert!(repaired.written, "the schema repair must reach disk");
    let on_disk: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
    assert_eq!(on_disk["version"], 1);
    assert_eq!(on_disk["hooks"]["stop"].as_array().unwrap().len(), 1);
    assert_eq!(on_disk["hooks"]["stop"][0]["command"], command);
}

/// Ownership survives an encoded WindowsInterop callback, and Kimi's
/// wrapped callback is decoded from its embedded PowerShell segment.
#[test]
fn ownership_survives_encoded_callbacks() {
    assert!(OWNED_COMMAND.owns_handler(&serde_json::json!({
        "command": "curl http://localhost:$BUILDMESH_PORT/api/attention/1"
    })));
    assert!(!OWNED_COMMAND.owns_handler(&serde_json::json!({ "command": "echo user" })));

    let payload = "curl $BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID";
    let encoded = format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        crate::env::encode_powershell(payload)
    );
    assert!(OWNED_COMMAND.owns_handler(&serde_json::json!({ "command": encoded })));

    let embedded = format!("if defined BUILDMESH_SESSION_ID {encoded}");
    let wrapped = Ownership {
        fields: &["command"],
        anchors: &["BUILDMESH_SESSION_ID", "BUILDMESH_PORT"],
    };
    assert!(wrapped.owns_handler(&serde_json::json!({ "command": embedded })));

    // A wrapper that embeds an unrelated PowerShell payload is still not ours.
    let unrelated = format!(
        "if defined OTHER powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        crate::env::encode_powershell("echo hello")
    );
    assert!(!wrapped.owns_handler(&serde_json::json!({ "command": unrelated })));
}
