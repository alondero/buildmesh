import { describe, it, expect, vi, beforeEach } from 'vitest';
import { TerminalWriter, MAX_PENDING_BYTES } from '../../src/components/Terminal/TerminalWriter';

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

      writer.append(1, 'hello');
      writer.append(1, ' world');
      expect(writeFn).not.toHaveBeenCalled();
      expect(writer.pendingBytes(1)).toBe(11);

      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      expect(writeFn).toHaveBeenCalledWith('hello world');
    });

    it('buffers byte chunks without stringifying them', () => {
      const writeFn = vi.fn();
      writer.register(1, writeFn);

      writer.append(1, new Uint8Array([0xe2, 0x96]));
      writer.append(1, new Uint8Array([0x88]));

      flush();
      expect(writeFn).toHaveBeenCalledOnce();
      expect(writeFn).toHaveBeenCalledWith(new Uint8Array([0xe2, 0x96, 0x88]));
    });

    it('only schedules one frame per batch', () => {
      writer.register(1, vi.fn());
      writer.append(1, 'a');
      writer.append(1, 'b');
      writer.append(1, 'c');
      expect(scheduledCallbacks).toHaveLength(1);
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
      writer.append(1, 'data');
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
      writer.append(1, 'data');
      writer.unregister(1);
      flush();
      expect(writeFn).not.toHaveBeenCalled();
    });

    it('does not write if node is re-registered (different entry) before flush', () => {
      const writeFn1 = vi.fn();
      const writeFn2 = vi.fn();
      writer.register(1, writeFn1);
      writer.append(1, 'old data');

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
      const fakeRaf = vi.fn((cb: () => void) => { cb(); return 0; });
      vi.stubGlobal('requestAnimationFrame', fakeRaf);
      try {
        const defaultWriter = new TerminalWriter();
        const writeFn = vi.fn();
        defaultWriter.register(1, writeFn);
        expect(() => defaultWriter.append(1, 'x')).not.toThrow();
        expect(fakeRaf).toHaveBeenCalledTimes(1);
        // The fake raf invoked the callback synchronously, so the write should have flushed.
        expect(writeFn).toHaveBeenCalledWith('x');
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
      writer.append(1, 'abcde');
      flush();
      expect(writer.pendingBytes(1)).toBe(0);
      writer.append(1, 'xyz');
      expect(writer.pendingBytes(1)).toBe(3);
    });
  });

  describe('pendingBytes', () => {
    it('returns 0 for unknown node', () => {
      expect(writer.pendingBytes(999)).toBe(0);
    });

    it('returns accumulated buffer length', () => {
      writer.register(1, vi.fn());
      writer.append(1, 'abc');
      writer.append(1, 'de');
      expect(writer.pendingBytes(1)).toBe(5);
    });
  });
});
