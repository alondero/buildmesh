//! Tests for `agent::process`.
//!
//! Split out of `src-tauri/src/agent/process.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path = "..."]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test agent::process::tests

use super::*;
use crate::agent::provider::{SpawnRecipe, WindowsShell};
use crate::agent::spawn::{open_pty_pair, spawn_child};
use crate::agent::spawn_environment;
use crate::models::EnvType;
use std::io::Write;

/// The activity from a write the queue **accepted**.
///
/// `write_input` answers `Ok` for an accepted write and also for a refused
/// one (a refusal carries default activity), so pinning only the activity
/// would let "the queue dropped it" read as "it decoded to nothing" — the
/// exact ambiguity issue #1530 exists to remove. Every decoding assertion
/// goes through here so the disposition is checked too.
fn accepted_activity(registry: &AgentProcessRegistry, id: i64, data: &[u8]) -> InputActivity {
    let outcome = registry.write_input(id, data).unwrap();
    assert_eq!(
        outcome.disposition,
        InputDisposition::Accepted,
        "expected the queue to accept {data:?}"
    );
    outcome.activity
}

#[test]
fn terminal_protocol_preserves_registry_report_fence_across_packets() {
    let registry = AgentProcessRegistry::new();
    let id = -930_020;
    insert_trivial_agent(&registry, id);
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    let agent = registry.get(&id).unwrap();
    *agent.writer_tx.lock().unwrap() = Some(tx);
    let stamp = registry.input_stamp(id).unwrap();
    for packet in [
        b"\x1b[I".as_slice(),
        b"\x1b[O",
        b"\x1b[12;34R",
        b"\x1b[?1;2c",
        b"\x1b[<0;24;12M",
    ] {
        for byte in packet {
            assert_eq!(
                accepted_activity(&registry, id, &[*byte]),
                InputActivity::default()
            );
            assert_eq!(rx.recv().unwrap(), vec![*byte]);
        }
        assert_eq!(registry.input_stamp(id).as_ref(), Some(&stamp));
        assert!(registry
            .commit_recovered_turn(id, &stamp, 1, || Ok(true))
            .unwrap());
    }
    assert_eq!(agent.last_submit_ms.load(Ordering::Relaxed), 0);
    assert!(!agent.first_user_input_logged.load(Ordering::Relaxed));
    registry.write_input(id, b"\x1b[").unwrap();
    rx.recv().unwrap();
    assert_eq!(
        registry.input_stamp_result(id),
        Err(InputUnavailable::UnknownInput)
    );
    assert!(!registry.input_is_current(id, &stamp));
    assert!(!registry
        .commit_recovered_turn(id, &stamp, 1, || panic!("incomplete input is ambiguous"))
        .unwrap());
    assert!(registry
        .write_bytes_if_current(id, b"continue", &stamp)
        .unwrap()
        .is_none());
    assert!(rx.try_recv().is_err());
    registry.write_input(id, b"A").unwrap();
    rx.recv().unwrap();
    assert_eq!(
        registry.input_stamp_result(id),
        Err(InputUnavailable::UnknownInput)
    );
    assert_ne!(
        agent.input_version.load(Ordering::Relaxed),
        InputStamp::decode(&stamp).unwrap().version
    );
    registry.kill_session(id);
}

#[test]
fn registry_recovers_input_fence_after_escape_then_submit_or_clear() {
    let id = -930_023;
    let (registry, rx) = testing::capturing_registry(id);
    let agent = registry.get(&id).unwrap();
    for boundary in [b'\r', 0x03] {
        let previous = registry.input_stamp(id).unwrap();
        registry.write_input(id, b"\x1b").unwrap();
        assert_eq!(rx.recv().unwrap(), b"\x1b");
        assert_eq!(
            registry.input_stamp_result(id),
            Err(InputUnavailable::UnknownInput)
        );
        assert!(!registry
            .commit_recovered_turn(id, &previous, i64::MAX, || panic!(
                "pending Escape must block recovery"
            ))
            .unwrap());
        assert!(registry
            .write_bytes_if_current(id, b"continue", &previous)
            .unwrap()
            .is_none());
        assert!(rx.try_recv().is_err());

        let last_submit = agent.last_submit_ms.load(Ordering::Relaxed);
        assert_eq!(
            accepted_activity(&registry, id, &[boundary]),
            InputActivity {
                user_input: true,
                submitted: boundary == b'\r',
            }
        );
        assert_eq!(rx.recv().unwrap(), vec![boundary]);
        let current = registry
            .input_stamp(id)
            .expect("boundary makes reports bindable again");
        assert_ne!(current, previous);
        assert!(!registry
            .commit_recovered_turn(id, &previous, i64::MAX, || panic!(
                "old report cannot cross new input"
            ))
            .unwrap());
        assert!(registry
            .commit_recovered_turn(id, &current, i64::MAX, || Ok(true))
            .unwrap());
        if boundary == b'\r' {
            assert!(agent.last_submit_ms.load(Ordering::Relaxed) > 0);
        } else {
            assert_eq!(agent.last_submit_ms.load(Ordering::Relaxed), last_submit);
        }
    }
    assert!(!accepted_activity(&registry, id, b"\x1b\r").submitted);
    assert_eq!(rx.recv().unwrap(), b"\x1b\r");
    assert_eq!(
        registry.input_stamp_result(id),
        Err(InputUnavailable::UnknownInput)
    );
    registry.write_input(id, b"\x03").unwrap();
    rx.recv().unwrap();

    registry.write_bytes(id, b"\x1b").unwrap();
    rx.recv().unwrap();
    registry.write_bytes(id, b"\r").unwrap();
    rx.recv().unwrap();
    assert_eq!(
        registry.input_stamp_result(id),
        Err(InputUnavailable::UnknownInput),
        "raw byte fragments must not acquire terminal event semantics"
    );
    registry.kill_session(id);
}

#[test]
fn dropped_clear_event_does_not_recover_an_uncertain_prompt() {
    let id = -930_024;
    let registry = AgentProcessRegistry::new();
    insert_trivial_agent(&registry, id);
    // Capacity 1 on purpose, so the `\x03` below genuinely cannot be
    // queued. That was always the intent of this fixture, but pre-#1530 the
    // loss was invisible: the write came back `Ok` with default activity.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    *registry.get(&id).unwrap().writer_tx.lock().unwrap() = Some(tx);
    let original = registry.input_stamp(id).unwrap();
    registry.write_input(id, b"\x1b").unwrap();
    // Issue #1530: the refused clear is *reported*, not swallowed. The
    // assertion used to be `activity == default`, which a silent drop and a
    // genuine no-op decoder answer identically — exactly the ambiguity that
    // let a dropped prompt read as delivered.
    assert_eq!(
        registry.write_input(id, b"\x03").unwrap().disposition,
        InputDisposition::Backpressured,
        "a full input queue must be reported, not dropped behind a success"
    );
    assert_eq!(
        registry.input_stamp_result(id),
        Err(InputUnavailable::UnknownInput)
    );
    assert_eq!(rx.recv().unwrap(), b"\x1b");
    assert!(
        rx.try_recv().is_err(),
        "the refused clear must never have been queued"
    );
    assert!(!registry
        .commit_recovered_turn(id, &original, i64::MAX, || panic!(
            "dropped clear cannot authorize recovery"
        ))
        .unwrap());
    assert_eq!(
        accepted_activity(&registry, id, b"\x03"),
        InputActivity {
            user_input: true,
            submitted: false
        }
    );
    assert_eq!(rx.recv().unwrap(), b"\x03");
    assert!(registry.input_stamp(id).is_some());
    registry.kill_session(id);
}

#[test]
fn registry_distinguishes_drafts_paste_and_actual_submission() {
    let registry = AgentProcessRegistry::new();
    let id = -930_021;
    insert_trivial_agent(&registry, id);
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    let agent = registry.get(&id).unwrap();
    *agent.writer_tx.lock().unwrap() = Some(tx);
    let original = registry.input_stamp(id).unwrap();
    assert_eq!(
        accepted_activity(&registry, id, b"draft"),
        InputActivity {
            user_input: true,
            submitted: false
        }
    );
    rx.recv().unwrap();
    assert_eq!(
        registry.input_stamp_result(id),
        Err(InputUnavailable::Draft)
    );
    registry.write_input(id, b"\x03").unwrap();
    rx.recv().unwrap();
    for packet in [b"\x1b[20".as_slice(), b"0~", b"line\n", b"\x1b[201", b"~"] {
        assert!(!accepted_activity(&registry, id, packet).submitted);
        rx.recv().unwrap();
    }
    assert_eq!(agent.last_submit_ms.load(Ordering::Relaxed), 0);
    assert_eq!(
        registry.input_stamp_result(id),
        Err(InputUnavailable::Draft)
    );
    assert!(!registry
        .commit_recovered_turn(id, &original, i64::MAX, || panic!(
            "new draft invalidates old report"
        ))
        .unwrap());
    assert_eq!(
        accepted_activity(&registry, id, b"\r"),
        InputActivity {
            user_input: true,
            submitted: true
        }
    );
    rx.recv().unwrap();
    assert!(agent.last_submit_ms.load(Ordering::Relaxed) > 0);
    assert_ne!(registry.input_stamp(id).unwrap(), original);
    registry.kill_session(id);
}

