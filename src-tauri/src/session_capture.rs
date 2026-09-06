//! Captures provider-assigned session IDs from PTY output.
//!
//! Some providers (e.g. Codex) auto-assign session UUIDs rather than accepting
//! one via CLI flag. This module watches PTY output and stores the captured ID
//! in the database for future resume operations.
//!
//! ## Split-read limitation (issue #1221)
//!
//! Capture happens at PTY read time. The PTY reader delivers fixed ≤8 KiB
//! slices, so two failure modes arise when a single banner straddles a read
//! boundary:
//!
//! 1. **Regex miss**: running `try_extract_session_id` against each chunk
//!    independently means a `session id: <uuid>` banner that starts near the
//!    end of one chunk and finishes in the next is silently dropped —
//!    `cli_session_id` stays NULL and `decide_startup_resume` refuses to
//!    resume the node, leaving it Suspended. Such nodes must be resumed
//!    manually, regenerated, or deleted — see issue #1191 for the original
//!    regression.
//!
//! 2. **UTF-8 corruption**: per-chunk `String::from_utf8_lossy` replaces any
//!    multi-byte UTF-8 sequence split across two reads with U+FFFD, corrupting
//!    text fed to `session_naming::on_output` (rename buffer sent to the LLM)
//!    and `autopilot::evaluator::on_output`. Frontend display is unaffected
//!    (raw bytes are base64'd separately in `agent::spawn`'s reader thread).
//!
//! [`ChunkCapture`] is the stateful wrapper that holds a small carry-over
//! buffer of bytes not yet released to downstream consumers, addressing both
//! failure modes in a single seam. [`try_extract_session_id`] remains the
//! pure core so existing single-string fixtures stay valid.

use once_cell::sync::Lazy;
use regex::Regex;

// Captures a UUID preceded by a provider-printed label like `session:`,
// `session id:`, `conversation:`, or `conversation id:`. `(?:\s+id)?` is
// OPTIONAL inside each branch so the same regex handles both the two-word
// Codex banner shape and the legacy single-word shape — for the latter,
// the `[:\s]+` after the alternation consumes the colon itself.
static LABELED_SESSION_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:session(?:\s+id)?|conversation(?:\s+id)?)[:\s]+([0-9a-f]{6,8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
        .unwrap()
});

/// Attempt to extract a session ID from PTY output. Returns the UUID if found.
pub fn try_extract_session_id(data: &str) -> Option<&str> {
    LABELED_SESSION_ID_RE
        .captures(data)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())
}

// ---------------------------------------------------------------------------
// ChunkCapture — stateful wrapper for the PTY-reader callback (issue #1221)
// ---------------------------------------------------------------------------

/// Maximum number of bytes the wrapper holds back across feed() calls.
///
/// Sized so that a complete banner (`session id: <uuid>`) fits inside the
/// carry-over even after the chunk boundary split it: `"session id: "`
/// (12 bytes) + a 36-char UUID = 48 bytes. 64 leaves headroom for
/// `conversation id:` (14 bytes) and any leading whitespace the provider
/// might emit before the label.
const CHUNK_CAPTURE_TAIL_CAP: usize = 64;

