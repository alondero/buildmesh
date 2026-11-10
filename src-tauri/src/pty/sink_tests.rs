use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

fn collecting_channel(received: &Arc<Mutex<Vec<u8>>>) -> Channel<InvokeResponseBody> {
    let rec = received.clone();
    Channel::new(move |body| {
        if let InvokeResponseBody::Raw(bytes) = body {
            rec.lock().unwrap().extend_from_slice(&bytes);
        }
        Ok(())
    })
}

fn counting_channel(hits: &Arc<AtomicUsize>) -> Channel<InvokeResponseBody> {
    let h = hits.clone();
    Channel::new(move |_| {
        h.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
}

#[test]
fn circuit_buffered_colour_queries_do_not_inject_late_input() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = OutputSink::new();
    sink.send_owned(b"before\x1b]10;?".to_vec());
    sink.send_owned(b"\x1b\\middle\x1b]11;?\x07after\x1b[31mred\x1b[0m".to_vec());
    sink.set(collecting_channel(&received));
    assert_eq!(
        &*received.lock().unwrap(),
        b"beforemiddleafter\x1b[31mred\x1b[0m"
    );
    sink.send_owned(b"\x1b]10;?\x1b\\".to_vec());
    assert!(
        received.lock().unwrap().ends_with(b"\x1b]10;?\x1b\\"),
        "live terminal queries must still reach xterm"
    );
}

#[test]
fn circuit_buffered_colour_query_split_at_subscribe_does_not_reply_late() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = OutputSink::new();
    sink.send_owned(b"before\x1b]10;?".to_vec());
    sink.set(collecting_channel(&received));
    assert_eq!(&*received.lock().unwrap(), b"before");

    sink.send_owned(b"\x1b\\after".to_vec());
    assert_eq!(&*received.lock().unwrap(), b"beforeafter");

    sink.send_owned(b"\x1b]11;?\x07".to_vec());
    assert!(received.lock().unwrap().ends_with(b"\x1b]11;?\x07"));
}

#[test]
fn circuit_buffered_non_query_colour_osc_is_preserved_across_subscribe() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = OutputSink::new();
    sink.send_owned(b"before\x1b]10;".to_vec());
    sink.set(collecting_channel(&received));
    assert_eq!(&*received.lock().unwrap(), b"before");

    sink.send_owned(b"rgb:abab/cdcd/efef\x07after".to_vec());
    assert_eq!(
        &*received.lock().unwrap(),
        b"before\x1b]10;rgb:abab/cdcd/efef\x07after"
    );
}

#[test]
fn circuit_pending_colour_osc_prefix_survives_channel_replacement() {
    let first_received = Arc::new(Mutex::new(Vec::new()));
    let second_received = Arc::new(Mutex::new(Vec::new()));
    let sink = OutputSink::new();
    sink.send_owned(b"before\x1b]10;".to_vec());
    sink.set(collecting_channel(&first_received));
    assert_eq!(&*first_received.lock().unwrap(), b"before");

    sink.set(collecting_channel(&second_received));
    sink.send_owned(b"rgb:abab/cdcd/efef\x07after".to_vec());

    assert_eq!(&*first_received.lock().unwrap(), b"before");
    assert_eq!(
        &*second_received.lock().unwrap(),
        b"\x1b]10;rgb:abab/cdcd/efef\x07after"
    );
}

#[test]
fn send_before_subscribe_flushes_in_order_on_register() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = OutputSink::new();
    sink.send_owned(b"hel".to_vec());
    sink.send_owned(b"lo".to_vec());
    sink.set(collecting_channel(&received));
    assert_eq!(&*received.lock().unwrap(), b"hello");
}

#[test]
fn send_after_subscribe_goes_to_channel_without_cloning_per_chunk() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = OutputSink::new();
    sink.set(collecting_channel(&received));
    sink.send_owned(b"ab".to_vec());
    sink.send_owned(b"cd".to_vec());
    assert_eq!(&*received.lock().unwrap(), b"abcd");
}