/// Raw registry input must leave the staged-draft stream unclaimable while
/// an escape sequence is open.
///
/// The claim in this test's name is about the *stream*, and that is what it
/// now asserts. The old assertion — that the stamp is byte-identical to a
/// pre-write capture — passed only because the cap-1 fixture dropped these
/// writes: a dropped write leaves the prompt pristine, which looked the
/// same as "the decoder considered the bytes and stayed uncertain". Send
/// them for real and the decoder reports `UnknownInput`, because an
/// unterminated `\x1b[A` makes the prompt uncertain. That is the stricter
/// and more useful guarantee: the circuit cannot append to, or submit, a
/// prompt whose boundaries it cannot prove.
#[test]
fn dropped_registry_input_does_not_advance_stream_or_activity() {
    let registry = AgentProcessRegistry::new();
    let id = -930_022;
    insert_trivial_agent(&registry, id);
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    *registry.get(&id).unwrap().writer_tx.lock().unwrap() = Some(tx);
    registry.write_bytes(id, b"\x1b[").unwrap();
    rx.recv().unwrap();
    registry.write_bytes(id, b"A").unwrap();
    rx.recv().unwrap();
    registry.write_bytes(id, b"I").unwrap();
    rx.recv().unwrap();
    assert_eq!(
        registry.input_stamp_result(id),
        Err(InputUnavailable::UnknownInput),
        "an unterminated escape sequence must leave the prompt uncertain, not stamped"
    );
    assert!(
        registry.input_stamp(id).is_none(),
        "no usable stamp exists while the sequence is open"
    );
    registry.kill_session(id);
}

#[test]
fn circuit_retirement_cannot_stop_a_replacement_process_generation() {
    let registry = AgentProcessRegistry::new();
    let id = -930_099;
    insert_trivial_agent(&registry, id);
    let old_generation = registry.get(&id).unwrap().generation;
    assert!(registry.kill_session_if_generation(id, old_generation));
    // Stop succeeded but its durable acknowledgement failed. A replacement
    // must survive the original cleanup request being retried.
    insert_trivial_agent(&registry, id);
    let replacement = registry.get(&id).unwrap().generation;
    assert_ne!(old_generation, replacement);
    assert!(!registry.kill_session_if_generation(id, old_generation));
    assert_eq!(registry.get(&id).unwrap().generation, replacement);
    assert!(registry.kill_session_if_generation(id, replacement));
}

#[test]
fn recovered_turn_database_work_does_not_lock_other_sessions() {
    let registry = Arc::new(AgentProcessRegistry::new());
    let id = -930_004;
    insert_trivial_agent(&registry, id);
    let other = -930_005;
    insert_trivial_agent(&registry, other);
    let (other_tx, other_rx) = std::sync::mpsc::sync_channel(8);
    *registry.get(&other).unwrap().writer_tx.lock().unwrap() = Some(other_tx);
    let input = registry.input_stamp(id).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (lookup_tx, lookup_rx) = std::sync::mpsc::channel();
    let recovery_registry = registry.clone();
    let recovery = std::thread::spawn(move || {
        recovery_registry
            .commit_recovered_turn(id, &input, i64::MAX, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(true)
            })
            .unwrap()
    });
    entered_rx.recv().unwrap();
    let lookup_registry = registry.clone();
    let lookup = std::thread::spawn(move || {
        let found = lookup_registry.get(&other).is_some();
        let written = lookup_registry
            .write_bytes(other, b"still responsive\r")
            .is_ok();
        lookup_tx.send(found && written).unwrap();
    });
    let responsive = lookup_rx.recv_timeout(std::time::Duration::from_secs(1));
    // Release even on failure so the regression reports a failure rather
    // than leaving blocked worker threads behind.
    release_tx.send(()).unwrap();
    assert!(recovery.join().unwrap());
    lookup.join().unwrap();
    registry.kill_session(id);
    registry.kill_session(other);
    assert!(
        responsive.unwrap(),
        "another session must remain accessible while SQLite is busy"
    );
    assert_eq!(other_rx.recv().unwrap(), b"still responsive\r");
}

