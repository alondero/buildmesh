use super::*;

#[test]
fn an_interrupt_needs_a_transcript_write_after_the_key_press() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("messages.jsonl");
    std::fs::write(
            &path,
            "{\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Write an essay\"}]}}\n",
        )
        .unwrap();
    let written = std::fs::metadata(&path).unwrap().modified().unwrap();
    let second = std::time::Duration::from_secs(1);
    assert!(interrupted_turn_at(
        TranscriptFormat::Mcode,
        &path,
        written - second
    ));
    assert!(!interrupted_turn_at(
        TranscriptFormat::Mcode,
        &path,
        written + second
    ));
}

#[test]
fn transcript_format_for_harness_routes_each_format() {
    assert_eq!(
        TranscriptFormat::for_harness("codex"),
        Some(TranscriptFormat::Codex)
    );
    assert_eq!(
        TranscriptFormat::for_harness("commandcode"),
        Some(TranscriptFormat::CommandCode)
    );
    assert_eq!(
        TranscriptFormat::for_harness("cursor"),
        Some(TranscriptFormat::Cursor)
    );
    assert_eq!(
        TranscriptFormat::for_harness("agy"),
        Some(TranscriptFormat::Agy)
    );
    assert_eq!(
        TranscriptFormat::for_harness("grok"),
        Some(TranscriptFormat::Grok)
    );
    assert_eq!(
        TranscriptFormat::for_harness("mcode"),
        Some(TranscriptFormat::Mcode)
    );
    assert_eq!(
        TranscriptFormat::for_harness("muse"),
        Some(TranscriptFormat::Muse)
    );
    assert_eq!(
        TranscriptFormat::for_harness("opencode"),
        Some(TranscriptFormat::OpenCode2)
    );
    assert_eq!(
        TranscriptFormat::for_harness("cline"),
        Some(TranscriptFormat::Cline)
    );
    for id in ["anthropic", "claude"] {
        assert_eq!(
            TranscriptFormat::for_harness(id),
            Some(TranscriptFormat::ClaudeCode),
            "{id} is Claude-backed and must stay on Claude Code"
        );
    }
}

#[test]
fn transcript_format_for_harness_rejects_unwired_harnesses() {
    for id in ["kimi", "dsh", "freebuff", "terminal", "", "totally-unknown"] {
        assert_eq!(
            TranscriptFormat::for_harness(id),
            None,
            "{id} has no wired reader and must not resolve to ClaudeCode"
        );
    }
}

#[test]
fn transcript_format_for_harness_matches_capability_catalog() {
    let catalog: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../src/types/generated/HarnessCapabilitiesTable.json"
    ))
    .expect("generated HarnessCapabilitiesTable.json must parse");
    let ids = catalog
        .get("ids")
        .and_then(|v| v.as_array())
        .expect("catalog JSON must include ids in Provider::all() order");
    let capabilities = catalog
        .get("capabilities")
        .and_then(|v| v.as_object())
        .expect("catalog JSON must include capabilities");
    assert!(
        !ids.is_empty(),
        "catalog ids must not be empty or the consistency check is vacuous"
    );
    for id in ids {
        let id = id.as_str().expect("catalog ids must be strings");
        let produces = capabilities
            .get(id)
            .and_then(|caps| caps.get("produces_readable_transcript"))
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| {
                panic!("catalog capabilities for {id} must carry produces_readable_transcript")
            });
        match TranscriptFormat::for_harness(id) {
                Some(format) => assert!(
                    produces,
                    "{id} has a wired {format:?} reader but advertises produces_readable_transcript=false"
                ),
                None => assert!(
                    !produces,
                    "{id} advertises produces_readable_transcript=true but has no for_harness arm"
                ),
            }
    }
    assert_eq!(
        TranscriptFormat::for_harness("claude"),
        Some(TranscriptFormat::ClaudeCode)
    );
}