#[test]
fn closed_sink_drops_further_sends() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = OutputSink::new();
    sink.set(collecting_channel(&received));
    sink.send_owned(b"keep".to_vec());
    sink.close();
    sink.send_owned(b"drop".to_vec());
    assert_eq!(&*received.lock().unwrap(), b"keep");
}

#[test]
fn ensure_returns_the_same_arc_for_a_session() {
    let id = 9_000_201;
    let a = AGENT.ensure(id);
    let b = AGENT.ensure(id);
    assert!(Arc::ptr_eq(&a, &b));
    AGENT.unregister(id);
}

#[test]
fn register_replaces_previous_channel() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let id = 9_000_202;
    AGENT.register(id, counting_channel(&first_hits));
    AGENT.register(id, counting_channel(&second_hits));
    AGENT.ensure(id).send_owned(b"x".to_vec());
    assert_eq!(first_hits.load(Ordering::SeqCst), 0);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    AGENT.unregister(id);
}

#[test]
fn unregister_is_idempotent() {
    let id = 9_000_203;
    let sink = AGENT.ensure(id);
    AGENT.unregister(id);
    AGENT.unregister(id);
    sink.send_owned(b"x".to_vec()); // closed — no panic
}

#[test]
fn replacement_reader_reuses_the_live_channel() {
    // Retry / resume / regenerate (agent) or a replacement Build/Run
    // reader call `ensure`. That must be the same Live sink the
    // terminal already subscribed, not a fresh Pending buffer the
    // frontend never sees.
    let received = Arc::new(Mutex::new(Vec::new()));
    let id = 9_000_204;
    BUILD_RUN.register(id, collecting_channel(&received));
    let first_reader = BUILD_RUN.ensure(id);
    first_reader.send_owned(b"boot".to_vec());

    let replacement_reader = BUILD_RUN.ensure(id);
    assert!(
        Arc::ptr_eq(&first_reader, &replacement_reader),
        "a new process incarnation must reuse the session-scoped sink"
    );
    replacement_reader.send_owned(b"-ok".to_vec());
    assert_eq!(&*received.lock().unwrap(), b"boot-ok");
    BUILD_RUN.unregister(id);
}

#[test]
fn unregister_starts_a_fresh_pending_sink() {
    let first = Arc::new(Mutex::new(Vec::new()));
    let second = Arc::new(Mutex::new(Vec::new()));
    let id = 9_000_205;
    BUILD_RUN.register(id, collecting_channel(&first));
    BUILD_RUN.unregister(id);

    BUILD_RUN.ensure(id).send_owned(b"late".to_vec());
    assert!(
        first.lock().unwrap().is_empty(),
        "bytes after unregister must not reach the disposed Channel"
    );

    BUILD_RUN.register(id, collecting_channel(&second));
    assert_eq!(
        &*second.lock().unwrap(),
        b"late",
        "ensure after unregister is a new Pending sink flushed on the next subscribe"
    );
    BUILD_RUN.unregister(id);
}

#[test]
fn agent_and_build_run_maps_do_not_cross_talk() {
    // Same numeric id, two surfaces. The reason the maps are separate.
    let id = 9_000_206;
    let agent_got = Arc::new(Mutex::new(Vec::new()));
    let build_got = Arc::new(Mutex::new(Vec::new()));
    AGENT.register(id, collecting_channel(&agent_got));
    BUILD_RUN.register(id, collecting_channel(&build_got));

    AGENT.ensure(id).send_owned(b"agent".to_vec());
    BUILD_RUN.ensure(id).send_owned(b"build".to_vec());

    assert_eq!(&*agent_got.lock().unwrap(), b"agent");
    assert_eq!(&*build_got.lock().unwrap(), b"build");
    AGENT.unregister(id);
    BUILD_RUN.unregister(id);
}

