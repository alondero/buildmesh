import { useCallback, useEffect, useRef, useState } from "react";
import {
  AgentNode,
  Mesh,
  NodeStatus,
  Provider,
  createNode,
  isAuthError,
  listMeshes,
  listNodes,
  listProviders,
  sendNodeKeys,
} from "../api";
import { ProviderIcon } from "../../components/Providers/ProviderIcon";
import { AppBar, CenterNote, PulseDots, Sheet } from "../ui";
import { useWsEvents } from "../useWsEvents";
import { useVisibilityPolling } from "../useVisibilityPolling";
import {
  PULL_REFRESH_THRESHOLD_PX,
  usePullToRefresh,
} from "../usePullToRefresh";
import { groupByHarness } from "../../lib/groups";
import { STATUS_CONFIG } from "../../lib/status";

type Props = {
  onOpenNode: (node: AgentNode) => void;
  onOpenAgentNodes: (mesh: Mesh) => void;
  onOpenIssues: (mesh: Mesh) => void;
  onOffline: () => void;
  onAuthFailed: () => void;
};

// `archived` sits outside the shared `STATUS_CONFIG` (desktop never shows a
// status badge for it — see the "Regenerate unavailable" comment in
// `Sidebar/NodeItem.tsx`). Mobile filters archived nodes out of the list
// entirely (see `visibleNodes` below), but `statusMeta` must stay total
// over the full `NodeStatus` union since `NodeRow` is a generic renderer.
const ARCHIVED_STATUS_META = { hex: "#555555", label: "Archived" };

// Module-scope type alias for the triage-deck chip action enum
// (issue #1377). Hoisted so `AttentionCard`'s `sent?: SentAction` prop
// doesn't have to repeat the literal union, and so the `useState<Map<...>>`
// calls in NodeList can avoid the `.tsx` JSX-vs-generic ambiguity that
// comes with back-to-back `<Map<...>>(...)` expressions.
type SentAction = "approve" | "reject";

function statusMeta(status: NodeStatus): { hex: string; label: string } {
  if (status === "archived") return ARCHIVED_STATUS_META;
  return STATUS_CONFIG[status];
}

// Issue #328 — the badge and the provider picker both consume the live
// `listProviders()` payload directly (no fallback list). Before the fetch
// resolves, `providers` is `[]` and every badge falls back to a neutral
// `#555` chip with the `ProviderIcon` gray-dot fallback (no brand mark).
// Once the fetch resolves, the chip's background tracks `meta.color` so
// it can't drift from the row label / status bar.

