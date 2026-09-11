import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  getMuseSessionTelemetry,
  MUSE_SESSION_TELEMETRY_EVENT,
  type ObservedMuseSessionTelemetry,
} from '../lib/tauri';

/**
 * Observed Muse session telemetry for one Agent Node (issue #1680).
 *
 * Fetches on mount when the node is a Muse harness, then stays current via
 * the `muse-session-telemetry` event. Non-Muse nodes skip IPC entirely so
 * the Usage Meter path and other harnesses never see this payload.
 */
export function useMuseSessionTelemetry(
  nodeId: number,
  provider: string,
): ObservedMuseSessionTelemetry | null {
  const [telemetry, setTelemetry] = useState<ObservedMuseSessionTelemetry | null>(null);
  const isMuse = provider === 'muse';

  const refresh = useCallback(() => {
    if (!isMuse) {
      setTelemetry(null);
      return;
    }
    void getMuseSessionTelemetry(nodeId)
      .then((next) => setTelemetry(next ?? null))
      .catch(() => setTelemetry(null));
  }, [isMuse, nodeId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (!isMuse) return;
    const unlisten = listen<ObservedMuseSessionTelemetry>(
      MUSE_SESSION_TELEMETRY_EVENT,
      (event) => {
        if (event.payload.node_id === nodeId) setTelemetry(event.payload);
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [isMuse, nodeId]);

  return isMuse ? telemetry : null;
}
