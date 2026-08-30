//! Time-and-size bounded coalescing of PTY read chunks (issue #1385).
//!
//! The OS PTY reader yields many small `&[u8]` slices during a build storm.
//! Emitting each slice as its own IPC message is the bulk of the old
//! Base64+JSON overhead. This batcher sits on a dedicated thread, drains a
//! channel of raw chunks, and flushes when either:
//!
//! - [`FLUSH_BYTES`] have accumulated (four PTY fills — see [`PTY_READ_BUF`]), or
//! - [`FLUSH_WINDOW`] has elapsed since the first byte of the batch (keeps
//!   keystroke echo inside the issue's <16 ms interactive budget).
//!
//! Disconnect on the producer side always flushes whatever is buffered, so
//! the last bytes of a session are not dropped.

use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

/// Size of the `read()` buffer in `pump_pty_output`. A build-storm fill
/// returns up to this many bytes in one OS read.
pub const PTY_READ_BUF: usize = 8192;

/// Cap on unflushed bytes before a batch is forced out. Four PTY fills
/// (32 KiB) so a storm of *full* 8 KiB `read()`s actually coalesces
/// instead of flushing on the first fill. Tiny interactive reads still
/// flush on [`FLUSH_WINDOW`].
pub const FLUSH_BYTES: usize = PTY_READ_BUF * 4;

/// Maximum time a byte sits in the batcher before being dispatched.
/// 8 ms is half a 60 Hz frame and well inside the <16 ms keystroke budget
/// (the frontend `TerminalWriter` already skips `requestAnimationFrame`
/// for ≤16-byte echoes — issue #1122).
pub const FLUSH_WINDOW: Duration = Duration::from_millis(8);

/// Bound on the reader→batcher queue. The sender blocks when full, which
/// is the same natural backpressure the pre-#1385 reader got by doing
/// `emit` inline: a stalled IPC path fills the PTY, the agent blocks,
/// memory stays bounded. 256 × 8 KiB reads ≈ 2 MiB worst case.
pub const QUEUE_CAP: usize = 256;

/// Drain `rx` and call `on_batch` with coalesced chunks using the
/// production window and size thresholds.
pub fn drain_batched(rx: mpsc::Receiver<Vec<u8>>, on_batch: impl FnMut(Vec<u8>)) {
    drain_batched_with(rx, FLUSH_WINDOW, FLUSH_BYTES, on_batch);
}

/// Run `body` as the PTY-reader producer, then drop the sender and join
/// the batcher.
///
/// `drain_batched` blocks on `recv()` until **every** `SyncSender` is
/// dropped. Joining the batcher while the reader still holds `tx` is a
/// deadlock on every PTY exit (the reader waits for the batcher, the
/// batcher waits for `Disconnected`). This helper is the only legal
/// pairing of "pump then join".
pub fn with_batcher(
    on_batch: impl FnMut(Vec<u8>) + Send + 'static,
    body: impl FnOnce(&mpsc::SyncSender<Vec<u8>>),
) {
    let (tx, rx) = mpsc::sync_channel(QUEUE_CAP);
    let batcher = std::thread::spawn(move || drain_batched(rx, on_batch));
    body(&tx);
    drop(tx);
    let _ = batcher.join();
}

