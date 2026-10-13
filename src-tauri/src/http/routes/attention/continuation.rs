//! Native session-continuation reconciliation at the identity seam
//! (issue #2263).
//!
//! Claude Code can carry a *running* process into a new native session and
//! records the transition in the transcript of the session it came from
//! (`{"type":"continued-in",…}`, read by the Claude Code transcript adapter).
//! Callbacks after that transition carry the successor id, which the ordering
//! token (issue #1364) reads as a previous process — which is what left runs
//! 455-457 busy with a report nothing could observe (issue #2263).
//!
//! The seam has two phases, split by what each one touches:
//!
//! - [`prove`] reads the lineage from the transcripts on disk. It is file I/O,
//!   so the route runs it on the blocking pool before the apply pass, with no
//!   database connection held.
//! - [`admit`] moves the stored identity. It is a conditional write, so it
//!   runs inside the apply pass and only while the node still carries the
//!   identity the proof was made against. An unrelated id, a superseded link
//!   in the chain, or a node whose identity moved on is never adopted.

use crate::services::transcript_reader::readers::claude_code;

/// How far a recorded continuation chain is followed before the lineage is
/// refused. A cycle or a corrupt chain cannot spin the walk.
const MAX_CONTINUATION_HOPS: usize = 8;

/// The successor session this callback is proven to be, or `None` when nothing
/// on disk proves it. A callback that already matches the stored identity (or
/// carries none to compare) needs no proof and returns `None` without reading a
/// transcript.
pub(super) fn prove(
    provider: &str,
    node_path: &str,
    stored: &str,
    hook: Option<&str>,
) -> Option<String> {
    let hook = reconciled_hook(stored, hook)?;
    proven_successor(provider, node_path, stored, hook)
}

/// Whether this callback may proceed, adopting a proven continuation first.
///
/// The write is conditional on the identity the proof was made against, so a
/// node that moved on — a fresh spawn, another capture — drops the callback
/// instead of overwriting the newer identity.
pub(super) fn admit(
    node_id: i64,
    generation: Option<i64>,
    stored: &str,
    hook: Option<&str>,
    successor: Option<&str>,
) -> Result<bool, String> {
    let Some(hook) = reconciled_hook(stored, hook) else {
        return Ok(true);
    };
    if successor != Some(hook) {
        return Ok(false);
    }
    match crate::db::adopt_continued_cli_session_id(node_id, stored, hook, generation) {
        Ok(true) => {
            tracing::info!(
                node_id,
                session_id = hook,
                "attention webhook adopted continued native session"
            );
            Ok(true)
        }
        Ok(false) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// The callback's own session id when it differs from the stored one *and* there
/// is a stored identity to differ from. `None` means there is nothing to
/// reconcile: an absent identity is the capture path's business (issue #1089),
/// and a matching id is already the current process.
fn reconciled_hook<'a>(stored: &str, hook: Option<&'a str>) -> Option<&'a str> {
    hook.filter(|hook| !hook.is_empty() && !stored.is_empty() && *hook != stored)
}

/// Walk the recorded `continued-in` chain forward from the stored session and
/// accept only its terminal session. An id the chain has already passed is
/// superseded, and a reversed record proves nothing: a predecessor is never
/// the successor.
fn proven_successor(provider: &str, node_path: &str, stored: &str, hook: &str) -> Option<String> {
    use crate::services::transcript_reader::TranscriptFormat;
    // Only a harness whose transcript is Claude Code's records this shape.
    // Cursor reuses the parser over its own workspace-scoped store and never
    // writes it, and no other harness has a continuation record at all.
    if TranscriptFormat::for_harness(provider) != Some(TranscriptFormat::ClaudeCode) {
        return None;
    }
    let mut current = stored.to_string();
    for _ in 0..MAX_CONTINUATION_HOPS {
        let Some(successor) = declared_successor(node_path, &current) else {
            return (current == hook).then_some(current);
        };
        current = successor;
    }
    None
}

