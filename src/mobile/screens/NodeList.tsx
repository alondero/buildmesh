import { SectionHeading } from '../components/nodeList/SectionHeading';
import { AttentionDeck } from '../components/nodeList/AttentionDeck';
import { NodeRow } from '../components/nodeList/NodeRow';
export { NodeRow } from '../components/nodeList/NodeRow';
import { ProviderPicker } from '../components/nodeList/ProviderPicker';
import { MeshActionsSheet } from '../components/nodeList/MeshActionsSheet';
import { MeshNodeSection } from '../components/nodeList/MeshNodeSection';
import { NodeOverview } from '../components/nodeList/NodeOverview';
import { NodeListNotices } from '../components/nodeList/NodeListNotices';
import { useNodeListSnapshot } from '../components/nodeList/useNodeListSnapshot';
import type { SentAction, SentMarker } from '../components/nodeList/types';
import { useEffect, useMemo, useRef, useState } from "react";
import { AgentNode, Mesh, createNode, isAuthError, newOperationKey, listProviders, sendNodeKeys } from "../api";
import { needsYou } from "../circuitRun";

import { AppBar, CenterNote, PulseDots } from "../ui";

import { usePullToRefresh } from "../usePullToRefresh";
// The hook owns the threshold value internally and exposes
// `isPastThreshold` as a boolean signal so callers don't pay a render
// per pixel (review feedback #1). No need to import the numeric
// `PULL_REFRESH_THRESHOLD_PX` from the hook in this file.

import CaptureIdea from "./CaptureIdea";
import { isSignalHealthProblem, nodeInputRequest } from "../../lib/status";

type Props = {
  onOpenNode: (node: AgentNode, prompt?: string) => void;
  view?: "overview" | "work" | "capture";
  onViewChange?: (view: "overview" | "work" | "capture") => void;
  onOpenAgentNodes: (mesh: Mesh) => void;
  onOpenIssues: (mesh: Mesh) => void;
  onOpenPulls?: (mesh: Mesh) => void;
  onOffline: () => void;
  /**
   * A poll succeeded, so the desktop answered a request again.
   *
   * Issue #2330: a `503` leaves the visible snapshot alone (an unavailable
   * read is not a verdict about the workspace), but the phone is demonstrably
   * reachable again — without this, the offline overlay raised during the
   * fault would outlive the fault and hide data that is now fresh.
   */
  onReachable?: () => void;
  onAuthFailed: () => void;
};

// Issue #328 — the badge and the provider picker both consume the live
// `listProviders()` payload directly (no fallback list). Before the fetch
// resolves, `providers` is `[]` and every badge falls back to a neutral
// `#555` chip with the `ProviderIcon` gray-dot fallback (no brand mark).
// Once the fetch resolves, the chip's background tracks `meta.color` so
// it can't drift from the row label / status bar.

