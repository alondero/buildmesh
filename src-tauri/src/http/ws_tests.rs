//! Tests for `http::ws`.
//!
//! Split out of `src-tauri/src/http/ws.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test http::ws::tests

use super::*;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, connect_async};

/// Every test in this module mutates process-global fanout state:
/// `KNOWN_NODES` and the shared tombstone fence, whose budget any sibling
/// test can exhaust. So they cannot run concurrently with each other — a
/// fence-stress test that fills the 4096-entry budget would evict another
/// test's tombstone and make its assertion lie. This is the same reasoning
/// the database tests serialise on (issue #2048), for the same reason: the
/// state under test is per-process, not per-test.
///
/// One lock for both accessors below — a second `static` would be a second
/// mutex and would not serialise anything against the first.
static FANOUT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Cheap: the whole module is a few seconds even run one at a time.
///
/// The lock is a `tokio` mutex, not a `std` one: the async tests hold it
/// across `.await`, and a `std::sync::MutexGuard` held across an await point
/// is unsound if the future ever migrates threads — which a test that
/// changes its runtime flavour would do.
fn fanout_lock() -> tokio::sync::MutexGuard<'static, ()> {
    // `blocking_lock` is what serialises here; `try_lock` would merely fail
    // whenever a sibling test already holds it. It panics inside a runtime,
    // which is the guard against a sync test growing an `.await`.
    FANOUT_LOCK.blocking_lock()
}

/// The async half of [`fanout_lock`], for the tests that hold it across
/// `.await`.
async fn fanout_lock_async() -> tokio::sync::MutexGuard<'static, ()> {
    FANOUT_LOCK.lock().await
}

/// Install a private database holding one `agent_nodes` row with the id the
/// test was given, for a test that drives the real connection handler
/// (which refuses ids with no row — issue #2019).
///
/// The id is written explicitly rather than let the database choose it.
/// Every isolated database numbers its rows from 1, while `KNOWN_NODES` and
/// the tombstone fence are process-global and keyed by node id — so a
/// database-assigned id would collide with every other test that created
/// node 1. That is not a theoretical collision either: `agent_node::delete`
/// now records a tombstone for the id it deletes, so one unrelated test
/// deleting node 1 is enough to fence another test's channel. A high,
/// test-local id is in no other test's reach.
///
/// The caller must already hold the module lock — this fixture is sync and
/// the lock is not. The handler thread adopts the same database through
/// [`serve_terminal_ws`].
fn terminal_ws_node(node_id: i64, name: &str) -> (crate::db::test_support::IsolatedDbGuard, i64) {
    let db = crate::db::test_support::isolated();
    let mesh = crate::db::create_mesh(
        &format!("ws-{name}"),
        &format!("/tmp/buildmesh_ws_test_{name}"),
    )
    .expect("terminal_ws_node: create_mesh should succeed");
    let row = crate::db::write_conn();
    row.execute(
        "INSERT INTO agent_nodes (id, mesh_id, name, path, status) \
             VALUES (?1, ?2, ?3, ?4, 'idle')",
        (
            node_id,
            mesh.id,
            format!("ws-{name}"),
            format!("/tmp/ws/{name}"),
        ),
    )
    .expect("terminal_ws_node: insert node with an explicit id should succeed");
    drop(row);
    (db, node_id)
}

/// Serve `connections` terminal sockets on an ephemeral loopback port and
/// hand back the URL to dial plus the server thread's join handle.
///
/// The server runs on a **dedicated thread** with a current-thread runtime
/// because two things are thread-local: the isolated-database install
/// (issue #2048) and the tokio worker that runs the handler. A
/// `tokio::spawn`ed task could be polled on a worker that resolves the
/// process-global database instead of this test's, which would make the
/// handler's node-existence check read the wrong rows.
///
/// Every connection a test needs comes from this one thread, so the
/// database is adopted once and a reconnect test does not stand up a
/// second adopter for the same database.
fn serve_terminal_ws(
    node_id: i64,
    device_id: Option<i64>,
    db: &crate::db::test_support::IsolatedDbHandle,
    connections: usize,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind terminal ws listener");
    listener
        .set_nonblocking(true)
        .expect("terminal ws listener nonblocking");
    let addr = listener.local_addr().expect("terminal ws local addr");
    let owned = db.clone();
    let server = std::thread::spawn(move || {
        let _adopted = crate::db::test_support::adopt(&owned);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("terminal ws runtime");
        runtime.block_on(async move {
            let listener = TcpListener::from_std(listener).expect("adopt terminal ws listener");
            for _ in 0..connections {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                if let Ok(ws) = accept_async(MaybeTls::Plain(stream)).await {
                    handle_ws_connection(ws, node_id, device_id).await;
                }
            }
        });
    });
    (format!("ws://{}/n/{}", addr, node_id), server)
}

#[test]
fn revocation_terminates_only_the_matching_device() {
    use broadcast::error::RecvError;
    // A root-token socket (None) is never revocable.
    assert!(!revocation_terminates(Ok(5), None));
    // A device socket closes on its own id, ignores others.
    assert!(revocation_terminates(Ok(5), Some(5)));
    assert!(!revocation_terminates(Ok(6), Some(5)));
    // Lagged → conservatively close any device socket (it just reconnects).
    assert!(revocation_terminates(Err(RecvError::Lagged(3)), Some(5)));
    assert!(!revocation_terminates(Err(RecvError::Lagged(3)), None));
    // A closed channel never forces a termination.
    assert!(!revocation_terminates(Err(RecvError::Closed), Some(5)));
}

#[tokio::test]
async fn revoking_the_device_closes_its_live_terminal_ws() {
    // The hard AC: a revoke must drop an already-open socket, not just the
    // next request. A node with no history sends nothing on connect, so the
    // only thing that ends the stream is the revocation signal.
    let device_id = 7777_i64;
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_001, "revocation");
    ensure_pty_channel(node_id);

    let (url, _server) = serve_terminal_ws(node_id, Some(device_id), &_db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();

    // Let the handler reach its `revocation::subscribe()` before we fire —
    // a broadcast only reaches receivers present at send time.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    super::super::revocation::revoke(device_id);

    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) => break true,
                Some(Ok(m)) if m.is_close() => break true,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "revoking the device must terminate its live WS");
}

#[tokio::test]
async fn ws_replays_history_on_connect() {
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_002, "replay-history");
    ensure_pty_channel(node_id);
    send_pty_output(node_id, b"hello from history\r\n");

    let (url, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();
    let msg = ws.next().await.unwrap().unwrap();
    assert!(msg.is_binary());
    assert_eq!(msg.into_data(), b"hello from history\r\n".to_vec());
}

