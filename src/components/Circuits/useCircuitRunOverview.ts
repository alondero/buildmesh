import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { circuitRunOverview } from '../../lib/tauri/circuitEvidence';
import { formatError } from '../../lib/errorUtils';
import type { CircuitRunOverview } from '../../types/generated/CircuitRunOverview';

/** A backend answer we can draw, or `null` (a mock or a skewed build). */
function usable(value: unknown): CircuitRunOverview | null {
  const candidate = value as Partial<CircuitRunOverview> | null;
  return candidate !== null
    && typeof candidate === 'object'
    && typeof candidate.run?.state === 'string'
    && Array.isArray(candidate.steps)
    && Array.isArray(candidate.graph?.nodes)
    && Array.isArray(candidate.attention?.pushes)
    ? candidate as CircuitRunOverview
    : null;
}

export interface RunOverviewState {
  overview: CircuitRunOverview | null;
  error: string | null;
  refresh: () => void;
}

/**
 * One Circuit Run's overview, kept current by `circuit-run-updated` for that
 * run. Each read takes a sequence number and only the newest may commit, so
 * an older read resolving late (or after the card switched runs or unmounted)
 * never overwrites newer state. A failed refresh keeps the last good overview
 * and reports the error beside it.
 */
export function useCircuitRunOverview(runId: number | null): RunOverviewState {
  const [overview, setOverview] = useState<CircuitRunOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const sequence = useRef(0);
  const refresh = useCallback(() => setTick((value) => value + 1), []);

  useEffect(() => {
    if (runId === null) return;
    let current = true;
    const subscription = listen<unknown>('circuit-run-updated', ({ payload }) => {
      if (current && typeof payload === 'object' && payload !== null && 'run_id' in payload && payload.run_id === runId) {
        setTick((value) => value + 1);
      }
    });
    void subscription.catch(() => {});
    return () => {
      current = false;
      void subscription.then((unlisten) => unlisten(), () => {});
    };
  }, [runId]);

  // A different run is a different subject: drop the old one at once rather
  // than showing its steps under the new run's tab.
  const [shownRunId, setShownRunId] = useState(runId);
  if (shownRunId !== runId) {
    setShownRunId(runId);
    setOverview(null);
    setError(null);
  }

  useEffect(() => {
    const request = ++sequence.current;
    if (runId === null) return;
    circuitRunOverview(runId).then(
      (next) => {
        if (sequence.current !== request) return;
        const checked = usable(next);
        if (checked === null) {
          setError('The app did not return a circuit run overview. Restart Buildmesh if this persists.');
          return;
        }
        setOverview(checked);
        setError(null);
      },
      (cause: unknown) => {
        if (sequence.current !== request) return;
        setError(formatError(cause));
      },
    );
    return () => {
      // Unmount or a newer read: whatever this one returns is stale.
      if (sequence.current === request) sequence.current += 1;
    };
  }, [runId, tick]);

  return { overview, error, refresh };
}