/// The successor `current` itself records, validated as identity we may adopt:
/// the next hop turns it into a transcript filename and the row that follows
/// reaches the harness as `--resume <id>`.
fn declared_successor(node_path: &str, current: &str) -> Option<String> {
    let path = claude_code::transcript_path(current, node_path)?;
    let successor = claude_code::continued_session_id(current, &path)?;
    crate::http::request::parse_cli_session_id(&successor)
}

/// Shared seam fixture: a temporary Claude home whose transcripts are written
/// *and* read through the production locator, under a `$USERPROFILE`/`$HOME`
/// override so nothing touches the developer's real profile. The node path is
/// the temporary directory itself, which resolves `claude_dir()` natively on
/// Windows and `HOME` elsewhere.
#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    pub(in crate::http::routes::attention) struct Fixture {
        home: tempfile::TempDir,
        node_path: String,
    }

    impl Fixture {
        pub(in crate::http::routes::attention) fn new(
            records: &[(&str, Vec<serde_json::Value>)],
        ) -> Self {
            let home = tempfile::tempdir().unwrap();
            let root = home.path().join(".claude");
            let node_path = home.path().to_str().unwrap().to_owned();
            with_claude_home(home.path(), || {
                for (session_id, lines) in records {
                    let path = claude_code::transcript_path(session_id, &node_path).unwrap();
                    assert!(
                        path.starts_with(&root),
                        "{session_id} resolved outside the fixture home: {}",
                        path.display()
                    );
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    let body = lines
                        .iter()
                        .map(|line| format!("{line}\n"))
                        .collect::<String>();
                    std::fs::write(&path, body).unwrap();
                }
            });
            Self { home, node_path }
        }

        pub(in crate::http::routes::attention) fn node_path(&self) -> &str {
            &self.node_path
        }

        pub(in crate::http::routes::attention) fn home_path(&self) -> &std::path::Path {
            self.home.path()
        }

        pub(in crate::http::routes::attention) fn prove(
            &self,
            provider: &str,
            stored: &str,
            hook: &str,
        ) -> Option<String> {
            with_claude_home(self.home.path(), || {
                prove(provider, &self.node_path, stored, Some(hook))
            })
        }
    }

    /// Run `body` with the CLI home resolved inside `home`. `ENV_LOCK` is the
    /// process-wide guard every env-mutating test holds, so this cannot race
    /// another test's override.
    pub(in crate::http::routes::attention) fn with_claude_home<T>(
        home: &std::path::Path,
        body: impl FnOnce() -> T,
    ) -> T {
        let home = std::ffi::OsStr::new(home.to_str().unwrap());
        let _guard = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        crate::env::with_env_vars(&[("USERPROFILE", Some(home)), ("HOME", Some(home))], body)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{with_claude_home, Fixture};
    use super::*;
    use serde_json::json;

    const OLD: &str = "d1b48df8-eb97-4867-b365-a34cdbdc5900";
    const NEW: &str = "a5acfe57-a409-4aaa-a644-4f7621934663";
    const LAST: &str = "0c9e6d55-2f18-4a7b-b6c2-51f0a3d97c48";

    fn continuation_record(session_id: &str, successor: serde_json::Value) -> serde_json::Value {
        json!({
            "type": "continued-in",
            "sessionId": session_id,
            "continuedInSessionId": successor,
            "timestamp": "2026-10-12T18:57:48.345Z",
        })
    }

    #[test]
    fn recorded_transition_proves_the_successor_callback() {
        // The run-455 shape: the predecessor's transcript carries the record.
        let fixture = Fixture::new(&[(
            OLD,
            vec![
                json!({"type": "user", "message": {"role": "user", "content": "keep going"}}),
                continuation_record(OLD, json!(NEW)),
            ],
        )]);
        assert_eq!(fixture.prove("anthropic", OLD, NEW).as_deref(), Some(NEW));
    }

    #[test]
    fn a_callback_without_a_session_id_needs_no_proof() {
        // Nothing to reconcile: the route's existing capture and fence own it.
        let fixture = Fixture::new(&[(OLD, vec![continuation_record(OLD, json!(NEW))])]);
        let node_path = fixture.node_path().to_owned();
        assert_eq!(fixture.prove("anthropic", OLD, OLD), None);
        with_claude_home(fixture.home_path(), || {
            assert_eq!(prove("anthropic", &node_path, OLD, Some("")), None);
            assert_eq!(prove("anthropic", &node_path, OLD, None), None);
            // An absent stored identity is the fill-only capture path's
            // business (issue #1089), not a continuation: no proof, and no
            // drop either — the callback must still be admitted.
            assert_eq!(prove("anthropic", &node_path, "", Some(OLD)), None);
            assert_eq!(prove("anthropic", &node_path, "", Some(NEW)), None);
            assert!(admit(1, Some(7), "", Some(NEW), None).unwrap());
            assert!(admit(1, Some(7), OLD, Some(OLD), None).unwrap());
            assert!(admit(1, Some(7), OLD, None, None).unwrap());
        });
    }

    #[test]
    fn an_unrelated_id_is_never_lineage() {
        let fixture = Fixture::new(&[(
            OLD,
            vec![json!({"type": "user", "message": {"role": "user", "content": "hi"}})],
        )]);
        let unrelated = "3f2b8c14-77d5-4a0e-9c31-6b0e2f7a1d55";
        assert_eq!(fixture.prove("anthropic", OLD, unrelated), None);
    }

    #[test]
    fn absent_malformed_and_foreign_records_prove_nothing() {
        let malformed = vec![
            // No successor at all.
            json!({"type": "continued-in", "sessionId": OLD}),
            // Successor is not a session id: it must never reach a path or argv.
            json!({"type": "continued-in", "sessionId": OLD, "continuedInSessionId": "../../escape"}),
            json!({"type": "continued-in", "sessionId": OLD, "continuedInSessionId": ""}),
            json!({"type": "continued-in", "sessionId": OLD, "continuedInSessionId": OLD}),
            // A record naming some other predecessor proves nothing about
            // this transcript's own identity.
            json!({"type": "continued-in", "sessionId": NEW, "continuedInSessionId": OLD}),
            // Right fields, wrong record type.
            json!({"type": "summary", "sessionId": OLD, "continuedInSessionId": NEW}),
            // Not JSON at all.
            json!("{\"type\":\"continued-in\","),
        ];
        let fixture = Fixture::new(&[(OLD, malformed)]);
        assert_eq!(fixture.prove("anthropic", OLD, NEW), None);
    }

    #[test]
    fn a_reversed_or_superseded_transition_is_refused() {
        let fixture = Fixture::new(&[
            (OLD, vec![continuation_record(OLD, json!(NEW))]),
            (NEW, vec![continuation_record(NEW, json!(LAST))]),
        ]);
        // Reversed: the stored session is the newer one, so its predecessor's
        // callback is a retired process, not a lineage.
        assert_eq!(fixture.prove("anthropic", NEW, OLD), None);
        // Superseded: the chain has moved on past the callback's session.
        assert_eq!(fixture.prove("anthropic", OLD, NEW), None);
        // Only the chain's terminal session is adoptable.
        assert_eq!(fixture.prove("anthropic", OLD, LAST).as_deref(), Some(LAST));
    }

    #[test]
    fn a_chain_that_never_ends_is_refused() {
        // A corrupt two-step cycle: the walk is bounded, so no successor of a
        // cyclic chain is ever adoptable.
        let first = "b1a4c0d2-58e3-4f76-9c05-2ad7b8e64f10";
        let second = "e57f9d10-6c2a-4b83-8f41-95d0c7e2ba63";
        let fixture = Fixture::new(&[
            (first, vec![continuation_record(first, json!(second))]),
            (second, vec![continuation_record(second, json!(first))]),
        ]);
        assert_eq!(fixture.prove("anthropic", first, second), None);
        assert_eq!(fixture.prove("anthropic", first, first), None);
    }

    #[test]
    fn only_a_claude_code_transcript_records_this_shape() {
        let fixture = Fixture::new(&[(OLD, vec![continuation_record(OLD, json!(NEW))])]);
        for provider in ["codex", "cursor", "grok", "opencode", "mcode", ""] {
            assert_eq!(fixture.prove(provider, OLD, NEW), None, "{provider}");
        }
    }
}