#[tokio::test]
async fn ws_receives_live_pty_output() {
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_003, "live-output");
    ensure_pty_channel(node_id);

    let (url, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    send_pty_output(node_id, b"live data\r\n");

    let msg = ws.next().await.unwrap().unwrap();
    assert!(msg.is_binary());
    assert_eq!(msg.into_data(), b"live data\r\n".to_vec());
}

// Issue #1238 regression: the WS write_task must survive a broadcast
// `Lagged` (1024-slot overflow under a flood of PTY output). Pre-fix,
// `while let Ok(data) = rx.recv().await` exited on the Lagged variant
// and the task died silently — the socket stayed open but no PTY bytes
// flowed until the user manually reconnected. Post-fix, the task logs
// the gap, re-sends the history tail, and keeps forwarding.
#[tokio::test]
async fn ws_write_task_survives_broadcast_lag() {
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_004, "broadcast-lag");
    ensure_pty_channel(node_id);

    let (url, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();

    // Let the handler subscribe to the broadcast + finish initial-state
    // negotiation before we start flooding.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Overflow the broadcast buffer (issue #2019 shrank it to
    // `PTY_FANOUT_SLOTS` slots derived from the byte budget, so this
    // floods roughly 17× past capacity). The handler's write_task
    // interleaves `rx.recv()` (advances the receiver position) with
    // `write.send()` (blocks once the TCP/WS sink fills). Because the
    // client never calls `ws.next()` the sink fills, `write.send()`
    // blocks, and during the block these sends accumulate past capacity
    // — so the next `rx.recv()` on the handler's receiver returns
    // `RecvError::Lagged`.
    for i in 0..1100 {
        send_pty_output(node_id, format!("flood {}\n", i).into_bytes());
    }

    // Let the runtime service the handler so the Lagged event fires
    // and the recovery (history tail re-send) lands before we look for
    // the marker.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Send a unique marker the test can grep for in the downstream
    // frames. If the write_task survived the Lagged, this is forwarded;
    // if it died (the pre-fix bug), the marker never reaches the client
    // and the loop below runs out the deadline.
    let marker = b"MARKER_AFTER_LAG";
    send_pty_output(node_id, marker);

    // Drain the client. Per-iteration timeouts so we keep reading past
    // the history-tail re-send frame(s) without bailing on the first
    // quiet stretch.
    let mut received_marker = false;
    let mut all_data = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(msg))) => {
                let data = msg.into_data();
                all_data.extend_from_slice(&data);
                if data.windows(marker.len()).any(|w| w == marker) {
                    received_marker = true;
                    break;
                }
            }
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => continue, // quiet stretch — keep waiting
        }
    }
    assert!(
        received_marker,
        "WS write_task died after Lagged — marker never reached client. \
             Received {} bytes before deadline.",
        all_data.len()
    );
}

#[tokio::test]
async fn ws_history_then_live_output() {
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_005, "history-then-live");
    ensure_pty_channel(node_id);
    send_pty_output(node_id, b"old output\r\n");

    let (url, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();

    let msg1 = ws.next().await.unwrap().unwrap();
    assert!(msg1.is_binary());
    assert_eq!(msg1.into_data(), b"old output\r\n".to_vec());

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    send_pty_output(node_id, b"new output\r\n");

    let msg2 = ws.next().await.unwrap().unwrap();
    assert!(msg2.is_binary());
    assert_eq!(msg2.into_data(), b"new output\r\n".to_vec());
}