export default function NodeList({
  onOpenNode,
  onOpenAgentNodes,
  onOpenIssues,
  onOffline,
  onAuthFailed,
}: Props) {
  const [meshes, setMeshes] = useState<Mesh[] | null>(null);
  const [nodes, setNodes] = useState<AgentNode[] | null>(null);
  const [pickerMeshId, setPickerMeshId] = useState<number | null>(null);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [creating, setCreating] = useState<number | null>(null);
  const [meshActions, setMeshActions] = useState<Mesh | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mountedRef = useRef(true);

  // Triage deck (issue #1377): the last prompt / permission request per
  // awaiting-input node, learned from `agent-lifecycle` WS events (the wire
  // carries it as `semantic_turn.description`, with `message` as fallback).
  // There is no HTTP surface for it — /api/nodes returns bare AgentNode rows
  // — so on a cold app load the cards render the placeholder line until the
  // node's next lifecycle event arrives. Cleared the moment the node leaves
  // `awaiting_input` (or an `attention-cleared` lands) so a stale prompt
  // never outlives its card.
  const [lastPrompts, setLastPrompts] = useState<Map<number, string>>(
    new Map(),
  );
  // Per-card quick-action state (issue #1377, post-review rewrite).
  // `keyBusy` = which chip is currently in flight ("approve"/"reject" maps
  //   to the tap that opened the POST /api/nodes/{id}/input).
  // `keySent` = the LAST action the user took on this node ("approve" /
  //   "reject"). Tracking the action — not just a boolean — is what lets
  //   the right chip keep its label ("Approved ✓" / "Rejected ✗") while the
  //   *other* chip stays usable for a second-tap retraction… except the
  //   agent already saw the CR/LF, so a retraction would be confusing.
  //   The disable-after-send rule covers both chips with `sent !== undefined`
  //   so a user can't double-fire. Cleared the same way `lastPrompts` is:
  //   on any node reconciliation that drops the node out of
  //   `awaiting_input` AND on a fresh `agent-lifecycle` /
  //   `attention-cleared` event.
  //
  // The map types use the module-scope `SentAction` alias (see top of
  // file) — the inline `Map<number, "approve" | "reject">` form tripped
  // the `.tsx` parser when two adjacent `useState<Map<...>>` calls
  // followed each other (the second `<Map` was mis-interpreted as a JSX
  // element opening).
  const [keyBusy, setKeyBusy] = useState<Map<number, SentAction>>(
    new Map(),
  );
  const [keySent, setKeySent] = useState<Map<number, SentAction>>(new Map());

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const refresh = useCallback(async (isLatest: () => boolean) => {
    try {
      const [m, n] = await Promise.all([listMeshes(), listNodes()]);
      // `isLatest()` is the sequence-token check from `useVisibilityPolling`:
      // if a newer refresh started while this fetch was in flight, drop
      // our setState so the hung-fetch-during-mobile-suspend case doesn't
      // clobber fresh data. `mountedRef` is the unmount check.
      if (!isLatest() || !mountedRef.current) return;
      setMeshes(m);
      setNodes(n);
    } catch (e) {
      if (!isLatest() || !mountedRef.current) return;
      // A 401 means the token was revoked/expired — bounce to Connect
      // instead of claiming the desktop is offline.
      if (isAuthError(e)) onAuthFailed();
      else onOffline();
    }
  }, [onOffline, onAuthFailed]);

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
              n.id === msg.session_id
                ? { ...n, status: msg.status, signal_health: msg.signal_health }
                : n,
            )
          : prev,
      );
      // Triage deck (issue #1377): remember what the node is waiting on
      // while it's awaiting input, forget it the moment it isn't.
      setLastPrompts((prev) => {
        if (msg.status === "awaiting_input") {
          const text = msg.semantic_turn?.description ?? msg.message;
          if (!text) return prev;
          const next = new Map(prev);
          next.set(msg.session_id, text);
          return next;
        }
        if (!prev.has(msg.session_id)) return prev;
        const next = new Map(prev);
        next.delete(msg.session_id);
        return next;
      });
      if (msg.status !== "awaiting_input") clearSentMarker(msg.session_id);
    }
    if (msg.type === "attention-cleared" && mountedRef.current) {
      setLastPrompts((prev) => {
        if (!prev.has(msg.session_id)) return prev;
        const next = new Map(prev);
        next.delete(msg.session_id);
        return next;
      });
      clearSentMarker(msg.session_id);
    }
    void refresh(() => true);
  }, onAuthFailed);

  const clearSentMarker = (nodeId: number) => {
    setKeySent((prev) => {
      if (!prev.has(nodeId)) return prev;
      const next = new Map(prev);
      next.delete(nodeId);
      return next;
    });
  };

  // Triage card chips (issue #1377, post-review rewrite). The HTTP
  // `/api/nodes/{id}/input` route returns 200 OK with `{"ok":true}` only
  // after the bytes hit the PTY — so the await here is the delivery proof
  // (no more "Sent ✓" lying about a race-loser keystroke). On success we
  // record the action in `keySent` (which disables BOTH chips; see
  // `AttentionCard` below) and fire a refetch so the status transition
  // surfaces.
  const sendQuickAction = async (
    nodeId: number,
    action: "approve" | "reject",
  ) => {
    if (keyBusy.has(nodeId) || keySent.has(nodeId)) return;
    setKeyBusy((prev) => new Map(prev).set(nodeId, action));
    try {
      await sendNodeKeys(nodeId, action === "approve" ? "y\r" : "n\r");
      if (!mountedRef.current) return;
      setKeySent((prev) => new Map(prev).set(nodeId, action));
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
  }, [onAuthFailed]);

  const handleCreate = async (meshId: number, providerId: string) => {
    setPickerMeshId(null);
    setCreating(meshId);
    setError(null);
    try {
      const node = await createNode({ mesh_id: meshId, provider: providerId });
      if (!mountedRef.current) return;
      setCreating(null);
      onOpenNode(node);
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

  // Mobile-only QoL: pin awaiting-input nodes at the top so attention
  // is one tap away no matter how many meshes you have configured.
  const attentionNodes = visibleNodes
    .filter((n) => n.status === "awaiting_input")
    .sort((a, b) => a.id - b.id);

  // Triage-deck zombie-state reconciliation (issue #1377, post-review):
  // the WS handler clears `lastPrompts`/`keySent` on `agent-lifecycle`
  // transitions and `attention-cleared` events, but a node can leave
  // `awaiting_input` via plain polling too (reconnect, missed event,
  // refetch on tab return). Without this sweep, a card for a node that's
  // already `running` keeps its "Approved ✓" chip and prompt line until
  // the next lifecycle event — and the next attention ask on that same
  // node would inherit a stale prompt and a still-disabled chip.
  //
  // Runs as a `useEffect` keyed on the awaiting-id Set so it fires exactly
  // when the reconciliation surface changes (initial render + every nodes
  // refresh), never on unrelated re-renders. The functional `setState`
  // returns the same `Map` reference when nothing changed, so React skips
  // the re-render — no infinite loop.
  const awaitingIds = new Set(attentionNodes.map((n) => n.id));
  useEffect(() => {
    setKeySent((prev) => {
      let next: Map<number, "approve" | "reject"> | null = null;
      for (const id of prev.keys()) {
        if (!awaitingIds.has(id)) {
          if (next === null) next = new Map(prev);
          next.delete(id);
        }
      }
      return next ?? prev;
    });
    setLastPrompts((prev) => {
      let next: Map<number, string> | null = null;
      for (const id of prev.keys()) {
        if (!awaitingIds.has(id)) {
          if (next === null) next = new Map(prev);
          next.delete(id);
        }
      }
      return next ?? prev;
    });
  }, [awaitingIds]);

  // Bucket the remaining nodes by mesh. Attention nodes are EXCLUDED here
  // because they're already rendered in the "Needs attention" section above;
  // including them too would render each awaiting-input node twice (once
  // pinned, once under its mesh).
  const nodesByMesh = new Map<number, AgentNode[]>();
  for (const node of visibleNodes) {
    if (node.status === "awaiting_input") continue;
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

      {(pullToRefresh.pull > 0 || pullToRefresh.refreshing) && (
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
              {pullToRefresh.pull >= PULL_REFRESH_THRESHOLD_PX
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
          No meshes configured. Add a mesh on your desktop app to get started.
        </CenterNote>
      ) : (
        <div
          data-testid="node-list"
          className="list-scroll"
          style={{ flex: 1, overflowY: "auto", padding: "0 8px 8px" }}
          {...pullToRefresh.handlers}
        >
          {attentionNodes.length > 0 && (
            <section data-testid="attention-section" style={{ marginBottom: 8 }}>
              <SectionHeading color="var(--amber)">
                Needs attention
              </SectionHeading>
              <div
                className={`deck${attentionNodes.length === 1 ? " deck-single" : ""}`}
                data-testid="attention-deck"
              >
                {attentionNodes.map((node) => (
                  <AttentionCard
                    key={`attn-${node.id}`}
                    node={node}
                    meshName={meshes.find((m) => m.id === node.mesh_id)?.name}
                    prompt={lastPrompts.get(node.id)}
                    providers={providers}
                    busy={keyBusy.get(node.id)}
                    sent={keySent.get(node.id)}
                    onApprove={() => void sendQuickAction(node.id, "approve")}
                    onReject={() => void sendQuickAction(node.id, "reject")}
                    onFocus={() => onOpenNode(node)}
                  />
                ))}
              </div>
            </section>
          )}
          {meshes.map((mesh) => {
            const meshNodes = nodesByMesh.get(mesh.id) ?? [];
            return (
              <section key={mesh.id} style={{ marginBottom: 4 }}>
                <div
                  style={{
                    display: "flex",
                    alignItems: "center",
                    padding: "10px 12px 6px",
                    gap: 8,
                  }}
                >
                  <span
                    style={{
                      fontSize: 10,
                      fontWeight: 600,
                      color: "var(--text-faint)",
                      textTransform: "uppercase",
                      letterSpacing: "0.05em",
                      flex: 1,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {mesh.name}
                  </span>
                  <button
                    onClick={() => setPickerMeshId(mesh.id)}
                    aria-label={`New node in ${mesh.name}`}
                    data-testid={`new-node-${mesh.id}`}
                    className="chip-btn"
                    style={{ width: 38, padding: "8px 0", textAlign: "center", fontSize: 16, lineHeight: 1 }}
                  >
                    +
                  </button>
                  <button
                    onClick={() => setMeshActions(mesh)}
                    aria-label={`More actions for ${mesh.name}`}
                    data-testid={`mesh-actions-${mesh.id}`}
                    className="chip-btn"
                    style={{ width: 38, padding: "8px 0", textAlign: "center", fontSize: 14, lineHeight: 1 }}
                  >
                    ⋯
                  </button>
                </div>

                {creating === mesh.id ? (
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 8,
                      fontSize: 12,
                      color: "var(--accent)",
                      padding: "8px 12px 12px",
                    }}
                  >
                    <PulseDots /> Creating node…
                  </div>
                ) : meshNodes.length === 0 ? (
                  <div
                    style={{
                      fontSize: 12,
                      color: "var(--text-faint)",
                      padding: "8px 12px 12px",
                      fontStyle: "italic",
                    }}
                  >
                    No nodes — tap + to start an agent
                  </div>
                ) : (
                  meshNodes.map((node) => (
                    <NodeRow
                      key={node.id}
                      node={node}
                      onClick={() => onOpenNode(node)}
                      providers={providers}
                    />
                  ))
                )}
              </section>
            );
          })}
        </div>
      )}

      {pickerMeshId !== null && (
        <ProviderPicker
          providers={providers}
          onPick={(p) => handleCreate(pickerMeshId, p.id)}
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
        />
      )}

      {error && (
        <div className="toast error" data-testid="create-error">
          <span style={{ flex: 1 }}>{error}</span>
          <button
            onClick={() => setError(null)}
            aria-label="Dismiss"
            style={{
              background: "transparent",
              border: "none",
              color: "inherit",
              fontSize: 18,
              cursor: "pointer",
              padding: "0 4px",
            }}
          >
            ×
          </button>
        </div>
      )}
    </div>
  );
}

function SectionHeading({
  children,
  color,
}: {
  children: React.ReactNode;
  color: string;
}) {
  return (
    <div style={{ display: "flex", alignItems: "center", padding: "10px 12px 6px" }}>
      <span
        style={{
          fontSize: 10,
          fontWeight: 600,
          color,
          textTransform: "uppercase",
          letterSpacing: "0.05em",
        }}
      >
        {children}
      </span>
    </div>
  );
}

function MeshActionsSheet({
  mesh,
  onClose,
  onOpenAgentNodes,
  onOpenIssues,
}: {
  mesh: Mesh;
  onClose: () => void;
  onOpenAgentNodes: () => void;
  onOpenIssues: () => void;
}) {
  return (
    <Sheet onClose={onClose} testId="mesh-actions-sheet">
      <h3
        style={{
          fontSize: 13,
          fontWeight: 600,
          color: "var(--text-dim)",
          textTransform: "uppercase",
          letterSpacing: "0.05em",
          margin: 0,
          marginBottom: 12,
        }}
      >
        {mesh.name}
      </h3>
      <SheetButton
        onClick={onOpenAgentNodes}
        testId="mesh-sheet-discovered-nodes"
        label="Archive"
        hint="Resume an existing CLI session"
      />
      <SheetButton
        onClick={onOpenIssues}
        testId="mesh-sheet-issues"
        label="GitHub Issues"
        hint="Spawn an agent prefilled with an issue"
      />
    </Sheet>
  );
}

function SheetButton({
  onClick,
  testId,
  label,
  hint,
}: {
  onClick: () => void;
  testId: string;
  label: string;
  hint: string;
}) {
  return (
    <button
      onClick={onClick}
      data-testid={testId}
      className="card"
      style={{ display: "block", background: "var(--surface-2)" }}
    >
      <div style={{ fontSize: 14, fontWeight: 500, color: "#fff" }}>{label}</div>
      <div style={{ fontSize: 12, color: "var(--text-dim)", marginTop: 2 }}>
        {hint}
      </div>
    </button>
  );
}

// Triage deck card (issue #1377, post-review rewrite): one awaiting-input
// node with its context (mesh/repo, branch, last prompt) and one-tap
// answers. The whole upper body is the "Focus Terminal" tap target;
// Approve/Reject answer the prompt via the dedicated `/api/nodes/{id}/input`
// HTTP route without ever opening the terminal.
//
// State machine (review feedback): `sent` is the SPECIFIC action the user
// took on this card — "approve" or "reject". When set, BOTH chips are
// disabled (the agent already saw the CR/LF — a retraction would either be
// ignored or worse, send an opposite prompt into a stream the agent has
// already moved past). The chip whose action was sent shows the success
// label; the other stays greyed out. `busy` (in-flight POST) still takes
// precedence over `sent` so the "Sending…" feedback isn't lost.
//
// The previous design had a separate "Focus terminal" chip alongside the
// card-body tap target — two competing buttons doing the same thing on a
// 120px card, and they took width away from the action chips. Dropped; the
// card body is the focus target.
function AttentionCard({
  node,
  meshName,
  prompt,
  providers,
  busy,
  sent,
  onApprove,
  onReject,
  onFocus,
}: {
  node: AgentNode;
  meshName?: string;
  prompt?: string;
  providers?: Provider[];
  busy?: SentAction;
  sent?: SentAction;
  onApprove: () => void;
  onReject: () => void;
  onFocus: () => void;
}) {
  // Same live-provider lookup contract as `NodeRow` (issue #328): the label
  // and chip colour come from the fetched `listProviders()` payload, with a
  // deterministic raw-id fallback before it resolves.
  const providerMeta = providers?.find((p) => p.id === node.provider);
  const providerLabel = providerMeta?.label ?? node.provider;
  // BOTH chips disable when an action was taken (or is in flight) on this
  // card — see the state machine comment above. `sent` (enum) gives us the
  // strict superset that the previous boolean `sent` couldn't: the
  // disabled check is a one-liner, no separate per-chip sent state to
  // reconcile.
  const chipsDisabled = busy !== undefined || sent !== undefined;
  const approveLabel =
    busy === "approve"
      ? "Sending…"
      : sent === "approve"
        ? "Approved ✓"
        : "Approve (Y)";
  const rejectLabel =
    busy === "reject"
      ? "Sending…"
      : sent === "reject"
        ? "Rejected ✗"
        : "Reject (N)";
  return (
    <div className="deck-card" data-testid={`attn-card-${node.id}`}>
      <button
        type="button"
        className="deck-body"
        data-testid={`node-${node.id}`}
        aria-label={`Open ${node.name} terminal`}
        onClick={onFocus}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <ProviderIcon
            providerId={node.provider}
            withBackground
            backgroundColor={providerMeta?.color}
            fallbackGlyph={providerMeta?.icon}
            chipTestId="node-avatar"
            title={providerLabel}
            className="h-4 w-4"
          />
          <span
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: 14,
              fontWeight: 500,
              color: "#fff",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              textAlign: "left",
            }}
          >
            {node.name}
          </span>
        </div>
        <div
          style={{
            fontSize: 11,
            color: "var(--text-faint)",
            marginTop: 4,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            textAlign: "left",
          }}
        >
          {meshName ?? "…"}
          {node.branch ? ` · ⎇ ${node.branch}` : ""} · {providerLabel}
        </div>
        <div
          data-testid={`attn-prompt-${node.id}`}
          style={{
            fontSize: 12,
            color: "var(--amber)",
            marginTop: 8,
            textAlign: "left",
            display: "-webkit-box",
            WebkitLineClamp: 2,
            WebkitBoxOrient: "vertical",
            overflow: "hidden",
            overflowWrap: "anywhere",
          }}
        >
          {prompt ?? "Waiting for the agent's prompt…"}
        </div>
      </button>
      <div className="deck-chips">
        <button
          type="button"
          className="deck-chip approve"
          data-testid={`attn-approve-${node.id}`}
          disabled={chipsDisabled}
          onClick={onApprove}
        >
          {approveLabel}
        </button>
        <button
          type="button"
          className="deck-chip reject"
          data-testid={`attn-reject-${node.id}`}
          disabled={chipsDisabled}
          onClick={onReject}
        >
          {rejectLabel}
        </button>
      </div>
    </div>
  );
}