#[test]
fn recovered_turn_commits_only_for_the_observed_process_input_and_lifecycle() {
    let registry = AgentProcessRegistry::new();
    let id = -930_003;
    insert_trivial_agent(&registry, id);
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    *registry.get(&id).unwrap().writer_tx.lock().unwrap() = Some(tx);
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_nodes (id INTEGER PRIMARY KEY, status TEXT,
            session_started_at INTEGER, status_changed_at TEXT);
            INSERT INTO agent_nodes VALUES (-930003, 'running', 1, '2026-09-13T18:25:20Z');",
    )
    .unwrap();
    let lifecycle = "1:2026-09-13T18:25:20Z";
    let input = registry.input_stamp(id).unwrap();
    let agent = registry.get(&id).unwrap();
    let completed_at = chrono::Utc::now().timestamp_millis() - 1000;
    let commit = || {
        crate::db::complete_agent_turn_if_current_inner(&conn, id, lifecycle)
            .map_err(|e| e.to_string())
    };
    assert!(registry
        .commit_recovered_turn(id, &input, completed_at, || {
            assert!(
                agent.writer_tx.try_lock().is_err(),
                "input stays fenced through the DB commit"
            );
            commit()
        })
        .unwrap());
    assert_eq!(
        conn.query_row("SELECT status FROM agent_nodes", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "ready"
    );

    // A lifecycle change after observation must win even when the new
    // state is also Running. Comparing only status would accept it.
    conn.execute_batch(
        "UPDATE agent_nodes SET status='running', status_changed_at='2026-09-13T18:30:00Z'",
    )
    .unwrap();
    assert!(!registry
        .commit_recovered_turn(id, &input, completed_at, commit)
        .unwrap());
    assert_eq!(
        conn.query_row("SELECT status FROM agent_nodes", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "running"
    );

    registry.write_bytes(id, b"draft").unwrap();
    assert_eq!(rx.recv().unwrap(), b"draft");
    assert!(registry.input_stamp(id).is_none());
    assert!(!registry
        .commit_recovered_turn(id, &input, completed_at, || panic!(
            "draft must prevent mutation"
        ))
        .unwrap());
    registry.write_bytes(id, b"\r").unwrap();
    assert_eq!(rx.recv().unwrap(), b"\r");
    let after_submit = registry.input_stamp(id).unwrap();
    assert!(!registry
        .commit_recovered_turn(id, &after_submit, completed_at, || panic!(
            "even a prompt submitted before observation supersedes old native completion"
        ))
        .unwrap());
    assert!(!registry
        .commit_recovered_turn(id, &input, i64::MAX, || panic!(
            "a changed input version must prevent mutation"
        ))
        .unwrap());
    insert_trivial_agent(&registry, id);
    assert!(!registry
        .commit_recovered_turn(id, &after_submit, i64::MAX, || panic!(
            "a replacement process cannot consume its predecessor's completion"
        ))
        .unwrap());
    registry.kill_session(id);
    assert!(!registry
        .commit_recovered_turn(id, &after_submit, i64::MAX, || panic!(
            "a dead process cannot publish completion"
        ))
        .unwrap());
}

#[test]
fn circuit_continuation_cannot_append_to_or_submit_a_user_draft() {
    let registry = AgentProcessRegistry::new();
    let id = -930_001;
    insert_trivial_agent(&registry, id);
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    *registry.get(&id).unwrap().writer_tx.lock().unwrap() = Some(tx);
    let observed = registry.input_stamp(id).unwrap();
    registry.write_bytes(id, b"unfinished draft").unwrap();
    assert_eq!(rx.recv().unwrap(), b"unfinished draft");
    assert!(
        registry.input_stamp(id).is_none(),
        "a draft already present before classification is ineligible"
    );
    assert!(registry
        .write_bytes_if_current(id, b"continue", &observed)
        .unwrap()
        .is_none());
    assert!(rx.try_recv().is_err());
    registry.write_bytes(id, b"\r").unwrap();
    assert_eq!(rx.recv().unwrap(), b"\r");
    let observed = registry.input_stamp(id).unwrap();
    let staged = registry
        .write_bytes_if_current(id, b"staged prompt", &observed)
        .unwrap()
        .unwrap();
    assert_eq!(rx.recv().unwrap(), b"staged prompt");
    registry.write_bytes(id, b"more user input").unwrap();
    assert_eq!(rx.recv().unwrap(), b"more user input");
    assert!(
        registry
            .write_bytes_if_current(id, b"\r", &staged)
            .unwrap()
            .is_none(),
        "Enter must not submit newer input"
    );
    assert!(rx.try_recv().is_err());
    registry.kill_session(id);
}

#[test]
fn input_stamp_ignores_navigation_and_tracks_backspace_to_empty() {
    assert_eq!(
        input_buffer_len_after(0, b"\x1b[A\x1b[I"),
        UNKNOWN_INPUT_BUFFER_LEN
    );
    assert_eq!(input_buffer_len_after(0, b"draft"), 5);
    assert_eq!(
        input_buffer_len_after(5, b"\x1b[D\x7f\x7f\x7f\x7f\x7f"),
        UNKNOWN_INPUT_BUFFER_LEN
    );
    assert_eq!(input_buffer_len_after(5, b"\x7f\x7f\x7f\x7f\x7f"), 0);
    assert_eq!(input_buffer_len_after(5, b"\x03"), 0);
    assert_eq!(
        input_buffer_len_after(0, b"\x1b[200~line\n two\x1b[201~"),
        8
    );
    assert_eq!(
        input_buffer_len_after(UNKNOWN_INPUT_BUFFER_LEN, b"\x1b[D\x7f"),
        UNKNOWN_INPUT_BUFFER_LEN
    );
    assert_eq!(
        input_buffer_len_after(UNKNOWN_INPUT_BUFFER_LEN, b"\x15"),
        UNKNOWN_INPUT_BUFFER_LEN
    );
    assert_eq!(input_buffer_len_after(0, b"\x10"), UNKNOWN_INPUT_BUFFER_LEN);
}

#[test]
fn input_buffer_state_preserves_bracketed_paste_across_packets() {
    assert_eq!(
        input_buffer_state_after(0, false, b"\x1b[200~draft"),
        (5, true)
    );
    assert_eq!(input_buffer_state_after(5, true, b"\n"), (5, true));
    assert_eq!(input_buffer_state_after(5, true, b"\x1b[201~"), (5, false));
}

/// The #1531 fixture: a live child whose writer thread is parked in
/// `recv()` with nothing queued, plus a flag the writer sets once its
/// channel closes. Shared by the teardown tests that need the agent
/// itself rather than a registry entry.
fn idle_writer_process(session_id: i64) -> (AgentProcess, Arc<AtomicBool>) {
    let recipe = SpawnRecipe {
        binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
        base_args: if cfg!(windows) {
            vec!["/c".into(), "exit".into(), "0".into()]
        } else {
            vec!["-c".into(), "exit 0".into()]
        },
        trailing_args: Vec::new(),
        windows_shell: WindowsShell::Direct,
    };
    let cwd = std::env::current_dir().unwrap();
    let cmd = spawn_environment::wrap(
        recipe,
        EnvType::Windows,
        None,
        None,
        &cwd.to_string_lossy(),
        session_id,
        false,
    )
    .expect("an unsandboxed command always assembles");
    let pair = open_pty_pair(24, 80).expect("open pty pair");
    let child = spawn_child(&pair, cmd).expect("spawn child");
    let writer = pair.master.take_writer().expect("take writer");
    let writer_exited = Arc::new(AtomicBool::new(false));
    let writer_exited_thread = writer_exited.clone();
    // Capacity matches production (`PTY_WRITER_CHANNEL_CAPACITY`) on purpose.
    // A capacity of 1 made these decoding fixtures depend on the writer
    // thread winning a scheduling race, and the pre-#1530 silent drop hid
    // the flakiness by reporting a full queue as a successful write. These
    // tests are about input decoding, not about queue saturation — the
    // saturation path has its own fixtures.
    let (writer_tx, writer_rx) =
        std::sync::mpsc::sync_channel::<Vec<u8>>(crate::agent::spawn::PTY_WRITER_CHANNEL_CAPACITY);
    let queue = Arc::new(InputQueueGauge::default());
    let writer_queue = Arc::clone(&queue);
    let writer_thread = std::thread::spawn(move || {
        let mut writer = writer;
        while let Ok(bytes) = writer_rx.recv() {
            writer_queue.release(bytes.len());
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
        writer_exited_thread.store(true, Ordering::SeqCst);
    });
    let agent = AgentProcess::new(
        child,
        writer_tx,
        queue,
        Some(writer_thread),
        pair.master,
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
        None,
        None,
        std::time::Instant::now(),
        0,
    );
    (agent, writer_exited)
}

/// `pub(crate)` so [`super::testing::capturing_registry`] can reuse the
/// fixture rather than duplicating the spawn setup.
pub(crate) fn insert_trivial_agent(
    registry: &AgentProcessRegistry,
    session_id: i64,
) -> (u64, Arc<AtomicBool>) {
    let (agent, writer_exited) = idle_writer_process(session_id);
    let generation = registry.insert(session_id, agent);
    (generation, writer_exited)
}

use testing::blocked_writer_registry;

/// Fill the queue and confirm the next write is *reported*, not swallowed.
///
/// This is the issue's headline regression: pre-fix the same sequence
/// dropped the bytes and returned `Ok`, so a caller recorded a prompt as
/// delivered while the agent never saw it.
#[test]
fn a_saturated_queue_reports_backpressure_and_leaves_telemetry_untouched() {
    let id = -930_030;
    // Capacity 1: the writer takes the first buffer and parks, so the second
    // fills the single slot and the third is refused.
    let (registry, writer) = blocked_writer_registry(id, 1);
    let agent = registry.get(&id).unwrap();
    assert_eq!(
        registry.input_queue_totals(),
        (0, 0),
        "baseline is an empty queue"
    );

    // Fill the queue with *empty* buffers. They occupy a message slot each
    // without producing any input activity, which keeps the first-user-input
    // checkpoint unset — so the assertion after the refusal proves the
    // refused write is what left telemetry untouched, rather than an earlier
    // accepted keystroke having already set it.
    assert!(registry.write_bytes(id, b"").unwrap().is_accepted());
    wait_for(|| writer.taken() == 1, "writer to park on the first buffer");
    assert!(registry.write_bytes(id, b"").unwrap().is_accepted());
    assert_eq!(
        agent.input_queue(),
        (1, 0),
        "one slot still buffered, zero bytes"
    );
    assert!(
        !agent.first_user_input_logged.load(Ordering::SeqCst),
        "an empty buffer is not user input, so the checkpoint is still unset"
    );

    let stamp_before = registry
        .input_stamp_result(id)
        .expect("an empty prompt with no pending sequence has a stamp");

    let refused = registry.write_input(id, b"lost prompt").unwrap();
    assert_eq!(
        refused.disposition,
        InputDisposition::Backpressured,
        "a full queue must be reported, never reported as success"
    );
    assert_eq!(refused.activity, InputActivity::default());

    // The refusal must leave every downstream signal untouched: no decoder
    // advance, no first-user-input checkpoint, no activity.
    assert!(
        !agent.first_user_input_logged.load(Ordering::SeqCst),
        "a refused write must not claim the user typed"
    );
    assert_eq!(
        registry.input_stamp_result(id),
        Ok(stamp_before),
        "the refused bytes must not have staged a draft: had they been applied, \
             the 11-byte prompt would report Err(Draft) instead"
    );

    // And it must not have quietly joined the queue either.
    assert_eq!(
        agent.input_queue(),
        (1, 0),
        "a refusal must not be accounted as queued"
    );
    assert!(
        !writer
            .release_and_collect(2)
            .iter()
            .any(|chunk| chunk == b"lost prompt"),
        "the refused buffer must never reach the PTY"
    );
    registry.kill_session(id);
}

/// A write that cannot even be attempted must leave no gauge residue.
///
/// Issue #1530 review finding 2: the sender was resolved *after* the
/// reservation, so a write arriving at an already-retired incarnation
/// stranded its bytes as a phantom backlog. `close_input` has already run
/// by then, so nothing would ever zero them, and the entry outlives the
/// writer thread — the `DIAG` line would report the leak indefinitely.
#[test]
fn a_write_to_a_retired_session_leaves_no_gauge_residue() {
    let registry = AgentProcessRegistry::new();
    let id = -930_034;
    insert_trivial_agent(&registry, id);
    let agent = registry.get(&id).unwrap();
    // The shape `close_input` leaves behind: the sender taken, so no
    // `try_send` can ever succeed and no further `reset()` is coming.
    *agent.writer_tx.lock().unwrap() = None;

    let error = registry.write_input(id, b"never lands").unwrap_err();
    assert_eq!(error, "Agent not running");
    assert_eq!(
        agent.input_queue(),
        (0, 0),
        "a write that could not be attempted must not strand gauge bytes"
    );
    assert_eq!(registry.input_queue_totals(), (0, 0));
    registry.kill_session(id);
}

/// Drain, retry, and land every byte exactly once and in order.
#[test]
fn a_drained_queue_accepts_the_retry_with_exact_bytes_and_no_duplicate() {
    let id = -930_031;
    // Capacity 1: the writer parks holding `alpha`, `beta` fills the only
    // free slot, and `gamma` is the refusal under test.
    let (registry, writer) = blocked_writer_registry(id, 1);
    let agent = registry.get(&id).unwrap();

    assert!(registry.write_input(id, b"alpha").unwrap().is_accepted());
    wait_for(|| writer.taken() == 1, "writer to park");
    assert!(registry.write_input(id, b"beta").unwrap().is_accepted());
    let refused = registry.write_input(id, b"gamma").unwrap();
    assert_eq!(refused.disposition, InputDisposition::Backpressured);
    // The first byte of a partially-drained "gamma" would prove a split.
    assert_eq!(agent.input_queue(), (1, 4));

    // Drain, then retry the identical bytes. A refusal never queued them,
    // so re-sending cannot duplicate.
    writer.release_and_collect(2);
    wait_for(
        || agent.input_queue() == (0, 0),
        "queue to return to baseline",
    );
    let retry = registry.write_input(id, b"gamma").unwrap();
    assert!(retry.is_accepted(), "a drained queue must accept the retry");

    let delivered = writer.release_and_collect(3);
    assert_eq!(
        delivered,
        vec![b"alpha".to_vec(), b"beta".to_vec(), b"gamma".to_vec()],
        "exact bytes, original order, no duplicate"
    );
    assert_eq!(
        agent.input_queue(),
        (0, 0),
        "byte usage returns to baseline after draining"
    );
    registry.kill_session(id);
}

/// The byte ceiling must refuse a write that the message ceiling would let
/// through, and — critically — must not make an oversized paste
/// undeliverable (issue #1498: a real 17,508-byte paste is one entry).
#[test]
fn the_byte_ceiling_refuses_a_backlog_a_paste_can_still_be_sent_through() {
    let id = -930_032;
    let (registry, writer) = blocked_writer_registry(id, 64);
    let agent = registry.get(&id).unwrap();
    // 32 KiB a message: 64 of these is 2 MiB, well past the 1 MiB byte
    // ceiling, so it is the *byte* bound that stops the loop below — not
    // the 64-message bound. That gap is exactly the pairing this test
    // exists to prove is load-bearing.
    let chunk = vec![b'y'; 32 * 1024];

    let mut accepted = 0;
    while registry.write_input(id, &chunk).unwrap().is_accepted() {
        accepted += 1;
        let (_messages, bytes) = agent.input_queue();
        assert!(
            bytes <= PTY_INPUT_QUEUE_BYTE_CAPACITY as u64,
            "the byte ceiling must hold, saw {bytes}"
        );
        assert!(
            accepted <= 64,
            "the fixture should never need the message bound"
        );
    }
    assert!(
        accepted > 1,
        "the fixture must build a real backlog, got {accepted}"
    );
    assert!(
        accepted < 64,
        "the byte ceiling should bite before the 64-message bound; it stopped after {accepted}"
    );
    wait_for(|| writer.taken() >= 1, "writer to park");

    // A half-cap paste is refused on top of that backlog, even though the
    // message bound still has room.
    let oversize = vec![b'x'; PTY_INPUT_QUEUE_BYTE_CAPACITY / 2 + 1];
    let refusal = registry.write_input(id, &oversize).unwrap();
    assert_eq!(
        refusal.disposition,
        InputDisposition::Backpressured,
        "a backlog under the message bound but over the byte bound must still be refused"
    );
    let (_messages, bytes) = agent.input_queue();
    assert!(
        bytes <= PTY_INPUT_QUEUE_BYTE_CAPACITY as u64,
        "the byte ceiling must hold: {bytes} > {PTY_INPUT_QUEUE_BYTE_CAPACITY}"
    );

    // With the queue drained, a buffer larger than the whole cap is still
    // admitted whole. Rejecting it would mean no paste of any size could be
    // sent, and splitting it would break issue #1498.
    writer.release_and_collect(accepted);
    wait_for(|| agent.input_queue() == (0, 0), "queue to drain");
    assert!(
        registry.write_input(id, &oversize).unwrap().is_accepted(),
        "an oversized paste must be admitted whole once the queue is empty"
    );
    assert_eq!(
        writer
            .release_and_collect(accepted + 1)
            .last()
            .unwrap()
            .len(),
        oversize.len()
    );
    registry.kill_session(id);
}

/// A #1498-sized bracketed paste stays one accepted logical write even
/// while the byte ceiling is in force.
#[test]
fn a_large_bracket_paste_stays_one_accepted_write() {
    let id = -930_033;
    let (registry, writer) = blocked_writer_registry(id, 64);
    let mut paste = b"\x1b[200~".to_vec();
    paste.extend(std::iter::repeat_n(b'p', 17_508 - 12));
    paste.extend_from_slice(b"\x1b[201~");
    assert_eq!(paste.len(), 17_508);

    assert!(registry.write_input(id, &paste).unwrap().is_accepted());
    let delivered = writer.release_and_collect(1);
    assert_eq!(
        delivered.len(),
        1,
        "a paste must never be split into several writes"
    );
    assert_eq!(delivered[0], paste, "the paste must arrive byte-for-byte");
    registry.kill_session(id);
}

/// Poll until `condition` holds, so a test never depends on how fast a
/// writer thread gets scheduled.
fn wait_for(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn insert_and_get() {
    let registry = AgentProcessRegistry::new();
    assert!(registry.is_empty());
}

/// The user-facing half of `handover_to_agent`: an empty selection must be
/// refused at the IPC boundary rather than staging nothing into a target's
/// input box (where it would look like a submitted empty prompt). The
/// command itself needs a real `AppHandle`, so the rule is tested here.
#[test]
fn handover_rejects_text_with_nothing_in_it() {
    for text in ["", " ", "\n", "\t \r\n"] {
        let err = validate_handover_text(text).expect_err("whitespace-only is not a handover");
        assert!(err.contains("Nothing to hand over"), "{err}");
    }
    // The selection is handed over as-is: surrounding whitespace is part of
    // what the user selected, so it is not trimmed off the payload.
    for text in ["fix the race\r", "\n  fix the race\n"] {
        assert!(
            validate_handover_text(text).is_ok(),
            "{text:?} carries text"
        );
    }
}

#[test]
fn write_bytes_errors_on_missing() {
    let registry = AgentProcessRegistry::new();
    let result = registry.write_bytes(999, b"test");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not running"));
}

#[test]
fn resize_pty_errors_on_missing() {
    let registry = AgentProcessRegistry::new();
    let result = registry.resize_pty(999, 80, 24);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not running"));
}

#[test]
fn is_alive_false_for_missing() {
    let registry = AgentProcessRegistry::new();
    assert!(!registry.is_alive(&999));
}

/// Fresh spawn calls `kill_session` before it starts the process. The
/// terminal has already subscribed by then, so the no-process cleanup
/// must preserve that node-scoped output subscription for the process
/// that is about to start.
#[test]
fn kill_session_without_process_preserves_output_subscription() {
    use tauri::ipc::{Channel, InvokeResponseBody};

    let session_id = -915_4010;
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_by_channel = received.clone();
    let channel = Channel::new(move |body| {
        if let InvokeResponseBody::Raw(bytes) = body {
            received_by_channel.lock().unwrap().extend(bytes);
        }
        Ok(())
    });
    crate::agent::output::register(session_id, channel);

    let registry = AgentProcessRegistry::new();
    registry.kill_session(session_id);
    crate::agent::output::ensure(session_id).send_owned(b"visible".to_vec());

    assert_eq!(&*received.lock().unwrap(), b"visible");
    crate::agent::output::unregister(session_id);
}

// -----------------------------------------------------------------------
// first_user_input timing-gate tests (spawn-latency investigation)
//
// `record_first_input_if_first` is the pure helper that `write_bytes`
// calls after a successful PTY write. It does the compare-exchange,
// emits the `spawn_timing:` log line on the false→true transition,
// and returns whether it logged. Tests pin the one-shot contract.
// -----------------------------------------------------------------------

#[test]
fn record_first_input_flips_flag_on_first_call() {
    let flag = AtomicBool::new(false);
    let spawn_start = std::time::Instant::now();

    let logged = record_first_input_if_first(&flag, spawn_start, 42);

    assert!(
        logged,
        "first call must return true (the log line was emitted)"
    );
    assert!(
        flag.load(Ordering::SeqCst),
        "first call must flip the flag to true"
    );
}

#[test]
fn record_first_input_skips_on_subsequent_calls() {
    let flag = AtomicBool::new(false);
    let spawn_start = std::time::Instant::now();

    // First call: the log line is emitted.
    assert!(record_first_input_if_first(&flag, spawn_start, 42));

    // Second call: CAS sees true→false, returns Err, helper returns false.
    // No log line is emitted — the `spawn_timing: first_user_input`
    // checkpoint fires exactly once per session, no matter how many
    // keypresses follow.
    assert!(!record_first_input_if_first(&flag, spawn_start, 42));
    assert!(!record_first_input_if_first(&flag, spawn_start, 42));

    // Flag is still true (we never reset it).
    assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn record_first_input_has_no_data_dependency() {
    // The helper takes no `data` argument — its contract is purely
    // "flip the flag once on the first invocation, never again".
    // The empty-write behavior (a zero-byte focus event still
    // tripping the gate) lives at the `write_bytes` call site: the
    // helper runs unconditionally after a successful write+flush.
    // This test pins the helper's data-independence so a future
    // refactor that adds a `data.is_empty()` guard to the helper
    // would surface as a test failure here, not as a silently-lost
    // checkpoint log in production.
    let flag = AtomicBool::new(false);
    let spawn_start = std::time::Instant::now();

    assert!(record_first_input_if_first(&flag, spawn_start, 42));
    assert!(flag.load(Ordering::SeqCst));
}

/// A `kill_session` teardown must be observable as deliberate by the
/// reader thread (via the shared `deliberate_kill` flag) — this is
/// what stops the reader's 3s early-exit heuristic from misreading a
/// stale-process kill (spawn step 2), node close, or app shutdown as
/// a failed `--resume` and stamping the node `Error` ("failed to
/// start") while a replacing spawn is booting fine.
#[test]
fn kill_session_sets_deliberate_kill_flag() {
    let recipe = SpawnRecipe {
        binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
        base_args: if cfg!(windows) {
            vec!["/c".into(), "exit".into(), "0".into()]
        } else {
            vec!["-c".into(), "exit 0".into()]
        },
        trailing_args: Vec::new(),
        windows_shell: WindowsShell::Direct,
    };
    let cwd = std::env::current_dir().unwrap();
    let cmd = spawn_environment::wrap(
        recipe,
        EnvType::Windows,
        None,
        None,
        &cwd.to_string_lossy(),
        -915_4002,
        false,
    )
    .expect("an unsandboxed command always assembles");

    let pair = open_pty_pair(24, 80).expect("open pty pair");
    let child = spawn_child(&pair, cmd).expect("spawn child");
    let writer = pair.master.take_writer().expect("take writer");

    let deliberate_kill = Arc::new(AtomicBool::new(false));
    let registry = AgentProcessRegistry::new();
    // Issue #1122: stand up the dedicated writer thread + channel
    // before inserting, mirroring the production `register_agent`
    // ordering. The test only asserts `deliberate_kill` propagation,
    // not the writer thread behaviour, but the registry now requires
    // the channel field to be present.
    // Capacity matches production (`PTY_WRITER_CHANNEL_CAPACITY`) on purpose.
    // A capacity of 1 made these decoding fixtures depend on the writer
    // thread winning a scheduling race, and the pre-#1530 silent drop hid
    // the flakiness by reporting a full queue as a successful write. These
    // tests are about input decoding, not about queue saturation — the
    // saturation path has its own fixtures.
    let (writer_tx, writer_rx) =
        std::sync::mpsc::sync_channel::<Vec<u8>>(crate::agent::spawn::PTY_WRITER_CHANNEL_CAPACITY);
    let queue = Arc::new(InputQueueGauge::default());
    let writer_queue = Arc::clone(&queue);
    let writer_thread = std::thread::spawn(move || {
        let mut writer = writer;
        while let Ok(bytes) = writer_rx.recv() {
            writer_queue.release(bytes.len());
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    });
    registry.insert(
        -915_4002,
        AgentProcess::new(
            child,
            writer_tx,
            queue,
            Some(writer_thread),
            pair.master,
            Arc::new(AtomicBool::new(true)),
            deliberate_kill.clone(),
            None,
            None,
            std::time::Instant::now(),
            0,
        ),
    );

    registry.kill_session(-915_4002);

    assert!(
        deliberate_kill.load(Ordering::SeqCst),
        "kill_session must set deliberate_kill before closing the PTY, \
             so the reader epilogue skips the early-exit Error heuristic"
    );
}

/// Issue #1531, end to end: killing a session whose writer thread is
/// parked in `recv()` must leave no writer blocked on the channel, must
/// reap the registry entry, and must not wedge.
///
/// This is deliberately *not* the test that pins the close-before-join
/// ordering — `teardown_closes_input_before_joining_the_writer` is, and
/// it asserts a flag instead of the clock. The distinction is not
/// cosmetic (issue #2049): when the sender is dropped after the join the
/// writer still exits moments later, so by the time `kill_session`
/// returns `writer_exited` is set either way, and the only other signal
/// is a two-second delay that `kill_process_tree` (`taskkill /F /T`, a
/// subprocess spawn that measured 2.20s on its own on a loaded 24-core
/// host) makes indistinguishable from contention noise. The old 1.5s
/// assertion here could only ever flake, never prove.
#[test]
fn kill_session_unblocks_idle_writer_promptly() {
    let recipe = SpawnRecipe {
        binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
        base_args: if cfg!(windows) {
            vec!["/c".into(), "exit".into(), "0".into()]
        } else {
            vec!["-c".into(), "exit 0".into()]
        },
        trailing_args: Vec::new(),
        windows_shell: WindowsShell::Direct,
    };
    let cwd = std::env::current_dir().unwrap();
    let session_id = -915_1531;
    let cmd = spawn_environment::wrap(
        recipe,
        EnvType::Windows,
        None,
        None,
        &cwd.to_string_lossy(),
        session_id,
        false,
    )
    .expect("an unsandboxed command always assembles");

    let pair = open_pty_pair(24, 80).expect("open pty pair");
    let child = spawn_child(&pair, cmd).expect("spawn child");
    let writer = pair.master.take_writer().expect("take writer");

    let writer_exited = Arc::new(AtomicBool::new(false));
    let writer_exited_thread = writer_exited.clone();
    // Capacity matches production (`PTY_WRITER_CHANNEL_CAPACITY`) on purpose.
    // A capacity of 1 made these decoding fixtures depend on the writer
    // thread winning a scheduling race, and the pre-#1530 silent drop hid
    // the flakiness by reporting a full queue as a successful write. These
    // tests are about input decoding, not about queue saturation — the
    // saturation path has its own fixtures.
    let (writer_tx, writer_rx) =
        std::sync::mpsc::sync_channel::<Vec<u8>>(crate::agent::spawn::PTY_WRITER_CHANNEL_CAPACITY);
    let queue = Arc::new(InputQueueGauge::default());
    let writer_queue = Arc::clone(&queue);
    let writer_thread = std::thread::spawn(move || {
        let mut writer = writer;
        while let Ok(bytes) = writer_rx.recv() {
            writer_queue.release(bytes.len());
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
        writer_exited_thread.store(true, Ordering::SeqCst);
    });

    let registry = AgentProcessRegistry::new();
    registry.insert(
        session_id,
        AgentProcess::new(
            child,
            writer_tx,
            queue,
            Some(writer_thread),
            pair.master,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
            None,
            None,
            std::time::Instant::now(),
            0,
        ),
    );

    let started = std::time::Instant::now();
    // Hold our own handle so the AgentProcess outlives its removal from the
    // registry. Without it, dropping the registry entry drops `writer_tx`
    // and closes the channel anyway, so the test cannot tell whether
    // `kill_session` dropped the sender itself — which is the #1531
    // property under test.
    let held = registry.get(&session_id).expect("inserted above");
    registry.kill_session(session_id);
    let elapsed = started.elapsed();

    // The #1531 regression — joining before dropping the sender, so the
    // join pays `join_with_timeout`'s 2s fallback — is detected
    // deterministically: if `close_input()` did not run, `writer_tx` still
    // holds its sender and this fails regardless of timing.
    assert!(
        held.writer_tx.lock().unwrap().is_none(),
        "kill_session must drop the writer sender before joining (#1531); \
             it is still held {elapsed:?} after the call"
    );
    // Wall clock is only a hang guard here, not the discriminator. The
    // previous 1500ms bound sat inside the range the scheduler actually
    // needs under CPU contention — this test has taken 2.24s on a loaded
    // machine while the join completed without the fallback — so it fired
    // on healthy runs (issue #2049 class).
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "kill_session must not hang: took {elapsed:?}"
    );
    assert!(
        writer_exited.load(Ordering::SeqCst),
        "the idle writer thread must have stopped by the time \
             kill_session returns; nothing may stay blocked in recv() \
             holding the PTY writer"
    );
    assert!(
        !registry.contains(&session_id),
        "kill_session must reap the current incarnation from the registry"
    );
}

/// The #1531 ordering invariant itself, asserted deterministically:
/// `teardown_incarnation` must drop the input sender *before* joining
/// the writer, so an idle writer is joined instead of hitting the
/// two-second fallback and being detached.
///
/// The return value is the fact that distinguishes the two orders; the
/// elapsed time cannot (issue #2049: under CPU contention the clock
/// cannot tell a joined writer from a detached one). Reordering the two
/// steps makes this fail in ~2s with the message below, and it does so
/// without depending on how fast the host happens to run `taskkill`.
#[test]
fn teardown_closes_input_before_joining_the_writer() {
    let session_id = -915_1539;
    let (agent, writer_exited) = idle_writer_process(session_id);

    // `JoinPolicy::Both` mirrors `kill_session`; this fixture has no
    // reader handle, so only the writer join is exercised.
    let outcome = teardown_incarnation(session_id, &agent, JoinPolicy::Both, false);

    assert!(outcome.reader_joined.is_none());
    assert!(
        outcome.writer_joined,
        "teardown_incarnation must close the writer channel before \
             joining: an idle writer was detached at the two-second \
             fallback instead of being joined (issue #1531)"
    );
    assert!(
        writer_exited.load(Ordering::SeqCst),
        "the joined writer must have run its exit path"
    );
}

#[test]
fn insert_assigns_distinct_generation_tokens() {
    let registry = AgentProcessRegistry::new();
    let (g1, _) = insert_trivial_agent(&registry, -915_1532);
    let (g2, _) = insert_trivial_agent(&registry, -915_1533);
    assert_ne!(g1, 0, "insert must overwrite the unassigned 0 token");
    assert_ne!(g2, 0);
    assert_ne!(g1, g2);
    registry.kill_session(-915_1532);
    registry.kill_session(-915_1533);
}

#[test]
fn stale_generation_cannot_remove_replacement() {
    let registry = AgentProcessRegistry::new();
    let session_id = -915_1534;
    let (gen1, _) = insert_trivial_agent(&registry, session_id);
    registry.kill_session(session_id);
    let (gen2, _) = insert_trivial_agent(&registry, session_id);
    assert_ne!(gen1, gen2);

    registry.reap_incarnation(session_id, gen1);
    assert!(
        registry.contains(&session_id),
        "an old incarnation must not reap the replacement"
    );
    assert_eq!(registry.get(&session_id).unwrap().generation, gen2);

    registry.kill_session(session_id);
    assert!(!registry.contains(&session_id));
}

#[test]
fn reap_incarnation_cleans_up_natural_exit() {
    let registry = AgentProcessRegistry::new();
    let session_id = -915_1535;
    let (generation, writer_exited) = insert_trivial_agent(&registry, session_id);

    registry.reap_incarnation(session_id, generation);

    assert!(
        !registry.contains(&session_id),
        "natural exit must drop the registry entry"
    );
    assert!(
        writer_exited.load(Ordering::SeqCst),
        "natural exit must close the writer channel so the thread exits"
    );
}

#[test]
fn reap_incarnation_does_not_reap_a_replacement() {
    let registry = AgentProcessRegistry::new();
    let session_id = -915_1536;
    let (gen1, _) = insert_trivial_agent(&registry, session_id);
    registry.kill_session(session_id);
    let (gen2, replacement_writer_exited) = insert_trivial_agent(&registry, session_id);

    registry.reap_incarnation(session_id, gen1);

    assert!(
        registry.contains(&session_id),
        "old reader EOF must not remove the replacement process"
    );
    assert_eq!(registry.get(&session_id).unwrap().generation, gen2);
    assert!(
        !replacement_writer_exited.load(Ordering::SeqCst),
        "old EOF must not close the replacement's input channel"
    );

    registry.kill_session(session_id);
}

#[test]
fn reap_incarnation_is_a_noop_after_kill_session() {
    let registry = AgentProcessRegistry::new();
    let session_id = -915_1537;
    let (generation, _) = insert_trivial_agent(&registry, session_id);
    registry.kill_session(session_id);
    assert!(!registry.contains(&session_id));

    registry.reap_incarnation(session_id, generation);
    assert!(
        !registry.contains(&session_id),
        "kill_session already claimed exclusive teardown"
    );
}

/// Restricted-token grants are keyed by session id. Phase 3 registers
/// the *replacement*'s grants before `insert` tears down `prev`; that
/// overwrite teardown must not call `sandbox_cleanup` or it revokes
/// the live child's ACEs.
#[cfg(target_os = "windows")]
#[test]
fn insert_replacement_does_not_revoke_new_sandbox_grants() {
    use crate::sandbox::spawn::{
        cleanup_restricted, has_restricted_cleanup, insert_restricted_cleanup_for_test,
    };

    let registry = AgentProcessRegistry::new();
    let session_id = -915_1538;
    let (_gen1, _) = insert_trivial_agent(&registry, session_id);

    // Simulate phase 3 of the replacement spawn: grants already live
    // under this session id before register_agent → insert.
    insert_restricted_cleanup_for_test(session_id);
    assert!(has_restricted_cleanup(session_id));

    let (_gen2, _) = insert_trivial_agent(&registry, session_id);
    assert!(
        has_restricted_cleanup(session_id),
        "overwriting prev must not revoke the replacement's session-scoped sandbox grants"
    );

    registry.kill_session(session_id);
    assert!(
        !has_restricted_cleanup(session_id),
        "kill_session must still revoke grants for the final incarnation"
    );
    // Belt-and-braces if an earlier assert failed mid-test.
    cleanup_restricted(session_id);
}

/// Regression guard for issue #287: when the agent CLI exits
/// naturally, the watcher must drop the master within ~1s of the
/// exit so the reader thread can EOF and `reader_alive` can flip
/// to `false`. Without this, a Windows 28120 ConPTY build leaves
/// the node stuck "running" until the app restarts.
///
/// Platform-agnostic by design: we drive a real portable-pty child
/// (so the types match production exactly) but only assert the
/// watcher's observable contract — that it takes the master once
/// the child has exited.
#[test]
fn watcher_drops_master_after_child_exit() {
    // Trivial command: exit immediately, no output. The watcher
    // is what we're testing, not the child's output, so a 0-byte
    // recipe keeps the test focused.
    let recipe = SpawnRecipe {
        binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
        base_args: if cfg!(windows) {
            vec!["/c".into(), "exit".into(), "0".into()]
        } else {
            vec!["-c".into(), "exit 0".into()]
        },
        trailing_args: Vec::new(),
        windows_shell: WindowsShell::Direct,
    };
    let cwd = std::env::current_dir().unwrap();
    let cmd = spawn_environment::wrap(
        recipe,
        EnvType::Windows,
        None,
        None,
        &cwd.to_string_lossy(),
        -915_4001,
        false,
    )
    .expect("an unsandboxed command always assembles");

    let pair = open_pty_pair(24, 80).expect("open pty pair");
    let child = spawn_child(&pair, cmd).expect("spawn child");

    let child_arc: Arc<Mutex<Box<dyn Child + Send + Sync>>> = Arc::new(Mutex::new(child));
    let master_arc: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
        Arc::new(Mutex::new(Some(pair.master)));

    let watcher = watch_child_exit(child_arc.clone(), master_arc.clone());

    // ConPTY startup may negotiate terminal capabilities before launching
    // the child. Measure the teardown deadline from observed child exit,
    // not from spawn (this test deliberately has no terminal responder).
    let exit_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while child_arc.lock().unwrap().try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < exit_deadline,
            "probe child did not exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // The watcher polls every 500ms, so retain the original 2s bound.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while master_arc.lock().unwrap().is_some() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        master_arc.lock().unwrap().is_none(),
        "watch_child_exit should drop the master within 2s of child exit (issue #287: \
             without this, a Windows 28120 ConPTY build leaves nodes stuck 'running')"
    );

    // The watcher self-terminates after taking the master; this
    // join is best-effort and just keeps the test process clean.
    let _ = watcher.join();
}

// -----------------------------------------------------------------------
// provider_is_plain_terminal attention-signal tests (issue #535 / #550)
//
// These pin the per-provider attention-cleared gating without driving
// `write_to_agent`'s full PTY path. A plain terminal's Enter is just
// shell input — flipping status to `Running` would render a spurious
// cyan badge for a shell sitting at a prompt, so we skip the
// `attention-cleared` emit (and the `Running` status flip) for
// Terminal-typed nodes.
// -----------------------------------------------------------------------

#[test]
fn plain_terminal_provider_skips_attention_signals() {
    // The "terminal" harness id resolves to the plain-shell executor.
    assert!(provider_is_plain_terminal("terminal"));
}

/// Esc and Ctrl+C arrive from xterm as single-byte writes; an arrow key
/// or any longer sequence starting with Esc is not an interrupt.
#[test]
fn only_bare_esc_and_ctrl_c_are_interrupt_keys() {
    assert!(is_interrupt_key(b"\x1b"));
    assert!(is_interrupt_key(b"\x03"));
    for other in [b"\x1b[A".as_slice(), b"\x1b\x1b", b"\r", b"q", b""] {
        assert!(!is_interrupt_key(other), "{other:?}");
    }
}

/// MiniMax recorded its aborted tool three seconds after Esc, past a
/// single fixed check. The watcher keeps looking until the record lands,
/// and stops as soon as the node moves on by another path.
#[test]
fn interrupt_watch_waits_for_a_late_record_and_stops_when_the_node_moves() {
    use std::cell::RefCell;
    let run = |answers: &[InterruptProbe], attempts: u32| {
        let answers = RefCell::new(answers.iter().copied());
        let calls = RefCell::new(0);
        let recorded = await_interrupt_record(attempts, std::time::Duration::ZERO, || {
            *calls.borrow_mut() += 1;
            answers
                .borrow_mut()
                .next()
                .unwrap_or(InterruptProbe::NotYet)
        });
        (recorded, calls.into_inner())
    };
    use InterruptProbe::*;
    assert_eq!(run(&[NotYet, NotYet, Recorded], 20), (true, 3));
    assert_eq!(run(&[NotYet, Moved, Recorded], 20), (false, 2));
    assert_eq!(run(&[], 4), (false, 4));
    let window = crate::agent::session_lifecycle::INTERRUPT_POLL
        * crate::agent::session_lifecycle::INTERRUPT_ATTEMPTS;
    assert!(window >= std::time::Duration::from_secs(6), "{window:?}");
}

/// Only harnesses that report a prompt starting a turn may have an
/// unconfirmed Enter reverted; for the rest a quiet Enter may be work.
#[test]
fn prompt_submission_confirmation_follows_the_harness() {
    for id in ["anthropic", "codex", "grok", "opencode"] {
        assert!(harness_confirms_prompt_submission(id), "{id}");
    }
    for id in ["agy", "mcode", "kimi", "terminal", "cursor"] {
        assert!(!harness_confirms_prompt_submission(id), "{id}");
    }
}

/// Issue #2344: re-sending is only safe for a harness that *discards* early
/// input. OpenCode's TUI does (its renderer drains stdin when it attaches
/// the reader); everything else either buffers the keystrokes — in which
/// case a replay would run the prompt twice — or has never reported the
/// drop at all.
#[test]
fn submit_replay_follows_the_harness() {
    assert!(
        harness_replays_unconfirmed_submit("opencode"),
        "opencode discards input written before its TUI attaches the reader"
    );
    for id in [
        "anthropic",
        "codex",
        "grok",
        "agy",
        "mcode",
        "kimi",
        "cursor",
        "terminal",
    ] {
        assert!(!harness_replays_unconfirmed_submit(id), "{id}");
    }
}

/// Feed keystrokes through the real decoder, exactly as the enqueue path
/// does, so the retention is exercised against the activity and prompt
/// length it will actually be handed — not a hand-written activity that
/// could agree with the implementation by accident.
fn feed(replay: &mut SubmitReplay, input: &mut TerminalInput, data: &[u8]) {
    let activity = input.accept_event(data);
    replay.record(data, activity, input.len);
}

/// The retained bytes must be the stream the user actually produced,
/// editing keys included — re-encoding the prompt from the decoder's
/// character count would replay the wrong text.
#[test]
fn the_retained_draft_is_the_exact_keystroke_stream() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    feed(&mut replay, &mut input, b"fix the parser");
    feed(&mut replay, &mut input, b"\x7f");
    feed(&mut replay, &mut input, b"login");
    feed(&mut replay, &mut input, b"\r");
    assert_eq!(
        replay.take_replay(true, true),
        Ok(b"fix the parser\x7flogin\r".to_vec()),
        "the backspace belongs in the replayed prompt, not the decoded one"
    );
}

/// The Enter the user pressed decides the terminator; xterm sends `\r`, but
/// a paste or a raw byte stream can carry `\n`.
#[test]
fn the_replay_repeats_the_terminator_that_was_pressed() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    feed(&mut replay, &mut input, b"go\n");
    assert_eq!(replay.take_replay(true, true), Ok(b"go\n".to_vec()));
}

/// A single buffer can arrive with the prompt and its Enter already
/// joined — a paste, or `write_bytes` streaming raw bytes. Reading the
/// submit as "the draft is whatever came before, nothing since" would
/// retain an *empty* prompt and replay nothing but a bare Enter, which is
/// the one thing that cannot start a turn.
#[test]
fn a_buffer_carrying_the_prompt_and_its_enter_retains_the_prompt() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    feed(&mut replay, &mut input, b"run the tests\r");
    assert_eq!(
        replay.take_replay(true, true),
        Ok(b"run the tests\r".to_vec())
    );

    // The second prompt in the same buffer is its own, not part of the
    // first one's draft.
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    feed(&mut replay, &mut input, b"first\rsecond\r");
    assert_eq!(replay.take_replay(true, true), Ok(b"first\r".to_vec()));
}

/// Terminal replies are the harness talking, not the user typing. Echoing
/// one back into the composer would corrupt the very prompt we are trying
/// to rescue.
#[test]
fn terminal_replies_are_never_retained_as_prompt_text() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    for packet in [
        b"\x1b[I".as_slice(),
        b"\x1b[12;34R",
        b"\x1b]11;rgb:ff/00/ab\x07",
    ] {
        feed(&mut replay, &mut input, packet);
    }
    feed(&mut replay, &mut input, b"hello");
    feed(&mut replay, &mut input, b"\r");
    assert_eq!(replay.take_replay(true, true), Ok(b"hello\r".to_vec()));
}