#[tokio::test]
async fn ws_input_reaches_write_to_pty() {
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_006, "input-forwarding");
    ensure_pty_channel(node_id);

    let (url, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();

    ws.send(tungstenite::Message::Text("ls -la\n".into()))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[test]
fn ensure_pty_channel_is_idempotent() {
    ensure_pty_channel(9999);
    ensure_pty_channel(9999);
    let nodes = get_known_nodes();
    let locked = nodes.read();
    assert!(locked.contains_key(&9999));
}

#[test]
fn subscribe_pty_creates_channel_if_missing() {
    let _rx = subscribe_pty(8888);
    let nodes = get_known_nodes();
    let locked = nodes.read();
    assert!(locked.contains_key(&8888));
}

#[test]
fn send_pty_output_delivers_to_subscriber() {
    ensure_pty_channel(7777);
    let mut rx = subscribe_pty(7777);
    send_pty_output(7777, vec![0x41, 0x42, 0x43]);
    let received = rx.try_recv().unwrap();
    assert_eq!(received, vec![0x41, 0x42, 0x43]);
}

#[test]
fn send_pty_output_appends_to_history() {
    ensure_pty_channel(5555);
    send_pty_output(5555, vec![0x41, 0x42]);
    send_pty_output(5555, vec![0x43, 0x44]);
    let history = get_pty_history(5555);
    assert_eq!(history, vec![0x41, 0x42, 0x43, 0x44]);
}

#[test]
fn history_buffer_caps_at_limit() {
    ensure_pty_channel(4444);
    let big_chunk = vec![0x58; HISTORY_BUFFER_CAP + 100];
    send_pty_output(4444, big_chunk);
    let history = get_pty_history(4444);
    assert_eq!(history.len(), HISTORY_BUFFER_CAP);
}

#[test]
fn get_pty_history_returns_empty_for_unknown_node() {
    let history = get_pty_history(3333);
    assert!(history.is_empty());
}

#[test]
fn send_pty_output_no_panic_without_subscribers() {
    ensure_pty_channel(6666);
    send_pty_output(6666, vec![1, 2, 3]);
}

#[test]
fn send_pty_output_no_panic_for_unknown_node() {
    send_pty_output(1111, vec![1, 2, 3]);
}

#[test]
fn scrollback_captures_output() {
    ensure_pty_channel(50055);
    let _rx = subscribe_pty(50055);
    send_pty_output(50055, vec![0x48, 0x65, 0x6c, 0x6c, 0x6f]);
    send_pty_output(50055, vec![0x20, 0x57, 0x6f, 0x72, 0x6c, 0x64]);
    let history = get_pty_history(50055);
    assert_eq!(history, b"Hello World");
}

#[test]
fn scrollback_respects_max_size() {
    ensure_pty_channel(40044);
    let _rx = subscribe_pty(40044);
    let chunk = vec![0x41; HISTORY_BUFFER_CAP];
    send_pty_output(40044, chunk);
    send_pty_output(40044, vec![0x42, 0x43]);
    let history = get_pty_history(40044);
    assert_eq!(history.len(), HISTORY_BUFFER_CAP);
    assert_eq!(history[history.len() - 1], 0x43);
    assert_eq!(history[history.len() - 2], 0x42);
}

#[test]
fn scrollback_empty_for_unknown_node() {
    let history = get_pty_history(30033);
    assert!(history.is_empty());
}

#[test]
fn clear_scrollback_removes_buffer() {
    ensure_pty_channel(20022);
    let _rx = subscribe_pty(20022);
    send_pty_output(20022, vec![1, 2, 3]);
    clear_scrollback(20022);
    let history = get_pty_history(20022);
    assert!(history.is_empty());
    assert!(
        pty_channel_state(20022).is_some(),
        "a kill drops the retained bytes but keeps the channel — the node id \
             comes back on restart (issue #2019)"
    );
}

// --- Channel retirement vs. process exit (issue #2019) ------------------
//
// Before #2019 a deleted node's `NodeChannel` could only ever have its
// bytes cleared, so every create/delete cycle left a 1024-slot sender and
// its 128 KiB history in the process-global map. These tests pin the two
// halves of the split: permanent deletion retires the entry, process exit
// keeps it.

#[test]
fn retiring_a_deleted_node_releases_its_channel_and_its_allocations() {
    let _serial = fanout_lock();
    let node_id = 21_001;
    ensure_pty_channel(node_id);
    let _rx = subscribe_pty(node_id);
    send_pty_output(node_id, vec![0x41; HISTORY_BUFFER_CAP]);

    let (history_len, history_capacity) =
        pty_channel_state(node_id).expect("a live channel before retirement");
    assert_eq!(history_len, HISTORY_BUFFER_CAP);
    assert!(
        history_capacity >= HISTORY_BUFFER_CAP,
        "the retained ring must really be allocated for this test to mean \
             anything, got capacity {history_capacity}"
    );

    retire_pty_channel(node_id);

    assert!(
        pty_channel_state(node_id).is_none(),
        "retirement must drop the whole entry — sender, ring slots and history — \
             not just its bytes"
    );
    assert!(get_pty_history(node_id).is_empty());

    // Nothing that runs after the delete may bring the entry back: a late
    // ensure, a producer's last write, or a reconnecting client.
    ensure_pty_channel(node_id);
    send_pty_output(node_id, b"late producer");
    assert!(
        pty_channel_state(node_id).is_none(),
        "a late ensure or output write must not resurrect a retired channel"
    );
    assert!(
        matches!(
            subscribe_pty(node_id).try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ),
        "a retired node hands back a closed receiver, not a fresh channel"
    );
    assert!(pty_channel_state(node_id).is_none());
}

#[test]
fn a_process_exit_keeps_the_channel_so_a_restart_reuses_it() {
    let _serial = fanout_lock();
    let node_id = 21_002;
    ensure_pty_channel(node_id);
    send_pty_output(node_id, b"before the kill");

    clear_scrollback(node_id);

    assert!(
        pty_channel_state(node_id).is_some(),
        "process exit must NOT retire the channel: a restarted agent reuses the \
             same node id, and clearing the map here would discard the retained \
             terminal context on purpose (issue #2019)"
    );
    assert!(
        get_pty_history(node_id).is_empty(),
        "the kill does drop the bytes"
    );
    let (history_len, history_capacity) =
        pty_channel_state(node_id).expect("channel survives the kill");
    assert_eq!(history_len, 0);
    assert_eq!(
        history_capacity, 0,
        "clearing must release the cap-sized ring, not leave a dormant allocation"
    );

    // The restart reuses the id, so the live fanout must still work.
    let mut rx = subscribe_pty(node_id);
    send_pty_output(node_id, b"after the restart");
    assert_eq!(rx.try_recv().unwrap().as_ref(), b"after the restart");
}

#[test]
fn deleting_one_node_leaves_its_sibling_streaming() {
    let _serial = fanout_lock();
    let (deleted, survivor) = (21_003_i64, 21_004_i64);
    ensure_pty_channel(deleted);
    ensure_pty_channel(survivor);
    send_pty_output(deleted, b"retired node");
    send_pty_output(survivor, b"kept node");
    let mut rx = subscribe_pty(survivor);

    retire_pty_channel(deleted);

    assert_eq!(
        get_pty_history(survivor),
        b"kept node",
        "a sibling's history survives"
    );
    assert!(pty_channel_state(survivor).is_some());
    send_pty_output(survivor, b"still live");
    assert_eq!(rx.try_recv().unwrap().as_ref(), b"still live");
    assert!(pty_channel_state(survivor).is_some());
}

#[test]
fn repeated_node_lifetimes_reclaim_every_entry() {
    let _serial = fanout_lock();
    // Thousands of create/delete cycles is what a churny workspace does over
    // a week of opening and closing agent nodes.
    const CYCLES: i64 = 2000;
    const BASE: i64 = 9_000_000;
    let survivor = BASE - 1;
    ensure_pty_channel(survivor);
    send_pty_output(survivor, b"still here");

    for cycle in 0..CYCLES {
        let node_id = BASE + cycle;
        ensure_pty_channel(node_id);
        let _rx = subscribe_pty(node_id);
        send_pty_output(node_id, vec![b'y'; 4096]);
        send_pty_output(node_id, b"more");
        retire_pty_channel(node_id);
        assert!(
            pty_channel_state(node_id).is_none(),
            "create/delete cycle {cycle} left its entry in the map"
        );
    }

    let leaked = {
        let nodes = get_known_nodes();
        let locked = nodes.read();
        // Only this test's own id range. Sibling tests' channels are real
        // and are meant to still be in the map.
        locked
            .keys()
            .copied()
            .filter(|id| *id >= BASE && *id < BASE + CYCLES)
            .count()
    };
    assert_eq!(leaked, 0, "create/delete churn must not accumulate entries");
    assert!(
        get_retired_nodes().lock().ids.len() <= RETIRED_NODE_MEMORY,
        "the resurrection fence must stay bounded too — otherwise it becomes \
             the next leak"
    );
    assert_eq!(
        get_pty_history(survivor),
        b"still here",
        "a node that was never deleted keeps its channel and history"
    );
}

#[test]
fn one_chunk_allocation_is_shared_by_every_subscriber() {
    let _serial = fanout_lock();
    let node_id = 21_005;
    ensure_pty_channel(node_id);
    let mut first = subscribe_pty(node_id);
    let mut second = subscribe_pty(node_id);

    send_pty_output(node_id, b"shared bytes\r\n");

    let a = first.try_recv().unwrap();
    let b = second.try_recv().unwrap();
    assert_eq!(a, b, "every subscriber sees the same bytes");
    assert_eq!(
        a.as_ptr(),
        b.as_ptr(),
        "the payload must be one shared allocation, not a per-subscriber clone: \
             pre-#2019 every receiver got its own Vec and a slow client's memory \
             scaled with the number of subscribers"
    );
}

#[test]
fn a_slow_subscriber_pins_at_most_the_byte_budget() {
    let _serial = fanout_lock();
    let node_id = 21_006;
    ensure_pty_channel(node_id);
    // The slow client: subscribed, then deliberately never read during the
    // flood, exactly like a phone on a stalled link.
    let mut slow = subscribe_pty(node_id);

    for _ in 0..256 {
        send_pty_output(node_id, vec![b'x'; PTY_MAX_CHUNK_BYTES]);
    }

    let (history_len, _) = pty_channel_state(node_id).expect("live channel");
    assert!(
        history_len <= HISTORY_BUFFER_CAP,
        "retained history must stay capped after 8 MiB of output, got {history_len}"
    );

    // Measure what the never-drained subscriber actually pinned rather than
    // trusting the constants. `Lagged` advances the cursor instead of
    // returning a message, so the drain keeps going across the report.
    let mut lag_reports = 0u64;
    let mut retained_chunks = 0usize;
    let mut retained_bytes = 0usize;
    loop {
        match slow.try_recv() {
            Ok(chunk) => {
                assert!(
                    chunk.len() <= PTY_MAX_CHUNK_BYTES,
                    "a slot may never carry more than its chunk cap, got {}",
                    chunk.len()
                );
                retained_chunks += 1;
                retained_bytes += chunk.len();
            }
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => lag_reports += skipped,
            Err(broadcast::error::TryRecvError::Closed)
            | Err(broadcast::error::TryRecvError::Empty) => break,
        }
    }

    assert!(
        lag_reports > 0,
        "256 chunks must overrun a {PTY_FANOUT_SLOTS}-slot ring"
    );
    assert!(
        retained_chunks > 0,
        "the ring still holds the tail, so lag recovery has something to deliver"
    );
    assert!(
        retained_chunks <= PTY_FANOUT_SLOTS,
        "a drained-nowhere subscriber must not pin more slots than the budget has"
    );
    assert!(
        retained_bytes <= PTY_FANOUT_BYTE_BUDGET,
        "a slow subscriber pinned {retained_bytes} bytes, over the {PTY_FANOUT_BYTE_BUDGET} budget"
    );
}

#[test]
fn an_oversized_pty_write_is_split_without_losing_or_reordering_bytes() {
    let _serial = fanout_lock();
    let node_id = 21_007;
    ensure_pty_channel(node_id);
    let mut rx = subscribe_pty(node_id);
    let payload: Vec<u8> = (0..(PTY_MAX_CHUNK_BYTES * 3 + 17))
        .map(|i| (i % 251) as u8)
        .collect();

    send_pty_output(node_id, &payload);

    let mut streamed = Vec::new();
    let mut chunks = 0usize;
    while let Ok(chunk) = rx.try_recv() {
        assert!(chunk.len() <= PTY_MAX_CHUNK_BYTES);
        streamed.extend_from_slice(&chunk);
        chunks += 1;
    }
    assert_eq!(chunks, 4, "three whole slots plus the 17-byte remainder");
    assert_eq!(
        streamed, payload,
        "the split must be byte-exact and in order — xterm.js reassembles frames"
    );
    assert_eq!(get_pty_history(node_id), payload);
}

#[tokio::test]
async fn deleting_a_node_closes_its_live_terminal_socket() {
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_007, "delete-live-socket");
    ensure_pty_channel(node_id);
    let (url, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();
    // Let the handler subscribe before the delete lands.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    retire_pty_channel(node_id);

    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) => break true,
                Some(Ok(m)) if m.is_close() => break true,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        closed,
        "retiring a deleted node must close the socket its phone is watching, \
             not leave an orphaned stream nobody can write to"
    );
    assert!(pty_channel_state(node_id).is_none());
}

