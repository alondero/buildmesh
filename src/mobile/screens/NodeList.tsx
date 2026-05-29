import { useCallback, useEffect, useRef, useState } from "react";
import {
  AgentNode,
  EventMsg,
  Mesh,
  NodeStatus,
  Provider,
  createNode,
  eventsWsUrl,
  listMeshes,
  listNodes,
  listProviders,
} from "../api";

type Props = {
  onOpenNode: (node: AgentNode) => void;
  onOpenSessions: (mesh: Mesh) => void;
  onOpenIssues: (mesh: Mesh) => void;
  onOffline: () => void;
};

const STATUS_COLORS: Record<NodeStatus, string> = {
  idle: "#2196f3",
  running: "#4caf50",
  suspended: "#9e9e9e",
  error: "#f44336",
  awaiting_input: "#ff9800",
  archived: "#555",
};

const FALLBACK_PROVIDERS: Provider[] = [
  { id: "anthropic", label: "Anthropic (Claude)", color: "#1d7cfc", icon: "A" },
  { id: "minimax", label: "MiniMax", color: "#6366f1", icon: "M" },
  { id: "agy", label: "Antigravity CLI", color: "#10b981", icon: "G" },
  { id: "opencode", label: "OpenCode", color: "#f59e0b", icon: "O" },
];

export default function NodeList({
  onOpenNode,
  onOpenSessions,
  onOpenIssues,
  onOffline,
}: Props) {
  const [meshes, setMeshes] = useState<Mesh[] | null>(null);
  const [nodes, setNodes] = useState<AgentNode[] | null>(null);
  const [pickerMeshId, setPickerMeshId] = useState<number | null>(null);
  const [providers, setProviders] = useState<Provider[]>(FALLBACK_PROVIDERS);
  const [creating, setCreating] = useState<number | null>(null);
  const [meshActions, setMeshActions] = useState<Mesh | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [m, n] = await Promise.all([listMeshes(), listNodes()]);
      setMeshes(m);
      setNodes(n);
    } catch {
      onOffline();
    }
  }, [onOffline]);

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
    try {
      const node = await createNode({ mesh_id: meshId, provider: providerId });
      setCreating(null);
      onOpenNode(node);
    } catch (e) {
      setCreating(null);
      alert((e as Error).message);
    }
  };

  if (meshes === null) {
    return (
      <div
        data-testid="nodelist-loading"
        style={{ padding: 24, color: "#666", textAlign: "center" }}
      >
        Loading…
      </div>
    );
  }

  if (meshes.length === 0) {
    return (
      <div style={{ padding: 24, color: "#666", textAlign: "center" }}>
        No meshes configured. Add a mesh on your desktop app to get started.
      </div>
    );
  }

  const nodesByMesh = new Map<number, AgentNode[]>();
  for (const node of nodes ?? []) {
    if (!nodesByMesh.has(node.mesh_id)) nodesByMesh.set(node.mesh_id, []);
    nodesByMesh.get(node.mesh_id)!.push(node);
  }

  // Mobile-only QoL: pin awaiting-input nodes at the top so attention
  // is one tap away no matter how many meshes you have configured.
  const attentionNodes = (nodes ?? [])
    .filter((n) => n.status === "awaiting_input")
    .sort((a, b) => a.id - b.id);

  return (
    <div
      data-testid="node-list"
      style={{ flex: 1, overflowY: "auto", padding: "0 8px 8px" }}
    >
      {attentionNodes.length > 0 && (
        <section
          data-testid="attention-section"
          style={{ marginBottom: 8 }}
        >
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
                color: "#ff9800",
                textTransform: "uppercase",
                letterSpacing: "0.05em",
                flex: 1,
              }}
            >
              Needs attention
            </span>
          </div>
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
                  color: "#555",
                  textTransform: "uppercase",
                  letterSpacing: "0.05em",
                  flex: 1,
                }}
              >
                {mesh.name}
              </span>
              <button
                onClick={() => setPickerMeshId(mesh.id)}
                aria-label={`New node in ${mesh.name}`}
                data-testid={`new-node-${mesh.id}`}
                style={{
                  width: 28,
                  height: 28,
                  borderRadius: 6,
                  background: "#2a2a2a",
                  border: "1px solid #444",
                  color: "#888",
                  fontSize: 18,
                  cursor: "pointer",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                }}
              >
                +
              </button>
              <button
                onClick={() => setMeshActions(mesh)}
                aria-label={`More actions for ${mesh.name}`}
                data-testid={`mesh-actions-${mesh.id}`}
                style={{
                  width: 28,
                  height: 28,
                  borderRadius: 6,
                  background: "#2a2a2a",
                  border: "1px solid #444",
                  color: "#888",
                  fontSize: 16,
                  cursor: "pointer",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                }}
              >
                ⋯
              </button>
            </div>

            {creating === mesh.id ? (
              <div
                style={{
                  fontSize: 12,
                  color: "#2196f3",
                  padding: "8px 12px 12px",
                }}
              >
                Creating node…
              </div>
            ) : meshNodes.length === 0 ? (
              <div
                style={{
                  fontSize: 12,
                  color: "#444",
                  padding: "8px 12px 12px",
                  fontStyle: "italic",
                }}
              >
                No nodes
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
    <div
      data-testid="mesh-actions-sheet"
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 100,
        display: "flex",
        alignItems: "flex-end",
      }}
    >
      <div
        onClick={onClose}
        style={{ position: "absolute", inset: 0, background: "rgba(0,0,0,0.6)" }}
      />
      <div
        style={{
          position: "relative",
          width: "100%",
          background: "#1a1a1a",
          borderRadius: "16px 16px 0 0",
          padding: 20,
          paddingBottom: "max(20px, env(safe-area-inset-bottom))",
          zIndex: 1,
        }}
      >
        <h3
          style={{
            fontSize: 13,
            fontWeight: 600,
            color: "#888",
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
      </div>
    </div>
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
      style={{
        display: "block",
        width: "100%",
        textAlign: "left",
        padding: "14px 16px",
        borderRadius: 10,
        background: "#2a2a2a",
        border: "1px solid transparent",
        marginBottom: 8,
        cursor: "pointer",
        color: "inherit",
      }}
    >
      <div style={{ fontSize: 14, fontWeight: 500, color: "#fff" }}>{label}</div>
      <div style={{ fontSize: 12, color: "#888", marginTop: 2 }}>{hint}</div>
    </button>
  );
}