#[test]
fn pending_buffer_truncates_to_cap_without_panicking_or_reordering() {
    // Reproduces the noisy-build-pre-mount scenario: a batcher pushes
    // chunks well past PENDING_CAP before the frontend subscribes.
    // The retained tail must be the LAST PENDING_CAP bytes in arrival
    // order, with no panic, no deadlock, and no interleaving between
    // chunks that arrived in separate send_owned calls.
    let id = 9_000_207;
    let sink = OutputSink::new();
    let chunk = vec![0xAB_u8; 64 * 1024]; // 64 KiB -- typical batcher batch
    let chunk_count = (PENDING_CAP / chunk.len()) + 8; // ~overflow by 8 chunks
    for n in 0..chunk_count {
        // Stamp each chunk with its index in the low byte of its tail so
        // we can detect reordering after truncation.
        let mut stamped = chunk.clone();
        let last = stamped.len() - 1;
        stamped[last] = n as u8;
        sink.send_owned(stamped);
    }

    let received = Arc::new(Mutex::new(Vec::new()));
    sink.set(collecting_channel(&received));
    let live = match &*sink.inner.read() {
        SinkState::Live(live) => Arc::clone(live),
        _ => panic!("expected live sink"),
    };
    for sequence in 1..=(PENDING_CAP / super::super::output_window::FRAME_BYTES) as u64 {
        live.window.acknowledge(sequence);
    }

    let got = received.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        PENDING_CAP,
        "subscription flush must deliver exactly the kept tail, no more"
    );
    // First retained byte must come from a mid-stream chunk (the leading
    // bytes of the very first chunks were dropped). We can verify
    // ordering by walking the trailing stamp and asserting the indexes
    // are strictly ascending and contiguous.
    let stamps: Vec<u8> = got
        .iter()
        .copied()
        .filter(|b| *b < chunk_count as u8)
        .collect();
    assert!(
        stamps.windows(2).all(|w| w[0] < w[1]),
        "chunk stamps must remain in arrival order after truncation"
    );
    // The first stamp is the lowest non-dropped chunk index. Its
    // preceding bytes (all 0xAB except the last) must be the bulk of
    // the chunk -- the truncation can only have lopped off whole
    // chunks plus possibly a partial tail of one.
    assert!(
        stamps[0] >= (chunk_count - (PENDING_CAP / chunk.len()) - 1) as u8,
        "truncation must not retain more than PENDING_CAP worth of chunks"
    );

    BUILD_RUN.unregister(id);
}

#[test]
fn pending_buffer_single_chunk_larger_than_cap_keeps_only_its_tail() {
    // A single oversize send (e.g. one mega-payload from a noisy tool)
    // must keep the LAST PENDING_CAP bytes of that single chunk --
    // dropping the leading bytes, not the trailing ones.
    let id = 9_000_208;
    let sink = OutputSink::new();
    let big: Vec<u8> = (0..(PENDING_CAP * 2) as u32)
        .map(|i| (i & 0xFF) as u8)
        .collect();
    sink.send_owned(big.clone());

    let received = Arc::new(Mutex::new(Vec::new()));
    sink.set(collecting_channel(&received));
    let live = match &*sink.inner.read() {
        SinkState::Live(live) => Arc::clone(live),
        _ => panic!("expected live sink"),
    };
    for sequence in 1..=(PENDING_CAP / super::super::output_window::FRAME_BYTES) as u64 {
        live.window.acknowledge(sequence);
    }

    let got = received.lock().unwrap().clone();
    assert_eq!(got.len(), PENDING_CAP);
    let expected_tail: Vec<u8> = big[big.len() - PENDING_CAP..].to_vec();
    assert_eq!(
        got, expected_tail,
        "single oversize chunk must retain only its trailing PENDING_CAP bytes"
    );

    BUILD_RUN.unregister(id);
}

#[test]
fn set_on_closed_sink_does_not_resurrect_into_live() {
    // Closed is documented terminal. A subsequent set() (e.g. a stale
    // subscribe callback landing after disposal) must NOT bring the
    // sink back to Live and leak bytes through the new channel.
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = OutputSink::new();
    sink.set(collecting_channel(&received));
    sink.send_owned(b"keep".to_vec());
    sink.close();
    sink.set(collecting_channel(&received)); // must be a no-op
    sink.send_owned(b"drop".to_vec());
    assert_eq!(
        &*received.lock().unwrap(),
        b"keep",
        "set on a closed sink must not transition back to Live"
    );
}