/// Text the composer has discarded is not a prompt. Ctrl+C, and
/// backspacing every character away, both empty it — and if the retention
/// kept those bytes the next replay would type them out in front of the
/// user.
#[test]
fn discarded_prompt_text_is_not_carried_into_the_next_prompt() {
    // Each clear must leave the decoder reading as an *empty* prompt; a partial
    // backspace run would leave characters behind and is not a discard at
    // all. The runs are sized from the text they erase so the fixture
    // cannot drift from the model it is asserting about.
    let typed = "abandoned".len();
    let clears: [Vec<u8>; 3] = [b"\x03".to_vec(), vec![0x7f; typed], {
        // Type past the empty prompt, then back over both runs.
        let mut typed_more = b"more".to_vec();
        typed_more.extend(std::iter::repeat(0x7f).take(typed + "more".len()));
        typed_more
    }];
    for clear in &clears {
        let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
        feed(&mut replay, &mut input, b"abandoned");
        feed(&mut replay, &mut input, clear);
        assert!(
            input.empty_prompt(),
            "fixture precondition: {clear:?} must empty the composer"
        );
        feed(&mut replay, &mut input, b"kept");
        feed(&mut replay, &mut input, b"\r");
        assert_eq!(
            replay.take_replay(true, true),
            Ok(b"kept\r".to_vec()),
            "clear {clear:?} must not survive into the replayed prompt"
        );
    }
}

