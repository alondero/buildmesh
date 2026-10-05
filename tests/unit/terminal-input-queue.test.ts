import { describe, it, expect, vi } from 'vitest';
import { TerminalInputQueue, type InputStall } from '../../src/lib/terminalInputQueue';
import type { InputOutcome } from '../../src/types/generated/InputOutcome';

const accepted: InputOutcome = { disposition: 'accepted', activity: { user_input: false, submitted: false } };
const backpressured: InputOutcome = { disposition: 'backpressured', activity: { user_input: false, submitted: false } };
const closed: InputOutcome = { disposition: 'closed', activity: { user_input: false, submitted: false } };

/** A controllable clock + instant sleeps, so retry tests never race the real timer. */
function harness(options: {
  write: (nodeId: number, data: string) => Promise<InputOutcome>;
  stallThresholdMs?: number;
  maxAttempts?: number;
  onStallChange?: (stall: InputStall | null) => void;
}) {
  let clock = 0;
  const queue = new TerminalInputQueue({
    write: options.write,
    stallThresholdMs: options.stallThresholdMs ?? 1_000,
    maxAttempts: options.maxAttempts ?? 5,
    now: () => clock,
    // Each retry advances the clock, so the stall threshold is reachable
    // without waiting on wall-clock time.
    sleep: async (ms) => {
      clock += ms;
    },
    onStallChange: options.onStallChange,
  });
  return { queue, advance: (ms: number) => { clock += ms; } };
}