export function NodeRow({
  node,
  onClick,
  providers,
}: {
  node: AgentNode;
  onClick: () => void;
  providers?: Provider[];
}) {
  const meta = statusMeta(node.status);
  const needsInput = node.status === "awaiting_input";
  // Single source of truth for the badge + label: the live `listProviders()`
  // payload (issue #328). The fallback (`'?' / '#555'` + raw id) fires when:
  //   * the fetch hasn't resolved yet (`providers === []` initially), or
  //   * the node's provider id isn't in the live list (e.g. a since-removed
  //     harness profile). Both cases get a deterministic grey badge so the
  //     row's left edge still has consistent rhythm.
  // `providerMeta` carries color + icon for the chip and the same `.label`
  // drives the row subtitle — keeps the badge and the label in lockstep.
  const providerMeta = providers?.find((p) => p.id === node.provider);
  const providerLabel = providerMeta?.label ?? node.provider;
  return (
    <button
      onClick={onClick}
      data-testid={`node-${node.id}`}
      className="card"
      style={needsInput ? { borderColor: "rgba(255, 152, 0, 0.4)" } : undefined}
    >
      <ProviderIcon
        // `backgroundColor` drives the chip from the live `meta.color`
        // (issue #328). When the fetch hasn't resolved, `ProviderIcon`
        // falls back to its unknown-provider gray (`'#555'`). Icon size
        // `h-4 w-4` keeps the full-bleed brand marks visually balanced
        // with the sparse monochrome glyphs at the established mobile
        // rhythm (16px icon inside the 34×34 chip).
        // `fallbackGlyph` feeds the live `meta.icon` letter back in for a
        // custom Claude-compatible Proxied account (issue #948): its slug
        // has no brand mark, so without this the row shows a bare dot
        // where the pre-#328 badge showed the wire letter.
        providerId={node.provider}
        withBackground
        backgroundColor={providerMeta?.color}
        fallbackGlyph={providerMeta?.icon}
        chipTestId="node-avatar"
        title={providerLabel}
        className="h-4 w-4"
      />
      <div style={{ flex: 1, minWidth: 0 }}>
        <div
          style={{
            fontSize: 14,
            fontWeight: 500,
            color: "#fff",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {node.name}
        </div>
        <div
          style={{
            fontSize: 11,
            color: "var(--text-faint)",
            marginTop: 2,
            display: "flex",
            alignItems: "center",
            gap: 5,
            overflow: "hidden",
            whiteSpace: "nowrap",
          }}
        >
          {providerLabel} ·
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: meta.hex,
              flexShrink: 0,
              display: "inline-block",
            }}
          />
          <span style={{ color: meta.hex }}>{meta.label}</span>
        </div>
      </div>
      <div
        style={{
          width: 3,
          height: 34,
          borderRadius: 2,
          background: meta.hex,
          flexShrink: 0,
        }}
      />
    </button>
  );
}

