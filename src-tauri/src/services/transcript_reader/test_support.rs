//! Shared on-disk fixture utilities; format assertions live with each reader.
use std::path::{Path, PathBuf};

pub(crate) fn write_fixture(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "buildmesh_transcript_{name}_{}.jsonl",
        std::process::id()
    ));
    std::fs::write(&path, body).unwrap();
    path
}

pub(crate) fn fixture(reader: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/transcripts")
        .join(reader)
        .join(name)
}

/// Exercise real file reads across the seam, including I/O failure and drift.
pub(crate) fn assert_jsonl_contract(
    reader: &dyn super::adapter::TranscriptReader,
    valid: &Path,
    changed: &Path,
    expected: &str,
) {
    use super::{types::build_tail, TranscriptTail, UnavailableReason};
    let parsed = reader.read_tail(valid, "session", 1).unwrap();
    assert_eq!(
        parsed.turns.len(),
        1,
        "tail retains exactly the newest turn"
    );
    assert_eq!(parsed.last_assistant_message.as_deref(), Some(expected));
    let digest = reader.last_assistant_message(valid, "session").unwrap();
    assert_eq!(digest.last_assistant_message.as_deref(), Some(expected));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    let valid_body = std::fs::read_to_string(valid).unwrap();
    std::fs::write(&path, format!("malformed record\n{valid_body}")).unwrap();
    assert_eq!(
        reader
            .read_tail(&path, "session", 1)
            .unwrap()
            .last_assistant_message
            .as_deref(),
        Some(expected)
    );
    assert_eq!(
        reader
            .last_assistant_message(&path, "session")
            .unwrap()
            .last_assistant_message
            .as_deref(),
        Some(expected)
    );

    // More than 256 KiB of non-dialogue records puts the answer outside the
    // byte window and exercises the full-stream fallback for each reader.
    std::fs::write(&path, format!("{valid_body}\n{}", "{}\n".repeat(100_000))).unwrap();
    assert_eq!(
        reader
            .last_assistant_message(&path, "session")
            .unwrap()
            .last_assistant_message
            .as_deref(),
        Some(expected)
    );

    std::fs::write(&path, "").unwrap();
    for parsed in [
        reader.read_tail(&path, "session", 1),
        reader.last_assistant_message(&path, "session"),
    ] {
        assert_eq!(
            build_tail(parsed.unwrap()),
            TranscriptTail::unavailable(UnavailableReason::Empty)
        );
    }
    for parsed in [
        reader.read_tail(changed, "session", 1),
        reader.last_assistant_message(changed, "session"),
    ] {
        assert_eq!(
            build_tail(parsed.unwrap()),
            TranscriptTail::unavailable(UnavailableReason::ShapeChanged)
        );
    }

    for bad_path in [dir.path().join("missing.jsonl"), dir.path().to_path_buf()] {
        assert_eq!(
            reader.read_tail(&bad_path, "session", 1),
            Err(UnavailableReason::Unreadable)
        );
        assert_eq!(
            reader.last_assistant_message(&bad_path, "session"),
            Err(UnavailableReason::Unreadable)
        );
    }
    // Decode failure must not masquerade as an empty or successfully parsed store.
    std::fs::write(&path, [0xff, b'\n']).unwrap();
    assert_eq!(
        reader.read_tail(&path, "session", 1),
        Err(UnavailableReason::Unreadable)
    );
    assert_eq!(
        reader.last_assistant_message(&path, "session"),
        Err(UnavailableReason::Unreadable)
    );
}
