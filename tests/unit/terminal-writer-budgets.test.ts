import { describe, it, expect, vi, beforeEach } from 'vitest';
import {
  TerminalWriter,
  MAX_PENDING_BYTES,
  MAX_PENDING_CHUNKS,
  MAX_INFLIGHT_WRITES,
  INTERACTIVE_FAST_PATH_BYTES,
  type TerminalWriteData,
} from '../../src/components/Terminal/TerminalWriter';

/**
 * Budget tests for issue #2018. The byte cap, coalescing, fast path and
 * UTF-8 framing rules are pinned in `terminal-writer.test.ts`; this file
 * covers what the caps became *bounded by*: chunk objects, in-flight parser
 * work, and the reference release that makes eviction amortized.
 */
describe('TerminalWriter budgets (issue #2018)', () => {
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

  /** A chunk guaranteed to take the rAF path rather than the fast path. */
  function rafChunk(marker = 'x'): string {
    return `${marker}${'y'.repeat(INTERACTIVE_FAST_PATH_BYTES)}`;
  }

  /** A sink honouring xterm's `write(data, callback)` completion contract. */
  function completionSink() {
    const calls: TerminalWriteData[] = [];
    const completions: (() => void)[] = [];
    const fn = (data: TerminalWriteData, done?: () => void) => {
      calls.push(data);
      if (done) completions.push(done);
    };
    return {
      fn,
      calls,
      get outstanding() {
        return completions.length;
      },
      completeAll() {
        while (completions.length > 0) completions.shift()!();
      },
    };
  }

  describe('chunk-object budget', () => {
    it('caps queued chunk objects, not just payload bytes', () => {
      // The byte cap alone would admit 65,536 of these 64-byte chunks; the
      // object budget is what keeps the array (and its per-chunk headers)
      // bounded when a PTY reads in small slices.
      writer.register(1, vi.fn());
      const chunk = rafChunk();
      for (let i = 0; i < MAX_PENDING_CHUNKS * 3; i++) writer.append(1, chunk);

      expect(writer.queuedChunks(1)).toBe(MAX_PENDING_CHUNKS);
      expect(writer.pendingBytes(1)).toBe(MAX_PENDING_CHUNKS * chunk.length);
      expect(writer.pendingBytes(1)).toBeLessThan(MAX_PENDING_BYTES);
    });

    it('keeps the newest chunks, in arrival order, when the object cap evicts', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      // Distinct chunks so the survivor set is identifiable.
      const chunks = Array.from({ length: MAX_PENDING_CHUNKS * 2 }, (_, i) =>
        `chunk-${String(i).padStart(6, '0')}${'z'.repeat(40)}`
      );
      chunks.forEach(chunk => writer.append(1, chunk));

      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      const written = writeFn.mock.calls[0][0] as string;
      expect(written).toBe(chunks.slice(chunks.length - MAX_PENDING_CHUNKS).join(''));
    });

    it('still enforces the byte cap for large chunks below the object cap', () => {
      writer.register(1, vi.fn());
      const chunk = 'L'.repeat(64 * 1024);
      const chunks = Math.ceil(MAX_PENDING_BYTES / chunk.length) + 10;
      for (let i = 0; i < chunks; i++) writer.append(1, chunk);

      expect(writer.pendingBytes(1)).toBeLessThanOrEqual(MAX_PENDING_BYTES);
      expect(writer.queuedChunks(1)).toBeLessThan(MAX_PENDING_CHUNKS);
    });

    it('never drops the only pending chunk, even when it breaches both caps', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      const oversized = 'x'.repeat(MAX_PENDING_BYTES + 100);
      writer.append(1, oversized);

      expect(writer.queuedChunks(1)).toBe(1);
      flush();
      expect(writeFn).toHaveBeenCalledWith(oversized);
    });

    it('excludes evicted chunks from pending byte accounting', () => {
      writer.register(1, vi.fn());
      const chunk = rafChunk();
      for (let i = 0; i < MAX_PENDING_CHUNKS + 500; i++) writer.append(1, chunk);

      // Accounting describes the live window only: if a dropped chunk still
      // counted, this would exceed the cap by the evicted bytes.
      expect(writer.pendingBytes(1)).toBe(writer.queuedChunks(1) * chunk.length);
      expect(writer.pendingBytes(1)).toBeLessThanOrEqual(MAX_PENDING_BYTES);
    });

    it('does not accumulate state across repeated overflow cycles', () => {
      writer.register(1, vi.fn());
      const chunk = rafChunk();
      for (let cycle = 0; cycle < 5; cycle++) {
        for (let i = 0; i < MAX_PENDING_CHUNKS; i++) writer.append(1, chunk);
        flush();
        expect(writer.queuedChunks(1)).toBe(0);
        expect(writer.pendingBytes(1)).toBe(0);
      }
      // One live node, one pending-cap refill each cycle — a leak here would
      // show up as a growing entry count or a queue that never drains.
      expect(writer.has(1)).toBe(true);
      expect(writer.queuedChunks(1)).toBe(0);
    });
  });

  describe('ordering and framing across eviction', () => {
    it('preserves order, split UTF-8 and ANSI frames in the surviving window', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      // Filler large enough to push the interesting tail past the object cap
      // once, so the mixed chunks below survive *after* eviction.
      for (let i = 0; i < MAX_PENDING_CHUNKS; i++) writer.append(1, rafChunk('f'));
      expect(writer.queuedChunks(1)).toBe(MAX_PENDING_CHUNKS);

      // Mixed string/byte tail: an ANSI SGR opener straddling the
      // string/bytes boundary, with a split ▀ (U+2580) across byte chunks.
      writer.append(1, 'tail\x1b[3');
      writer.append(1, new TextEncoder().encode('1m▀'));
      writer.append(1, ' world' + 'w'.repeat(INTERACTIVE_FAST_PATH_BYTES));

      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      const written = writeFn.mock.calls[0][0];
      expect(typeof written).toBe('string');
      // `.includes` rather than `toContain`: a mismatch must not print the
      // whole ~256 KiB surviving window.
      expect(written.includes('tail\x1b[31m▀ world')).toBe(true);
      // The tail is still the newest data: nothing after it was dropped.
      expect(written.endsWith('1m▀ world' + 'w'.repeat(INTERACTIVE_FAST_PATH_BYTES))).toBe(true);
      expect(writer.queuedChunks(1)).toBe(0);
    });

    it('keeps byte chunks as bytes after object-cap eviction', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      for (let i = 0; i < MAX_PENDING_CHUNKS; i++) writer.append(1, new Uint8Array(64).fill(102));
      // One window's worth of later chunks evicts the earlier window whole.
      for (let i = 0; i < MAX_PENDING_CHUNKS; i++) writer.append(1, new Uint8Array(64).fill(103));
      expect(writer.queuedChunks(1)).toBe(MAX_PENDING_CHUNKS);

      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      const written = writeFn.mock.calls[0][0];
      expect(written).toBeInstanceOf(Uint8Array);
      expect((written as Uint8Array).byteLength).toBe(MAX_PENDING_CHUNKS * 64);
      // Every byte is from the surviving window: the evicted one is absent.
      expect(written.every((byte: number) => byte === 103)).toBe(true);
    });

    it('passes a single queued chunk through without copying it', () => {
      // One chunk per frame is the common steady-state shape; merging it would
      // allocate a redundant Uint8Array and copy every byte for nothing.
      const bytes = new Uint8Array(64).fill(7);
      const byteWriter = vi.fn();
      writer.register(1, byteWriter);
      writer.append(1, bytes);
      flush();
      expect(byteWriter.mock.calls[0][0]).toBe(bytes);

      const text = 'x'.repeat(INTERACTIVE_FAST_PATH_BYTES + 1);
      const stringWriter = vi.fn();
      const stringNode = 2;
      writer.register(stringNode, stringWriter);
      writer.append(stringNode, text);
      flush();
      expect(stringWriter.mock.calls[0][0]).toBe(text);
    });
  });

  describe('xterm in-flight budget', () => {
    it('hands the sink a parse-completion callback and counts it', () => {
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      writer.append(1, rafChunk());

      flush();
      expect(sink.calls).toHaveLength(1);
      expect(writer.inFlightWrites(1)).toBe(1);
      sink.completeAll();
      expect(writer.inFlightWrites(1)).toBe(0);
    });

    it('stops handing over payloads while the budget is exhausted', () => {
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      const block = 'b'.repeat(4096);

      // One flush per frame, never completing: this is a parser that lags
      // the producer. Pre-#2018 every one of these reached xterm.
      for (let frame = 0; frame < MAX_INFLIGHT_WRITES + 10; frame++) {
        writer.append(1, block);
        flush();
      }

      expect(sink.calls).toHaveLength(MAX_INFLIGHT_WRITES);
      expect(writer.inFlightWrites(1)).toBe(MAX_INFLIGHT_WRITES);
      // The withheld output is retained, not discarded.
      expect(writer.queuedChunks(1)).toBeGreaterThan(0);
    });

    it('resumes flushing as soon as xterm reports completion', () => {
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      const block = 'b'.repeat(4096);
      for (let frame = 0; frame < MAX_INFLIGHT_WRITES + 3; frame++) {
        writer.append(1, block);
        flush();
      }
      expect(sink.calls).toHaveLength(MAX_INFLIGHT_WRITES);

      sink.completeAll();
      flush();
      expect(sink.calls.length).toBeGreaterThan(MAX_INFLIGHT_WRITES);
    });

    it('drains the whole backlog once the parser keeps up', () => {
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      const block = 'b'.repeat(4096);
      const frames = 50;
      for (let frame = 0; frame < frames; frame++) {
        writer.append(1, block);
        flush();
        // A parser keeping pace: completion each frame.
        sink.completeAll();
      }
      flush();
      expect(sink.calls).toHaveLength(frames);
      expect(writer.queuedChunks(1)).toBe(0);
      expect(writer.inFlightWrites(1)).toBe(0);
    });

    it('keeps the interactive fast path exempt from the budget', () => {
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      const block = 'b'.repeat(4096);
      for (let frame = 0; frame < MAX_INFLIGHT_WRITES; frame++) {
        writer.append(1, block);
        flush();
      }
      expect(writer.inFlightWrites(1)).toBe(MAX_INFLIGHT_WRITES);
      expect(writer.queuedChunks(1)).toBe(0);

      // A keystroke echo must land even though the parser is saturated.
      const callsBefore = sink.calls.length;
      writer.append(1, 'a');
      expect(sink.calls).toHaveLength(callsBefore + 1);
      expect(sink.calls[sink.calls.length - 1]).toBe('a');
      // ...and must not consume a bulk budget slot either: the cap exists to
      // bound backlog, and four typed keys must not defer agent output.
      expect(writer.inFlightWrites(1)).toBe(MAX_INFLIGHT_WRITES);
    });

    it('does not re-arm a frame while the parser is saturated', () => {
      // The backoff is event-driven. Re-arming here would busy-poll rAF at
      // vsync rate for the whole parse, and pinning `frameRequested` would
      // also deny the interactive fast path above.
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      const block = 'b'.repeat(4096);
      for (let frame = 0; frame < MAX_INFLIGHT_WRITES + 10; frame++) {
        writer.append(1, block);
        flush();
      }

      expect(sink.calls).toHaveLength(MAX_INFLIGHT_WRITES);
      expect(scheduledCallbacks).toHaveLength(0);
      expect(writer.queuedChunks(1)).toBeGreaterThan(0);
    });

    it('does not wake the renderer when a completion finds an empty queue', () => {
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      writer.append(1, rafChunk());
      flush();
      expect(writer.queuedChunks(1)).toBe(0);

      sink.completeAll();
      // A burst that has ended must not cost a vsync tick per completed write.
      expect(scheduledCallbacks).toHaveLength(0);
    });

    it('does not leak an in-flight slot when the sink throws synchronously', () => {
      // MAX_INFLIGHT_WRITES leaked slots and the writer is bricked for the
      // rest of the session, so a synchronous throw must undo its own count.
      let calls = 0;
      writer.register(
        1,
        (_data, done) => {
          if (calls++ === 0) throw new Error('terminal gone');
          done?.();
        },
        { completionAware: true },
      );

      writer.append(1, rafChunk());
      expect(() => flush()).toThrow('terminal gone');
      expect(writer.inFlightWrites(1)).toBe(0);

      // Still usable afterwards.
      writer.append(1, rafChunk());
      expect(() => flush()).not.toThrow();
      expect(writer.inFlightWrites(1)).toBe(0);
    });

    it('ignores a duplicate parse completion instead of underflowing', () => {
      let done: (() => void) | undefined;
      writer.register(
        1,
        (_data, d) => {
          done = d;
        },
        { completionAware: true },
      );
      writer.append(1, rafChunk());
      flush();
      expect(writer.inFlightWrites(1)).toBe(1);

      done?.();
      done?.();
      done?.();
      expect(writer.inFlightWrites(1)).toBe(0);
    });

    it('never defers a sink that does not report completion', () => {
      // Completion tracking is opt-in: a sink that ignores `done` must not
      // be able to stall the queue.
      const calls: TerminalWriteData[] = [];
      writer.register(1, (data) => calls.push(data));
      const block = 'b'.repeat(4096);
      const frames = MAX_INFLIGHT_WRITES + 5;
      for (let frame = 0; frame < frames; frame++) {
        writer.append(1, block);
        flush();
      }
      expect(calls).toHaveLength(frames);
      expect(writer.inFlightWrites(1)).toBe(0);
    });

    it('keeps holding bounded output when the parser never completes', () => {
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      const chunk = rafChunk();
      for (let i = 0; i < MAX_PENDING_CHUNKS * 2; i++) {
        writer.append(1, chunk);
        flush();
      }

      // No completion ever arrives: the writer must degrade to the
      // established drop-oldest policy, not grow.
      expect(sink.calls.length).toBeLessThanOrEqual(MAX_INFLIGHT_WRITES);
      expect(writer.queuedChunks(1)).toBeLessThanOrEqual(MAX_PENDING_CHUNKS);
      expect(writer.pendingBytes(1)).toBeLessThanOrEqual(MAX_PENDING_BYTES);
    });
  });

  describe('hidden window and restore', () => {
    it('bounds a paused-flush backlog and hands over the newest window', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      // Nothing calls flush(): the scheduler never fires, as when Chromium
      // suspends rAF for a hidden or minimized window.
      const line = (i: number) => `line-${String(i).padStart(6, '0')}${'x'.repeat(50)}\n`;
      const total = MAX_PENDING_CHUNKS * 3;
      for (let i = 0; i < total; i++) writer.append(1, line(i));

      expect(writer.queuedChunks(1)).toBe(MAX_PENDING_CHUNKS);
      expect(writer.pendingBytes(1)).toBeLessThanOrEqual(MAX_PENDING_BYTES);
      // One frame requested for the whole hidden window, not one per chunk.
      expect(scheduledCallbacks).toHaveLength(1);

      // Restore.
      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      const written = writeFn.mock.calls[0][0] as string;
      expect(written.endsWith(line(total - 1))).toBe(true);
      expect(writer.queuedChunks(1)).toBe(0);
      expect(writer.pendingBytes(1)).toBe(0);
    });

    it('survives repeated hide and restore cycles for a live hidden agent', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      for (let cycle = 0; cycle < 4; cycle++) {
        for (let i = 0; i < 200; i++) writer.append(1, rafChunk(`c${cycle}`));
        flush();
      }
      // Every cycle's output was delivered; nothing accumulated in between.
      expect(writeFn).toHaveBeenCalledTimes(4);
      expect(writer.queuedChunks(1)).toBe(0);
      expect(writer.pendingBytes(1)).toBe(0);
      expect(writer.has(1)).toBe(true);
    });

    it('hands nothing to xterm while the flush scheduler stays paused', () => {
      // The defining property of a hidden window: rAF never fires, so the
      // backlog is bounded inside the writer instead of being written
      // eagerly. Nothing here may dispose the entry either — a hidden agent's
      // terminal is persistent and must survive to the restore.
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      for (let i = 0; i < MAX_PENDING_CHUNKS * 2; i++) writer.append(1, rafChunk());

      expect(writeFn).not.toHaveBeenCalled();
      expect(writer.has(1)).toBe(true);
      expect(writer.queuedChunks(1)).toBe(MAX_PENDING_CHUNKS);

      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      expect(writer.has(1)).toBe(true);
    });
  });

  describe('deletion', () => {
    it('releases the queued backlog when the node is unregistered', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);
      for (let i = 0; i < 100; i++) writer.append(1, rafChunk());
      expect(writer.queuedChunks(1)).toBe(100);

      writer.unregister(1);
      expect(writer.pendingBytes(1)).toBe(0);
      expect(writer.queuedChunks(1)).toBe(0);

      flush();
      expect(writeFn).not.toHaveBeenCalled();
    });

    it('ignores a parse completion that arrives after deletion', () => {
      const sink = completionSink();
      writer.register(1, sink.fn, { completionAware: true });
      writer.append(1, rafChunk());
      flush();
      expect(writer.inFlightWrites(1)).toBe(1);

      writer.unregister(1);
      expect(() => sink.completeAll()).not.toThrow();
      expect(writer.has(1)).toBe(false);

      flush();
      expect(sink.calls).toHaveLength(1);
    });

    it('does not carry backlog across a delete and re-register', () => {
      const first = vi.fn();
      writer.register(1, first);
      for (let i = 0; i < MAX_PENDING_CHUNKS; i++) writer.append(1, rafChunk());
      writer.unregister(1);

      const second = vi.fn();
      writer.register(1, second);
      expect(writer.queuedChunks(1)).toBe(0);
      expect(writer.pendingBytes(1)).toBe(0);
      expect(writer.inFlightWrites(1)).toBe(0);

      flush();
      expect(first).not.toHaveBeenCalled();
      expect(second).not.toHaveBeenCalled();
    });
  });
});