/// True if `byte` is a UTF-8 continuation byte (top two bits = `10xxxxxx`).
/// Used by `ChunkCapture::feed` to advance the trim index to the next
/// char boundary; the manual implementation is portable across toolchains
/// (avoids relying on `<[u8]>::is_char_boundary`).
#[inline]
fn is_utf8_continuation_byte(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// Stateful wrapper around PTY chunk consumption.
///
/// The PTY reader delivers raw bytes in fixed ≤8 KiB slices (see
/// `agent::spawn::pump_pty_output`). Two failure modes arise when a single
/// banner or UTF-8 character straddles a slice boundary:
///
/// 1. The `try_extract_session_id` regex matches per chunk and silently drops
///    a banner that starts in one chunk and finishes in the next.
/// 2. Per-chunk `from_utf8_lossy` replaces split multi-byte UTF-8 with
///    U+FFFD, corrupting text downstream of the capture seam.
///
/// `ChunkCapture` holds a small `pending` buffer of bytes not yet released
/// to downstream consumers. On each call:
///
/// - Append the new chunk to `pending`.
/// - Run the regex against `pending` decoded as UTF-8 (lossy). If it
///   matches, latch (`captured = true`), drain `pending`, and return
///   `(decoded_text, Some(uuid))` — post-latch corruption is harmless
///   because the session ID has been captured.
/// - If not matched and `pending.len() <= CHUNK_CAPTURE_TAIL_CAP`, return
///   `("", None)` — the entire payload fits inside the carry-over, and the
///   regex still gets to scan it prepended to the next chunk.
/// - If not matched and `pending.len() > CHUNK_CAPTURE_TAIL_CAP`, trim the
///   oldest bytes off `pending` until the kept tail is at most
///   `CHUNK_CAPTURE_TAIL_CAP` bytes (advanced to a UTF-8 char boundary so
///   no half-character leaks through), and return those bytes as decoded
///   text. The kept tail — which may still end mid-multi-byte-char — is
///   held back so the regex sees it prepended to the next chunk and the
///   downstream consumers never see a corrupted U+FFFD on its own.
///
/// Two contracts for downstream consumers (pinned by tests):
///
/// - **No U+FFFD** in the released prefix — the trim boundary is always
///   advanced to a UTF-8 char boundary before splitting.
/// - **No duplication** in the released prefix — each byte is returned at
///   most once across the whole feed sequence.
///
/// One trade-off worth flagging: the held-back tail is the *last*
/// `CHUNK_CAPTURE_TAIL_CAP` bytes the wrapper has seen, so downstream
/// consumers never see those final bytes per session. Acceptable here
/// because the rename buffer caps at 4000 chars (LLM rename fires at
/// ~1500) and the autopilot tail is a circular buffer. Pinned by
/// [`tests::chunk_capture_releases_in_order_without_duplication`].
///
/// Once `captured` is set, subsequent calls short-circuit: no regex work,
/// no carry-over bookkeeping — the chunk's bytes are returned verbatim
/// (lossy-decoded, the same U+FFFD-on-split-UTF-8 the rest of the codebase
/// tolerated before issue #1221; harmless at that point because the
/// session ID has been written). Pinned by
/// [`tests::chunk_capture_short_circuits_after_latch`].
#[derive(Default)]
pub struct ChunkCapture {
    pending: Vec<u8>,
    captured: bool,
}

impl ChunkCapture {
    /// Pre-arm the latch so subsequent `feed` calls skip the regex
    /// entirely. Used by the reader thread when the caller already knows
    /// the node doesn't need session capture (e.g. providers we
    /// pre-assigned a UUID to via `SessionIdMode::Assign`).
    pub fn mark_captured(&mut self) {
        self.captured = true;
    }

    /// Feed a PTY chunk. Returns:
    ///
    /// - The decoded text downstream consumers (session_naming,
    ///   autopilot::evaluator) should process. Clean UTF-8 — no U+FFFD
    ///   corruption from split multi-byte chars.
    /// - The captured session UUID, if this chunk (or the carried-over
    ///   tail + this chunk) matched the regex. Once non-`None`, the
    ///   internal latch fires and subsequent calls return `None` for the
    ///   UUID even if more chunks arrive.
    pub fn feed(&mut self, chunk: &[u8]) -> (String, Option<String>) {
        if self.captured {
            // Post-latch: the session ID has been captured. Don't bother
            // with carry-over bookkeeping — just hand the bytes through.
            // Lossy decoding here matches the pre-#1221 behaviour for the
            // remainder of the session; no functional impact.
            return (String::from_utf8_lossy(chunk).into_owned(), None);
        }

        // 1. Append the new chunk to the carry-over.
        self.pending.extend_from_slice(chunk);

        // 2. Run the regex on the lossy-decoded carry-over.
        let decoded = String::from_utf8_lossy(&self.pending);
        // Convert to an owned `String` before mutating `self.pending` —
        // `try_extract_session_id` returns `Option<&str>` borrowed from
        // `decoded` (and thus `self.pending`); the borrow checker needs
        // the owned form to release the immutable borrow before the
        // `mem::take` below.
        if let Some(uuid) = try_extract_session_id(&decoded).map(str::to_string) {
            // Latch fires. Drain the carry-over and return the full
            // decoded text — it includes the held-back tail bytes that
            // completed the banner across the chunk boundary, plus
            // potentially a trailing incomplete UTF-8 sequence (we
            // discard any pending bytes after this chunk because we no
            // longer care).
            self.captured = true;
            let text = std::mem::take(&mut self.pending);
            // The bytes we are about to return may end mid-UTF-8, which
            // would round-trip through from_utf8_lossy as U+FFFD — but
            // the latch has fired so the downstream consumers no longer
            // matter, and the captured UUID is already an owned String.
            return (String::from_utf8_lossy(&text).into_owned(), Some(uuid));
        }

        // 3. No match. Decide what to hold back vs. release.
        if self.pending.len() <= CHUNK_CAPTURE_TAIL_CAP {
            // Everything fits in the carry-over — wait for more chunks.
            // No downstream text yet; the regex still has the whole
            // buffer to scan on the next call.
            return (String::new(), None);
        }

        // 4. Trim oldest bytes off `pending` so the kept tail is at most
        //    CHUNK_CAPTURE_TAIL_CAP bytes, advanced to a UTF-8 char
        //    boundary so the released prefix is valid UTF-8 (downstream
        //    consumers never see U+FFFD) and the kept tail starts at the
        //    start of a char. The tail may end mid-multi-byte-char if
        //    `pending` itself ends mid-char; the next feed() stitches it.
        let mut trim_to = self.pending.len() - CHUNK_CAPTURE_TAIL_CAP;
        // Advance `trim_to` to the next char boundary: a UTF-8
        // continuation byte (top two bits = `10xxxxxx`) is mid-char,
        // and we want to land at the start of a new char. End-of-buffer
        // (`trim_to == len`) is always a valid boundary, so this loop
        // terminates.
        while trim_to < self.pending.len() && is_utf8_continuation_byte(self.pending[trim_to]) {
            trim_to += 1;
        }

        // `split_off` is the byte-exact move: bytes [0..trim_to) stay in
        // `self.pending` (which is now the kept tail), bytes [trim_to..]
        // are moved to a new Vec (the released prefix).
        let tail = self.pending.split_off(trim_to);
        let released = std::mem::replace(&mut self.pending, tail);
        // By construction `released` is valid UTF-8 (its last byte sits
        // at a char boundary), so `from_utf8_lossy` never substitutes
        // U+FFFD. Use lossy anyway so a future invariant slip is
        // diagnosed (U+FFFD leaking through) instead of crashing the
        // reader thread.
        let released_text = String::from_utf8_lossy(&released).into_owned();

        (released_text, None)
    }
}

#[cfg(test)]
mod tests {
    use super::{try_extract_session_id, ChunkCapture, CHUNK_CAPTURE_TAIL_CAP};

    /// Codex's interactive TUI startup banner prints `session id: <UUID>`
    /// (two words, then `:`). The earlier regex only matched the single-word
    /// `session:` shape, so every Codex node's `cli_session_id` stayed NULL
    /// and `decide_startup_resume` was forced to leave them Suspended.
    #[test]
    fn captures_session_id_with_two_word_label() {
        assert_eq!(
            try_extract_session_id("session id: 01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// The mirror form — `conversation id:` — was already supported; pin it
    /// explicitly so a future shrink of the alternation cannot silently
    /// regress it.
    #[test]
    fn captures_conversation_id_with_two_word_label() {
        assert_eq!(
            try_extract_session_id("conversation id: 01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// Legacy `session: <UUID>` shape (single word, colon) was the original
    /// supported form. Keep it pinned so the two-word fix doesn't drop it.
    #[test]
    fn captures_session_with_single_word_label() {
        assert_eq!(
            try_extract_session_id("session: 01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// The single-word `conversation: <UUID>` shape shares its match path
    /// with `session:` (the `[:\s]+` after the alternation consumes the
    /// colon). Pin it explicitly so a future shrink of the alternation
    /// that drops `(?:\s+id)?` from the `conversation` branch cannot
    /// silently regress this shape while leaving the two-word test green.
    #[test]
    fn captures_conversation_with_single_word_label() {
        assert_eq!(
            try_extract_session_id("conversation: 01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// A bare UUID without a leading label must not match — capturing an
    /// unattributed UUID would race against the orchestrator's pre-write
    /// (`spawn.rs:1544-1547` `let cli_uuid = uuid::Uuid::new_v4()`).
    #[test]
    fn ignores_uuid_without_label() {
        assert!(try_extract_session_id("01a024d2-7cd6-7ea2-b907-531b0d261be7").is_none());
    }

    /// Real Codex banner shape — the label sits in a multi-line block with
    /// ANSI-free prose around it.
    #[test]
    fn extracts_from_real_codex_exec_banner_block() {
        let block = "OpenAI Codex v0.148.0\n--------\nsession id: 01a024d2-7cd6-7ea2-b907-531b0d261be7\n--------\n";
        assert_eq!(
            try_extract_session_id(block),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    // --- ChunkCapture (issue #1221) ---

    /// Issue #1221, primary case: the `session id: <uuid>` banner is split
    /// across two PTY reads. The per-chunk `try_extract_session_id` path used
    /// to silently drop the banner because neither half contains the full
    /// UUID, leaving `cli_session_id` NULL and the node Suspended at resume.
    /// The wrapper carries the unmatched tail across calls so the regex sees
    /// the concatenation.
    #[test]
    fn chunk_capture_captures_session_id_split_across_two_chunks() {
        let mut cap = ChunkCapture::default();
        // First chunk: label + start of UUID, no full match.
        let (text1, uuid1) = cap.feed(b"session id: 01a024");
        assert!(uuid1.is_none(), "no UUID in the first half: text={text1:?}");
        // The text returned on this chunk is "" — the carry-over still
        // holds the whole 19 bytes, since len <= TAIL_CAP.
        assert!(
            text1.is_empty(),
            "no text released before the carry-over fills: text={text1:?}"
        );

        // Second chunk: the rest of the UUID.
        let (text2, uuid2) = cap.feed(b"d2-7cd6-7ea2-b907-531b0d261be7\n");
        assert_eq!(
            uuid2.as_deref(),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            "wrapper must stitch the two halves and match the full UUID"
        );
        // After latch, the wrapper returns the concatenation (which the
        // naming buffer / evaluator consume). The exact text content can
        // include U+FFFD on the latch-firing chunk if the held-back tail
        // ended mid-UTF-8 (ASCII banner so it won't here, but we only
        // assert the UUID round-trip is what matters).
        assert!(
            text2.contains("01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            "downstream text must contain the captured UUID: text={text2:?}"
        );
    }

    /// Mirror of the primary case for the `conversation id:` label — same
    /// alternation branch, different surface string. Pins the second shape
    /// so a future shrink of the alternation that drops the `conversation`
    /// branch is caught by this test (the `try_extract_session_id`
    /// `captures_conversation_id_with_two_word_label` test pins the
    /// single-chunk shape; this test pins the cross-chunk one).
    #[test]
    fn chunk_capture_captures_conversation_id_split_across_two_chunks() {
        let mut cap = ChunkCapture::default();
        assert!(cap
            .feed(b"OpenAI Codex v0.148.0\n--------\nconversation id: 01a02")
            .1
            .is_none());
        let (_, uuid) = cap.feed(b"4d2-7cd6-7ea2-b907-531b0d261be7\n--------\n");
        assert_eq!(
            uuid.as_deref(),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            "wrapper must stitch the two halves for the conversation: label too"
        );
    }

    /// Issue #1221, secondary case: a 3-byte UTF-8 character (CJK block
    /// character `█`) split as `[0xe2, 0x96]` in one chunk and `[0x88]` in
    /// the next. The per-chunk `from_utf8_lossy` path used to substitute
    /// U+FFFD, corrupting the text handed to `session_naming::on_output`
    /// and `autopilot::evaluator::on_output`. The wrapper holds back the
    /// trailing incomplete sequence so the downstream text contains the
    /// real character once the carry-over is pushed past it.
    #[test]
    fn chunk_capture_returns_clean_text_without_split_utf8_corruption() {
        // Build an input long enough that the CJK char falls outside the
        // 64-byte held-back tail by the time we've fed everything: ~70 x's
        // (padding past the tail cap) + the split CJK char + enough
        // trailing filler to push the CJK char out of the carry-over.
        let mut input: Vec<u8> = "x".repeat(70).into_bytes();
        input.extend_from_slice(&[0xe2, 0x96]); // chunk-1 half of █
        input.extend_from_slice(&[0x88]); // chunk-2 half of █
        input.extend_from_slice(b"hello world");
        input.extend_from_slice(&"y".repeat(80).into_bytes());

        // Split at the CJK boundary so the test exercises the same
        // split that motivated the fix.
        let split_at = 70 + 2; // right after the second byte of █
        let (chunk1, chunk2) = input.split_at(split_at);
        assert!(chunk1.ends_with(&[0xe2, 0x96]));
        assert!(chunk2.starts_with(&[0x88]));

        let mut cap = ChunkCapture::default();
        let (text1, _) = cap.feed(chunk1);
        let (text2, _) = cap.feed(chunk2);

        let combined = format!("{text1}{text2}");
        assert!(
            !combined.contains('\u{FFFD}'),
            "downstream text must not contain U+FFFD: got {combined:?}"
        );
        assert!(
            combined.contains('\u{2588}'),
            "CJK char must appear correctly in downstream text: got {combined:?}"
        );
        // The CJK char is surrounded by x's and "hello world" in the
        // released text — assert its position to pin the exact byte-exact
        // stitching (no off-by-one in the carry-over boundary advance).
        let cjk_pos = combined.find('\u{2588}').expect("CJK char present");
        assert_eq!(
            &combined[..cjk_pos],
            &"x".repeat(70),
            "CJK char should be at byte position 70 of the released text"
        );
        assert!(
            combined[cjk_pos..].starts_with("\u{2588}hello world"),
            "CJK char should be followed by 'hello world' in the released text: got {:?}",
            &combined[cjk_pos..]
        );
    }

    /// The wrapper holds back at most `CHUNK_CAPTURE_TAIL_CAP` bytes for
    /// the regex scan. Anything older than that is released to downstream
    /// consumers in arrival order. No duplication, no silent drops in the
    /// released prefix; the held-back tail is exactly the last 64 bytes
    /// the wrapper has seen (and may legitimately never be released —
    /// the regex scan needs them prepended to the next chunk).
    ///
    /// Pinned here so a future "optimisation" that drops the carry-over
    /// prematurely can't silently corrupt the rename / autopilot tail.
    #[test]
    fn chunk_capture_releases_in_order_without_duplication() {
        // ~120 bytes total so the carry-over trims at least twice and
        // the concatenation covers multiple chunks. ASCII-only so UTF-8
        // isn't a confound.
        let input: String = (b'a'..=b'z').cycle().take(120).map(char::from).collect();
        let mut cap = ChunkCapture::default();
        // Feed in 30-byte chunks so the carry-over (64 bytes) sees a
        // mix of "freshly appended" and "held-back" content per call.
        let mut collected = String::new();
        for chunk_bytes in input.as_bytes().chunks(30) {
            let (text, _) = cap.feed(chunk_bytes);
            collected.push_str(&text);
        }
        // The released prefix is exactly `input.len() - CHUNK_CAPTURE_TAIL_CAP`
        // bytes: the wrapper trims the oldest bytes each call and
        // returns them in order, with no duplication.
        let expected_len = input.len() - CHUNK_CAPTURE_TAIL_CAP;
        assert_eq!(
            collected.len(),
            expected_len,
            "released text must be exactly input minus the held-back tail cap"
        );
        assert_eq!(
            collected,
            &input[..expected_len],
            "released text must be the oldest input bytes, in arrival order, byte-exact"
        );
    }

    /// Once the regex matches and the latch fires, the wrapper must stop
    /// running the regex and stop maintaining the carry-over — the rest of
    /// the session's PTY output is just bytes to ship downstream. Verify
    /// by feeding a chunk that *looks* like a banner after the latch; it
    /// must NOT re-trigger (would be benign but wasteful, and would risk
    /// overwriting a valid `cli_session_id` written via the structured
    /// hook with a delayed PTY banner).
    #[test]
    fn chunk_capture_short_circuits_after_latch() {
        let mut cap = ChunkCapture::default();
        let (_, uuid) = cap.feed(b"session id: 01a024d2-7cd6-7ea2-b907-531b0d261be7\n");
        assert_eq!(
            uuid.as_deref(),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
        // Post-latch: even a chunk that would normally trigger a fresh
        // regex match must return `None` for the UUID — the latch is
        // sticky. The text is still returned (verbatim to downstream).
        let (text, uuid) =
            cap.feed(b"\nsome more output\nsession id: ffff0000-7cd6-7ea2-b907-531b0d261be7\n");
        assert!(uuid.is_none(), "latch must not re-fire on later chunks");
        assert!(text.contains("some more output"));
    }

    /// Post-latch the wrapper reverts to per-chunk lossy decoding (the
    /// carry-over bookkeeping has been dropped; the regex has already
    /// fired). Pin the trade-off: split UTF-8 in later chunks WILL be
    /// U+FFFD-substituted, exactly as it was before #1221. This is
    /// harmless because the session ID has been captured, but a future
    /// "optimisation" must not silently tighten the contract.
    #[test]
    fn chunk_capture_post_latch_uses_per_chunk_lossy_decoding() {
        let mut cap = ChunkCapture::default();
        // Latch first.
        let _ = cap.feed(b"session id: 01a024d2-7cd6-7ea2-b907-531b0d261be7\n");
        // Feed a chunk with a CJK char split across an 8-byte boundary.
        // Post-latch, the wrapper does NOT stitch — U+FFFD is expected.
        let (text, uuid) = cap.feed(&[0xe2, 0x96]);
        assert!(uuid.is_none(), "latch remains sticky");
        let (text2, uuid) = cap.feed(&[0x88, b'h', b'i']);
        assert!(uuid.is_none(), "latch remains sticky");
        // The CJK char across the post-latch boundary is U+FFFD —
        // documenting the pre-#1221 behaviour the wrapper deliberately
        // preserves post-latch.
        let combined = format!("{text}{text2}");
        assert!(
            combined.contains('\u{FFFD}'),
            "post-latch U+FFFD is the documented contract: got {combined:?}"
        );
        assert!(
            combined.contains("hi"),
            "non-split bytes still flow through unchanged: got {combined:?}"
        );
    }

    /// Empty chunk must not panic, must not run the regex, must not
    /// release text. This is a corner case the pump's `Ok(0)` branch can
    /// theoretically reach if a future `Read` impl returns zero-length
    /// reads, even though `pump_pty_output` treats `Ok(0)` as EOF.
    #[test]
    fn chunk_capture_empty_chunk_is_a_noop() {
        let mut cap = ChunkCapture::default();
        let (text, uuid) = cap.feed(b"");
        assert!(text.is_empty());
        assert!(uuid.is_none());
        // Subsequent real chunk must still match normally — no state
        // was lost on the empty call.
        let (_, uuid) = cap.feed(b"session id: 01a024d2-7cd6-7ea2-b907-531b0d261be7");
        assert_eq!(
            uuid.as_deref(),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// Single-chunk happy path: the banner arrives in one chunk (the
    /// common case for providers whose startup banner fits inside 8 KiB).
    /// The wrapper must latch and return the UUID on the first call, and
    /// the returned text must contain the banner verbatim.
    #[test]
    fn chunk_capture_single_chunk_match_returns_uuid_and_text() {
        let mut cap = ChunkCapture::default();
        let banner = "session id: 01a024d2-7cd6-7ea2-b907-531b0d261be7";
        let (text, uuid) = cap.feed(banner.as_bytes());
        assert_eq!(
            uuid.as_deref(),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
        assert_eq!(text, banner);
    }

    /// When the regex never matches across many small chunks, the wrapper
    /// must release text incrementally as `pending` grows past
    /// `CHUNK_CAPTURE_TAIL_CAP` — otherwise the naming buffer would
    /// starve for the first ~64 bytes of every session.
    #[test]
    fn chunk_capture_releases_text_when_carry_over_grows() {
        let mut cap = ChunkCapture::default();
        // First 60 bytes: fits in carry-over, returns "".
        let (t1, _) = cap.feed(&b"x".repeat(60));
        assert!(t1.is_empty());
        // Next 20 bytes: pending now 80 bytes; carry-over trims to 64,
        // releasing 16 bytes of "x".
        let (t2, _) = cap.feed(&b"y".repeat(20));
        assert_eq!(t2.len(), 16);
        assert!(t2.chars().all(|c| c == 'x'));
    }
}