function ProviderPicker({
  providers,
  onPick,
  onCancel,
}: {
  providers: Provider[];
  onPick: (p: Provider) => void;
  onCancel: () => void;
}) {
  // Issue #575 / ADR-0016 — group the Spawn Options by `harness_id`
  // (== `group_key` on the wire). The backend already orders rows by
  // `(is_terminal, rank_of(harness_id))` so the order is preserved
  // here. The first row in each bucket is the native harness header
  // (clickable = native launch); subsequent rows are Proxied children
  // rendered indented. Mobile is read-only (no reorder), so this is
  // the same shape the desktop sidebar/probes render, just sized for
  // a touch sheet. The bucketing is shared with the desktop
  // `GroupedProviderMenu` and `MeshPropertiesTab` via `groupByHarness`
  // (issue #583 cleanup).
  const groups = groupByHarness(providers);

  return (
    <Sheet onClose={onCancel} testId="provider-picker">
      <h3
        style={{
          fontSize: 15,
          fontWeight: 600,
          color: "#fff",
          margin: 0,
          marginBottom: 14,
        }}
      >
        New Agent Node
      </h3>
      {groups.map(([harnessId, group]) => {
        const native = group[0];
        const children = group.slice(1);
        return (
          <div key={harnessId} data-testid={`spawn-group-${harnessId}`} style={{ marginBottom: 8 }}>
            <button
              type="button"
              onClick={() => onPick(native)}
              data-testid={`provider-${native.id}`}
              className="card"
              style={{ background: "var(--surface-2)" }}
            >
              <ProviderIcon
                // `fallbackGlyph` feeds the row's own wire letter in for a
                // harness profile with no brand mark (issue #1086), the same
                // way `NodeRow` does for a custom Proxied account (#948).
                // Here the row IS the live `listProviders()` record, so
                // `native.icon` is the value `NodeRow` has to look up — and
                // it keeps the chip's glyph on the same source as its colour.
                providerId={native.id}
                withBackground
                backgroundColor={native.color}
                fallbackGlyph={native.icon}
                chipTestId={`picker-avatar-${native.id}`}
                title={native.label}
                className="h-4 w-4"
              />
              <span style={{ flex: 1, fontSize: 15, color: "var(--text)" }}>{native.label}</span>
              <span style={{ fontSize: 9, color: "var(--text-faint)", textTransform: "uppercase", letterSpacing: 1 }}>harness</span>
            </button>
            {children.map((child) => (
              <button
                type="button"
                key={child.id}
                onClick={() => onPick(child)}
                data-testid={`provider-${child.id}`}
                className="card"
                style={{ background: "var(--surface-2)", marginLeft: 18 }}
              >
                <ProviderIcon
                  // Same fallback as the header row above (issue #1086) —
                  // a custom Claude-compatible Proxied account's slug has no
                  // brand mark, so without this the 28px chip shows a bare
                  // dot where the row's wire letter belongs.
                  providerId={child.id}
                  withBackground
                  chipSize={28}
                  backgroundColor={child.color}
                  fallbackGlyph={child.icon}
                  chipTestId={`picker-avatar-${child.id}`}
                  title={child.label}
                  className="h-3.5 w-3.5"
                />
                <span style={{ fontSize: 14, color: "var(--text)" }}>{child.label}</span>
              </button>
            ))}
          </div>
        );
      })}
    </Sheet>
  );
}