#[test]
fn stale_acknowledgement_and_teardown_cannot_change_replacement_or_sibling() {
    let maps = OutputSinks::new();
    let old = counting_channel(&Arc::new(AtomicUsize::new(0)));
    let old_id = old.id();
    maps.register(42, old);
    let hits = Arc::new(AtomicUsize::new(0));
    let channel = counting_channel(&hits);
    let channel_id = channel.id();
    maps.register(42, channel);
    for _ in 0..super::super::output_window::WINDOW_FRAMES {
        maps.ensure(42).send_owned(vec![1]);
    }
    maps.acknowledge(42, old_id, 8);
    maps.acknowledge(99, channel_id, 8);
    maps.unregister_channel(42, old_id);
    let sink = maps.ensure(42);
    let live = match &*sink.inner.read() {
        SinkState::Live(live) => Arc::clone(live),
        _ => panic!("replacement was torn down"),
    };
    assert_eq!(live.window.id(), channel_id);
    maps.unregister_channel(42, channel_id);
    assert!(matches!(&*sink.inner.read(), SinkState::Closed));
}

#[test]
fn killing_a_process_releases_its_batched_queue_without_closing_the_terminal() {
    use crate::pty::batch;
    use std::sync::mpsc;
    use std::time::Duration;
    let sink = Arc::new(OutputSink::new());
    let (sent_tx, sent_rx) = mpsc::channel();
    let channel = Channel::new(move |body| {
        if let InvokeResponseBody::Raw(bytes) = body {
            sent_tx.send(bytes).unwrap();
        }
        Ok(())
    });
    let channel_id = channel.id();
    sink.set(channel);
    // Exhaust the Channel window before running multiple process incarnations.
    for _ in 0..super::super::output_window::WINDOW_FRAMES {
        sink.send_owned(vec![1]);
    }
    for _ in 0..20 {
        let producer = sink.producer(Arc::new(AtomicBool::new(false)));
        let batch_producer = producer.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            batch::with_batcher(
                move |bytes| batch_producer.send_owned(bytes),
                |tx| {
                    started_tx.send(()).unwrap();
                    for _ in 0..batch::QUEUE_CAP * 2 {
                        if tx.send(vec![2; batch::PTY_READ_BUF]).is_err() {
                            break;
                        }
                    }
                },
            );
            done_tx.send(()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        producer.cancel();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        thread.join().unwrap();
    }
    assert_eq!(
        sent_rx.try_iter().count(),
        super::super::output_window::WINDOW_FRAMES,
        "cancelled output entered the Channel"
    );
    let live = match &*sink.inner.read() {
        SinkState::Live(live) => Arc::clone(live),
        _ => panic!("process kill disposed the terminal"),
    };
    assert_eq!(live.window.id(), channel_id);
    live.window
        .acknowledge(super::super::output_window::WINDOW_FRAMES as u64);
    let replacement = sink.producer(Arc::new(AtomicBool::new(false)));
    replacement.send_owned(b"replacement".to_vec());
    assert_eq!(
        sent_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        b"replacement"
    );
}

#[test]
fn registration_racing_old_teardown_preserves_the_replacement() {
    use std::sync::Barrier;
    let maps = Arc::new(OutputSinks::new());
    for _ in 0..128 {
        let old = counting_channel(&Arc::new(AtomicUsize::new(0)));
        let old_id = old.id();
        maps.register(42, old);
        let replacement = counting_channel(&Arc::new(AtomicUsize::new(0)));
        let replacement_id = replacement.id();
        let barrier = Arc::new(Barrier::new(2));
        let teardown_maps = Arc::clone(&maps);
        let teardown_barrier = Arc::clone(&barrier);
        let thread = std::thread::spawn(move || {
            teardown_barrier.wait();
            teardown_maps.unregister_channel(42, old_id);
        });
        barrier.wait();
        maps.register(42, replacement);
        thread.join().unwrap();
        let sink = maps.ensure(42);
        assert!(
            matches!(&*sink.inner.read(), SinkState::Live(live) if live.window.id() == replacement_id)
        );
    }
}