#[tokio::test]
async fn reconnecting_to_a_deleted_node_closes_instead_of_resurrecting_it() {
    // A phone whose node was deleted while it was offline reconnects
    // automatically; that reconnect must not re-create the channel the
    // retirement just reclaimed.
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_008, "reconnect-deleted");
    retire_pty_channel(node_id);
    let (url, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 1);

    let (mut ws, _) = connect_async(&url).await.unwrap();

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) => break "closed",
                Some(Ok(m)) if m.is_close() => break "closed",
                Some(Ok(m)) if m.is_binary() => break "sent-bytes",
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .unwrap_or("hung");
    assert_eq!(
        outcome, "closed",
        "a deleted node must not stream to a phone"
    );
    assert!(
        pty_channel_state(node_id).is_none(),
        "the reconnect must not have recreated the retired entry"
    );
}

#[tokio::test]
async fn a_reconnecting_client_replays_history_then_receives_live_output() {
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_009, "reconnect-history");
    ensure_pty_channel(node_id);

    let (first, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 2);
    let (mut ws, _) = connect_async(&first).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    send_pty_output(node_id, b"first session\r\n");
    assert_eq!(
        ws.next().await.unwrap().unwrap().into_data(),
        b"first session\r\n".to_vec()
    );
    drop(ws);

    // Output produced while no client is attached must still be replayed to
    // the next one.
    send_pty_output(node_id, b"while offline\r\n");

    let (mut ws, _) = connect_async(&first).await.unwrap();
    let replayed = ws.next().await.unwrap().unwrap().into_data();
    assert_eq!(
        replayed,
        b"first session\r\nwhile offline\r\n".to_vec(),
        "a reconnecting client gets the retained history of a surviving node"
    );
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    send_pty_output(node_id, b"second session\r\n");
    assert_eq!(
        ws.next().await.unwrap().unwrap().into_data(),
        b"second session\r\n".to_vec()
    );
}