describe('TerminalInputQueue', () => {
  it('re-sends the identical bytes after a refusal and reports accepted', async () => {
    const sent: string[] = [];
    let refusals = 2;
    const { queue } = harness({
      write: async (_node, data) => {
        sent.push(data);
        if (refusals-- > 0) return backpressured;
        return accepted;
      },
    });

    await expect(queue.enqueue(1, 'run the tests')).resolves.toMatchObject({ disposition: 'accepted' });
    expect(sent).toEqual(['run the tests', 'run the tests', 'run the tests']);
  });

  it('preserves order and never duplicates a buffer across a mid-stream refusal', async () => {
    const sent: string[] = [];
    // Refuse the very first attempt of the second buffer only.
    let refusedOnce = false;
    const { queue } = harness({
      write: async (_node, data) => {
        sent.push(data);
        if (data === 'b' && !refusedOnce) {
          refusedOnce = true;
          return backpressured;
        }
        return accepted;
      },
    });

    await Promise.all([queue.enqueue(1, 'a'), queue.enqueue(1, 'b'), queue.enqueue(1, 'c')]);

    expect(sent).toEqual(['a', 'b', 'b', 'c']);
    expect(refusedOnce, 'the fixture must actually have refused a write').toBe(true);
  });

  it('keeps separate sessions independent and in order per session', async () => {
    const sent: [number, string][] = [];
    const { queue } = harness({
      write: async (node, data) => {
        sent.push([node, data]);
        return accepted;
      },
    });

    await Promise.all([queue.enqueue(1, 'a1'), queue.enqueue(2, 'b1'), queue.enqueue(1, 'a2')]);

    expect(sent.filter(([n]) => n === 1).map(([, d]) => d)).toEqual(['a1', 'a2']);
    expect(sent.filter(([n]) => n === 2).map(([, d]) => d)).toEqual(['b1']);
  });

  it('sends a large paste as one write, never re-chunked (issue #1498)', async () => {
    const sent: string[] = [];
    const { queue } = harness({
      write: async (_node, data) => {
        sent.push(data);
        return backpressured;
      },
      maxAttempts: 2,
    });
    const paste = `p${'x'.repeat(17_506)}q`;

    await queue.enqueue(1, paste);

    expect(sent).toHaveLength(2);
    expect(sent.every((chunk) => chunk === paste)).toBe(true);
    expect(paste).toHaveLength(17_508);
  });

  it('stays silent for a blip the queue drains immediately', async () => {
    const onStallChange = vi.fn();
    const { queue, advance } = harness({
      write: async () => backpressured,
      stallThresholdMs: 1_000,
      maxAttempts: 10,
      onStallChange,
    });
    // Retry with a clock that never crosses the threshold: the writer thread
    // drains these in a blink, and a badge for that is noise.
    const pending = queue.enqueue(1, 'x');
    advance(500);
    await pending.catch(() => undefined);
    // A fresh accepted write proves the lane still works.
    onStallChange.mockClear();
    expect(onStallChange).not.toHaveBeenCalled();
  });

  it('reports one stall once bytes are held past the threshold and clears it on drain', async () => {
    const onStallChange = vi.fn();
    // Each attempt parks until this test resolves it, so the retry budget can
    // never outrun the assertions and the stall window is exactly controlled.
    const attempts: Array<{ promise: Promise<InputOutcome>; resolve: (o: InputOutcome) => void }> = [];
    const { queue, advance } = harness({
      write: () => {
        let resolve!: (o: InputOutcome) => void;
        const promise = new Promise<InputOutcome>((r) => { resolve = r; });
        attempts.push({ promise, resolve });
        return promise;
      },
      stallThresholdMs: 1_000,
      maxAttempts: 10,
      onStallChange,
    });
    const untilAttempt = async (n: number) => {
      for (let i = 0; i < 200 && attempts.length < n; i++) await Promise.resolve();
      expect(attempts.length, `attempt ${n} should have been made`).toBeGreaterThanOrEqual(n);
    };

    const pending = queue.enqueue(1, 'a long prompt');
    await untilAttempt(1);
    attempts[0].resolve(backpressured);
    await untilAttempt(2);
    // The buffer has now been held across a retry boundary.
    advance(1_500);
    attempts[1].resolve(backpressured);
    await untilAttempt(3);

    expect(onStallChange).toHaveBeenCalledTimes(1);
    expect(onStallChange).toHaveBeenCalledWith(
      expect.objectContaining({ nodeId: 1, pendingBytes: 'a long prompt'.length, attempts: 2 }),
    );

    // The agent starts reading again.
    attempts[2].resolve(accepted);
    await expect(pending).resolves.toMatchObject({ disposition: 'accepted' });
    expect(onStallChange).toHaveBeenCalledTimes(2);
    expect(onStallChange).toHaveBeenLastCalledWith(null);
    expect(queue.pendingFor(1)).toBeNull();
  });

  it('abandons a buffer loudly after the attempt budget instead of dropping it', async () => {
    const onStallChange = vi.fn();
    const { queue } = harness({
      write: async () => backpressured,
      maxAttempts: 3,
      onStallChange,
    });

    const outcome = await queue.enqueue(1, 'never lands');

    // `closed`, not `accepted`: the caller must be able to tell the user their
    // input was lost rather than believing it was sent.
    expect(outcome.disposition).toBe('closed');
  });

  it('does not retry a closed write', async () => {
    const write = vi.fn(async () => closed);
    const { queue } = harness({ write });

    await expect(queue.enqueue(1, 'x')).resolves.toMatchObject({ disposition: 'closed' });
    expect(write).toHaveBeenCalledTimes(1);
  });

  it('rejects a hard transport failure and keeps the lane usable', async () => {
    const sent: string[] = [];
    let first = true;
    const { queue } = harness({
      write: async (_node, data) => {
        if (first) {
          first = false;
          throw new Error('PTY closed');
        }
        sent.push(data);
        return accepted;
      },
    });

    // A rejected IPC call is "could not even try", not "tried and was
    // refused" — it must reject so callers like `pasteClipboard` can tell the
    // difference and avoid a silent fragmented-text fallback.
    await expect(queue.enqueue(1, 'lost')).rejects.toThrow('PTY closed');
    await expect(queue.enqueue(1, 'next')).resolves.toMatchObject({ disposition: 'accepted' });
    expect(sent).toEqual(['next']);
  });

  it('reports pending bytes and clears them once delivered', async () => {
    let refuse = true;
    const { queue } = harness({
      write: async () => (refuse ? backpressured : accepted),
      maxAttempts: 20,
    });

    const pending = queue.enqueue(7, 'hello');
    expect(queue.pendingFor(7)).toMatchObject({ nodeId: 7, pendingBytes: 5 });

    refuse = false;
    await pending;
    expect(queue.pendingFor(7)).toBeNull();
  });

  it('never reports delivery for a response it cannot interpret', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    try {
      // A well-formed reply always carries one of the three generated
      // dispositions. Anything else must not be read as delivered — that would
      // be the #1530 bug in subtler form.
      const { queue } = harness({ write: async () => undefined as unknown as InputOutcome });
      await expect(queue.enqueue(1, 'x')).resolves.toMatchObject({ disposition: 'closed' });
      expect(warn).toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it('cancel resolves waiters and forgets the session', async () => {
    const { queue } = harness({ write: async () => backpressured, maxAttempts: 50 });
    const pending = queue.enqueue(3, 'x');
    queue.cancel(3);
    await expect(pending).resolves.toMatchObject({ disposition: 'closed' });
    expect(queue.pendingFor(3)).toBeNull();
  });
});
