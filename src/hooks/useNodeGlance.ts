import { useEffect, useState } from 'react';
import type { NodeGlance } from '../types/generated/NodeGlance';
import { formatError } from '../lib/errorUtils';

export interface NodeGlanceState {
  glance: NodeGlance | null;
  error: string | null;
  /** How long this client has held `glance`, for counting elapsed times. */
  sinceReadMs: number;
}

/**
 * Read one node's glance while `enabled`, re-reading every `intervalMs` and
 * ticking elapsed times every second. Nothing is read while disabled, so a
 * glance costs nothing until someone looks at it.
 */
export function useNodeGlance(
  nodeId: number,
  enabled: boolean,
  read: (nodeId: number) => Promise<NodeGlance>,
  intervalMs = 5000,
): NodeGlanceState {
  const [result, setResult] = useState<{ nodeId: number; glance: NodeGlance | null; error: string | null; readAt: number }>(
    { nodeId, glance: null, error: null, readAt: 0 },
  );
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const glance = await read(nodeId);
        if (!disposed) setResult({ nodeId, glance, error: null, readAt: Date.now() });
      } catch (error) {
        if (!disposed) setResult(current => ({ ...current, nodeId, error: formatError(error) }));
      } finally {
        // Chained, so a slow read never overlaps the next one.
        if (!disposed) timer = setTimeout(() => void tick(), intervalMs);
      }
    };
    void tick();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [nodeId, enabled, read, intervalMs]);

  const hasGlance = enabled && result.nodeId === nodeId && result.glance !== null;
  useEffect(() => {
    if (!hasGlance) return;
    const clock = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(clock);
  }, [hasGlance]);

  const current = result.nodeId === nodeId ? result : { glance: null, error: null, readAt: 0 };
  return {
    glance: current.glance,
    error: current.error,
    sinceReadMs: current.glance ? Math.max(0, now - current.readAt) : 0,
  };
}
