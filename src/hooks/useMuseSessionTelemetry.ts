import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  getMuseSessionTelemetry,
  MUSE_SESSION_TELEMETRY_EVENT,
  type ObservedMuseSessionTelemetry,
} from '../lib/tauri';

type TelemetrySlot = {
  nodeId: number;
  data: ObservedMuseSessionTelemetry | null;
};

/**
 * Observed Muse session telemetry for one Agent Node (issue #1680).
 *
 * Fetches on mount when the node is a Muse harness, then stays current via
 * the `muse-session-telemetry` event. Non-Muse nodes skip IPC entirely so
 * the Usage Meter path and other harnesses never see this payload.
 *
 * Each in-flight fetch and listener is owned by the `(nodeId, isMuse)` pair
 * that launched it. A later node switch, unmount, or rejection cannot commit
 * into a different owner; the visible value is also keyed, so a previous
 * node's tokens never remain on screen while the next fetch is in flight.
 */
export function useMuseSessionTelemetry(
  nodeId: number,
  provider: string,
): ObservedMuseSessionTelemetry | null {
  const [slot, setSlot] = useState<TelemetrySlot | null>(null);
  const isMuse = provider === 'muse';

  useEffect(() => {
    if (!isMuse) return;
    let cancelled = false;
    void getMuseSessionTelemetry(nodeId)
      .then((next) => {
        if (cancelled) return;
        setSlot({ nodeId, data: next ?? null });
      })
      .catch(() => {
        if (cancelled) return;
        setSlot({ nodeId, data: null });
      });
    return () => {
      cancelled = true;
    };
  }, [isMuse, nodeId]);

  useEffect(() => {
    if (!isMuse) return;
    let cancelled = false;
    const unlisten = listen<ObservedMuseSessionTelemetry>(
      MUSE_SESSION_TELEMETRY_EVENT,
      (event) => {
        if (cancelled) return;
        if (event.payload.node_id !== nodeId) return;
        setSlot({ nodeId, data: event.payload });
      },
    );
    return () => {
      cancelled = true;
      unlisten.then((fn) => fn());
    };
  }, [isMuse, nodeId]);

  if (!isMuse) return null;
  return slot?.nodeId === nodeId ? slot.data : null;
}