/// Every skip reason is reachable, and each is a decision rather than a
/// crash: a replay that lands on top of the user's newer text would be
/// worse than no replay at all.
#[test]
fn a_replay_is_refused_when_it_would_not_be_the_users_prompt() {
    let submitted = |replay: &mut SubmitReplay, input: &mut TerminalInput| {
        feed(replay, input, b"first");
        feed(replay, input, b"\r");
    };

    let (mut opted_out, mut input) = (SubmitReplay::default(), TerminalInput::default());
    submitted(&mut opted_out, &mut input);
    assert_eq!(
        opted_out.take_replay(false, true),
        Err(ReplaySkip::HarnessOptedOut)
    );

    let (mut occupied, mut input) = (SubmitReplay::default(), TerminalInput::default());
    submitted(&mut occupied, &mut input);
    feed(&mut occupied, &mut input, b"resent");
    assert_eq!(
        occupied.take_replay(true, false),
        Err(ReplaySkip::PromptOccupied),
        "text typed since the submit must never be overwritten"
    );
    // The refusal puts the draft back, so the *next* submit can still be
    // rescued rather than being consumed by a probe.
    feed(&mut occupied, &mut input, b"\x7f\x7f\x7f\x7f\x7f\x7f");
    assert_eq!(occupied.take_replay(true, true), Ok(b"first\r".to_vec()));

    let mut empty = SubmitReplay::default();
    assert_eq!(
        empty.take_replay(true, true),
        Err(ReplaySkip::NoRetainedDraft)
    );
}

