import { useCallback, useEffect, useRef, useState } from "react";
import {
  AgentNode,
  EventMsg,
  Mesh,
  NodeStatus,
  Provider,
  createNode,
  eventsWsUrl,
  isAuthError,
  listMeshes,
  listNodes,
  listProviders,
} from "../api";
import { AppBar, CenterNote, PulseDots, Sheet } from "../ui";
import { ProviderIcon } from "../../components/Providers/ProviderIcon";
import { useAsyncEffect } from "../../hooks/useAsyncEffect";
import { groupByHarness } from "../../lib/groups";

type Props = {
  onOpenNode: (node: AgentNode) => void;
  onOpenAgentNodes: (mesh: Mesh) => void;
  onOpenIssues: (mesh: Mesh) => void;
  onOffline: () => void;
  onAuthFailed: () => void;
};

export const STATUS_META: Record<NodeStatus, { color: string; label: string }> = {
  idle: { color: "#2196f3", label: "idle" },
  running: { color: "#4caf50", label: "running" },
  suspended: { color: "#9e9e9e", label: "suspended" },
  error: { color: "#f44336", label: "error" },
  awaiting_input: { color: "#ff9800", label: "needs input" },
  archived: { color: "#555", label: "archived" },
  // `pending` is now part of the generated SessionStatus union (issue #359);
  // the two-stage spawn flow sets it while stage-2 (worktree + PTY) runs.
  pending: { color: "#9c27b0", label: "starting" },
  // `spawning` (issue #654) — agent process is launched but the early-exit
  // window (< 3s) has not yet elapsed. The orchestrator writes this transient
  // state between `start_reader` returning and the conditional Running
  // promotion. Visually mirrors `pending` (also a transient "in-progress"
  // state) so the user sees the node move Pending → Spawning → Running
  // across stage-2.
  spawning: { color: "#9c27b0", label: "starting" },
};

// Offline fallback shown only when the /providers fetch fails. The live list is
// the user's dynamic harness profiles; this is a degraded static default (issue
// #538 dropped the legacy enum rows / `legacy` flag).
// `resumable` mirrors the Rust derivation in `ProviderInfo`:
// `supports_resume() && produces_readable_transcript()` (models/mod.rs).
// Anthropic is the only adapter that produces a readable transcript, so it's
// the only fallback row the resume picker (issue #550) should surface. Keeping
// the flag honest here matters: if /providers 401s and we fall back, the user
// still won't see ghost rows they can't actually resume.
// Issue #575 — every fallback row also populates the Spawn Option wire
// shape (harness_id, provider_id, is_proxied, group_key) so the
// `ProviderPicker` group render doesn't have to special-case the
// offline fallback.
const FALLBACK_PROVIDERS: Provider[] = [
  { id: "claude", label: "Claude Code", color: "#1d7cfc", icon: "A", resumable: true, harness_id: "claude", provider_id: null, is_proxied: false, group_key: "claude" },
  { id: "agy", label: "Antigravity CLI", color: "#10b981", icon: "G", resumable: false, harness_id: "agy", provider_id: null, is_proxied: false, group_key: "agy" },
  { id: "opencode", label: "OpenCode", color: "#f59e0b", icon: "O", resumable: false, harness_id: "opencode", provider_id: null, is_proxied: false, group_key: "opencode" },
];

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
  const [providers, setProviders] = useState<Provider[]>(FALLBACK_PROVIDERS);
  const [creating, setCreating] = useState<number | null>(null);
  const [meshActions, setMeshActions] = useState<Mesh | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [m, n] = await Promise.all([listMeshes(), listNodes()]);
      setMeshes(m);
      setNodes(n);
    } catch (e) {
      // A 401 means the token was revoked/expired — bounce to Connect
      // instead of claiming the desktop is offline.
      if (isAuthError(e)) onAuthFailed();
      else onOffline();
    }
  }, [onOffline, onAuthFailed]);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 5000);
    return () => clearInterval(id);
  }, [refresh]);

  // Live attention events via /ws/events. On any event, refetch so the
  // list reflects the new status immediately rather than waiting for the
  // 5-second poll. WS drop falls back to polling silently.
  useWsEvents(refresh, onAuthFailed);

  // Lazy-load the provider list; fallback gives the user something to tap
  // even if the request 401s or the server hasn't woken up yet.
  useEffect(() => {
    listProviders()
      .then((p) => p.length > 0 && setProviders(p))
      .catch(() => {});
  }, []);

  const handleCreate = async (meshId: number, providerId: string) => {
    setPickerMeshId(null);
    setCreating(meshId);
    setError(null);
    try {
      const node = await createNode({ mesh_id: meshId, provider: providerId });
      setCreating(null);
      onOpenNode(node);
    } catch (e) {
      setCreating(null);
      setError((e as Error).message);
    }
  };

  // Archived nodes are history, not actionable work — hide them on mobile.
  const visibleNodes = (nodes ?? []).filter((n) => n.status !== "archived");

  const nodesByMesh = new Map<number, AgentNode[]>();
  for (const node of visibleNodes) {
    if (!nodesByMesh.has(node.mesh_id)) nodesByMesh.set(node.mesh_id, []);
    nodesByMesh.get(node.mesh_id)!.push(node);
  }

  // Mobile-only QoL: pin awaiting-input nodes at the top so attention
  // is one tap away no matter how many meshes you have configured.
  const attentionNodes = visibleNodes
    .filter((n) => n.status === "awaiting_input")
    .sort((a, b) => a.id - b.id);

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
          style={{ flex: 1, overflowY: "auto", padding: "0 8px 8px" }}
        >
          {attentionNodes.length > 0 && (
            <section data-testid="attention-section" style={{ marginBottom: 8 }}>
              <SectionHeading color="var(--amber)">
                Needs attention
              </SectionHeading>
              {attentionNodes.map((node) => (
                <NodeRow
                  key={`attn-${node.id}`}
                  node={node}
                  onClick={() => onOpenNode(node)}
                />
              ))}
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

export function NodeRow({ node, onClick }: { node: AgentNode; onClick: () => void }) {
  const meta = STATUS_META[node.status] ?? { color: "#555", label: node.status };
  const needsInput = node.status === "awaiting_input";
  return (
    <button
      onClick={onClick}
      data-testid={`node-${node.id}`}
      className="card"
      style={needsInput ? { borderColor: "rgba(255, 152, 0, 0.4)" } : undefined}
    >
      <ProviderIcon
        providerId={node.provider}
        withBackground
        chipTestId="node-avatar"
        className="h-5 w-5"
        title={node.provider}
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
          {node.provider} ·
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: meta.color,
              flexShrink: 0,
              display: "inline-block",
            }}
          />
          <span style={{ color: meta.color }}>{meta.label}</span>
        </div>
      </div>
      <div
        style={{
          width: 3,
          height: 34,
          borderRadius: 2,
          background: meta.color,
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
              <div
                style={{
                  width: 34,
                  height: 34,
                  borderRadius: 8,
                  background: native.color,
                  color: "#fff",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  fontSize: 14,
                  fontWeight: 700,
                  flexShrink: 0,
                }}
              >
                {native.icon}
              </div>
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
                <div
                  style={{
                    width: 28,
                    height: 28,
                    borderRadius: 6,
                    background: child.color,
                    color: "#fff",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    fontSize: 12,
                    fontWeight: 700,
                    flexShrink: 0,
                  }}
                >
                  {child.icon}
                </div>
                <span style={{ fontSize: 14, color: "var(--text)" }}>{child.label}</span>
              </button>
            ))}
          </div>
        );
      })}
    </Sheet>
  );
}