#[tokio::test]
async fn a_restarted_node_replays_nothing_and_then_streams_live_output() {
    let _serial = fanout_lock_async().await;
    let (_db, node_id) = terminal_ws_node(9_500_010, "restart-streams");
    ensure_pty_channel(node_id);
    send_pty_output(node_id, b"stale pre-kill bytes");
    // The kill path: retained bytes go, channel stays.
    clear_scrollback(node_id);

    let (url, _server) = serve_terminal_ws(node_id, None, &_db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    send_pty_output(node_id, b"post-restart output");

    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("the restarted node must stream")
        .unwrap()
        .unwrap()
        .into_data();
    assert_eq!(
        frame,
        b"post-restart output".to_vec(),
        "the first frame after a restart must be live output, not the pre-kill \
             bytes the kill already dropped"
    );
    assert!(pty_channel_state(node_id).is_some());
}

/// A phone reconnecting to a node deleted *before this process started* has
/// no tombstone to consult — the fence is process memory, and it is also
/// bounded, so the same is true once eviction has dropped a tombstone. The
/// `agent_nodes` row is what refuses that reconnect, which is why this is
/// the case the in-memory deny list alone could never fix.
#[tokio::test]
async fn a_node_deleted_before_startup_is_refused_and_creates_no_channel() {
    let _serial = fanout_lock_async().await;
    // An isolated database with no node row at all: exactly what a restart
    // looks like to the socket handler.
    let db = crate::db::test_support::isolated();
    let node_id = 7_000_001_i64;
    let (url, server) = serve_terminal_ws(node_id, None, &db.handle(), 1);

    let (mut ws, _) = connect_async(&url).await.unwrap();

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) => break "closed",
                Some(Ok(m)) if m.is_close() => break "closed",
                Some(Ok(m)) if m.is_binary() => break "sent-bytes",
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .unwrap_or("hung");
    let _ = server.join();

    assert_eq!(
        outcome, "closed",
        "a node with no row must not get a terminal socket"
    );
    assert!(
        pty_channel_state(node_id).is_none(),
        "the refused connect must not leave a channel behind — that entry is \
             what a reconnect loop would pile up (issue #2019)"
    );
}

/// The fence and the creation paths must be one atomic step, not two
/// interleaved ones.
///
/// Before the fix, `retire_pty_channel` released the map lock before
/// recording the tombstone, so a create landing in that window inserted a
/// fresh channel for an id that was about to be marked deleted — and nothing
/// ever retired it. Hammer both sides from several threads and assert the
/// invariant that holds when they are atomic: a retired id has no entry.
#[test]
fn a_create_racing_a_retire_never_leaves_a_channel_behind() {
    let _serial = fanout_lock();
    let node_id = 30_001_i64;
    let creators = 6;
    let rounds = 400;

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handles: Vec<_> = (0..creators)
        .map(|_| {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(AtomicOrdering::SeqCst) {
                    ensure_pty_channel(node_id);
                    let _ = subscribe_pty(node_id);
                }
            })
        })
        .collect();

    for _ in 0..rounds {
        retire_pty_channel(node_id);
    }
    stop.store(true, AtomicOrdering::SeqCst);
    for handle in handles {
        handle.join().expect("creator thread should not panic");
    }

    // Deliberately no final retire: that would clean up exactly the entry
    // this test is looking for. Every create after the first retirement was
    // refused, so the only way an entry can exist here is the race.
    assert!(
        pty_channel_state(node_id).is_none(),
        "a create that raced a retirement left a channel for a retired id"
    );
    assert!(is_retired(node_id), "the tombstone must still be recorded");
}

/// The tombstone set is bounded, so it is allowed to forget. What must not
/// move is the *refusal*: once the fence has evicted this id, a
/// reconnection still may not conjure a channel, because the node row is
/// gone.
#[tokio::test]
async fn an_evicted_tombstone_does_not_reopen_the_channel() {
    let _serial = fanout_lock_async().await;
    let db = crate::db::test_support::isolated();
    let node_id = 7_100_001_i64;
    retire_pty_channel(node_id);
    assert!(is_retired(node_id));
    // Push this id out of the bounded fence.
    for offset in 1..=(RETIRED_NODE_MEMORY as i64) {
        retire_pty_channel(7_200_000 + offset);
    }
    assert!(
        !is_retired(node_id),
        "the fence is bounded, so the oldest tombstone must have been evicted"
    );

    let (url, server) = serve_terminal_ws(node_id, None, &db.handle(), 1);
    let (mut ws, _) = connect_async(&url).await.unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) => break "closed",
                Some(Ok(m)) if m.is_close() => break "closed",
                Some(Ok(m)) if m.is_binary() => break "sent-bytes",
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .unwrap_or("hung");
    let _ = server.join();

    assert_eq!(
        outcome, "closed",
        "an evicted tombstone must not reopen a terminal"
    );
    assert!(pty_channel_state(node_id).is_none());
}

/// A fenced id is refused by both creation paths, and the refusal is a
/// closed receiver rather than a silent empty channel.
#[test]
fn a_fence_lookups_stay_cheap_enough_to_hold_under_the_map_lock() {
    let _serial = fanout_lock();
    // The lookup runs while `KNOWN_NODES.write()` is held, which is the
    // lock every PTY write needs. Fill the fence to its cap and assert the
    // membership answer is still exact at the bound — the HashSet keeps
    // this O(1); a linear scan over the cap is what this pins against.
    let target = 8_000_001_i64;
    retire_pty_channel(target);
    for offset in 1..=(RETIRED_NODE_MEMORY as i64) {
        retire_pty_channel(8_100_000 + offset);
    }
    assert!(!is_retired(target), "evicted at the cap");
    retire_pty_channel(target);
    assert!(is_retired(target), "re-recorded after eviction");
    assert!(is_retired(8_100_000 + RETIRED_NODE_MEMORY as i64 - 1));
    assert!(!is_retired(8_100_001), "the oldest of that batch is gone");
}

// --- ProcessRegistryApi mock tests ---

struct MockRegistry {
    write_called: AtomicBool,
    resize_called: AtomicBool,
    last_write_data: std::sync::Mutex<Vec<u8>>,
    last_resize: std::sync::Mutex<(u16, u16)>,
    should_fail: bool,
    activity: crate::agent::process::InputActivity,
    disposition: crate::agent::process::InputDisposition,
    /// What `input_queue` reports, so the stalled-input event can be
    /// asserted end to end without the process-global registry.
    queue_depth: (usize, u64),
}

impl MockRegistry {
    fn new() -> Self {
        Self {
            write_called: AtomicBool::new(false),
            resize_called: AtomicBool::new(false),
            last_write_data: std::sync::Mutex::new(vec![]),
            last_resize: std::sync::Mutex::new((0, 0)),
            should_fail: false,
            activity: crate::agent::process::InputActivity {
                user_input: true,
                submitted: false,
            },
            disposition: crate::agent::process::InputDisposition::Accepted,
            queue_depth: (0, 0),
        }
    }
    fn failing() -> Self {
        Self {
            should_fail: true,
            ..Self::new()
        }
    }
    fn submitted() -> Self {
        Self {
            activity: crate::agent::process::InputActivity {
                user_input: true,
                submitted: true,
            },
            ..Self::new()
        }
    }
    /// A refusal that *claims* to be a submit.
    ///
    /// Deliberately unrealistic: a real refused write carries default
    /// activity, so this shape can only occur if the code under test fails
    /// to gate on the disposition. That is exactly what makes it a useful
    /// fixture — the autoclear test can only pass because
    /// `write_mobile_input_with_sink` checks `is_accepted()`, not because
    /// the mock happened to carry empty activity.
    fn submitted_backpressured() -> Self {
        Self {
            disposition: crate::agent::process::InputDisposition::Backpressured,
            ..Self::submitted()
        }
    }
}

