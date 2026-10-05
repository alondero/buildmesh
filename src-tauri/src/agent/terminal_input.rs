//! Streaming input accounting. Terminal replies are transported to the harness,
//! but are not evidence that a human edited or submitted its prompt.

use ts_rs::TS;

pub(super) const UNKNOWN_INPUT_BUFFER_LEN: usize = usize::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, TS)]
#[ts(export, export_to = "InputActivity.ts")]
pub struct InputActivity {
    pub user_input: bool,
    pub submitted: bool,
}

/// What the PTY input queue actually did with one buffer (issue #1530).
///
/// Before #1530 a full writer queue was logged and dropped, and the caller was
/// told the write succeeded — so a user could be promised their prompt arrived
/// while it had vanished. Every transport surface (desktop IPC, mobile
/// HTTP/WS, the coordinator drive) now receives this instead of an `Ok(())`
/// that meant nothing, and the recovery — one ordered retry buffer at the
/// client seam — is driven off `Backpressured`.
///
/// `Backpressured` is *definitively not delivered*: the buffer never entered
/// the queue, which is what makes a client-side retry safe to perform without
/// risking a duplicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case", export_to = "InputDisposition.ts")]
pub enum InputDisposition {
    /// The buffer is queued for the writer thread. The only value that
    /// licenses recording activity, first-input telemetry, or a coordinator
    /// "delivered" verdict.
    Accepted,
    /// The queue was at capacity (messages or bytes) and rejected the buffer.
    /// Nothing was enqueued; the caller still holds the exact bytes and may
    /// retry them in order, later.
    Backpressured,
    /// The writer thread is gone or the process incarnation is retired —
    /// `kill_session` ran, or the child died. Terminal: retrying cannot
    /// succeed, and the buffer is gone.
    Closed,
}

/// A write's disposition together with the input activity its bytes *actually*
/// produced.
///
/// The pairing is the point: on `Backpressured` the decoder is deliberately
/// left untouched, so `activity` is `default()` and no side-effect runs. That
/// is what stops a dropped prompt from being recorded as a first user input
/// (issue #1530 verification: "telemetry remains untouched").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, TS)]
#[ts(export, export_to = "InputOutcome.ts")]
pub struct InputOutcome {
    pub disposition: InputDisposition,
    pub activity: InputActivity,
}

impl InputOutcome {
    pub fn accepted(activity: InputActivity) -> Self {
        Self { disposition: InputDisposition::Accepted, activity }
    }

    pub fn backpressured() -> Self {
        Self { disposition: InputDisposition::Backpressured, activity: InputActivity::default() }
    }

    pub fn is_accepted(&self) -> bool {
        self.disposition == InputDisposition::Accepted
    }
}

impl Default for InputDisposition {
    /// `Closed`, so `InputOutcome::default()` fails *safe*.
    ///
    /// A defaulted outcome that read as `Accepted` would let any future field
    /// or path that forgets to set a disposition silently assert delivery — the
    /// precise failure mode #1530 exists to remove, in a subtler form. This
    /// matches the coordinator's "fail safe, not fail open" rule for its
    /// ledger guard: an unknown disposition must not be optimistically
    /// `Accepted`.
    fn default() -> Self {
        Self::Closed
    }
}

/// Why an input write could not be completed, in the vocabulary a caller can
/// act on.
///
/// `write_bytes_if_current` already returned `Ok(None)` for "you lost the
/// draft-ownership guard" — a *retry with the same stamp is pointless* signal.
/// Folding backpressure into that same `Option` would have made a rejected
/// write indistinguishable from a lost guard, and the circuit would have
/// silently discarded the prompt. The two are therefore separate types
/// (issue #1530).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputWriteError {
    /// The queue refused the buffer. Retryable, in order, with the same bytes.
    Backpressured,
    /// No writer thread, or the incarnation was retired.
    Closed,
    /// The caller supplied an unparseable input-ownership stamp.
    InvalidStamp,
}

impl std::fmt::Display for InputWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Deliberately distinct from "Agent not running": a caller (or a
            // human reading a log) must be able to tell a retryable stall
            // apart from a dead process.
            Self::Backpressured => f.write_str("PTY input queue is full — the agent is not reading its input (retryable)"),
            Self::Closed => f.write_str("Agent not running"),
            Self::InvalidStamp => f.write_str("Invalid input ownership stamp"),
        }
    }
}

impl std::error::Error for InputWriteError {}

impl InputWriteError {
    pub fn disposition(&self) -> InputDisposition {
        match self {
            Self::Backpressured => InputDisposition::Backpressured,
            Self::Closed | Self::InvalidStamp => InputDisposition::Closed,
        }
    }
}

// ---- The PTY input queue's byte bound ------------------------------------

