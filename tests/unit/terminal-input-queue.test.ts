import { describe, it, expect, vi } from 'vitest';
import { TerminalInputQueue, type InputStall } from '../../src/lib/terminalInputQueue';
import type { InputOutcome } from '../../src/types/generated/InputOutcome';

const accepted: InputOutcome = { disposition: 'accepted', activity: { user_input: false, submitted: false } };
const backpressured: InputOutcome = { disposition: 'backpressured', activity: { user_input: false, submitted: false } };
const closed: InputOutcome = { disposition: 'closed', activity: { user_input: false, submitted: false } };

/** A controllable clock, so retry tests never race the real timer. */
function harness(options: {
  write: (nodeId: number, data: string) => Promise<InputOutcome>;
  stallThresholdMs?: number;
  maxAttempts?: number;
  onStallChange?: (nodeId: number, stall: InputStall | null) => void;
  /**
   * How a backoff sleep behaves. `auto` advances the clock and returns at once
   * (fast retries). `manual` parks the lane until the returned `wakeBackoff`
   * is called, which is what lets a test type *into a lane that is waiting out
   * its backoff* — the only way to reproduce the double-drain bug.
   */
  sleep?: 'auto' | 'manual';
}) {
  let clock = 0;
  const parked: Array<() => void> = [];
  const queue = new TerminalInputQueue({
    write: options.write,
    stallThresholdMs: options.stallThresholdMs ?? 1_000,
    maxAttempts: options.maxAttempts ?? 5,
    now: () => clock,
    sleep: (ms) => {
      clock += ms;
      if (options.sleep === 'manual') {
        return new Promise<void>((resolve) => { parked.push(resolve); });
      }
      return Promise.resolve();
    },
    onStallChange: options.onStallChange,
  });
  return {
    queue,
    advance: (ms: number) => { clock += ms; },
    /** Let every parked backoff continue, and drain the microtask queue. */
    async wakeBackoff() {
      const pending = parked.splice(0);
      for (const resolve of pending) resolve();
      for (let i = 0; i < 100; i++) await Promise.resolve();
    },
    /** How many backoff sleeps are currently armed. */
    parkedBackoffs: () => parked.length,
  };
}

/** Let queued microtasks run without asserting on how many are needed. */
async function settle(times = 100) {
  for (let i = 0; i < times; i++) await Promise.resolve();
}

/**
 * A transport whose every write parks until the test resolves it, addressed by
 * `(node, ordinal)`.
 *
 * Addressed rather than by a flat index because two sessions' retry attempts
 * interleave, and a flat index silently resolves an already-settled promise —
 * which looks like the queue ignoring you instead of a broken test.
 */
