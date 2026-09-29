//! Streaming input accounting. Terminal replies are transported to the harness,
//! but are not evidence that a human edited or submitted its prompt.

pub(super) const UNKNOWN_INPUT_BUFFER_LEN: usize = usize::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputActivity {
    pub user_input: bool,
    pub submitted: bool,
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
