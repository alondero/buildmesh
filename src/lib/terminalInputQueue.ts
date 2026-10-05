/**
 * The one ordered retry buffer for PTY input, owned at the transport seam
 * (issue #1530).
 *
 * Before #1530 the backend dropped the user's bytes whenever its bounded writer
 * queue was full and *still answered success*, so a prompt could vanish with
 * the UI reporting it delivered. The backend now answers with a typed
 * disposition; this module is the single place that reacts to it, so no caller
 * invents its own retry behaviour and none of them can accidentally retry in a
 * different order.
 *
 * Three properties are load-bearing and each is covered by a unit test:
 *
 * 1. **Order.** One lane per session, one write in flight, strict FIFO. Keystrokes
 *    and pastes reach the PTY in the order the user produced them.
 * 2. **No duplicate.** A buffer leaves the queue only on `accepted`. A
 *    `backpressured` buffer was definitively *not* queued (the backend's decoder
 *    does not advance on a refusal), so re-sending those identical bytes cannot
 *    double-write — that is what makes an automatic retry safe rather than a
 *    correctness hazard.
 * 3. **Whole-paste fidelity.** A buffer is handed to the transport as the one
 *    string the caller produced, so a 17.5 KB bracketed paste stays a single
 *    logical write (issue #1498). Nothing here splits, paces, or re-chunks.
 *
 * The stall signal is deliberately *slow to appear*: `enqueue` does not report
 * a stall for a blip that the writer thread drains in a few milliseconds,
 * because a transient transport hiccup is not information the user can act on.
 * Only bytes held past `stallThresholdMs` are surfaced, and they clear
 * themselves the moment the queue drains.
 */

import type { InputOutcome } from '../types/generated/InputOutcome';

/** A session whose input is being held rather than delivered. */
export interface InputStall {
  nodeId: number;
  /** Exact bytes still awaiting delivery — the user can see what is pending. */
  pendingBytes: number;
  /** How many delivery attempts have been refused so far. */
  attempts: number;
}

export interface TerminalInputQueueOptions {
  /** The transport. Rejects only on a hard IPC failure, never on backpressure. */
  write: (nodeId: number, data: string) => Promise<InputOutcome>;
  /**
   * How long bytes may be held before the stall is worth showing. The writer
   * thread drains continuously, so an ordinary burst resolves in well under a
   * second; crossing this threshold means the agent has genuinely stopped
   * reading its input.
   */
  stallThresholdMs?: number;
  /** Total delivery attempts per buffer before it is abandoned. */
  maxAttempts?: number;
  baseDelayMs?: number;
  maxDelayMs?: number;
  /** Called with the stall to show, or `null` when the session is clear. */
  onStallChange?: (stall: InputStall | null) => void;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

/** A queued buffer's caller-facing promise. */
interface Waiter {
  resolve: (outcome: InputOutcome) => void;
  reject: (error: unknown) => void;
}

interface Lane {
  /** FIFO of buffers not yet accepted. The head is the one being retried. */
  queue: string[];
  /** Resolvers for each queued buffer, index-aligned with `queue`. */
  waiters: Waiter[];
  /** A write is in flight, or a retry timer is armed. */
  busy: boolean;
  attempts: number;
  /**
   * When the head buffer first failed, or `null` if it has not failed yet.
   *
   * Explicitly nullable rather than `0`-as-sentinel: a clock reading zero at
   * the moment of the first refusal would otherwise look identical to "never
   * held", and the stall would never be reported.
   */
  heldSinceMs: number | null;
  stallReported: boolean;
}

const DEFAULT_STALL_THRESHOLD_MS = 1_200;
const DEFAULT_MAX_ATTEMPTS = 40;
const DEFAULT_BASE_DELAY_MS = 50;
const DEFAULT_MAX_DELAY_MS = 1_000;

const ACCEPTED: InputOutcome = { disposition: 'accepted', activity: { user_input: false, submitted: false } };

/**
 * Is this a disposition the backend contract actually produces?
 *
 * `InputOutcome` is generated from Rust, so a well-formed reply always has one
 * of the three known dispositions. Anything else means the transport answered
 * with a shape we cannot interpret — and reporting that as a delivery would
 * recreate the exact bug this module exists to close, in a subtler form.
 */
function isOutcome(value: unknown): value is InputOutcome {
  const disposition = (value as InputOutcome | undefined)?.disposition;
  return disposition === 'accepted' || disposition === 'backpressured' || disposition === 'closed';
}

export class TerminalInputQueue {
  private readonly lanes = new Map<number, Lane>();
  private readonly write: TerminalInputQueueOptions['write'];
  private readonly stallThresholdMs: number;
  private readonly maxAttempts: number;
  private readonly baseDelayMs: number;
  private readonly maxDelayMs: number;
  private readonly now: () => number;
  private readonly sleep: (ms: number) => Promise<void>;
  /**
   * Every stall observer, constructor-supplied and subscribed alike.
   *
   * One set, one fan-out: an earlier shape kept `onStallChange` as its own
   * field *and* added it here, so every report was delivered twice.
   */
  private readonly listeners = new Set<(stall: InputStall | null) => void>();

