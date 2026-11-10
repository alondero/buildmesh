use super::*;
use std::sync::{mpsc, Arc};
use std::time::Duration;

fn fixture() -> (Arc<OutputWindow>, mpsc::Receiver<Vec<u8>>) {
    let (tx, rx) = mpsc::channel();
    let channel = Channel::new(move |body| {
        if let InvokeResponseBody::Raw(bytes) = body {
            tx.send(bytes).unwrap();
        }
        Ok(())
    });
    (Arc::new(OutputWindow::new(channel, Vec::new())), rx)
}

#[test]
fn stalled_first_body_bounds_sends_and_payload_then_preserves_the_entire_stream() {
    let (window, rx) = fixture();
    let producer = Arc::clone(&window);
    let thread = std::thread::spawn(move || {
        for index in 0..20_001u32 {
            let size = if index == 0 { FRAME_BYTES } else { 512 };
            let mut bytes = vec![0; size];
            bytes[..4].copy_from_slice(&index.to_le_bytes());
            // Split UTF-8 and ANSI bytes are opaque to the admission window.
            bytes[4..9].copy_from_slice(&[0xe2, 0x96, 0x88, 0x1b, b'[']);
            producer.send(bytes, &|| true);
        }
    });
    let mut initial = Vec::new();
    for _ in 0..WINDOW_FRAMES {
        initial.push(rx.recv_timeout(Duration::from_secs(5)).unwrap());
    }
    {
        let state = window.state.lock();
        assert_eq!(state.sent, WINDOW_FRAMES as u64);
        assert_eq!(state.outstanding.len(), WINDOW_FRAMES);
        assert_eq!(
            state.outstanding.iter().sum::<usize>(),
            FRAME_BYTES + (WINDOW_FRAMES - 1) * 512
        );
        assert!(state.replay.is_empty());
    }
    assert!(rx.try_recv().is_err());
    for index in 0..20_001u32 {
        let bytes = if (index as usize) < initial.len() {
            initial[index as usize].clone()
        } else {
            rx.recv_timeout(Duration::from_secs(5)).unwrap()
        };
        assert_eq!(&bytes[..4], &index.to_le_bytes());
        assert_eq!(&bytes[4..9], &[0xe2, 0x96, 0x88, 0x1b, b'[']);
        window.acknowledge(index as u64 + 1);
    }
    thread.join().unwrap();
    assert!(window.state.lock().outstanding.is_empty());
}

#[test]
fn replay_and_oversized_live_batches_share_the_same_window() {
    let (tx, rx) = mpsc::channel();
    let window = Arc::new(OutputWindow::new(
        Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                tx.send(bytes).unwrap();
            }
            Ok(())
        }),
        vec![1; FRAME_BYTES * 10],
    ));
    window.start();
    let producer = Arc::clone(&window);
    let thread = std::thread::spawn(move || producer.send(vec![2; FRAME_BYTES * 3 + 1], &|| true));
    for sequence in 1..=14 {
        let bytes = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let expected = if sequence <= 10 { 1 } else { 2 };
        assert!(bytes.iter().all(|byte| *byte == expected));
        assert!(bytes.len() <= FRAME_BYTES);
        if sequence == 14 {
            assert_eq!(bytes.len(), 1);
        }
        assert!(window.state.lock().outstanding.len() <= WINDOW_FRAMES);
        assert!(window.state.lock().outstanding.iter().sum::<usize>() <= WINDOW_BYTES);
        window.acknowledge(sequence);
    }
    thread.join().unwrap();
}

#[test]
fn cumulative_duplicate_and_future_acks_cannot_manufacture_credit() {
    let (window, rx) = fixture();
    for _ in 0..WINDOW_FRAMES {
        window.send(vec![1], &|| true);
    }
    window.acknowledge(WINDOW_FRAMES as u64 + 1);
    assert_eq!(window.state.lock().outstanding.len(), WINDOW_FRAMES);
    window.acknowledge(3);
    window.acknowledge(3);
    window.acknowledge(1);
    assert_eq!(window.state.lock().outstanding.len(), WINDOW_FRAMES - 3);
    drop(rx);
}

#[test]
fn cancellation_wakes_a_blocked_producer_without_needing_an_ack() {
    let (window, rx) = fixture();
    for _ in 0..WINDOW_FRAMES {
        window.send(vec![1], &|| true);
    }
    let producer = Arc::clone(&window);
    let (done_tx, done_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        producer.send(vec![2], &|| true);
        done_tx.send(()).unwrap();
    });
    window.close();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    thread.join().unwrap();
    assert_eq!(rx.try_iter().count(), WINDOW_FRAMES);
    assert!(window.state.lock().outstanding.is_empty());
}

#[test]
fn channel_send_failure_closes_the_window_instead_of_waiting_forever() {
    let window = OutputWindow::new(
        Channel::new(|_| Err(tauri::Error::FailedToReceiveMessage)),
        Vec::new(),
    );
    for _ in 0..WINDOW_FRAMES + 1 {
        window.send(vec![1], &|| true);
    }
    assert!(window.state.lock().closed);
    assert!(window.state.lock().outstanding.is_empty());
}

#[test]
fn cancelled_waiter_does_not_wait_for_another_producers_credit() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (window, _rx) = fixture();
    for _ in 0..WINDOW_FRAMES {
        window.send(vec![1], &|| true);
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let owner_window = Arc::clone(&window);
    let owner = std::thread::spawn(move || {
        let first = AtomicBool::new(true);
        owner_window.send(vec![2], &|| {
            if first.swap(false, Ordering::AcqRel) {
                entered_tx.send(()).unwrap();
            }
            true
        });
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // The predicate runs under the state lock. Acquiring it here means the
    // owner has reached its credit wait, and retains batch ownership.
    assert!(window.state.lock().sending);
    let cancelled = Arc::new(AtomicBool::new(false));
    let waiter_cancelled = Arc::clone(&cancelled);
    let waiter_window = Arc::clone(&window);
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let first = AtomicBool::new(true);
        waiter_window.send(vec![3], &|| {
            // Capture the condition before signalling, so cancellation races
            // the wait itself rather than short-circuiting before it starts.
            let current = !waiter_cancelled.load(Ordering::Acquire);
            if first.swap(false, Ordering::AcqRel) {
                waiting_tx.send(()).unwrap();
            }
            current
        });
        done_tx.send(()).unwrap();
    });
    waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    cancelled.store(true, Ordering::Release);
    window.wake();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    waiter.join().unwrap();
    assert!(
        window.state.lock().sending,
        "the unrelated owner should still be waiting for credit"
    );
    window.close();
    owner.join().unwrap();
}
