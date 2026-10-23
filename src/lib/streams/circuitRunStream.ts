import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { createStreamSubscription } from './streamSubscription';
import type { CircuitRunUpdatedPayload } from '../../types/generated/CircuitEvents';

/**
 * Owner of the `circuit-run-updated` live stream (issue #2531).
 *
 * The circuit worker emits this event when a run changes state
 * (`src-tauri/src/services/circuit_worker/mod.rs`). Before the refactor
 * six modules subscribed to it and each one re-implemented the same
 * payload filter ("is this update about MY run?"), so the rule drifted
 * and each subscriber cost its own backend registration. This module is
 * the single owner: it holds the ONE `listen`, converts the raw payload
 * into a typed update, and publishes that update to each consumer.
 * Consumers ask "did run N change?" (or "did any run change?") instead
 * of matching raw payloads.
 *
 * The event name is declared here — next to the code that subscribes to
 * it — so the constant and its subscriber are changed together. The Rust
 * half emits the same string from
 * `src-tauri/src/services/circuit_worker/mod.rs`.
 */
export const CIRCUIT_RUN_UPDATED_EVENT = 'circuit-run-updated';

/** How many updates a consumer keeps. Older ones are dropped. */
const MAX_RETAINED_UPDATES = 32;

const EMPTY: readonly CircuitRunUpdate[] = [];

/**
 * The run states the worker emits. `pending` / `running` / `paused` are
 * live (the run still owns agents), `completed` / `failed` / `cancelled`
 * are terminal (the run has swept its agents). Anything else is unknown
 * and is reported as such rather than guessed at.
 */
const KNOWN_RUN_STATES = new Set([
  'pending', 'running', 'paused', 'completed', 'failed', 'cancelled',
]);
const LIVE_RUN_STATES = new Set(['pending', 'running', 'paused']);
const TERMINAL_RUN_STATES = new Set(['completed', 'failed', 'cancelled']);

/**
 * One update on the circuit-run stream.
 *
 * A well-formed payload names the run that moved; a payload that does
 * not is still an update — the worker emitted, so something in the
 * circuit subsystem changed and the surfaces that re-read the whole
 * snapshot must still refresh. `unattributed` is therefore not an error
 * case: it is "the event fired, but it did not name a run".
 */
export type CircuitRunUpdate =
  | {
      /** A run changed state. */
      readonly kind: 'transition';
      /** The run this transition belongs to. */
      readonly runId: number;
      /** The raw state string the worker emitted. */
      readonly state: string;
      /** The state is one this stream knows (see `KNOWN_RUN_STATES`). */
      readonly known: boolean;
      /** The run still owns agents. */
      readonly live: boolean;
      /** The run has finished and swept its agents. */
      readonly terminal: boolean;
    }
  | {
      /** The event fired, but its payload did not describe a run. */
      readonly kind: 'unattributed';
    };

const UNATTRIBUTED: CircuitRunUpdate = { kind: 'unattributed' };

/**
 * Convert a raw `circuit-run-updated` payload into a typed update.
 *
 * The backend sends `{ run_id, state }` (`CircuitRunUpdatedPayload`). A
 * payload that does not name a run becomes an `unattributed` update
 * rather than being dropped: the event still means "the circuit
 * subsystem moved", and the surfaces that re-read the whole snapshot
 * depend on that.
 */
export function toCircuitRunUpdate(payload: unknown): CircuitRunUpdate {
  if (typeof payload !== 'object' || payload === null) return UNATTRIBUTED;
  const candidate = payload as Partial<CircuitRunUpdatedPayload>;
  if (typeof candidate.run_id !== 'number' || !Number.isFinite(candidate.run_id)) {
    return UNATTRIBUTED;
  }
  if (typeof candidate.state !== 'string') return UNATTRIBUTED;
  return {
    kind: 'transition',
    runId: candidate.run_id,
    state: candidate.state,
    known: KNOWN_RUN_STATES.has(candidate.state),
    live: LIVE_RUN_STATES.has(candidate.state),
    terminal: TERMINAL_RUN_STATES.has(candidate.state),
  };
}

/**
 * The match rule: does this update concern `runId`?
 *
 * This is the rule the six former subscribers each re-implemented. It
 * lives here so "which run is this about?" has exactly one answer, and
 * so an update that named no run belongs to no run.
 */
export function concernsRun(update: CircuitRunUpdate, runId: number): boolean {
  return update.kind === 'transition' && update.runId === runId;
}

const subscription = createStreamSubscription<CircuitRunUpdate>(
  'circuitRunStream',
  async (notify) => {
    const unlisten = await listen<CircuitRunUpdatedPayload>(
      CIRCUIT_RUN_UPDATED_EVENT,
      ({ payload }) => {
        notify(toCircuitRunUpdate(payload));
      },
    );
    return unlisten;
  },
);

/**
 * Subscribe to every circuit run update.
 *
 * For non-React owners (the agent-node store) and for consumers that
 * care about "the circuit subsystem moved" rather than about one run.
 */
export function subscribeCircuitRunUpdates(
  listener: (update: CircuitRunUpdate) => void,
): () => void {
  return subscription.subscribe(listener);
}

/**
 * The transitions of one run, together with the run they belong to.
 *
 * The run is part of the state so a run switch can be answered without a
 * state update: the hook returns the empty array for a run it has no
 * transitions for, instead of clearing the previous run's transitions
 * from an effect. Clearing them from an effect would change the returned
 * value a second time, and a consumer whose effect depends on it would
 * fetch the new run twice.
 */
interface RunUpdatesState {
  /** The run the transitions belong to, or `null` when there are none. */
  readonly runId: number | null;
  readonly updates: readonly CircuitRunUpdate[];
}

/**
 * The transitions that concern `runId`, oldest first.
 *
 * `runId === null` means "no run in scope" and subscribes to nothing —
 * a consumer with no subject must not be woken by another run's
 * transition. The returned array is replaced (never mutated) on every
 * matching transition, so it can be used directly as an effect
 * dependency.
 */
export function useCircuitRunUpdates(runId: number | null): readonly CircuitRunUpdate[] {
  const [state, setState] = useState<RunUpdatesState>({ runId: null, updates: EMPTY });

  useEffect(() => {
    if (runId === null) return;
    return subscription.subscribe((update) => {
      if (!concernsRun(update, runId)) return;
      setState((previous) => {
        // A different run is a different subject: start its history over.
        const base = previous.runId === runId ? previous.updates : EMPTY;
        return { runId, updates: [...base, update].slice(-MAX_RETAINED_UPDATES) };
      });
    });
  }, [runId]);

  return state.runId === runId ? state.updates : EMPTY;
}

/** Test-only: drop every consumer and detach the backend subscription. */
export function __resetCircuitRunStreamForTests(): void {
  subscription.reset();
}