  constructor(options: TerminalInputQueueOptions) {
    this.write = options.write;
    this.stallThresholdMs = options.stallThresholdMs ?? DEFAULT_STALL_THRESHOLD_MS;
    this.maxAttempts = options.maxAttempts ?? DEFAULT_MAX_ATTEMPTS;
    this.baseDelayMs = options.baseDelayMs ?? DEFAULT_BASE_DELAY_MS;
    this.maxDelayMs = options.maxDelayMs ?? DEFAULT_MAX_DELAY_MS;
    this.now = options.now ?? (() => Date.now());
    this.sleep = options.sleep ?? ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
    if (options.onStallChange) this.listeners.add(options.onStallChange);
  }

  /**
   * Watch stalled input. The transport owns the queue, so the UI subscribes
   * here rather than the transport reaching into a store — the dependency
   * arrow stays store → transport.
   */
  subscribeStall(listener: (stall: InputStall | null) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /**
   * Hand `data` to the session's PTY, preserving order behind anything already
   * queued.
   *
   * Resolves with the disposition that ultimately applied: `accepted` once the
   * bytes are genuinely queued — however many refusals that took — or `closed`
   * if the session's writer is gone or the retry budget is exhausted.
   * *Rejects* when the transport itself failed (the IPC call rejected), so a
   * caller that needs to distinguish "could not even try" from "tried and was
   * refused" still can. A fire-and-forget caller (`.catch(console.error)`) is
   * unaffected; an awaiting caller learns the truth about *its own* buffer.
   */
  enqueue(nodeId: number, data: string): Promise<InputOutcome> {
    const lane = this.laneFor(nodeId);
    return new Promise<InputOutcome>((resolve, reject) => {
      lane.queue.push(data);
      lane.waiters.push({ resolve, reject });
      void this.drain(nodeId, lane);
    });
  }

  /** Drop everything pending for a session (teardown, node closed). */
  cancel(nodeId: number): void {
    const lane = this.lanes.get(nodeId);
    if (!lane) return;
    // Resolving rather than rejecting: a torn-down session is a normal end, and
    // an unhandled rejection here would be noise on a path that is already
    // closing.
    for (const waiter of lane.waiters) waiter.resolve({ ...ACCEPTED, disposition: 'closed' });
    lane.queue.length = 0;
    lane.waiters.length = 0;
    this.clearStall(lane);
    this.lanes.delete(nodeId);
  }

  /** Test/introspection seam: what is currently held for a session. */
  pendingFor(nodeId: number): InputStall | null {
    const lane = this.lanes.get(nodeId);
    if (!lane) return null;
    return {
      nodeId,
      pendingBytes: lane.queue.reduce((total, buffer) => total + buffer.length, 0),
      attempts: lane.attempts,
    };
  }

  private laneFor(nodeId: number): Lane {
    let lane = this.lanes.get(nodeId);
    if (!lane) {
      lane = { queue: [], waiters: [], busy: false, attempts: 0, heldSinceMs: null, stallReported: false };
      this.lanes.set(nodeId, lane);
    }
    return lane;
  }

  private async drain(nodeId: number, lane: Lane): Promise<void> {
    // One writer per session: this is what makes FIFO order and the
    // no-duplicate guarantee hold under concurrent keystrokes.
    if (lane.busy) return;
    const buffer = lane.queue[0];
    if (buffer === undefined) {
      this.retireIfEmpty(nodeId, lane);
      return;
    }

    lane.busy = true;
    let outcome: InputOutcome;
    try {
      const raw = await this.write(nodeId, buffer);
      if (!isOutcome(raw)) {
        // A reply we cannot interpret. Resolve as `closed` rather than
        // throwing: we certainly did not deliver, retrying is pointless, and a
        // fire-and-forget keystroke caller has no rejection handler — throwing
        // here would turn a protocol oddity into an unhandled rejection on a
        // hot path. The warning keeps it diagnosable without claiming success.
        console.warn(
          `terminal input: write_to_agent returned an unrecognised response for node ${nodeId}; ` +
            'treating the write as not delivered',
        );
        outcome = { ...ACCEPTED, disposition: 'closed' };
      } else {
        outcome = raw;
      }
    } catch (error) {
      // A hard transport failure (the IPC call itself rejected) is *not* a
      // backpressure signal and retrying it would only repeat the same error.
      // It is re-thrown to this buffer's caller rather than folded into
      // `closed`, so the existing contract holds: `api.writeToAgent` rejects
      // when the write could not be attempted at all (e.g. `pasteClipboard`
      // relies on that to avoid silently falling back to fragmented text).
      // `InputDisposition::Closed` is reserved for the backend's own verdict
      // that the writer thread is gone.
      lane.busy = false;
      lane.attempts = 0;
      lane.queue.shift();
      const waiter = lane.waiters.shift();
      this.retireIfEmpty(nodeId, lane);
      this.continueDrain(nodeId, lane);
      waiter?.reject(error);
      return;
    }

    lane.busy = false;
    if (outcome.disposition === 'accepted') {
      lane.attempts = 0;
      this.settleHead(lane, outcome);
      this.retireIfEmpty(nodeId, lane);
      this.continueDrain(nodeId, lane);
      return;
    }

    // `closed` is terminal — the writer thread is gone or the session was
    // retired, so this buffer can never be delivered.
    if (outcome.disposition === 'closed') {
      lane.attempts = 0;
      this.settleHead(lane, outcome);
      this.retireIfEmpty(nodeId, lane);
      this.continueDrain(nodeId, lane);
      return;
    }

    // Backpressured. The buffer stays at the head, so the next attempt re-sends
    // the identical bytes in the identical position — never a duplicate, never
    // out of order.
    lane.attempts += 1;
    if (lane.heldSinceMs === null) lane.heldSinceMs = this.now();
    this.reportStallIfHeld(nodeId, lane);
    if (lane.attempts >= this.maxAttempts) {
      // Give up loudly: settle the buffer as `closed` so the caller can tell
      // the user their input was lost, rather than dropping it silently.
      lane.attempts = 0;
      this.settleHead(lane, { ...ACCEPTED, disposition: 'closed' });
      this.retireIfEmpty(nodeId, lane);
      this.continueDrain(nodeId, lane);
      return;
    }

    await this.sleep(this.backoff(lane.attempts));
    void this.drain(nodeId, lane);
  }

  /** Keep the lane moving after a head buffer was settled. */
  private continueDrain(nodeId: number, lane: Lane): void {
    if (lane.queue.length > 0) void this.drain(nodeId, lane);
  }

  /** Remove the head buffer and settle its waiter as accepted/closed. */
  private settleHead(lane: Lane, outcome: InputOutcome): void {
    lane.queue.shift();
    lane.waiters.shift()?.resolve(outcome);
  }

  /**
   * Withdraw the stall and forget the lane once nothing is left holding.
   *
   * Called synchronously right after `settleHead` rather than from the next
   * `drain()` call. Resolving a waiter and clearing the badge must land in the
   * same synchronous block: a caller that `await`s the write would otherwise
   * resume on a microtask *before* the badge was withdrawn, so the UI could
   * report the input as delivered while still showing it as stalled.
   */
  private retireIfEmpty(nodeId: number, lane: Lane): void {
    if (lane.queue.length > 0) return;
    this.clearStall(lane);
    this.lanes.delete(nodeId);
  }

  private backoff(attempt: number): number {
    // Exponential with a ceiling, so a long stall does not turn into a
    // tight polling loop against a wedged process.
    return Math.min(this.baseDelayMs * 2 ** (attempt - 1), this.maxDelayMs);
  }

  private reportStallIfHeld(nodeId: number, lane: Lane): void {
    if (lane.stallReported || lane.heldSinceMs === null) return;
    if (this.now() - lane.heldSinceMs < this.stallThresholdMs) return;
    lane.stallReported = true;
    this.notifyStall({
      nodeId,
      pendingBytes: lane.queue.reduce((total, buffer) => total + buffer.length, 0),
      attempts: lane.attempts,
    });
  }

  private clearStall(lane: Lane): void {
    lane.heldSinceMs = null;
    if (!lane.stallReported) return;
    lane.stallReported = false;
    this.notifyStall(null);
  }

  private notifyStall(stall: InputStall | null): void {
    for (const listener of this.listeners) listener(stall);
  }
}
