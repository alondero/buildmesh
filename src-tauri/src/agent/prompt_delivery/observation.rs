//! Bounded, per-process observations; no classifier registration is required.

use std::sync::Mutex;
use std::time::{Duration, Instant};

const TAIL_BYTES: usize = 16_000;

#[derive(Debug, Default)]
pub(crate) struct OutputObservation(Mutex<Output>);

#[derive(Debug, Default)]
struct Output {
    tail: String,
    bytes: u64,
    last_output: Option<Instant>,
}

pub(super) struct Snapshot {
    pub cursor: u64,
    pub quiet: Option<Duration>,
    pub text: String,
}

impl OutputObservation {
    pub(crate) fn on_output(&self, text: &str) {
        self.record(text, Instant::now());
    }

    pub(super) fn record(&self, text: &str, now: Instant) {
        let mut output = self.0.lock().unwrap();
        output.bytes = output.bytes.saturating_add(text.len() as u64);
        output.last_output = Some(now);
        output.tail.push_str(text);
        if output.tail.len() > TAIL_BYTES {
            let mut start = output.tail.len() - TAIL_BYTES;
            while !output.tail.is_char_boundary(start) {
                start += 1;
            }
            output.tail.drain(..start);
        }
    }

    pub(super) fn snapshot(&self, since: u64, now: Instant) -> Snapshot {
        let output = self.0.lock().unwrap();
        let retained_start = output.bytes.saturating_sub(output.tail.len() as u64);
        let mut start = since
            .saturating_sub(retained_start)
            .min(output.tail.len() as u64) as usize;
        while !output.tail.is_char_boundary(start) {
            start += 1;
        }
        Snapshot {
            cursor: output.bytes,
            quiet: output
                .last_output
                .map(|at| now.saturating_duration_since(at)),
            text: crate::session_naming::ANSI_ESCAPE
                .replace_all(&output.tail[start..], "")
                .into_owned(),
        }
    }
}