export default function NodeList({
  onOpenNode,
  view = "overview",
  onViewChange,
  onOpenAgentNodes,
  onOpenIssues,
  onOpenPulls,
  onOffline,
  onReachable = () => {},
  onAuthFailed,
}: Props) {
  const [query, setQuery] = useState("");
  const [captureBusy, setCaptureBusy] = useState(false);
  const [filter, setFilter] = useState("all");
  const [localView, setLocalView] = useState(view);
  const activeView = onViewChange ? view : localView;
  const changeView = onViewChange ?? setLocalView;
  const [pickerMeshId, setPickerMeshId] = useState<number | null>(null);
  const [creating, setCreating] = useState<number | null>(null);
  const [meshActions, setMeshActions] = useState<Mesh | null>(null);
  const [error, setError] = useState<string | null>(null);
  /**
   * A node quick-create committed but could not start (issue #2331). Held
   * rather than navigated to, because `onOpenNode` unmounts this screen and
   * would take the reason with it.
   */
  const [committed, setCommitted] = useState<AgentNode | null>(null);
  /** The quick-create submission currently in flight, for key retention. */
  const quickCreateKey = useRef<{ submission: string; key: string } | null>(null);
  const mountedRef = useRef(true);

  // Request text comes from the same lifecycle snapshot as status.
  // Per-card quick-action state (issue #1377, post-review rewrite).
  // `keyBusy` = which chip is currently in flight ("approve"/"reject" maps
  //   to the tap that opened the POST /api/nodes/{id}/input).
  // `keySent` = the LAST action the user took on this node, bound to the
  //   request it answered (issue #1966). Tracking the action — not just a
  //   boolean — is what lets the right chip keep its label ("Approved ✓" /
  //   "Rejected ✗") while the *other* chip stays usable for a second-tap
  //   retraction… except the agent already saw the CR/LF, so a retraction
  //   would be confusing. The disable-after-send rule covers both chips with
  //   `sent !== undefined` so a user can't double-fire. Cleared on status
  //   reconciliation: on any node reconciliation that drops the node out of
  //   `awaiting_input`, when the request itself is replaced, AND on a fresh
  //   `agent-lifecycle` / `attention-cleared` event.
  //
  // The map types use the module-scope `SentAction` alias (see top of
  // file) — the inline `Map<number, "approve" | "reject">` form tripped
  // the `.tsx` parser when two adjacent `useState<Map<...>>` calls
  // followed each other (the second `<Map` was mis-interpreted as a JSX
  // element opening).
  const [keyBusy, setKeyBusy] = useState<Map<number, SentAction>>(new Map());
  const [keySent, setKeySent] = useState<Map<number, SentMarker>>(new Map());

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const clearSentMarker = (nodeId: number) => {
    setKeySent((prev) => {
      if (!prev.has(nodeId)) return prev;
      const next = new Map(prev);
      next.delete(nodeId);
      return next;
    });
  };

  const { meshes, nodes, circuitRuns, providers, setProviders, staleNotice, setStaleNotice, refresh } = useNodeListSnapshot({
    mountedRef, clearSentMarker, onOffline, onReachable, onAuthFailed,
  });

  // Triage card chips (issue #1377, post-review rewrite). The HTTP
  // `/api/nodes/{id}/input` route returns 200 OK with `{"ok":true}` only
  // after the bytes hit the PTY — so the await here is the delivery proof
  // (no more "Sent ✓" lying about a race-loser keystroke). On success we
  // record the action in `keySent` against the request that received it
  // (which disables BOTH chips; see `AttentionCard` below) and fire a refetch
  // so the status transition surfaces.
  //
  // The `requestKey` is captured at tap time, not read back afterwards: a
  // replacement request can land while the POST is in flight, and binding the
  // marker to the *new* request would disable a request the user never
  // answered (issue #1966).
  const sendQuickAction = async (
    nodeId: number,
    requestKey: string,
    action: SentAction,
  ) => {
    if (keyBusy.has(nodeId) || keySent.get(nodeId)?.requestKey === requestKey) return;
    setKeyBusy((prev) => new Map(prev).set(nodeId, action));
    try {
      await sendNodeKeys(nodeId, action === "approve" ? "y\r" : "n\r");
      if (!mountedRef.current) return;
      setKeySent((prev) => new Map(prev).set(nodeId, { requestKey, action }));
      void refresh(() => true);
    } catch (e) {
      if (!mountedRef.current) return;
      if (isAuthError(e)) {
        onAuthFailed();
        return;
      }
      setError((e as Error).message);
    } finally {
      if (mountedRef.current) {
        setKeyBusy((prev) => {
          const next = new Map(prev);
          next.delete(nodeId);
          return next;
        });
      }
    }
  };

  const handleCreate = async (
    meshId: number,
    providerId: string,
    configurationId?: string,
  ) => {
    setPickerMeshId(null);
    setCreating(meshId);
    setError(null);
    setCommitted(null);
    // Issue #2331: one operation key per submission, reused for retries of it,
    // so a retry after a lost response resolves to the same node instead of
    // creating a duplicate. Quick-create has no draft to key off, so the
    // submission identity is the mesh/harness/configuration triple; a different
    // triple is a different submission and must mint a fresh key (the server
    // rejects a key reused with different content as a conflict).
    const submission = `${meshId}|${providerId}|${configurationId ?? ""}`;
    const keyRef = quickCreateKey.current;
    const operationKey = keyRef?.submission === submission ? keyRef.key : newOperationKey();
    quickCreateKey.current = { submission, key: operationKey };
    try {
      const outcome = await createNode({
        mesh_id: meshId,
        provider: providerId,
        configuration_id: configurationId,
        operation_key: operationKey,
      });
      if (!mountedRef.current) return;
      setCreating(null);
      if (outcome.outcome === "active") {
        // Landed and running: the submission is complete, so retire the key.
        quickCreateKey.current = null;
        onOpenNode(outcome.node);
        return;
      }
      // Committed but not running (issue #2331). `onOpenNode` unmounts this
      // screen, so navigating now would throw away the only thing that says
      // what to fix. Hold the node and let the user go there deliberately —
      // the same shape CaptureIdea uses.
      quickCreateKey.current = null;
      setCommitted(outcome.node);
      setError(
        `Node created but not started: ${
          outcome.outcome === "start_failed"
            ? outcome.start_error
            : outcome.reason
        } Open it to retry.`,
      );
    } catch (e) {
      if (!mountedRef.current) return;
      setCreating(null);
      if (isAuthError(e)) {
        onAuthFailed();
        return;
      }
      setError((e as Error).message);
    }
  };

  // Archived nodes are history, not actionable work — hide them on mobile.
  const visibleNodes = (nodes ?? []).filter((n) => n.status !== "archived");

  const problemNodes = visibleNodes.filter(
    (n) =>
      n.status === "error" ||
      (n.status !== "awaiting_input" && isSignalHealthProblem(n.signal_health)),
  );
  const runningCount = visibleNodes.filter(
    (n) => n.status === "running",
  ).length;
  const circuitNeeds = (node: AgentNode) => {
    const run = circuitRuns.get(node.id);
    return run !== undefined && needsYou(run);
  };
  const attentionCount = visibleNodes.filter((node) =>
    node.status === 'awaiting_input' || problemNodes.some((p) => p.id === node.id) || circuitNeeds(node),
  ).length;
  const circuitAttentionNodes = visibleNodes.filter((node) => circuitNeeds(node) &&
    node.status !== 'awaiting_input' && !problemNodes.some((p) => p.id === node.id));
  const matches = (node: AgentNode) => {
    const mesh = meshes?.find((m) => m.id === node.mesh_id);
    const textMatches =
      `${node.name} ${node.branch} ${mesh?.name} ${node.provider}`
        .toLowerCase()
        .includes(query.toLowerCase());
    return (
      textMatches &&
      (filter === "all" ||
        (filter === "attention"
          ? node.status === "awaiting_input" ||
            circuitNeeds(node) ||
            problemNodes.some((p) => p.id === node.id)
          : node.status === filter))
    );
  };

  // Mobile-only QoL: pin awaiting-input nodes at the top so attention
  // is one tap away no matter how many meshes you have configured.
  const attentionNodes = visibleNodes
    .filter((n) => n.status === "awaiting_input")
    .sort((a, b) => a.id - b.id);

  // Triage-deck zombie-state reconciliation (issue #1377, post-review,
  // round-2): the WS handler clears `keySent` on
  // `agent-lifecycle` transitions and `attention-cleared` events, but a
  // node can leave `awaiting_input` via plain polling too (reconnect,
  // missed event, refetch on tab return). Without this sweep, a card
  // for a node that's already `running` keeps its "Approved ✓" chip
  // and prompt line until the next lifecycle event — and the next
  // attention ask on that same node would inherit a stale prompt and
  // a still-disabled chip.
  //
  // Issue #1966 extends the sweep to a second, more common case: a node
  // stays `awaiting_input` across a request boundary. A harness that answers
  // one question and immediately asks another never leaves the status, so no
  // event fires and the id-keyed sweep below could not tell the two requests
  // apart. The sweep is therefore keyed on the request (`id:key`), not the
  // node, which covers a replaced request and a resumed node in one rule.
  //
  // Round-2 review: synchronizing state from other state via useEffect
  // IS an anti-pattern. The right shape is to prune inside the
  // authoritative transition points (`refresh()` and the WS handler).
  // We keep a single useEffect here for the polling case (no event
  // fires when a node leaves `awaiting_input` via plain HTTP refresh),
  // but the effect's dep MUST be a stable reference: an inline
  // `new Set(...)` would change identity on every render and fire
  // the effect on every render — a React 101 violation. Memoizing on
  // the sorted `id:requestKey` list (the canonical serialization of "which
  // requests are outstanding") gives us stable identity until the set
  // actually changes.
  const liveRequestList = useMemo(
    () =>
      attentionNodes
        .map((n) => `${n.id}:${nodeInputRequest(n)?.key ?? ""}`)
        .join(","),
    [attentionNodes],
  );
  const liveRequests = useMemo(
    () => new Set(liveRequestList ? liveRequestList.split(",") : []),
    [liveRequestList],
  );
  useEffect(() => {
    setKeySent((prev) => {
      let next: Map<number, SentMarker> | null = null;
      for (const [id, marker] of prev) {
        if (!liveRequests.has(`${id}:${marker.requestKey}`)) {
          if (next === null) next = new Map(prev);
          next.delete(id);
        }
      }
      return next ?? prev;
    });
  }, [liveRequests]);

  // Bucket the remaining nodes by mesh. Attention nodes are EXCLUDED here
  // because they're already rendered in the "Needs attention" section above;
  // including them too would render each awaiting-input node twice (once
  // pinned, once under its mesh).
  const nodesByMesh = new Map<number, AgentNode[]>();
  for (const node of visibleNodes) {
    if (
      activeView === "overview" &&
      (node.status === "awaiting_input" ||
        circuitNeeds(node) ||
        problemNodes.some((p) => p.id === node.id))
    )
      continue;
    if (activeView === "work" && !matches(node)) continue;
    if (!nodesByMesh.has(node.mesh_id)) nodesByMesh.set(node.mesh_id, []);
    nodesByMesh.get(node.mesh_id)!.push(node);
  }

  // Pull-to-refresh (issue #1377) — user-initiated, so the result always
  // applies (the same `isLatest` stance the WS path takes).
  const pullToRefresh = usePullToRefresh(
    () => refresh(() => true),
    meshes !== null,
  );

  return (
    <div className="screen">
      <AppBar
        title="Buildmesh"
        subtitle={
          meshes === null
            ? undefined
            : `${meshes.length} ${meshes.length === 1 ? "mesh" : "meshes"} · ${visibleNodes.length} ${visibleNodes.length === 1 ? "node" : "nodes"}`
        }
      />

      {activeView === "capture" ? (
        <div className="list-scroll">
          <CaptureIdea
            meshes={meshes ?? []}
            providers={providers}
            onStarted={(node) => {
              changeView("work");
              onOpenNode(node);
            }}
            onAuthFailed={onAuthFailed}
            onBusyChange={setCaptureBusy}
          />
        </div>
      ) : (
        <>
          {(pullToRefresh.isPastThreshold || pullToRefresh.refreshing) && (
            <div
              ref={pullToRefresh.bindIndicator}
              data-testid="pull-indicator"
              className="pull-indicator"
            >
              {pullToRefresh.refreshing ? (
                <PulseDots />
              ) : (
                <span
                  style={{ fontSize: 11, color: "var(--text-faint)" }}
                  data-testid="pull-indicator-label"
                >
                  {pullToRefresh.isPastThreshold
                    ? "Release to refresh"
                    : "Pull to refresh"}
                </span>
              )}
            </div>
          )}

          {meshes === null ? (
            <div
              data-testid="nodelist-loading"
              style={{
                flex: 1,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <PulseDots />
            </div>
          ) : meshes.length === 0 ? (
            <CenterNote>
              No meshes configured. Add a mesh on your desktop app to get
              started.
            </CenterNote>
          ) : (
            <div
              data-testid="node-list"
              className="list-scroll"
              style={{ flex: 1, overflowY: "auto", padding: "0 8px 8px" }}
              {...pullToRefresh.handlers}
            >
              <div className="overview-heading">
                <h1>{activeView === "overview" ? "Overview" : "Work"}</h1>
              </div>
              {activeView === "overview" && (
                <>
                  <NodeOverview attentionCount={attentionCount} runningCount={runningCount} visibleNodes={visibleNodes} problemNodes={problemNodes} meshes={meshes} providers={providers} circuitRuns={circuitRuns} setFilter={setFilter} changeView={changeView} onOpenNode={onOpenNode} />
                </>
              )}
              {activeView === "work" && (
                <div className="work-filters">
                  <input
                    className="field"
                    aria-label="Search work"
                    placeholder="Search agents, meshes, branches…"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                  />
                  <div className="filter-chips" aria-label="Filter work">
                    {[
                      ["all", "All"],
                      ["attention", "Needs attention"],
                      ["running", "Running"],
                      ["idle", "Idle"],
                    ].map(([value, label]) => (
                      <button
                        key={value}
                        className="chip-btn"
                        aria-pressed={filter === value}
                        onClick={() => setFilter(value)}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                  {!visibleNodes.some(matches) && (
                    <CenterNote>
                      No agents match. Try another search or filter.
                    </CenterNote>
                  )}
                </div>
              )}
              {activeView === 'overview' && circuitAttentionNodes.length > 0 && (
                <section aria-label="Circuit Runs need you">
                  <SectionHeading color="var(--amber)">Circuit Runs need you</SectionHeading>
                  {circuitAttentionNodes.map((node) => (
                    <NodeRow key={node.id} node={node} providers={providers}
                      meshName={meshes.find((m) => m.id === node.mesh_id)?.name}
                      circuitRun={circuitRuns.get(node.id)} onClick={() => onOpenNode(node)} />
                  ))}
                </section>
              )}
              <AttentionDeck activeView={activeView} attentionNodes={attentionNodes} meshes={meshes} circuitRuns={circuitRuns} providers={providers} keyBusy={keyBusy} keySent={keySent} sendQuickAction={sendQuickAction} onOpenNode={onOpenNode} />
              {meshes.map((mesh) => {
                const meshNodes = nodesByMesh.get(mesh.id) ?? [];
                if (
                  activeView === "work" &&
                  (query || filter !== "all") &&
                  !meshNodes.length
                )
                  return null;
                return (
                  <MeshNodeSection key={mesh.id} mesh={mesh} meshNodes={meshNodes} creating={creating} providers={providers} circuitRuns={circuitRuns} setPickerMeshId={setPickerMeshId} setMeshActions={setMeshActions} onOpenNode={onOpenNode} onOpenIssues={onOpenIssues} onOpenAgentNodes={onOpenAgentNodes} onOpenPulls={onOpenPulls} />
                );
              })}
            </div>
          )}
        </>
      )}
      <nav className="mobile-nav" aria-label="Main navigation">
        {(
          [
            ["overview", "Overview", "◉"],
            ["work", "Work", "▤"],
            ["capture", "Capture", "+"],
          ] as const
        ).map(([key, label, icon]) => (
          <button
            key={key}
            disabled={captureBusy}
            aria-current={activeView === key ? "page" : undefined}
            onClick={() => changeView(key)}
          >
            <span aria-hidden="true">{icon}</span>
            {label}
          </button>
        ))}
      </nav>
      {pickerMeshId !== null && (
        <ProviderPicker
          providers={providers}
          onChanged={() => {
            void listProviders()
              .then(setProviders)
              .catch((e: unknown) => setError(String(e)));
          }}
          onPick={(providerId, configurationId) =>
            handleCreate(pickerMeshId, providerId, configurationId)
          }
          onCancel={() => setPickerMeshId(null)}
        />
      )}

      {meshActions && (
        <MeshActionsSheet
          mesh={meshActions}
          onClose={() => setMeshActions(null)}
          onOpenAgentNodes={() => {
            const m = meshActions;
            setMeshActions(null);
            onOpenAgentNodes(m);
          }}
          onOpenIssues={() => {
            const m = meshActions;
            setMeshActions(null);
            onOpenIssues(m);
          }}
          onOpenPulls={onOpenPulls ? () => {
            const m = meshActions;
            setMeshActions(null);
            onOpenPulls(m);
          } : undefined}
        />
      )}

      <NodeListNotices staleNotice={staleNotice} setStaleNotice={setStaleNotice} error={error} setError={setError} committed={committed} setCommitted={setCommitted} onOpenNode={onOpenNode} />
    </div>
  );
}