impl ProcessRegistryApi for MockRegistry {
    fn write_input(
        &self,
        session_id: i64,
        data: &[u8],
    ) -> Result<crate::agent::process::InputOutcome, String> {
        self.write_bytes(session_id, data)?;
        Ok(crate::agent::process::InputOutcome {
            disposition: self.disposition,
            activity: self.activity,
        })
    }
    fn write_bytes(
        &self,
        _session_id: i64,
        data: &[u8],
    ) -> Result<crate::agent::process::InputOutcome, String> {
        if self.should_fail {
            return Err("mock error".into());
        }
        self.write_called.store(true, AtomicOrdering::SeqCst);
        *self.last_write_data.lock().unwrap() = data.to_vec();
        Ok(crate::agent::process::InputOutcome {
            disposition: self.disposition,
            activity: self.activity,
        })
    }
    fn input_queue(&self, _session_id: i64) -> (usize, u64) {
        self.queue_depth
    }
    fn resize_pty(&self, _session_id: i64, cols: u16, rows: u16) -> Result<(), String> {
        if self.should_fail {
            return Err("mock error".into());
        }
        self.resize_called.store(true, AtomicOrdering::SeqCst);
        *self.last_resize.lock().unwrap() = (cols, rows);
        Ok(())
    }
}

#[test]
fn forward_mobile_input_writes_to_registry() {
    let mock = MockRegistry::new();
    forward_mobile_input_with(&mock, 1, "hello");
    assert!(mock.write_called.load(AtomicOrdering::SeqCst));
    assert_eq!(*mock.last_write_data.lock().unwrap(), b"hello");
}

#[test]
fn forward_mobile_input_handles_registry_error() {
    let mock = MockRegistry::failing();
    forward_mobile_input_with(&mock, 1, "hello");
    assert!(!mock.write_called.load(AtomicOrdering::SeqCst));
}

/// Issue #1530: a refused write must not run the attention autoclear.
///
/// Autoclear fires on input containing CR/LF, i.e. a *submit*. If a
/// full-queue refusal ran it anyway, the node would be marked as no longer
/// awaiting input for a prompt that never reached it — the phone would see
/// the spinner stop and no answer ever appear.
///
/// The fixture is a refusal that *claims* to be a submit, so this fails
/// loudly if the disposition gate is ever removed.
#[test]
fn a_backpressured_mobile_write_does_not_run_the_attention_autoclear() {
    let mock = MockRegistry::submitted_backpressured();
    let sink = crate::agent::session_lifecycle::testing::RecordingSink::new();
    // `\r` is a submit, so the accepted path below is the one that would
    // clear attention.
    let outcome = write_mobile_input_with_sink(&mock, &sink, 1, "build it\r").unwrap();
    assert_eq!(
        outcome.disposition,
        crate::agent::process::InputDisposition::Backpressured
    );
    assert!(
        mock.write_called.load(AtomicOrdering::SeqCst),
        "the write was attempted"
    );
    assert!(
        sink.attention_cleared().is_empty(),
        "a prompt that was never queued must not clear attention, got {:?}",
        sink.attention_cleared()
    );
}

/// The same input on an accepted write does clear attention, so the test
/// above is pinning the backpressure branch rather than a fixture that
/// never could have cleared anything.
#[test]
fn an_accepted_mobile_write_still_clears_attention() {
    // A submit looks the node up to decide on a confirmation check.
    let _db = crate::db::test_support::isolated();
    let mock = MockRegistry::submitted();
    let sink = crate::agent::session_lifecycle::testing::RecordingSink::new();
    let outcome = write_mobile_input_with_sink(&mock, &sink, 1, "build it\r").unwrap();
    assert!(outcome.is_accepted());
    assert_eq!(
        sink.attention_cleared(),
        vec![1],
        "an accepted submit must still clear the node's attention"
    );
}

/// A backpressured WebSocket write announces the stall on the event
/// broadcast, and says how deep the queue was.
///
/// Issue #1530 review: this path used to read the process-global registry
/// directly, so it could not be asserted through a mock. With `input_queue`
/// on the trait, the whole path — refusal, depth lookup, emitted payload —
/// is observable here.
#[test]
fn a_backpressured_websocket_write_emits_a_stalled_input_event() {
    let mut mock = MockRegistry::new();
    mock.disposition = crate::agent::process::InputDisposition::Backpressured;
    mock.queue_depth = (3, 96);
    let mut events = super::super::events::subscribe();

    forward_mobile_input_with(&mock, 42, "hello");

    // The event broadcast is process-global, so a concurrent sibling test
    // can put its own event on this receiver before ours. Look for *our*
    // node's stall rather than demanding that the next event happen to be
    // ours; `EventMsg` deliberately has no `Debug`, so an unexpected
    // variant is skipped and a missing stall is reported by shape.
    let mut ours = None;
    while let Ok(event) = events.try_recv() {
        if let super::super::events::EventMsg::TerminalInputStalled {
            session_id,
            queued_messages,
            queued_bytes,
        } = event
        {
            if session_id == 42 {
                ours = Some((queued_messages, queued_bytes));
                break;
            }
        }
    }
    let (queued_messages, queued_bytes) = ours.expect("a terminal-input-stalled event for node 42");
    assert_eq!(
        queued_messages, 3,
        "the event must carry the registry's own depth"
    );
    assert_eq!(queued_bytes, 96);
}

/// A refused write must still reach the phone, and an accepted one must not
/// claim to be stalled.
#[test]
fn an_accepted_websocket_write_emits_no_stalled_input_event() {
    let mock = MockRegistry::new();
    let mut events = super::super::events::subscribe();

    // Node 43 is this test's alone: its backpressured sibling announces
    // stalls for node 42, and asserting on the whole process-global
    // broadcast instead made this test fail whenever that sibling ran
    // concurrently (reproducible on the base commit, 3 of 5 module runs).
    forward_mobile_input_with(&mock, 43, "hello");

    while let Ok(event) = events.try_recv() {
        if let super::super::events::EventMsg::TerminalInputStalled { session_id, .. } = event {
            assert_ne!(
                session_id, 43,
                "a delivered keystroke must not raise a stall for its own node"
            );
        }
    }
}