/// A draft past the retention cap is abandoned whole. Half a prompt
/// replayed as a prompt is indistinguishable, to the user, from their own
/// text — the one outcome the replay must never produce.
#[test]
fn an_oversized_draft_is_abandoned_rather_than_truncated() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    for _ in 0..3 {
        feed(
            &mut replay,
            &mut input,
            &vec![b'x'; SUBMIT_REPLAY_BYTE_CAPACITY / 2],
        );
    }
    feed(&mut replay, &mut input, b"\r");
    assert_eq!(
        replay.take_replay(true, true),
        Err(ReplaySkip::NoRetainedDraft)
    );
}

/// Issue #2344's headline regression, end to end through the real enqueue
/// path: the prompt typed into a fresh node goes out once, and when the
/// harness confirms nothing the same prompt goes out again — exactly once.
#[test]
fn an_unconfirmed_prompt_reaches_the_pty_again_and_only_once() {
    let id = -930_040;
    let (registry, rx) = testing::capturing_registry(id);
    let agent = registry.get(&id).unwrap();

    for chunk in [&b"fix "[..], b"the parser", b"\r"] {
        assert!(registry.write_input(id, chunk).unwrap().is_accepted());
    }
    assert_eq!(rx.recv().unwrap(), b"fix ");
    assert_eq!(rx.recv().unwrap(), b"the parser");
    assert_eq!(rx.recv().unwrap(), b"\r");
    assert!(
        rx.try_recv().is_err(),
        "the first send is the only one so far"
    );

    // The revert found the node back at an empty prompt, so the harness
    // lost these bytes: re-send the whole prompt, terminator included.
    assert_eq!(
        agent.replay_unconfirmed_submit(true),
        SubmitReplayOutcome::Sent
    );
    assert_eq!(rx.recv().unwrap(), b"fix the parser\r");

    // One shot per incarnation — a second silent revert must not stack
    // copies of the same prompt into the composer.
    assert_eq!(
        agent.replay_unconfirmed_submit(true),
        SubmitReplayOutcome::Skipped(ReplaySkip::AlreadyReplayed)
    );
    assert!(rx.try_recv().is_err(), "no second replay");
}

