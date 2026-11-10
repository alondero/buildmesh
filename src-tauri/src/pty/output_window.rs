//! Credit window before Tauri's raw Channel storage, not after body decoding.
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use tauri::ipc::{Channel, InvokeResponseBody};

pub(super) const FRAME_BYTES: usize = 32 * 1024;
pub(super) const WINDOW_FRAMES: usize = 64;
pub(super) const WINDOW_BYTES: usize = 256 * 1024;

struct State {
    outstanding: VecDeque<usize>,
    sent: u64,
    acknowledged: u64,
    replay: VecDeque<u8>,
    closed: bool,
    sending: bool,
}

impl State {
    fn has_credit(&self, bytes: usize) -> bool {
        self.outstanding.len() < WINDOW_FRAMES
            && self.outstanding.iter().sum::<usize>() + bytes <= WINDOW_BYTES
    }
}

pub(super) struct OutputWindow {
    channel: Channel<InvokeResponseBody>,
    state: Mutex<State>,
    ready: Condvar,
}

impl OutputWindow {
    pub(super) fn new(channel: Channel<InvokeResponseBody>, replay: Vec<u8>) -> Self {
        Self {
            channel,
            state: Mutex::new(State {
                outstanding: VecDeque::with_capacity(WINDOW_FRAMES),
                sent: 0,
                acknowledged: 0,
                replay: replay.into(),
                closed: false,
                sending: false,
            }),
            ready: Condvar::new(),
        }
    }

    pub(super) fn id(&self) -> u32 {
        self.channel.id()
    }

    fn dispatch(&self, state: &mut State, bytes: Vec<u8>) {
        state.outstanding.push_back(bytes.len());
        state.sent += 1;
        if let Err(error) = self.channel.send(InvokeResponseBody::Raw(bytes)) {
            tracing::error!("PTY output Channel send failed: {error}");
            state.closed = true;
            state.replay = VecDeque::new();
            state.outstanding.clear();
            self.ready.notify_all();
        }
    }

    fn replay(&self, state: &mut State) {
        while !state.closed && !state.replay.is_empty() {
            let count = FRAME_BYTES.min(state.replay.len());
            if !state.has_credit(count) {
                break;
            }
            let bytes = state.replay.drain(..count).collect();
            self.dispatch(state, bytes);
        }
        if state.replay.is_empty() {
            state.replay = VecDeque::new();
        }
    }

    pub(super) fn start(&self) {
        self.replay(&mut self.state.lock());
    }

    pub(super) fn acknowledge(&self, sequence: u64) {
        let mut state = self.state.lock();
        // Cumulative acknowledgements are idempotent, including duplicate/stale IPCs.
        if sequence <= state.acknowledged || sequence > state.sent || state.closed {
            return;
        }
        for _ in state.acknowledged..sequence {
            state.outstanding.pop_front();
        }
        state.acknowledged = sequence;
        self.replay(&mut state);
        self.ready.notify_all();
    }

    /// Only the dedicated PTY batcher waits here. Its bounded sync_channel then
    /// backpressures the reader and the OS PTY; no task or unbounded queue is spawned.
    pub(super) fn send(&self, data: Vec<u8>, is_current: &dyn Fn() -> bool) {
        let mut state = self.state.lock();
        // Ownership waits use the same cancellable condition as credit waits.
        // A retired producer must not park behind a different live producer.
        while !state.closed && is_current() && state.sending {
            self.ready.wait(&mut state);
        }
        if state.closed || !is_current() {
            return;
        }
        state.sending = true;
        // Preserve the original allocation for ordinary echoes and batches.
        let mut remainder = data;
        while !remainder.is_empty() {
            while !state.closed
                && is_current()
                && (!state.replay.is_empty() || !state.has_credit(remainder.len().min(FRAME_BYTES)))
            {
                self.ready.wait(&mut state);
            }
            if state.closed || !is_current() {
                break;
            }
            let tail = if remainder.len() > FRAME_BYTES {
                remainder.split_off(FRAME_BYTES)
            } else {
                Vec::new()
            };
            self.dispatch(&mut state, remainder);
            remainder = tail;
        }
        state.sending = false;
        self.ready.notify_all();
    }

    pub(super) fn wake(&self) {
        // Pair with the wait's state lock so epoch cancellation cannot miss a wakeup.
        let _state = self.state.lock();
        self.ready.notify_all();
    }

    pub(super) fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        state.replay = VecDeque::new();
        state.outstanding.clear();
        self.ready.notify_all();
    }
}

#[cfg(test)]
#[path = "output_window_tests.rs"]
mod tests;