/// Test-visible variant with injectable thresholds so the time-window
/// test does not depend on the production 8 ms constant.
pub fn drain_batched_with(
    rx: mpsc::Receiver<Vec<u8>>,
    window: Duration,
    max_bytes: usize,
    mut on_batch: impl FnMut(Vec<u8>),
) {
    loop {
        let first = match rx.recv() {
            Ok(chunk) => chunk,
            Err(_) => return,
        };
        // Take ownership of the first Vec — no extra alloc for a single
        // fill. Reserve up to `max_bytes` so coalesced extends don't
        // reallocate on the storm path.
        let mut buf = first;
        buf.reserve(max_bytes.saturating_sub(buf.len()));
        let deadline = Instant::now() + window;

        loop {
            if buf.len() >= max_bytes {
                break;
            }
            match rx.try_recv() {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk);
                    continue;
                }
                Err(TryRecvError::Disconnected) => {
                    on_batch(buf);
                    return;
                }
                Err(TryRecvError::Empty) => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(chunk) => buf.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    on_batch(buf);
                    return;
                }
            }
        }
        on_batch(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn collect(rx: mpsc::Receiver<Vec<u8>>, window: Duration, max_bytes: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        drain_batched_with(rx, window, max_bytes, |batch| out.push(batch));
        out
    }

    #[test]
    fn coalesces_chunks_that_arrive_before_disconnect() {
        let (tx, rx) = mpsc::channel();
        tx.send(b"hel".to_vec()).unwrap();
        tx.send(b"lo ".to_vec()).unwrap();
        tx.send(b"world".to_vec()).unwrap();
        drop(tx);

        let batches = collect(rx, Duration::from_millis(50), 64);
        assert_eq!(batches, vec![b"hello world".to_vec()]);
    }

    #[test]
    fn flushes_when_size_threshold_is_crossed() {
        let (tx, rx) = mpsc::sync_channel(8);
        // First chunk is under the 4-byte cap; the second pushes us over,
        // so they still land in one batch (we never split a chunk).
        tx.send(vec![1, 2, 3]).unwrap();
        tx.send(vec![4, 5]).unwrap();
        drop(tx);

        let batches = collect(rx, Duration::from_secs(5), 4);
        assert_eq!(batches, vec![vec![1, 2, 3, 4, 5]]);
    }

    #[test]
    fn size_threshold_splits_a_burst_into_multiple_batches() {
        let (tx, rx) = mpsc::sync_channel(8);
        tx.send(vec![1, 2, 3, 4]).unwrap();
        tx.send(vec![5, 6, 7, 8]).unwrap();
        drop(tx);

        let batches = collect(rx, Duration::from_secs(5), 4);
        assert_eq!(batches, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
    }

    #[test]
    fn preserves_split_utf8_bytes_across_chunks() {
        // U+2588 FULL BLOCK is e2 96 88 — split across two OS reads the
        // same way `pump_pty_output` can. Concatenation must be exact;
        // xterm (and ChunkCapture) reassemble at the byte level.
        let (tx, rx) = mpsc::channel();
        tx.send(vec![0xe2]).unwrap();
        tx.send(vec![0x96, 0x88]).unwrap();
        drop(tx);

        let batches = collect(rx, Duration::from_millis(50), 64);
        assert_eq!(batches, vec![vec![0xe2, 0x96, 0x88]]);
    }

    #[test]
    fn disconnect_with_nothing_buffered_emits_nothing() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        drop(tx);
        let batches = collect(rx, Duration::from_millis(50), 64);
        assert!(batches.is_empty());
    }

    #[test]
    fn with_batcher_drops_the_producer_so_join_cannot_deadlock() {
        // Regression pin for the spawn.rs join-while-holding-tx deadlock:
        // if `with_batcher` forgot `drop(tx)` this test hangs until the
        // runner timeout instead of returning.
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = received.clone();
        let started = Instant::now();
        with_batcher(
            move |batch| rec.lock().unwrap().extend_from_slice(&batch),
            |tx| {
                tx.send(b"xyz".to_vec()).unwrap();
            },
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "batcher join hung — producer SyncSender was not dropped"
        );
        assert_eq!(&*received.lock().unwrap(), b"xyz");
    }

    #[test]
    fn two_full_pty_fills_coalesce_under_production_flush_bytes() {
        // The PTY reader fills 8 KiB. FLUSH_BYTES is 4 fills, so two
        // back-to-back full reads must become one IPC dispatch — the
        // whole point of the batcher on a build storm.
        let (tx, rx) = mpsc::sync_channel(8);
        tx.send(vec![1u8; PTY_READ_BUF]).unwrap();
        tx.send(vec![2u8; PTY_READ_BUF]).unwrap();
        drop(tx);

        let batches = collect(rx, Duration::from_secs(5), FLUSH_BYTES);
        assert_eq!(batches.len(), 1, "two full fills must coalesce");
        assert_eq!(batches[0].len(), PTY_READ_BUF * 2);
    }

    #[test]
    fn production_flush_bytes_is_several_pty_fills() {
        assert!(
            FLUSH_BYTES > PTY_READ_BUF,
            "FLUSH_BYTES ({FLUSH_BYTES}) must exceed one PTY fill ({PTY_READ_BUF}) or a build storm never batches"
        );
    }

    #[test]
    fn time_window_flushes_an_isolated_chunk_without_waiting_for_more() {
        let (tx, rx) = mpsc::sync_channel(8);
        let handle = thread::spawn(move || collect(rx, Duration::from_millis(20), 4096));

        tx.send(b"a".to_vec()).unwrap();
        // Sit past the window so the batcher cannot be waiting for a
        // second chunk. 80 ms is generous against CI scheduling noise.
        thread::sleep(Duration::from_millis(80));
        tx.send(b"b".to_vec()).unwrap();
        drop(tx);

        let batches = handle.join().expect("batcher thread");
        assert_eq!(
            batches,
            vec![b"a".to_vec(), b"b".to_vec()],
            "isolated chunks separated by more than the window must not coalesce"
        );
    }
}
