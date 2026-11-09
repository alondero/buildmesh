/**
 * Shared subscription machinery for the frontend's live event streams
 * (issue #2531).
 *
 * A stream owner needs the same four things:
 *
 *   1. ONE backend subscription for its event, no matter how many
 *      consumers read it — the point of the refactor. Six hand-rolled
 *      `listen` calls for `circuit-run-updated` meant six payload
 *      filters to keep in step with each other.
 *   2. A typed update published to each consumer, so no consumer has to
 *      match a raw payload.
 *   3. A subscription lifecycle the consumer owns: subscribe on mount,
 *      release on unmount, re-subscribe when the scope changes.
 *   4. A clean teardown: when the last consumer leaves, the backend
 *      subscription goes with it — the bus is clean, not half-wired.
 *
 * This module owns (1), (3) and (4) so the four stream owners stay
 * small enough to read at a glance. The payload → typed update
 * conversion and the match rule stay in each owner, because those are
 * the parts that genuinely differ per stream.
 *
 * The owner passes its own `attach` function, so each stream module
 * still contains one `listen` call for its own event — the
 * grep that proves "one subscriber per stream" still works.
 */

/** A live event stream owned by one module. */
export interface StreamSubscription<T> {
  /**
   * Register a consumer. The returned function releases it; when the
   * last consumer is released the backend subscription is detached too.
   */
  subscribe(consumer: (update: T) => void): () => void;
  /**
   * Test-only: drop the consumers and detach the backend subscription
   * so the next test starts from a fresh bus.
   */
  reset(): void;
}

type BackendSubscription =
  | { kind: 'idle' }
  | { kind: 'attaching'; owner: symbol }
  | { kind: 'attached'; owner: symbol; unlisten: () => void };

/**
 * Build a stream subscription over one backend event.
 *
 * `attach` performs the backend registration and resolves with the
 * handle that detaches it. It is called at most once per live
 * subscription: when the first consumer arrives, and again after the
 * last one left (a re-subscribe on scope change re-attaches).
 */
export function createStreamSubscription<T>(
  label: string,
  attach: (notify: (update: T) => void) => Promise<() => void>,
): StreamSubscription<T> {
  const consumers = new Set<(update: T) => void>();
  let backend: BackendSubscription = { kind: 'idle' };
  const ownsRegistration = (owner: symbol) => backend.kind !== 'idle' && backend.owner === owner;

  const releaseBackend = (): void => {
    const previous = backend;
    backend = { kind: 'idle' };
    // The handle is cleared before the call, so a throwing handle still
    // leaves the subscription in a releasable state. The error is NOT
    // swallowed: the caller (the agent-node store's rollback) reports
    // it and keeps unregistering the rest.
    if (previous.kind === 'attached') previous.unlisten();
  };

  const ensureAttached = (): void => {
    if (backend.kind !== 'idle') return;
    const owner = Symbol(label);
    // Claim the slot before calling external code: attach may notify or
    // subscribe synchronously. Every callback belongs to this registration,
    // including notifications delivered before its promise resolves.
    backend = { kind: 'attaching', owner };
    const notify = (update: T): void => {
      for (const consumer of [...consumers]) {
        if (!ownsRegistration(owner)) return;
        consumer(update);
      }
    };
    const failed = (cause: unknown): void => {
      // A retired rejection must not clear a replacement's attachment slot.
      if (ownsRegistration(owner)) backend = { kind: 'idle' };
      console.warn(`[${label}] subscription failed:`, cause);
    };
    let pending: Promise<() => void>;
    try {
      pending = attach(notify);
    } catch (cause) {
      failed(cause);
      return;
    }
    void pending.then(
      (unlisten) => {
        if (!ownsRegistration(owner)) {
          // Everyone left before the backend registration resolved.
          unlisten?.();
          return;
        }
        backend = { kind: 'attached', owner, unlisten };
      },
      failed,
    ).catch(failed);
  };

  return {
    subscribe(consumer) {
      consumers.add(consumer);
      ensureAttached();
      let released = false;
      return () => {
        if (released) return;
        released = true;
        consumers.delete(consumer);
        if (consumers.size > 0) return;
        releaseBackend();
      };
    },
    reset() {
      consumers.clear();
      try {
        releaseBackend();
      } catch (cause) {
        console.warn(`[${label}] reset failed:`, cause);
      }
    },
  };
}