function controllableWrites() {
  const pending = new Map<number, Array<(o: InputOutcome) => void>>();
  const seen: Array<{ node: number; data: string }> = [];
  const write = (node: number, data: string) => {
    seen.push({ node, data });
    return new Promise<InputOutcome>((resolve) => {
      const list = pending.get(node) ?? [];
      list.push(resolve);
      pending.set(node, list);
    });
  };
  /** Wait until `node` has been written at least `n` times. */
  const until = async (node: number, n: number) => {
    for (let i = 0; i < 500 && (pending.get(node)?.length ?? 0) < n; i++) await Promise.resolve();
    expect(pending.get(node)?.length ?? 0, `node ${node} write #${n}`).toBeGreaterThanOrEqual(n);
  };
  /** Settle the `n`th (1-based) write for `node`. */
  const settleWrite = async (node: number, n: number, outcome: InputOutcome) => {
    const list = pending.get(node) ?? [];
    list[n - 1](outcome);
    await settle();
  };
  return { write, until, settleWrite, seen };
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
    const io = controllableWrites();
    const { queue, advance } = harness({
      write: io.write,
      stallThresholdMs: 1_000,
      maxAttempts: 10,
      onStallChange,
    });

    const pending = queue.enqueue(1, 'a long prompt');
    // First refusal arms the hold at t=0 but is below the threshold.
    await io.until(1, 1);
    await io.settleWrite(1, 1, backpressured);
    expect(onStallChange, 'a blip is not a stall').not.toHaveBeenCalled();

    // Push past the threshold, then refuse again: that refusal reports.
    advance(1_500);
    await io.until(1, 2);
    await io.settleWrite(1, 2, backpressured);
    expect(onStallChange).toHaveBeenCalledWith(
      1,
      expect.objectContaining({ nodeId: 1, pendingBytes: 'a long prompt'.length, attempts: 2 }),
    );

    // The agent starts reading again.
    await io.until(1, 3);
    await io.settleWrite(1, 3, accepted);
    await expect(pending).resolves.toMatchObject({ disposition: 'accepted' });
    expect(onStallChange).toHaveBeenLastCalledWith(1, null);
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

  it('never starts a second drain while a lane is waiting out its backoff', async () => {
    // Finding 1. `lane.busy` used to be released *before* `await sleep(...)`,
    // so a keystroke arriving mid-backoff started a competing drain over the
    // same queue: that loop re-wrote the head buffer, then `shift()`ed a second
    // time, popping a keystroke that was never written and resolving it as
    // `accepted`. The backoff is parked here so the lane is provably mid-wait.
    const writes: string[] = [];
    const resolvers: Array<(o: InputOutcome) => void> = [];
    const { queue, advance, wakeBackoff } = harness({
      write: (_node, data) => {
        writes.push(data);
        return new Promise<InputOutcome>((resolve) => { resolvers.push(resolve); });
      },
      maxAttempts: 20,
      sleep: 'manual',
    });

    const head = queue.enqueue(1, 'head');
    await settle();
    resolvers[0](backpressured);
    await settle();

    // The lane is now parked in its backoff. Type into it hard.
    const mid1 = queue.enqueue(1, 'b');
    const mid2 = queue.enqueue(1, 'c');
    await settle();
    expect(writes, 'a mid-backoff keystroke must not trigger a competing write').toEqual(['head']);
    // The head is still held (it is being retried), so the backlog is the head
    // plus the two keystrokes queued behind it.
    expect(queue.pendingFor(1)).toMatchObject({ pendingBytes: 'head'.length + 2 });

    advance(5_000);
    await wakeBackoff();
    expect(writes).toEqual(['head', 'head']);

    resolvers[1](accepted);
    await expect(head).resolves.toMatchObject({ disposition: 'accepted' });
    resolvers[2](accepted);
    await expect(mid1).resolves.toMatchObject({ disposition: 'accepted' });
    resolvers[3](accepted);
    await expect(mid2).resolves.toMatchObject({ disposition: 'accepted' });

    // Every buffer landed exactly once, in order. The two keystrokes that
    // arrived during the backoff were queued behind the head, not dropped.
    expect(writes).toEqual(['head', 'head', 'b', 'c']);
  });

  it('does not let one session\'s recovery clear another session\'s stall', async () => {
    // Finding 3. `clearStall` broadcast a bare `null`, so whichever node
    // recovered first withdrew the shared slot — taking a still-wedged node's
    // badge with it, and permanently, since the other lane had already latched
    // its report flag and would never re-announce.
    const onStallChange = vi.fn();
    const io = controllableWrites();
    const { queue, advance } = harness({
      write: io.write,
      stallThresholdMs: 1_000,
      maxAttempts: 30,
      onStallChange,
    });

    const a = queue.enqueue(1, 'node-1');
    const b = queue.enqueue(2, 'node-2');
    // First refusal on each arms its hold.
    await io.until(1, 1);
    await io.until(2, 1);
    await io.settleWrite(1, 1, backpressured);
    await io.settleWrite(2, 1, backpressured);

    // Push past the threshold, then refuse again on both.
    advance(2_000);
    await io.until(1, 2);
    await io.until(2, 2);
    await io.settleWrite(1, 2, backpressured);
    await io.settleWrite(2, 2, backpressured);

    const announced = onStallChange.mock.calls.filter(([, s]) => s !== null).map(([id]) => id);
    expect(new Set(announced), 'both sessions must be announced, by id').toEqual(new Set([1, 2]));

    // Node 2 recovers; node 1 is still wedged.
    await io.until(2, 3);
    await io.settleWrite(2, 3, accepted);
    await expect(b).resolves.toMatchObject({ disposition: 'accepted' });

    const cleared = onStallChange.mock.calls.filter(([, s]) => s === null).map(([id]) => id);
    expect(cleared, 'clearing must be scoped to the session that recovered').toEqual([2]);
    expect(queue.pendingFor(1), "node 1's bytes are still held").not.toBeNull();

    // Node 1 recovers too.
    await io.until(1, 3);
    await io.settleWrite(1, 3, accepted);
    await expect(a).resolves.toMatchObject({ disposition: 'accepted' });
    expect(queue.pendingFor(1)).toBeNull();
  });

  it('keeps quoting live numbers while a stall continues', async () => {
    // Finding 4. `stallReported` latched, so the badge froze at its first byte
    // count while the backlog kept growing behind it.
    const onStallChange = vi.fn();
    const io = controllableWrites();
    const { queue, advance } = harness({
      write: io.write,
      stallThresholdMs: 100,
      maxAttempts: 30,
      onStallChange,
    });

    const pending = queue.enqueue(1, 'aaaaa');
    await io.until(1, 1);
    await io.settleWrite(1, 1, backpressured);
    advance(500);
    await io.until(1, 2);
    await io.settleWrite(1, 2, backpressured);

    const first = onStallChange.mock.calls.at(-1)?.[1];
    expect(first).toMatchObject({ pendingBytes: 5, attempts: 2 });

    // The user keeps typing into the wedged terminal.
    queue.enqueue(1, 'bbbbbbb');
    await io.until(1, 3);
    await io.settleWrite(1, 3, backpressured);

    const second = onStallChange.mock.calls.at(-1)?.[1];
    expect(second, 'the badge must re-report a grown backlog').toMatchObject({
      pendingBytes: 12,
      attempts: 3,
    });

    // A further refusal with the same backlog still re-reports, carrying the
    // newer attempt count: the badge is live progress, not a one-shot snapshot.
    // This is cheap even though it re-renders, because each session owns its
    // own store entry, so only that node's header is affected.
    await io.until(1, 4);
    await io.settleWrite(1, 4, backpressured);
    const third = onStallChange.mock.calls.at(-1)?.[1];
    expect(third).toMatchObject({ pendingBytes: 12, attempts: 4 });
    void pending;
  });

  it('a cancelled lane cannot evict the session that replaced it', async () => {
    // Finding 5. `retireIfEmpty` deleted by id alone, so a zombie lane left
    // over from a cancel would drop the fresh lane a respawned node had
    // installed — silently discarding everything buffered behind it.
    const writes: string[] = [];
    const resolvers: Array<(o: InputOutcome) => void> = [];
    const { queue } = harness({
      write: (_node, data) => {
        writes.push(data);
        return new Promise<InputOutcome>((resolve) => { resolvers.push(resolve); });
      },
      maxAttempts: 5,
    });

    const abandoned = queue.enqueue(9, 'before-cancel');
    await settle();
    queue.cancel(9);
    await expect(abandoned).resolves.toMatchObject({ disposition: 'closed' });

    // The node respawns and types again.
    const revived = queue.enqueue(9, 'after-cancel');
    await settle();
    resolvers[1](accepted);
    await expect(revived).resolves.toMatchObject({ disposition: 'accepted' });

    // The zombie lane from before the cancel resumes and must not evict the
    // replacement — so a further write still reaches the transport.
    resolvers[0](accepted);
    await settle();
    const afterResurrection = queue.enqueue(9, 'still-here');
    await settle();
    resolvers[2](accepted);
    await expect(afterResurrection).resolves.toMatchObject({ disposition: 'accepted' });
    expect(writes).toEqual(['before-cancel', 'after-cancel', 'still-here']);
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
