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
} from "../api";
import { ProviderIcon } from "../../components/Providers/ProviderIcon";
import { AppBar, CenterNote, PulseDots, Sheet } from "../ui";
import { useWsEvents } from "../useWsEvents";
import { useVisibilityPolling } from "../useVisibilityPolling";
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
                  providers={providers}
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