/// The production wrapper resolves its sink and delegates to the same
/// accepted-input seam used by HTTP and WebSocket callers.
#[test]
fn write_mobile_input_dispatches_to_seam() {
    let mock = MockRegistry::new();
    write_mobile_input(&mock, 1, "y").expect("write_mobile_input dispatches");
    assert!(mock.write_called.load(AtomicOrdering::SeqCst));
    assert_eq!(*mock.last_write_data.lock().unwrap(), b"y");
}

/// Production wrapper's registry-error path must surface the
/// error verbatim (the autoclear side-effect doesn't run when the
/// PTY write fails — a regression that re-orders the seam so
/// autoclear runs first would surface here as a panic from
/// `DbOnlySink::write_status`).
#[test]
fn write_mobile_input_propagates_registry_error_at_production_path() {
    let mock = MockRegistry::failing();
    let Err(err) = write_mobile_input(&mock, 1, "y") else {
        panic!("write_mobile_input must surface registry errors");
    };
    assert!(err.contains("mock error"));
    assert!(
        !mock.write_called.load(AtomicOrdering::SeqCst),
        "failing registry must not record a successful write"
    );
}

// --- write_mobile_input (issue #1377, post-review) ---------------------
//
// The new `POST /api/nodes/{id}/input` route uses the same
// `write_mobile_input` helper, so the autoclear predicate
// ("an accepted submission triggers disarm + lifecycle emit") is
// shared between the WS path and the HTTP path. A regression
// that flips the predicate (e.g. autoclears on any input, or
// never autoclears) would silently break the triage-deck flow —
// `y\r` would either flip `awaiting_input` on every typed letter
// (UX catastrophe) or never flip it at all (no triage signal).
//
// These tests drive `write_mobile_input_with_sink` (the test seam
// introduced on PR #1643 review) with the shared
// `agent::session_lifecycle::testing::RecordingSink` so the
// autoclear side-effects are observable without touching the
// global SQLite state. The previous tests coupled the transport
// module to the `DbOnlySink` fallback, which panicked on an
// uninitialised DB and asserted only on the bytes that reached
// the registry — a paper tiger that never verified the autoclear
// side-effect actually ran.
use crate::agent::session_lifecycle::testing::RecordingSink;
use crate::models::SessionStatus;

/// `write_mobile_input` is the new HTTP-route entry point (issue
/// #1377). It must return the registry error verbatim so the route
/// can surface a `503 Service Unavailable` when the PTY process is
/// down — the pre-refactor `forward_mobile_input_with` swallowed
/// the error with `tracing::warn!`. A regression that re-introduces
/// the swallow would have the HTTP route always return 200 OK,
/// silently dropping keystrokes on a killed agent while the SPA
/// reported success.
///
/// Driven through `write_mobile_input_with_sink` so the autoclear
/// side-effect runs against a `RecordingSink` instead of
/// `DbOnlySink` — the previous form called `write_mobile_input`
/// directly and depended on the test-ordering lottery for
/// `db::init` to win the race.
#[test]
fn write_mobile_input_propagates_registry_error() {
    let mock = MockRegistry::failing();
    let sink = RecordingSink::new();
    let Err(err) = write_mobile_input_with_sink(&mock, &sink, 1, "y\r") else {
        panic!("write_mobile_input must surface registry errors");
    };
    assert!(err.contains("mock error"));
    assert!(
        !mock.write_called.load(AtomicOrdering::SeqCst),
        "failing registry must not record a successful write"
    );
    // The write failed, so the autoclear side-effect must not have
    // run — the sink stays clean. Catches a regression where a
    // refactor moves the autoclear BEFORE the registry write.
    assert_eq!(sink.writes(), Vec::<(i64, SessionStatus)>::new());
    assert_eq!(sink.attention_cleared(), Vec::<i64>::new());
}

/// An accepted submission triggers a Running transition and clear event.
/// The real registry's CR/LF and paste decoding is covered below.
#[test]
fn autoclear_predicate_cr_only() {
    // A submit looks the node up to decide on a confirmation check.
    let _db = crate::db::test_support::isolated();
    let mock = MockRegistry::submitted();
    let sink = RecordingSink::new();
    write_mobile_input_with_sink(&mock, &sink, 1, "y\r").expect("\\r writes");
    assert_eq!(*mock.last_write_data.lock().unwrap(), b"y\r");
    assert_eq!(sink.status(), Some(SessionStatus::Running));
    assert_eq!(sink.attention_cleared(), vec![1]);
}

/// The lifecycle consumer accepts the same submission evidence for LF.
#[test]
fn autoclear_predicate_lf_only() {
    // A submit looks the node up to decide on a confirmation check.
    let _db = crate::db::test_support::isolated();
    let mock = MockRegistry::submitted();
    let sink = RecordingSink::new();
    write_mobile_input_with_sink(&mock, &sink, 1, "n\n").expect("\\n writes");
    assert_eq!(*mock.last_write_data.lock().unwrap(), b"n\n");
    assert_eq!(sink.status(), Some(SessionStatus::Running));
    assert_eq!(sink.attention_cleared(), vec![1]);
}

/// Negative case: bare text (no CR/LF) MUST NOT autoclear. Catches
/// the regression class where a refactor widens the predicate to
/// fire on any input — every typed letter would then flip
/// `awaiting_input`, breaking the triage-deck UX.
#[test]
fn autoclear_predicate_no_newline_does_not_autoclear() {
    let mock = MockRegistry::new();
    let sink = RecordingSink::new();
    write_mobile_input_with_sink(&mock, &sink, 1, "y").expect("bare text writes");
    assert_eq!(*mock.last_write_data.lock().unwrap(), b"y");
    assert_eq!(sink.writes(), Vec::<(i64, SessionStatus)>::new());
    assert_eq!(sink.attention_cleared(), Vec::<i64>::new());
}