/// Ceiling on bytes *buffered but not yet written to the PTY* for one agent.
///
/// The channel's message bound alone is not a memory bound: one message is one
/// `write_to_agent` call, and issue #1498 traced a real 17,508-byte Windows
/// paste arriving as a single logical entry. 64 of those is over a megabyte
/// held on behalf of one stalled process, so the message count is paired with
/// this byte ceiling (issue #1530).
///
/// Set far above any realistic backlog — a genuine stall is the writer thread
/// being wedged on a full ConPTY pipe, not a user typing fast — so this only
/// fires when something is actually wrong, and never for ordinary typing.
pub const PTY_INPUT_QUEUE_BYTE_CAPACITY: usize = 1024 * 1024;

/// Live accounting for one agent's PTY input queue.
///
/// `std::sync::mpsc::SyncSender` exposes no depth or capacity introspection, so
/// the byte ceiling above can only be *enforced* and only be *observable* if
/// the enqueuer tracks it: the bound would otherwise be a constant nothing
/// reads, and "queue byte usage is observable in diagnostics and returns to
/// baseline" would have no source.
///
/// Cloned into the writer thread so the drain side can decrement after each
/// `recv`. Reserve/release are single atomic RMWs, and every reservation is
/// made under the per-agent `writer_tx` lock, so two concurrent enqueues can
/// never both observe the same headroom.
#[derive(Debug, Default)]
pub struct InputQueueGauge {
    queued_bytes: std::sync::atomic::AtomicU64,
    queued_messages: std::sync::atomic::AtomicUsize,
}

impl InputQueueGauge {
    /// Does the queue have room for a `bytes`-long buffer right now?
    ///
    /// A buffer larger than the whole cap is admitted when the queue is
    /// otherwise empty. This is what keeps the bound from becoming a deadlock
    /// for a legitimate oversized paste: the alternative — rejecting it — would
    /// mean no size of paste could ever be sent, and splitting it to fit would
    /// destroy the whole-paste semantics of issue #1498.
    pub(super) fn admits(&self, bytes: usize) -> bool {
        let queued = self.queued_bytes.load(std::sync::atomic::Ordering::Relaxed) as usize;
        queued == 0 || queued.saturating_add(bytes) <= PTY_INPUT_QUEUE_BYTE_CAPACITY
    }