/// An abandoned prompt must not come back. Submit, then cancel with Ctrl+C:
/// the composer is empty again, so the replay's "still empty" fence would
/// pass and a retained draft would put the cancelled prompt back on the
/// wire 30 seconds after the user abandoned it.
#[test]
fn a_prompt_the_user_cancelled_after_submitting_is_never_replayed() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    feed(&mut replay, &mut input, b"delete everything\r");
    assert!(input.empty_prompt(), "the submit empties the composer");
    feed(&mut replay, &mut input, b"\x03");
    assert!(
        input.empty_prompt(),
        "fixture precondition: Ctrl+C must leave the composer empty"
    );
    assert_eq!(
        replay.take_replay(true, true),
        Err(ReplaySkip::NoRetainedDraft),
        "an abandoned prompt must not be re-sent"
    );
}

/// Erasing a line is not a cancel. The user is correcting a typo before
/// retyping, and the unconfirmed submit behind it is still the only
/// recoverable work — so backspacing the composer empty must not throw that
/// prompt away. Contrast with the Ctrl+C test above.
#[test]
fn erasing_the_composer_does_not_abandon_the_outstanding_prompt() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    feed(&mut replay, &mut input, b"first\r");
    feed(&mut replay, &mut input, b"resent");
    assert_eq!(
        replay.take_replay(true, false),
        Err(ReplaySkip::PromptOccupied),
        "text typed since the submit is not ours to overwrite"
    );
    feed(&mut replay, &mut input, &[0x7f; 6]);
    assert!(input.empty_prompt());
    assert_eq!(
        replay.take_replay(true, true),
        Ok(b"first\r".to_vec()),
        "the abandoned line is recoverable again once the composer is clear"
    );
}