#[test]
fn mobile_input_uses_streaming_registry_evidence_for_attention() {
    // A submit looks the node up to decide on a confirmation check.
    let _db = crate::db::test_support::isolated();
    let id = -930_023;
    let (registry, received) = crate::agent::process::testing::capturing_registry(id);
    let sink = RecordingSink::new();
    let original = registry.input_stamp(id).unwrap();
    for packet in ["\x1b[", "I", "\x1b[12;", "34R", "\x1b[O"] {
        write_mobile_input_with_sink(registry.as_ref(), &sink, id, packet).unwrap();
        assert_eq!(received.recv().unwrap(), packet.as_bytes());
        assert!(sink.writes().is_empty());
        assert!(sink.attention_cleared().is_empty());
    }
    assert_eq!(registry.input_stamp(id).as_ref(), Some(&original));
    for packet in ["\x1b[20", "0~first\n", "second\rthird", "\x1b[201", "~"] {
        write_mobile_input_with_sink(registry.as_ref(), &sink, id, packet).unwrap();
        assert_eq!(received.recv().unwrap(), packet.as_bytes());
        assert!(sink.writes().is_empty(), "paste must not change lifecycle");
        assert!(sink.attention_cleared().is_empty());
        assert!(registry.input_stamp(id).is_none());
    }
    write_mobile_input_with_sink(registry.as_ref(), &sink, id, "\r").unwrap();
    assert_eq!(received.recv().unwrap(), b"\r");
    assert_eq!(sink.status(), Some(SessionStatus::Running));
    assert_eq!(sink.attention_cleared(), vec![id]);
    assert_ne!(registry.input_stamp(id).unwrap(), original);
    registry.kill_session(id);
}

#[test]
fn mobile_input_does_not_infer_submission_from_rejected_bytes() {
    let mock = MockRegistry {
        activity: crate::agent::process::InputActivity::default(),
        ..MockRegistry::new()
    };
    let sink = RecordingSink::new();
    write_mobile_input_with_sink(&mock, &sink, 1, "ignored\r\n").unwrap();
    assert_eq!(*mock.last_write_data.lock().unwrap(), b"ignored\r\n");
    assert!(sink.writes().is_empty());
    assert!(sink.attention_cleared().is_empty());
}

#[test]
fn handle_mobile_resize_calls_registry() {
    let mock = MockRegistry::new();
    handle_mobile_resize_with(&mock, 1, 120, 40);
    assert!(mock.resize_called.load(AtomicOrdering::SeqCst));
    assert_eq!(*mock.last_resize.lock().unwrap(), (120, 40));
}

#[test]
fn handle_mobile_resize_handles_registry_error() {
    let mock = MockRegistry::failing();
    handle_mobile_resize_with(&mock, 1, 80, 24);
    assert!(!mock.resize_called.load(AtomicOrdering::SeqCst));
}

// -- parse_resize_message (issue #1263) ----------------------------------
//
// The validation at the WS boundary pins the "no zero/oversized
// dimensions reach ConPTY" contract. The three-state return shape
// (`None` = not a resize frame, `Some(Ok)` = valid dims,
// `Some(Err)` = malformed resize frame) lets the caller decide
// between forwarding-as-input, applying, and warning-and-dropping.

#[test]
fn parse_resize_accepts_normal_dimensions() {
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":120,"rows":40}"#),
        Some(Ok((120, 40)))
    );
    // Boundary: exactly MAX_RESIZE_DIMENSION is allowed.
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":1000,"rows":1000}"#),
        Some(Ok((1000, 1000)))
    );
}

#[test]
fn parse_resize_rejects_zero_dimensions() {
    // 0×0 (and 0×N, N×0) must NOT reach the PTY — ConPTY tolerates
    // it today but the surface is fragile.
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":0,"rows":0}"#),
        Some(Err(ResizeError::ZeroDimension))
    );
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":80,"rows":0}"#),
        Some(Err(ResizeError::ZeroDimension))
    );
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":0,"rows":24}"#),
        Some(Err(ResizeError::ZeroDimension))
    );
}

#[test]
fn parse_resize_rejects_implausibly_large_dimensions() {
    // > MAX_RESIZE_DIMENSION — almost certainly a corrupt frame.
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":1001,"rows":40}"#),
        Some(Err(ResizeError::DimensionTooLarge))
    );
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":80,"rows":99999}"#),
        Some(Err(ResizeError::DimensionTooLarge))
    );
}

#[test]
fn parse_resize_returns_none_for_non_resize_messages() {
    // Not a resize message at all → caller forwards as input.
    assert_eq!(
        parse_resize_message(r#"{"type":"keystroke","data":"x"}"#),
        None
    );
    // No `type` field at all.
    assert_eq!(parse_resize_message(r#"{"cols":80,"rows":24}"#), None);
    // Non-JSON text input.
    assert_eq!(parse_resize_message("ls\n"), None);
    // Empty string.
    assert_eq!(parse_resize_message(""), None);
}

#[test]
fn parse_resize_handles_malformed_json_gracefully() {
    // Truncated/invalid JSON that DOES start with '{' → not a resize
    // message (the outer None). Caller forwards as input (or, for a
    // brace-prefixed garbage payload, drops — but it never crashes).
    assert_eq!(parse_resize_message("{not json"), None);
}

// -- parse_resize_message: malformed-payload cases (review feedback) ------
//
// Once `type == "resize"` is confirmed, ANY extraction failure must
// return `Some(Err(MalformedPayload))` — never `None`, which the caller
// would forward as PTY input (injection regression). These tests pin
// every variant serde can surface for a resize frame.

#[test]
fn parse_resize_rejects_string_cols_as_malformed_not_forwardable() {
    // `cols:"abc"` — type matches "resize" but cols is the wrong type.
    // Must NOT fall through to `None` (which the caller would dump
    // into the running shell as raw text).
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":"abc","rows":10}"#),
        Some(Err(ResizeError::MalformedPayload))
    );
}

#[test]
fn parse_resize_rejects_missing_fields_as_malformed() {
    // No cols/rows at all. After type confirmation this is a malformed
    // RESIZE frame, not "not a resize frame".
    assert_eq!(
        parse_resize_message(r#"{"type":"resize"}"#),
        Some(Err(ResizeError::MalformedPayload))
    );
}

#[test]
fn parse_resize_rejects_null_cols_as_malformed() {
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":null,"rows":10}"#),
        Some(Err(ResizeError::MalformedPayload))
    );
}

#[test]
fn parse_resize_rejects_negative_cols_as_malformed() {
    // `cols:-1` — serde's u64 rejects negatives at the type level.
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":-1,"rows":10}"#),
        Some(Err(ResizeError::MalformedPayload))
    );
}

#[test]
fn parse_resize_rejects_float_cols_as_malformed() {
    // `cols:1.5` — serde's u64 rejects non-integers.
    assert_eq!(
        parse_resize_message(r#"{"type":"resize","cols":1.5,"rows":10}"#),
        Some(Err(ResizeError::MalformedPayload))
    );
}

#[test]
fn parse_resize_rejects_wrong_type_field_as_malformed() {
    // `type:123` — the type tag itself is the wrong type. This means
    // "not a resize frame" → caller forwards as input (the only
    // correct post-confirmation `None` path).
    assert_eq!(
        parse_resize_message(r#"{"type":123,"cols":80,"rows":24}"#),
        None
    );
}
