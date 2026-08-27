import { useState } from "react";
import {
  AgentNode,
  ArchivedAgentNode,
  Mesh,
  discoverAgentNodes,
  importAndResume,
  isAuthError,
} from "../api";
import { AppBar, CenterNote, PulseDots } from "../ui";
import { useAsyncEffect } from "../../hooks/useAsyncEffect";
import { formatRelativeAge } from "../../lib/time";

type Props = {
  mesh: Mesh;
  onBack: () => void;
  onResumed: (node: AgentNode) => void;
  onAuthFailed?: () => void;
};

/// Newest activity first; nodes without a timestamp sink to the bottom.
export function sortAgentNodes(nodes: ArchivedAgentNode[]): ArchivedAgentNode[] {
  return [...nodes].sort((a, b) =>
    (b.timestamp ?? "").localeCompare(a.timestamp ?? ""),
  );
}

export default function ArchivedNodesScreen({
  mesh,
  onBack,
  onResumed,
  onAuthFailed,
}: Props) {
  const [nodes, setNodes] = useState<ArchivedAgentNode[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  // Resuming imports the node and spawns a real agent — expensive enough
  // that a stray tap shouldn't trigger it. First tap expands the card,
  // the explicit Resume button commits.
  const [selectedId, setSelectedId] = useState<string | null>(null);

  useAsyncEffect((signal) => {
    discoverAgentNodes(mesh.id)
      .then((s) => {
        if (signal.aborted) return;
        setNodes(sortAgentNodes(s));
      })
      .catch((e) => {
        if (signal.aborted) return;
        if (isAuthError(e)) {
          onAuthFailed?.();
          return;
        }
        setError((e as Error).message);
      });
  }, [mesh.id, onAuthFailed]);

  const resume = async (s: ArchivedAgentNode) => {
    setBusyId(s.session_id);
    setError(null);
    try {
      const node = await importAndResume(mesh.id, s);
      setBusyId(null);
      onResumed(node);
    } catch (e) {
      setBusyId(null);
      if (isAuthError(e)) {
        onAuthFailed?.();
        return;
      }
      setError((e as Error).message);
    }
  };

  const filtered = (nodes ?? []).filter((s) => {
    if (!filter.trim()) return true;
    const q = filter.toLowerCase();
    return (
      (s.first_message ?? "").toLowerCase().includes(q) ||
      (s.branch ?? "").toLowerCase().includes(q) ||
      (s.worktree_name ?? "").toLowerCase().includes(q)
    );
  });

  return (
    <div data-testid="discovered-nodes-screen" className="screen">
      <AppBar onBack={onBack} title="Archive" subtitle={mesh.name} />
      <div style={{ padding: 12, paddingBottom: 0 }}>
        <input
          placeholder="Search by message, branch, worktree…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          data-testid="discovered-nodes-filter"
          className="field"
        />
      </div>
      <div style={{ flex: 1, overflowY: "auto", padding: 12 }}>
        {error && (
          <div style={{ color: "var(--red)", padding: 12, fontSize: 13 }}>
            {error}
          </div>
        )}
        {!error && nodes === null && (
          <div style={{ padding: 24, textAlign: "center" }}>
            <PulseDots />
          </div>
        )}
        {nodes !== null && filtered.length === 0 && (
          <CenterNote testId="discovered-nodes-empty">
            {nodes.length === 0
              ? "No discoverable nodes for this mesh."
              : "No matches."}
          </CenterNote>
        )}
        {filtered.map((s) => {
          const open = selectedId === s.session_id;
          const busy = busyId === s.session_id;
          return (
            <div
              key={s.session_id}
              role="button"
              tabIndex={0}
              // Issue #1259 — mirror native <button> activation keys for
              // the role="button" wrapper so keyboard / switch-access users
              // can actually expand the card (WCAG 2.1.1, 4.1.2).
              // preventDefault stops Space from scrolling the page between
              // repeated activations.
              onClick={() => setSelectedId(open ? null : s.session_id)}
              onKeyDown={(e) => {
                // Only respond when the card itself is the focused element.
                // The nested "Resume node" button handles its own keyboard
                // activation; without this guard, a bubbled keydown from it
                // would collapse the card on top of the spawn action.
                if (e.target !== e.currentTarget) return;
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setSelectedId(open ? null : s.session_id);
                }
              }}
              data-testid={`node-${s.session_id}`}
              className="card"
              style={{
                display: "block",
                borderColor: open ? "var(--border-strong)" : "transparent",
              }}
            >
              <div
                style={{
                  fontSize: 13,
                  color: "#fff",
                  fontWeight: 500,
                  marginBottom: 4,
                  overflow: "hidden",
                  ...(open
                    ? { whiteSpace: "pre-wrap" as const, overflowWrap: "anywhere" as const }
                    : { textOverflow: "ellipsis", whiteSpace: "nowrap" as const }),
                }}
              >
                {s.first_message?.trim() || "(no first message)"}
              </div>
              <div
                style={{
                  display: "flex",
                  gap: 10,
                  fontSize: 11,
                  color: "var(--text-faint)",
                  flexWrap: "wrap",
                }}
              >
                {s.branch && <span>⎇ {s.branch}</span>}
                {s.worktree_name && <span>wt: {s.worktree_name}</span>}
                {s.timestamp && (
                  <span style={{ marginLeft: "auto" }}>
                    {timeAgo(s.timestamp)}
                  </span>
                )}
              </div>
              {open && (
                <div
                  onClick={(e) => e.stopPropagation()}
                  style={{ marginTop: 12 }}
                >
                  <button
                    className="btn-primary"
                    style={{ width: "100%" }}
                    disabled={busyId !== null}
                    data-testid={`node-resume-${s.session_id}`}
                    onClick={() => resume(s)}
                  >
                    {busy ? "Resuming…" : "Resume node"}
                  </button>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function timeAgo(iso: string): string {
  return formatRelativeAge(new Date(iso), new Date());
}
