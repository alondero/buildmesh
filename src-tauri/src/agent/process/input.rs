//! Input incarnation stamps and bounded replay state for unconfirmed submissions.

use super::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputStamp {
    pub(super) generation: u64,
    pub(super) version: u64,
}

#[derive(Clone, Copy)]
pub(super) enum InputFraming {
    ByteStream,
    TerminalEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputUnavailable {
    MissingProcess,
    Draft,
    UnknownInput,
    Paste,
}

impl InputStamp {
    pub(crate) fn encode(self) -> String {
        format!("{}:{}", self.generation, self.version)
    }

    pub(crate) fn decode(value: &str) -> Option<Self> {
        let (generation, version) = value.split_once(':')?;
        Some(Self {
            generation: generation.parse().ok()?,
            version: version.parse().ok()?,
        })
    }
}

/// Ceiling on the bytes retained for one unconfirmed-submit replay.
///
/// Deliberately far below [`PTY_INPUT_QUEUE_BYTE_CAPACITY`]: this buffer is
/// held for the whole confirmation window rather than drained within a write.
/// A draft that outgrows it is abandoned rather than truncated — half a prompt
/// replayed as a prompt reads, to the user, like their own text.
pub(super) const SUBMIT_REPLAY_BYTE_CAPACITY: usize = 64 * 1024;

/// Why a retained draft was not written again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplaySkip {
    /// The harness does not declare that it drops early prompts.
    HarnessOptedOut,
    /// No submit is outstanding, or its draft outgrew the retention cap.
    NoRetainedDraft,
    /// The user has typed again since the submit; their text is not ours to
    /// overwrite with a replayed copy.
    PromptOccupied,
    /// This incarnation already replayed one submit.
    AlreadyReplayed,
}

/// What happened when Buildmesh re-sent an unconfirmed prompt.
///
/// A refusal keeps the reason the write path gives — a full queue and a dead
/// agent call for different responses, and reporting both as "not replayed"
/// would hide a stuck PTY behind a missing draft.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SubmitReplayOutcome {
    /// The draft reached the PTY.
    Sent,
    /// There was nothing to re-send.
    Skipped(ReplaySkip),
    /// The PTY refused the bytes.
    Refused(InputWriteError),
}

/// The bytes one accepted keystroke added to the prompt behind it.
///
/// Retained rather than the *decoded* text: the replay re-sends the exact
/// stream the user produced, so editing keys (backspace, cursor movement)
/// reproduce the same composer contents instead of a best guess at them.
#[derive(Debug, Default)]
pub(super) struct SubmitReplay {
    /// Bytes typed into the prompt that has not been submitted yet.
    pub(super) pending: Vec<u8>,
    /// `pending` outgrew the cap, so it no longer describes one whole prompt.
    pub(super) truncated: bool,
    /// The last submitted draft, held until the harness confirms or the
    /// confirmation window closes.
    pub(super) submitted: Option<Vec<u8>>,
    /// The submit terminator to re-send with the draft (`\r` or `\n`).
    pub(super) terminator: u8,
    /// One replay has already been spent on this incarnation.
    pub(super) replayed: bool,
}

impl SubmitReplay {
    /// Fold one accepted write into the retained prompt.
    ///
    /// `prompt_len` is the decoder's own character count *after* this write, so
    /// the retention clears itself where the composer does — on Ctrl+C and on a
    /// backspace run — rather than carrying bytes the user has already
    /// discarded into the next replay.
    pub(super) fn record(&mut self, data: &[u8], activity: InputActivity, prompt_len: usize) {
        if activity.submitted {
            // One buffer can carry a prompt and its terminator together — a
            // paste, or a raw byte stream through `write_bytes` — so the text
            // ahead of the first CR/LF is still part of the prompt being
            // submitted. Splitting on the *first* terminator keeps a later
            // "text\rmore text\r" from carrying the second prompt into the
            // first one's draft.
            let split = data
                .iter()
                .position(|byte| *byte == b'\r' || *byte == b'\n')
                .unwrap_or(data.len());
            // The byte at `split` is the one that submitted *this* draft. Taking
            // `tail.last()` would pick up a later line ending in a compound
            // buffer such as "cmd1\rcmd2\n" and re-send the wrong terminator.
            self.terminator = data.get(split).copied().unwrap_or(b'\r');
            let text = &data[..split];
            self.submitted = if !self.truncated
                && self.pending.len().saturating_add(text.len()) <= SUBMIT_REPLAY_BYTE_CAPACITY
            {
                self.pending.extend_from_slice(text);
                Some(std::mem::take(&mut self.pending))
            } else {
                self.pending.clear();
                None
            };
            self.truncated = false;
            return;
        }
        // Terminal replies are the terminal talking, not the user; they are
        // not prompt content and must never be replayed back at it.
        if !activity.user_input {
            return;
        }
        if prompt_len == 0 {
            // Cleared without submitting: Ctrl+C, or backspacing to nothing.
            // Either way the *typed* text is gone and must not be replayed.
            self.pending.clear();
            self.truncated = false;
            // Ctrl+C additionally abandons the outstanding submit. Left in
            // place it would put a refused prompt back on the wire 30 seconds
            // after the user refused it: the composer is empty, so the
            // replay's "still empty" fence would pass.
            //
            // Backspacing to empty is *not* a cancel — the user is correcting a
            // typo, and the unconfirmed submit behind it is still the only
            // recoverable work — so the draft survives that.
            if data.contains(&0x03) {
                self.submitted = None;
            }
            return;
        }
        if self.pending.len().saturating_add(data.len()) > SUBMIT_REPLAY_BYTE_CAPACITY {
            self.pending.clear();
            self.truncated = true;
            return;
        }
        self.pending.extend_from_slice(data);
    }

    /// The draft to write again, or why it must not be.
    ///
    /// `prompt_empty` is the live decoder reading: it is the fence that stops
    /// a replay from landing on top of text the user has typed since.
    pub(super) fn take_replay(
        &mut self,
        harness_opted_in: bool,
        prompt_empty: bool,
    ) -> Result<Vec<u8>, ReplaySkip> {
        if !harness_opted_in {
            return Err(ReplaySkip::HarnessOptedOut);
        }
        if self.replayed {
            return Err(ReplaySkip::AlreadyReplayed);
        }
        let Some(draft) = self.submitted.take() else {
            return Err(ReplaySkip::NoRetainedDraft);
        };
        if !prompt_empty {
            // Put it back: the user's next submit may be the one worth
            // rescuing, and consuming the draft here would guarantee it is not.
            self.submitted = Some(draft);
            return Err(ReplaySkip::PromptOccupied);
        }
        self.replayed = true;
        let mut bytes = draft;
        bytes.push(self.terminator);
        Ok(bytes)
    }
}