/// Open a /ws/events WebSocket and call `onEvent` for every message.
/// Auto-reconnects with simple backoff (1/2/4/8s) — losing the events
/// stream falls back to the 5-second poll, so an outage is invisible
/// beyond a brief lag.
function useWsEvents(onEvent: () => void, onAuthError: () => void) {
  const onEventRef = useRef(onEvent);
  onEventRef.current = onEvent;
  const onAuthErrorRef = useRef(onAuthError);
  onAuthErrorRef.current = onAuthError;

  useAsyncEffect((signal) => {
    let ws: WebSocket | null = null;
    let reconnectTimer: number | null = null;
    let attempt = 0;

    const connect = async () => {
      if (signal.aborted) return;
      // Mint a single-use WS ticket (issue #500) before opening the socket.
      let url: string;
      try {
        url = await eventsWsUrl();
      } catch (e) {
        // A 401/403 from the mint means the cookie is gone — re-minting would
        // loop forever, so surface it for re-auth instead of backing off.
        if (isAuthError(e)) {
          onAuthErrorRef.current();
          return;
        }
        scheduleReconnect();
        return;
      }
      if (signal.aborted) return;
      try {
        ws = new WebSocket(url);
      } catch {
        scheduleReconnect();
        return;
      }
      ws.onopen = () => {
        attempt = 0;
      };
      ws.onmessage = (e) => {
        try {
          const msg = JSON.parse(typeof e.data === "string" ? e.data : "") as EventMsg;
          if (msg && (msg.type === "attention-needed" || msg.type === "attention-cleared")) {
            onEventRef.current();
          }
        } catch {
          // Ignore non-JSON frames silently.
        }
      };
      ws.onclose = () => {
        if (!signal.aborted) scheduleReconnect();
      };
      ws.onerror = () => {
        if (!signal.aborted) scheduleReconnect();
      };
    };

    const scheduleReconnect = () => {
      const delays = [1000, 2000, 4000, 8000];
      const delay = delays[Math.min(attempt, delays.length - 1)];
      attempt++;
      reconnectTimer = window.setTimeout(connect, delay);
    };

    connect();
    return () => {
      if (reconnectTimer) window.clearTimeout(reconnectTimer);
      if (ws) {
        ws.onclose = null;
        ws.onerror = null;
        ws.close();
      }
    };
  }, []);
}
