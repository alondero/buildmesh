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

type Props = {
  onOpenNode: (node: AgentNode) => void;
  onOpenSessions: (mesh: Mesh) => void;
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
};

const FALLBACK_PROVIDERS: Provider[] = [
  { id: "anthropic", label: "Anthropic (Claude)", color: "#1d7cfc", icon: "A" },
  { id: "minimax", label: "MiniMax", color: "#6366f1", icon: "M" },
  { id: "kimi", label: "Kimi", color: "#00c4c4", icon: "K" },
  { id: "agy", label: "Antigravity CLI", color: "#10b981", icon: "G" },
  { id: "opencode", label: "OpenCode", color: "#f59e0b", icon: "O" },
];

export default function NodeList({
  onOpenNode,
  onOpenSessions,
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
  useWsEvents(refresh);

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
          onOpenSessions={() => {
            const m = meshActions;
            setMeshActions(null);
            onOpenSessions(m);
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
  onOpenSessions,
  onOpenIssues,
}: {
  mesh: Mesh;
  onClose: () => void;
  onOpenSessions: () => void;
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
        onClick={onOpenSessions}
        testId="mesh-sheet-sessions"
        label="Previous Sessions"
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
      {providers.map((p) => (
        <button
          key={p.id}
          onClick={() => onPick(p)}
          data-testid={`provider-${p.id}`}
          className="card"
          style={{ background: "var(--surface-2)" }}
        >
          <div
            style={{
              width: 34,
              height: 34,
              borderRadius: 8,
              background: p.color,
              color: "#fff",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              fontSize: 14,
              fontWeight: 700,
              flexShrink: 0,
            }}
          >
            {p.icon}
          </div>
          <span style={{ fontSize: 15, color: "var(--text)" }}>{p.label}</span>
        </button>
      ))}
    </Sheet>
  );
}

/// Open a /ws/events WebSocket and call `onEvent` for every message.
/// Auto-reconnects with simple backoff (1/2/4/8s) — losing the events
/// stream falls back to the 5-second poll, so an outage is invisible
/// beyond a brief lag.
function useWsEvents(onEvent: () => void) {
  const onEventRef = useRef(onEvent);
  onEventRef.current = onEvent;

  useEffect(() => {
    let cancelled = false;
    let ws: WebSocket | null = null;
    let reconnectTimer: number | null = null;
    let attempt = 0;

    const connect = () => {
      if (cancelled) return;
      try {
        ws = new WebSocket(eventsWsUrl());
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
        if (!cancelled) scheduleReconnect();
      };
      ws.onerror = () => {
        if (!cancelled) scheduleReconnect();
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
      cancelled = true;
      if (reconnectTimer) window.clearTimeout(reconnectTimer);
      if (ws) {
        ws.onclose = null;
        ws.onerror = null;
        ws.close();
      }
    };
  }, []);
}
