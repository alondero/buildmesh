import { describe, it, expect, vi, beforeEach } from 'vitest';
import { TerminalWriter, MAX_PENDING_BYTES, INTERACTIVE_FAST_PATH_BYTES } from '../../src/components/Terminal/TerminalWriter';

describe('TerminalWriter', () => {
  let writer: TerminalWriter;
  let scheduledCallbacks: (() => void)[];
  let mockScheduler: (cb: () => void) => void;

  beforeEach(() => {
    scheduledCallbacks = [];
    mockScheduler = (cb) => scheduledCallbacks.push(cb);
    writer = new TerminalWriter(mockScheduler);
  });

  function flush() {
    const cbs = [...scheduledCallbacks];
    scheduledCallbacks.length = 0;
    cbs.forEach(cb => cb());
  }

  describe('register/unregister', () => {
    it('tracks registered nodes', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      expect(writer.has(1)).toBe(true);
    });

    it('unregister removes node', () => {
      writer.register(1, vi.fn());
      writer.unregister(1);
      expect(writer.has(1)).toBe(false);
    });

    it('append does nothing for unregistered node', () => {
      writer.append(999, 'data');
      expect(scheduledCallbacks).toHaveLength(0);
    });
  });

  describe('batching', () => {
    it('buffers data until flush', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);

      // Use rAF-sized chunks so the writer takes the rAF path. The
      // interactive fast path (issue #1122) flushes the first chunk
      // immediately and would bypass the buffer entirely.
      const first = 'x'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1);
      const second = 'y'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1);
      writer.append(1, first);
      writer.append(1, second);
      expect(writeFn).not.toHaveBeenCalled();
      expect(writer.pendingBytes(1)).toBe(first.length + second.length);

      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      expect(writeFn).toHaveBeenCalledWith(first + second);
    });

    it('buffers byte chunks without stringifying them', () => {
      // Use rAF-sized chunks so the writer takes the rAF path —
      // merging adjacent byte chunks into one Uint8Array via
      // `coalesceChunks` is meaningful only when both chunks land in
      // the same buffer before the frame fires (issue #1122 fast path
      // would flush each chunk immediately, so the byte-chunk merge
      // guarantee would be untestable here).
      const writeFn = vi.fn();
      writer.register(1, writeFn);

      const first = new Uint8Array(INTERACTIVE_FAST_PATH_BYTES + 1);
      first.set([0xe2, 0x96, 0x80], 0);
      const second = new Uint8Array(INTERACTIVE_FAST_PATH_BYTES + 1);
      second.set([0xe2, 0x96, 0x80], 0);
      writer.append(1, first);
      writer.append(1, second);

      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      const written = writeFn.mock.calls[0][0] as Uint8Array;
      expect(written).toBeInstanceOf(Uint8Array);
      // The two chunks must be back-to-back in the merged buffer.
      expect(written.byteLength).toBe(2 * (INTERACTIVE_FAST_PATH_BYTES + 1));
      expect(written[0]).toBe(0xe2);
      expect(written[1]).toBe(0x96);
      expect(written[2]).toBe(0x80);
    });

    it('only schedules one frame per batch', () => {
      // Use rAF-sized chunks so the writer takes the rAF path. Small
      // chunks flush via the interactive fast path (issue #1122) and
      // wouldn't schedule a frame at all — see the "interactive fast
      // path" describe block for the by-chunk-size split.
      writer.register(1, vi.fn());
      writer.append(1, 'x'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));
      writer.append(1, 'y'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));
      writer.append(1, 'z'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));
      expect(scheduledCallbacks).toHaveLength(1);
    });

    it('fast path leaves the rAF queue empty for small bursts', () => {
      // Mirror of the above: the fast path (issue #1122) handles small
      // single-byte chunks by writing directly, so the rAF queue stays
      // empty even across multiple sync appends. Without this, the
      // batching path would still schedule one frame per chunk.
      writer.register(1, vi.fn());
      writer.append(1, 'a');
      writer.append(1, 'b');
      writer.append(1, 'c');
      expect(scheduledCallbacks).toHaveLength(0);
    });

    it('allows a new batch after flush completes', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);

      writer.append(1, 'first');
      flush();
      expect(writeFn).toHaveBeenCalledWith('first');

      writer.append(1, 'second');
      flush();
      expect(writeFn).toHaveBeenCalledWith('second');
      expect(writeFn).toHaveBeenCalledTimes(2);
    });

    it('clears buffer after flush', () => {
      writer.register(1, vi.fn());
      // Use a chunk larger than the interactive fast path so the
      // writer takes the rAF path. The fast path flushes immediately
      // and would skip the buffer entirely — pendingBytes would be 0
      // before flush() even runs.
      writer.append(1, 'r'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));
      flush();
      expect(writer.pendingBytes(1)).toBe(0);
    });
  });

  describe('isolation between nodes', () => {
    it('separate nodes have independent buffers', () => {
      const write1 = vi.fn();
      const write2 = vi.fn();
      writer.register(1, write1);
      writer.register(2, write2);

      writer.append(1, 'for-node-1');
      writer.append(2, 'for-node-2');
      flush();

      expect(write1).toHaveBeenCalledWith('for-node-1');
      expect(write2).toHaveBeenCalledWith('for-node-2');
    });

    it('unregistering one node does not affect another', () => {
      const write1 = vi.fn();
      const write2 = vi.fn();
      writer.register(1, write1);
      writer.register(2, write2);

      writer.unregister(1);
      writer.append(2, 'still works');
      flush();

      expect(write2).toHaveBeenCalledWith('still works');
    });
  });

  describe('stale closure protection', () => {
    it('does not write if node is unregistered before flush', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      // rAF-sized chunk so the writer doesn't take the fast path
      // (which flushes immediately and bypasses the rAF).
      writer.append(1, 'r'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));
      writer.unregister(1);
      flush();
      expect(writeFn).not.toHaveBeenCalled();
    });

    it('does not write if node is re-registered (different entry) before flush', () => {
      const writeFn1 = vi.fn();
      const writeFn2 = vi.fn();
      writer.register(1, writeFn1);
      // rAF-sized chunk so the writer doesn't take the fast path.
      writer.append(1, 'r'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));

      writer.unregister(1);
      writer.register(1, writeFn2);
      flush();

      expect(writeFn1).not.toHaveBeenCalled();
      expect(writeFn2).not.toHaveBeenCalled();
    });
  });

  describe('default scheduler', () => {
    it('default scheduler does not throw when invoked through a method call', () => {
      // Regression: the default scheduler used to be `requestAnimationFrame` directly,
      // which in Chromium/WebView2 throws "Illegal invocation" when called as
      // `this.scheduler(cb)` because `this` is no longer `window`. The default must
      // therefore be a wrapper that calls requestAnimationFrame with the correct
      // receiver — exercising the default here would have caught the production bug
      // that silently dropped all PTY output.
      //
      // Chunk size note (issue #1122): the chunk has to be larger than
      // INTERACTIVE_FAST_PATH_BYTES so the writer takes the rAF path
      // rather than the interactive fast path. The fast path bypasses
      // the scheduler entirely, which would side-step the very trap
      // this regression test pins (the wrapped-scheduler contract).
      const fakeRaf = vi.fn((cb: () => void) => { cb(); return 0; });
      vi.stubGlobal('requestAnimationFrame', fakeRaf);
      try {
        const defaultWriter = new TerminalWriter();
        const writeFn = vi.fn();
        defaultWriter.register(1, writeFn);
        expect(() => defaultWriter.append(1, 'x'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1))).not.toThrow();
        expect(fakeRaf).toHaveBeenCalledTimes(1);
        // The fake raf invoked the callback synchronously, so the write should have flushed.
        expect(writeFn).toHaveBeenCalledWith('x'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));
      } finally {
        vi.unstubAllGlobals();
      }
    });
  });

  describe('pending-buffer cap', () => {
    // Chromium suspends requestAnimationFrame while the window is hidden or
    // minimized, so the scheduler never fires and chunks accumulate for as
    // long as agents keep streaming — overnight that's unbounded memory and
    // a giant freeze-inducing flush on restore. The writer must cap the
    // backlog by dropping the OLDEST chunks (they'd scroll straight out of
    // xterm's finite scrollback anyway).

    it('drops oldest chunks once pending bytes exceed the cap', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);

      const chunk = new Uint8Array(1024).fill(65); // 'A'
      const chunksToOverflow = Math.ceil(MAX_PENDING_BYTES / chunk.byteLength) + 5;
      for (let i = 0; i < chunksToOverflow; i++) {
        writer.append(1, chunk);
      }

      expect(writer.pendingBytes(1)).toBeLessThanOrEqual(MAX_PENDING_BYTES);
      expect(writer.pendingBytes(1)).toBeGreaterThan(0);
    });

    it('keeps the newest data when overflowing', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);

      writer.append(1, 'old-'.repeat(MAX_PENDING_BYTES / 4)); // fills the cap alone
      writer.append(1, 'NEWEST');

      flush();
      expect(writeFn).toHaveBeenCalled();
      const written = writeFn.mock.calls.map(c => String(c[0])).join('');
      expect(written.endsWith('NEWEST')).toBe(true);
    });

    it('never drops the only pending chunk, even if oversized', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);

      const oversized = 'x'.repeat(MAX_PENDING_BYTES + 100);
      writer.append(1, oversized);

      flush();
      expect(writeFn).toHaveBeenCalledWith(oversized);
    });

    it('pending byte accounting survives a flush cycle', () => {
      writer.register(1, vi.fn());
      // Use a chunk larger than the interactive fast path so the writer
      // takes the rAF path; the fast path flushes immediately and would
      // bypass the accounting this test pins.
      const first = 'x'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1);
      writer.append(1, first);
      flush();
      expect(writer.pendingBytes(1)).toBe(0);
      // Next chunk is also rAF-sized so it pending-accounts.
      const second = 'y'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1);
      writer.append(1, second);
      expect(writer.pendingBytes(1)).toBe(second.length);
    });
  });

  describe('pendingBytes', () => {
    it('returns 0 for unknown node', () => {
      expect(writer.pendingBytes(999)).toBe(0);
    });

    it('returns accumulated buffer length for rAF-sized chunks', () => {
      // rAF path: chunks > INTERACTIVE_FAST_PATH_BYTES accumulate in
      // the buffer pending a frame flush.
      writer.register(1, vi.fn());
      writer.append(1, 'x'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));
      writer.append(1, 'y'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1));
      expect(writer.pendingBytes(1)).toBe(2 * (INTERACTIVE_FAST_PATH_BYTES + 1));
    });

    it('returns 0 for fast-path-sized chunks (immediate flush)', () => {
      // Fast path: chunks ≤ INTERACTIVE_FAST_PATH_BYTES flush
      // synchronously, so pendingBytes is 0 right after append.
      writer.register(1, vi.fn());
      writer.append(1, 'abc');
      writer.append(1, 'de');
      expect(writer.pendingBytes(1)).toBe(0);
    });
  });

  describe('interactive fast path (issue #1122)', () => {
    // The writer's full code path stacks a `requestAnimationFrame` on top of
    // xterm's own internal render-rAF, adding one frame of latency per
    // keystroke echo. Interactive single-byte writes (the dominant cost in
    // the progressive-latency bug) bypass rAF and go straight to xterm so
    // the visible state lands on xterm's next frame, not our next frame +
    // xterm's next frame. Burst output (> INTERACTIVE_FAST_PATH_BYTES bytes)
    // still falls back to rAF batching so a verbose log dump stays at one
    // xterm write per frame.
    //
    // Complete UTF-8 byte sequences and JavaScript strings are safe for the
    // fast path. An incomplete byte sequence still falls back to rAF batching.
    // A multi-byte UTF-8 codepoint can arrive split across two PTY chunks —
    // writing the partial sequence directly would corrupt the character.
    // Keystroke echoes are virtually always ASCII, so the guard doesn't
    // regress the interactive latency win in practice.

    it('writes a single ASCII character directly without scheduling rAF', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      writer.append(1, 'a');
      expect(writeFn).toHaveBeenCalledWith('a');
      expect(scheduledCallbacks).toHaveLength(0);
      expect(writer.pendingBytes(1)).toBe(0);
    });

    it('writes a short ASCII string directly without scheduling rAF', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      // Exactly 4 bytes — at the fast-path boundary.
      writer.append(1, 'abcd');
      expect(writeFn).toHaveBeenCalledWith('abcd');
      expect(scheduledCallbacks).toHaveLength(0);
    });

    it('falls back to rAF for a small non-ASCII chunk to keep UTF-8 splits safe', () => {
      // Pinned regression for the build-run split-UTF-8 test. A 2-byte
      // chunk whose first byte is 0xE2 is the start of a 3-byte UTF-8
      // sequence (e.g. U+2580 ▀ = 0xE2 0x96 0x80). If we let the fast
      // path fire, the next chunk (0x80) would write a separate term.write
      // for the partial sequence and xterm would corrupt the character.
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      writer.append(1, new Uint8Array([0xe2, 0x96]));
      expect(writeFn).not.toHaveBeenCalled();
      expect(scheduledCallbacks).toHaveLength(1);
    });

    it('falls back to rAF for a small non-ASCII byte chunk (incomplete UTF-8 sequence)', () => {
      // Widened the fast path to handle complete UTF-8 sequences AND
      // any JS string (which is atomic from xterm's perspective). A
      // JS string `'a£'` is NOT incomplete — `isFastPathSafe` for
      // strings returns true unconditionally. The byte-chunk case is
      // where the partial-codepoint guard still matters: a `Uint8Array`
      // whose tail is mid-sequence must defer to rAF so the next
      // chunk can be merged.
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      // 2 bytes of a 3-byte UTF-8 sequence (start of ▀ U+2580).
      writer.append(1, new Uint8Array([0xe2, 0x96]));
      expect(writeFn).not.toHaveBeenCalled();
      expect(scheduledCallbacks).toHaveLength(1);
    });

    it('writes a complete UTF-8 sequence byte chunk directly (issue #1122 widening)', () => {
      // Widened from the strict-ASCII-only fast path: a complete
      // multi-byte UTF-8 sequence within the byte limit takes the
      // fast path because the chunk has no partial codepoint at the
      // boundary. ▀ U+2580 = 0xE2 0x96 0x80 — a 3-byte sequence that
      // arrives whole.
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      writer.append(1, new Uint8Array([0xe2, 0x96, 0x80]));
      expect(writeFn).toHaveBeenCalledTimes(1);
      expect(writeFn.mock.calls[0][0]).toBeInstanceOf(Uint8Array);
      expect(scheduledCallbacks).toHaveLength(0);
    });

    it('writes a multi-codepoint UTF-8 JS string directly (atom-safe strings)', () => {
      // JS strings are atomic from xterm's perspective (the renderer
      // handles surrogate pairs internally), so any string is safe —
      // including non-ASCII. The 16-byte fast-path cap is checked
      // against `byteLength` (the heuristic `data.length` matches
      // the byte count for JS strings only when all chars are ASCII,
      // so the cap is conservative for non-ASCII strings). This test
      // verifies a 4-char non-ASCII string takes the fast path
      // (4-char JS string is well within the 16-byte cap once
      // counted via the actual byte length).
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      writer.append(1, 'a£€');
      expect(writeFn).toHaveBeenCalledTimes(1);
      expect(writeFn).toHaveBeenCalledWith('a£€');
      expect(scheduledCallbacks).toHaveLength(0);
    });

    it('falls back to rAF for a partial UTF-8 sequence at chunk boundary', () => {
      // 2 bytes of a 3-byte UTF-8 sequence. The chunk ends mid-
      // codepoint — the next chunk (continuation bytes) will
      // complete it. Defer to rAF so chunks can be merged.
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      writer.append(1, new Uint8Array([0xe2, 0x96]));
      expect(writeFn).not.toHaveBeenCalled();
      expect(scheduledCallbacks).toHaveLength(1);
    });

    it('writes a 16-byte ASCII chunk directly (upper bound of fast path)', () => {
      // At the boundary: 16 bytes is exactly the cap. ASCII is safe
      // (no codepoint spans more than one byte), so the fast path
      // applies.
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      const payload = 'x'.repeat(INTERACTIVE_FAST_PATH_BYTES);
      writer.append(1, payload);
      expect(writeFn).toHaveBeenCalledWith(payload);
      expect(scheduledCallbacks).toHaveLength(0);
    });

    it('falls back to rAF for a 17-byte ASCII chunk (just over the cap)', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      const payload = 'y'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1);
      writer.append(1, payload);
      expect(writeFn).not.toHaveBeenCalled();
      expect(scheduledCallbacks).toHaveLength(1);
    });

    it('still coalesces over-sized bursts into one rAF-driven write', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      // 100 lines of agent output — each one is sized to exceed the
      // 16-byte interactive fast path so the writer takes the rAF
      // path on the first chunk. The fast path would flush each
      // chunk directly (the bug we're guarding against), defeating
      // the batching that this regression test pins.
      const line = 'build line: ' + 'x'.repeat(INTERACTIVE_FAST_PATH_BYTES) + '\n';
      for (let i = 0; i < 100; i++) {
        writer.append(1, line);
      }
      expect(writeFn).not.toHaveBeenCalled();
      expect(scheduledCallbacks).toHaveLength(1);
      flush();
      expect(writeFn).toHaveBeenCalledTimes(1);
    });
  });
});