    /// Account for a buffer that is about to be handed to the channel. The
    /// caller must hold the per-agent input lock so a rejected
    /// [`Self::admits`] check and this reservation cannot interleave with
    /// another enqueue.
    pub fn reserve(&self, bytes: usize) {
        self.queued_bytes.fetch_add(bytes as u64, std::sync::atomic::Ordering::Relaxed);
        self.queued_messages.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Undo a reservation whose `try_send` then failed, or hand a drained
    /// buffer back once the writer has written it.
    pub fn release(&self, bytes: usize) {
        self.queued_bytes.fetch_sub(bytes as u64, std::sync::atomic::Ordering::Relaxed);
        self.queued_messages.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Abandon every buffered byte — the sender is being dropped, so nothing
    /// queued can ever be drained and the counters must not be inherited by a
    /// later incarnation of the same session.
    pub fn reset(&self) {
        self.queued_bytes.store(0, std::sync::atomic::Ordering::Relaxed);
        self.queued_messages.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// `(queued_messages, queued_bytes)` for diagnostics.
    pub fn snapshot(&self) -> (usize, u64) {
        (
            self.queued_messages.load(std::sync::atomic::Ordering::Relaxed),
            self.queued_bytes.load(std::sync::atomic::Ordering::Relaxed),
        )
    }
}

#[derive(Clone, Default)]
pub(super) struct TerminalInput {
    pub len: usize,
    pub bracketed_paste: bool,
    pending: Vec<u8>,
}

impl TerminalInput {
    #[cfg(test)]
    pub fn with_state(len: usize, bracketed_paste: bool) -> Self {
        Self {
            len,
            bracketed_paste,
            pending: Vec::new(),
        }
    }

    pub fn has_pending_sequence(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn empty_prompt(&self) -> bool {
        self.len == 0 && !self.bracketed_paste && self.pending.is_empty()
    }

    /// One complete xterm onData event, not an arbitrary transport chunk.
    pub fn accept_event(&mut self, data: &[u8]) -> InputActivity {
        // A separate submit/clear event ends an unfinished keyboard escape.
        // ESC+CR in ONE event is Alt+Enter, which may insert a newline instead.
        if !self.bracketed_paste
            && matches!(data, b"\r" | b"\n" | b"\x03")
            && (self.pending == b"\x1b" || self.pending.starts_with(b"\x1b["))
        {
            self.pending.clear();
        }
        self.accept(data)
    }

    pub fn accept(&mut self, data: &[u8]) -> InputActivity {
        let mut activity = InputActivity::default();
        for &byte in data {
            if !self.pending.is_empty() {
                self.pending.push(byte);
                let sequence = &self.pending;
                // Only validated terminal replies have a transport-only interpretation.
                // Unknown escapes remain edits, including history/navigation.
                let complete = if sequence.len() == 2 {
                    !matches!(byte, b'[' | b']')
                } else if sequence.starts_with(b"\x1b]") {
                    byte == 0x07 || sequence.ends_with(b"\x1b\\")
                } else if sequence.starts_with(b"\x1b[M") {
                    sequence.len() >= 6 // X10 mouse: three opaque bytes
                } else {
                    (0x40..=0x7e).contains(&byte) || !(0x20..=0x3f).contains(&byte)
                };
                if complete || sequence.len() >= 128 {
                    if sequence == b"\x1b[200~" && !self.bracketed_paste {
                        self.bracketed_paste = true;
                        activity.user_input = true;
                    } else if sequence == b"\x1b[201~" && self.bracketed_paste {
                        self.bracketed_paste = false;
                        activity.user_input = true;
                    } else if self.bracketed_paste || !transport_reply(sequence) {
                        self.len = UNKNOWN_INPUT_BUFFER_LEN;
                        activity.user_input = true;
                    }
                    self.pending.clear();
                }
                continue;
            }
            if byte == 0x1b {
                self.pending.push(byte);
                continue;
            }
            activity.user_input = true;
            match byte {
                b'\r' | b'\n' if !self.bracketed_paste => {
                    self.len = 0;
                    activity.submitted = true;
                }
                0x03 if !self.bracketed_paste => self.len = 0,
                b'\r' | b'\n' if self.bracketed_paste && self.len == 0 => {
                    self.len = UNKNOWN_INPUT_BUFFER_LEN;
                }
                b'\r' | b'\n' if self.bracketed_paste => {}
                _ if self.len == UNKNOWN_INPUT_BUFFER_LEN => {}
                0x08 | 0x7f if !self.bracketed_paste => self.len = self.len.saturating_sub(1),
                byte if byte < 0x20 || byte == 0x7f => self.len = UNKNOWN_INPUT_BUFFER_LEN,
                byte if byte & 0xc0 != 0x80 => self.len = self.len.saturating_add(1),
                _ => {}
            }
        }
        activity
    }
}

fn numbers(value: &[u8], count: Option<usize>) -> bool {
    !value.is_empty()
        && count.is_none_or(|count| value.split(|byte| *byte == b';').count() == count)
        && value
            .split(|byte| *byte == b';')
            .all(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
}

fn transport_reply(sequence: &[u8]) -> bool {
    if matches!(sequence, b"\x1b[I" | b"\x1b[O" | b"\x1b[0n" | b"\x1b[3n") {
        return true;
    }
    if sequence.starts_with(b"\x1b[M") && sequence.len() == 6 {
        return true;
    }
    if let Some(body) = sequence.strip_prefix(b"\x1b]") {
        let Some(body) = body
            .strip_suffix(b"\x07")
            .or_else(|| body.strip_suffix(b"\x1b\\"))
        else {
            return false;
        };
        let mut parts = body.split(|byte| *byte == b';');
        let color = match parts.next() {
            Some(b"10" | b"11" | b"12") => parts.next(),
            Some(b"4") if parts.next().is_some_and(|index| numbers(index, Some(1))) => parts.next(),
            _ => None,
        };
        return parts.next().is_none()
            && color
                .and_then(|color| color.strip_prefix(b"rgb:"))
                .is_some_and(|rgb| {
                    rgb.split(|byte| *byte == b'/').count() == 3
                        && rgb.split(|byte| *byte == b'/').all(|part| {
                            !part.is_empty()
                                && part.len() <= 4
                                && part.iter().all(u8::is_ascii_hexdigit)
                        })
                });
    }
    let Some(body) = sequence.strip_prefix(b"\x1b[") else {
        return false;
    };
    let Some((&final_byte, params)) = body.split_last() else {
        return false;
    };
    match final_byte {
        b'R' => numbers(params.strip_prefix(b"?").unwrap_or(params), Some(2)),
        b'c' => {
            params
                .first()
                .is_some_and(|byte| matches!(byte, b'?' | b'>'))
                && numbers(&params[1..], None)
        }
        b'M' | b'm' => params
            .strip_prefix(b"<")
            .is_some_and(|params| numbers(params, Some(3))),
        b'y' => params
            .strip_suffix(b"$")
            .is_some_and(|params| numbers(params.strip_prefix(b"?").unwrap_or(params), Some(2))),
        // Window position/size replies; other t sequences are not accepted.
        b't' => matches!(params.first(), Some(b'4' | b'8' | b'9')) && numbers(params, Some(3)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_and_clear_recover_after_incomplete_keyboard_sequences() {
        for prefix in [b"\x1b".as_slice(), b"\x1b[", b"\x1b[12;"] {
            for boundary in [b'\r', b'\n', 0x03] {
                for split in 0..=prefix.len() {
                    let mut input = TerminalInput::default();
                    input.accept_event(&prefix[..split]);
                    input.accept_event(&prefix[split..]);
                    let activity = input.accept_event(&[boundary]);
                    assert_eq!(activity, InputActivity {
                        user_input: true,
                        submitted: boundary != 0x03,
                    }, "prefix {prefix:?} split {split}");
                    assert!(input.empty_prompt(), "prefix {prefix:?} split {split}");
                }
            }
        }
    }

    #[test]
    fn pasted_or_string_payload_controls_do_not_establish_a_boundary() {
        for prefix in [b"\x1b[200~\x1b".as_slice(), b"\x1b[200~\x1b[", b"\x1b]11;"] {
            for boundary in [b'\r', b'\n', 0x03] {
                let mut input = TerminalInput::default();
                input.accept_event(prefix);
                assert!(!input.accept_event(&[boundary]).submitted);
                assert!(!input.empty_prompt());
            }
        }
    }

    #[test]
    fn alt_enter_and_unframed_escape_controls_remain_uncertain() {
        for boundary in [b'\r', b'\n', 0x03] {
            let mut input = TerminalInput::default();
            assert!(!input.accept_event(&[0x1b, boundary]).submitted);
            assert!(!input.empty_prompt());

            let mut input = TerminalInput::default();
            input.accept(b"\x1b");
            assert!(!input.accept(&[boundary]).submitted);
            assert!(!input.empty_prompt());
        }
    }

    #[test]
    fn protocol_packets_are_inert_at_every_split() {
        for packet in [
            b"\x1b[I".as_slice(),
            b"\x1b[O",
            b"\x1b[12;34R",
            b"\x1b[?12;34R",
            b"\x1b[?1;2c",
            b"\x1b[>0;276;0c",
            b"\x1b[<0;24;12M",
            b"\x1b[<0;24;12m",
            b"\x1b[M !!",
            b"\x1b[8;24;80t",
            b"\x1b[0n",
            b"\x1b[?2004;1$y",
            b"\x1b]11;rgb:ffff/0000/abab\x07",
            b"\x1b]4;2;rgb:ff/00/ab\x1b\\",
        ] {
            for split in 0..=packet.len() {
                let mut input = TerminalInput::default();
                assert_eq!(input.accept(&packet[..split]), InputActivity::default());
                assert_eq!(input.accept(&packet[split..]), InputActivity::default());
                assert!(input.empty_prompt(), "packet {packet:?} split {split}");
            }
        }
    }

    #[test]
    fn split_paste_keeps_newlines_literal() {
        let mut input = TerminalInput::default();
        for byte in b"\x1b[200~hello\nworld\x1b[201~" {
            assert!(!input.accept(&[*byte]).submitted);
        }
        assert_eq!(input.len, 10);
        assert!(!input.empty_prompt());
        assert_eq!(
            input.accept(b"\r"),
            InputActivity {
                user_input: true,
                submitted: true
            }
        );
        assert!(input.empty_prompt());
    }

    #[test]
    fn navigation_and_unknown_sequences_fail_closed() {
        for packet in [
            b"\x1b[A".as_slice(),
            b"\x1b[D",
            b"\x1b[12R",
            b"\x1b[broken~",
            b"\x10",
        ] {
            let mut input = TerminalInput::default();
            assert!(input.accept(packet).user_input);
            assert!(!input.empty_prompt());
        }
        let mut input = TerminalInput::default();
        input.accept(b"\x1b[");
        assert!(input.has_pending_sequence());
        assert!(!input.empty_prompt());
        input.accept(&[b'1'; 130]);
        assert_eq!(input.len, UNKNOWN_INPUT_BUFFER_LEN);
    }

    #[test]
    fn replies_do_not_erase_a_draft_and_pasted_controls_are_content() {
        let mut input = TerminalInput::with_state(5, false);
        assert_eq!(input.accept(b"\x1b[I\x1b[12;34R"), InputActivity::default());
        assert_eq!(input.len, 5);
        assert!(!input.accept(b"\x1b[200~\x1b[I\n\x1b[201~").submitted);
        assert_eq!(input.len, UNKNOWN_INPUT_BUFFER_LEN);
        assert!(!input.empty_prompt());
    }
}