/// A compound buffer carries more than one line ending; the draft's
/// terminator is the one that submitted *it*, not whichever came last.
#[test]
fn a_compound_buffer_keeps_each_prompt_own_terminator() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    feed(&mut replay, &mut input, b"cmd1\rcmd2\n");
    assert_eq!(
        replay.take_replay(true, true),
        Ok(b"cmd1\r".to_vec()),
        "cmd1 was submitted by CR; the trailing LF belongs to cmd2"
    );
}

/// What survives an occupied prompt is the *next* submit, never the one that
/// was skipped. A retained draft is only ever read from a revert armed by a
/// submit, and that submit overwrites it — so putting it back cannot
/// resurrect a stale prompt, and dropping it would strand the newest one.
#[test]
fn a_skipped_replay_leaves_the_next_submit_as_the_only_candidate() {
    let (mut replay, mut input) = (SubmitReplay::default(), TerminalInput::default());
    feed(&mut replay, &mut input, b"occupied\r");
    feed(&mut replay, &mut input, b"typed");
    feed(&mut replay, &mut input, b"\x03");
    assert_eq!(
        replay.take_replay(true, true),
        Err(ReplaySkip::NoRetainedDraft),
        "erasing the line clears the outstanding prompt"
    );

    feed(&mut replay, &mut input, b"next\r");
    assert_eq!(
        replay.take_replay(true, true),
        Ok(b"next\r".to_vec()),
        "the newest submit is the one that can be rescued"
    );
}

/// A re-send is published like any other submit, so the node leaves `Ready`
/// when the TUI starts. The window that re-arms then finds the one-shot
/// spent, so a replay that also goes unconfirmed reverts the status without
/// putting the prompt on the wire twice.
#[test]
fn a_replayed_prompt_leaves_an_empty_composer_and_spends_the_one_shot() {
    let id = -930_044;
    let (registry, rx) = testing::capturing_registry(id);
    let agent = registry.get(&id).unwrap();
    assert!(registry
        .write_input(id, b"fix the parser\r")
        .unwrap()
        .is_accepted());
    while rx.try_recv().is_ok() {}

    assert_eq!(
        agent.replay_unconfirmed_submit(true),
        SubmitReplayOutcome::Sent
    );
    assert_eq!(rx.recv().unwrap(), b"fix the parser\r");
    assert!(agent.input_decoder.lock().unwrap().empty_prompt());

    assert_eq!(
        agent.replay_unconfirmed_submit(true),
        SubmitReplayOutcome::Skipped(ReplaySkip::AlreadyReplayed),
        "the re-armed window must revert without re-sending"
    );
    assert!(rx.try_recv().is_err(), "no duplicate prompt on the wire");
}

/// A harness that never dropped anything must not be re-fed: the flag is
/// consulted at the PTY, not inferred from the revert alone.
#[test]
fn a_harness_that_did_not_opt_in_is_never_replayed() {
    let id = -930_041;
    let (registry, rx) = testing::capturing_registry(id);
    let agent = registry.get(&id).unwrap();
    assert!(registry.write_input(id, b"hello\r").unwrap().is_accepted());
    while rx.try_recv().is_ok() {}
    assert_eq!(
        agent.replay_unconfirmed_submit(false),
        SubmitReplayOutcome::Skipped(ReplaySkip::HarnessOptedOut)
    );
    assert!(rx.try_recv().is_err(), "nothing re-sent");
}

/// A confirmed submit re-arms the protection, so a node left open for days
/// does not silently go back to dropping the first prompt of every later
/// turn once its one replay has been spent.
#[test]
fn a_confirmed_submit_rearms_the_replay() {
    let id = -930_042;
    let (registry, rx) = testing::capturing_registry(id);
    let agent = registry.get(&id).unwrap();
    let type_and_submit = |registry: &AgentProcessRegistry| {
        assert!(registry.write_input(id, b"first\r").unwrap().is_accepted());
        while rx.try_recv().is_ok() {}
    };

    type_and_submit(&registry);
    assert_eq!(
        agent.replay_unconfirmed_submit(true),
        SubmitReplayOutcome::Sent
    );
    assert!(rx.try_recv().is_ok(), "the dropped prompt went back out");
    while rx.try_recv().is_ok() {}

    // The harness confirmed that one, so the next drop is protected too.
    agent.rearm_submit_replay();
    type_and_submit(&registry);
    assert_eq!(
        agent.replay_unconfirmed_submit(true),
        SubmitReplayOutcome::Sent
    );
    assert_eq!(rx.recv().unwrap(), b"first\r");
}

/// The replay goes through the same decoder as a human keystroke, so the
/// node is left exactly where a typed-and-accepted prompt leaves it: at an
/// empty prompt.
#[test]
fn a_replayed_prompt_leaves_the_composer_empty() {
    let id = -930_043;
    let (registry, _rx) = testing::capturing_registry(id);
    let agent = registry.get(&id).unwrap();
    assert!(registry.write_input(id, b"hello\r").unwrap().is_accepted());
    assert_eq!(
        agent.replay_unconfirmed_submit(true),
        SubmitReplayOutcome::Sent
    );
    assert!(
        agent.input_decoder.lock().unwrap().empty_prompt(),
        "the replayed submit must clear the composer, not sit in it"
    );
}

#[test]
fn llm_providers_do_emit_attention_signals() {
    // Every non-terminal harness id resolves to an LLM executor, so none skip
    // attention signals. "minimax"/"kimi" are legacy ids that now resolve to
    // the Anthropic executor (still an LLM, still not plain-terminal).
    for id in ["anthropic", "minimax", "kimi", "agy", "opencode", "codex"] {
        assert!(
            !provider_is_plain_terminal(id),
            "LLM provider {id:?} should not skip attention signals"
        );
    }
}

/// Submitting a prompt must publish the stored `work_resumed` envelope.
/// The legacy attention-clear event has no status, so a desktop that
/// still shows "Starting…" never leaves it.
#[test]
fn submitted_input_publishes_the_running_lifecycle() {
    let blocking = crate::agent::session_lifecycle::testing::RecordingSink::new();
    let payload = crate::agent::session_lifecycle::commit_user_resumed(&blocking, 7)
        .unwrap()
        .expect("a live row accepts the resume");
    let desktop = crate::agent::session_lifecycle::testing::RecordingSink::new();
    super::publish_submitted_input(&desktop, payload);
    let events = desktop.lifecycle_changed();
    assert_eq!(
        events.len(),
        1,
        "submitting input must publish agent-lifecycle, not only the legacy clear"
    );
    assert_eq!(events[0].session_id, 7);
    assert_eq!(events[0].status, crate::models::SessionStatus::Running);
    assert_eq!(
        events[0].kind,
        crate::agent::session_lifecycle::LifecycleKind::WorkResumed
    );
    assert_eq!(*desktop.attention_cleared(), vec![7]);
}

/// Regression test for the sync core: an unregistered session must short-
/// circuit on the PTY write before the DB read. Without the `?` ordering
/// the unit-test DB-not-initialised panic surfaces as a test failure.
/// The full PTY-write + DB-update path needs a registered agent
/// (real `portable_pty` handles) and is covered by integration tests.
#[test]
fn write_to_agent_blocking_unknown_session_short_circuits_before_db() {
    let result = write_to_agent_blocking(999_999_999, "x".to_string());
    let err = result.expect_err("unknown session must surface an error");
    assert!(
        err.contains("Agent not running"),
        "expected 'Agent not running' error, got {err:?}"
    );
}
