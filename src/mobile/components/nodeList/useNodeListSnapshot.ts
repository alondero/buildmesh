import { useCallback, useRef, useState, useEffect, type RefObject } from 'react';
import { listMeshes, listNodes, listCircuitRuns, listProviders, isAuthError, isUnavailableError, type AgentNode, type Mesh, type NodeCircuitRun, type Provider } from '../../api';
import { lifecycleNodePatch } from '../../../lib/status';
import { useVisibilityPolling } from '../../useVisibilityPolling';
import { useWsEvents } from '../../useWsEvents';

export function useNodeListSnapshot({ mountedRef, clearSentMarker, onOffline, onReachable, onAuthFailed }: {
  mountedRef: RefObject<boolean>;
  clearSentMarker: (nodeId: number) => void;
  onOffline: () => void;
  onReachable: () => void;
  onAuthFailed: () => void;
}) {
  const [meshes, setMeshes] = useState<Mesh[] | null>(null);
  const [nodes, setNodes] = useState<AgentNode[] | null>(null);
  // Circuit Runs by implementation node id (issue #2314). Secondary data: it
  // never gates or discards the node list it decorates.
  const [circuitRuns, setCircuitRuns] = useState<Map<number, NodeCircuitRun>>(new Map());
  const [providers, setProviders] = useState<Provider[]>([]);
  /**
   * The desktop answered but could not read its own state (issue #2330).
   *
   * The list below is the last successful read, not a live one. Showing that
   * as a plain list would be a lie, and replacing it with an empty list would
   * be a worse one — so the data stays and this notice says why it may be
   * stale. Distinct from the offline overlay, which means the desktop could
   * not be reached at all.
   */
  const [staleNotice, setStaleNotice] = useState(false);
  const refreshRevision = useRef(0);

  const refresh = useCallback(
    async (isLatest: () => boolean, signal?: AbortSignal) => {
      const revision = ++refreshRevision.current;
      try {
        // A circuit read that fails (a desktop mid-restart, an older build)
        // must not take the node list down with it: `null` keeps the last
        // lines on screen instead of reporting the desktop offline.
        // `signal` is the polling hook's dispose signal (issue #1576): it
        // cancels these requests when the screen unmounts mid-flight.
        const [m, n, runs] = await Promise.all([
          listMeshes(signal),
          listNodes(signal),
          listCircuitRuns(signal).catch(() => null),
        ]);
        // `isLatest()` is the sequence-token check from `useVisibilityPolling`:
        // if a newer refresh started while this fetch was in flight, drop
        // our setState so the hung-fetch-during-mobile-suspend case doesn't
        // clobber fresh data. `mountedRef` is the unmount check.
        if (!isLatest() || !mountedRef.current || revision !== refreshRevision.current) return;
        setMeshes(m);
        setNodes(n);
        if (runs !== null) setCircuitRuns(new Map(runs.map((run) => [run.node_id, run])));
        // The read succeeded, so the snapshot is live again — the stale notice
        // has done its job (issue #2330).
        setStaleNotice(false);
        // Issue #2330: a `503` (the desktop is up but could not read
        // authoritative state) is not a verdict about the workspace, so the
        // list is left untouched — but the desktop has demonstrably answered
        // again, so any stale offline overlay must clear. Convergence is the
        // second half of the contract: keep the last-known data through the
        // failure, then reconcile as soon as a read succeeds.
        onReachable();
      } catch (e) {
        if (!isLatest() || !mountedRef.current || revision !== refreshRevision.current) return;
        // A 401 means the token was revoked/expired — bounce to Connect
        // instead of claiming the desktop is offline.
        if (isAuthError(e)) {
          onAuthFailed();
        } else if (isUnavailableError(e)) {
          // Issue #2330: the desktop *answered* — it just could not read
          // authoritative state. That is not "can't reach the desktop", and
          // the full-screen offline overlay would be a false claim that also
          // hides the data we deliberately kept. Show a notice that says the
          // snapshot may be stale, and leave the list on screen.
          setStaleNotice(true);
        } else {
          onOffline();
        }
      }
    },
    [mountedRef, onOffline, onAuthFailed, onReachable],
  );

  // 5s poll is the WS-fallback safety net — `useWsEvents` below drives
  // instant refreshes while the socket is up; the poll catches the gap
  // between WS drops and reconnects (and the case where WS is up but the
  // server has state we haven't heard an event for). The lifecycle /
  // visibility gating lives in `useVisibilityPolling` (issue #1261)
  // so any other mobile screen can reuse it without copy-pasting the
  // 30-line timer dance that used to live inline here.
  useVisibilityPolling(refresh, 5000);

  // Live attention + lifecycle events via /ws/events. An `agent-lifecycle`
  // event patches the affected node optimistically from the event body
  // (issue #1364) — instant status flip without waiting on the network —
  // and every event still triggers a refetch so the list reconciles with
  // /api/nodes (the source of truth after a reconnect or a lagged
  // broadcast). WS drop falls back to polling silently.
  //
  // WS events are inherently "latest" (they ARE the most recent server
  // state), so we wrap `refresh` to feed it an `isLatest` that always
  // returns true. The polling hook's sequence-token check still applies to
  // its own ticks; this wrapper just keeps the WS path from passing
  // `undefined` as `isLatest`.
  useWsEvents((msg) => {
    if (msg.type === "agent-lifecycle" && mountedRef.current) {
      setNodes((prev) =>
        prev
          ? prev.map((n) =>
              n.id === msg.session_id ? { ...n, ...lifecycleNodePatch(msg) } : n,
            )
          : prev,
      );
      // Triage deck (issue #1377): remember what the node is waiting on
      // while it's awaiting input, forget it the moment it isn't.
      if (msg.status !== "awaiting_input") clearSentMarker(msg.session_id);
    }
    if (msg.type === "attention-cleared" && mountedRef.current) {
      clearSentMarker(msg.session_id);
    }
    void refresh(() => true);
  }, onAuthFailed);

  // Lazy-load the provider list; fallback gives the user something to tap
  // even if the request 401s or the server hasn't woken up yet.
  useEffect(() => {
    listProviders()
      .then((p) => {
        if (mountedRef.current && p.length > 0) setProviders(p);
      })
      .catch((e) => {
        if (!mountedRef.current) return;
        if (isAuthError(e)) onAuthFailed();
      });
  }, [mountedRef, onAuthFailed]);


  return { meshes, nodes, circuitRuns, providers, setProviders, staleNotice, setStaleNotice, refresh };
}
