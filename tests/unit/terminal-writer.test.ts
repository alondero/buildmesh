import { describe, it, expect, vi, beforeEach } from 'vitest';
import { TerminalWriter } from '../../src/components/Terminal/TerminalWriter';

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