function NodeRow({
  node,
  onClick,
}: {
  node: AgentNode;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      data-testid={`node-${node.id}`}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: 12,
        background: "#1a1a1a",
        border: "1px solid transparent",
        borderRadius: 8,
        marginBottom: 6,
        cursor: "pointer",
        width: "100%",
        textAlign: "left",
        color: "inherit",
      }}
    >
      <div
        style={{
          width: 32,
          height: 32,
          borderRadius: 6,
          background: providerColor(node.provider),
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          fontSize: 14,
          fontWeight: 700,
          color: "#fff",
          flexShrink: 0,
        }}
      >
        {providerIcon(node.provider)}
      </div>
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
            color: "#666",
            marginTop: 2,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {node.provider} · {node.status}
        </div>
      </div>
      <div
        style={{
          width: 3,
          height: 32,
          borderRadius: 2,
          background: STATUS_COLORS[node.status] ?? "#555",
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
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 100,
        display: "flex",
        alignItems: "flex-end",
      }}
      data-testid="provider-picker"
    >
      <div
        onClick={onCancel}
        style={{ position: "absolute", inset: 0, background: "rgba(0,0,0,0.6)" }}
      />
      <div
        style={{
          position: "relative",
          width: "100%",
          background: "#1a1a1a",
          borderRadius: "16px 16px 0 0",
          padding: 20,
          paddingBottom: "max(20px, env(safe-area-inset-bottom))",
          zIndex: 1,
        }}
      >
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
            style={{
              display: "flex",
              alignItems: "center",
              gap: 12,
              padding: "14px 16px",
              borderRadius: 10,
              background: "#2a2a2a",
              border: "1px solid transparent",
              marginBottom: 8,
              cursor: "pointer",
              width: "100%",
              textAlign: "left",
              color: "inherit",
            }}
          >
            <div
              style={{
                width: 32,
                height: 32,
                borderRadius: 6,
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
            <span style={{ fontSize: 15, color: "#e0e0e0" }}>{p.label}</span>
          </button>
        ))}
      </div>
    </div>
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

function providerIcon(provider: string): string {
  return ({ anthropic: "A", minimax: "M", agy: "G", opencode: "O" } as Record<
    string,
    string
  >)[provider] ?? "?";
}

function providerColor(provider: string): string {
  return ({
    anthropic: "#1d7cfc",
    minimax: "#6366f1",
    agy: "#10b981",
    opencode: "#f59e0b",
  } as Record<string, string>)[provider] ?? "#555";
}
