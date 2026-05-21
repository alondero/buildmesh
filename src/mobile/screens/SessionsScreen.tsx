import { useEffect, useState } from "react";
import {
  AgentNode,
  DiscoveredSession,
  Mesh,
  discoverSessions,
  importAndResume,
} from "../api";

type Props = {
  mesh: Mesh;
  onBack: () => void;
  onResumed: (node: AgentNode) => void;
};

export default function SessionsScreen({ mesh, onBack, onResumed }: Props) {
  const [sessions, setSessions] = useState<DiscoveredSession[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    let cancelled = false;
    discoverSessions(mesh.id)
      .then((s) => {
        if (!cancelled) setSessions(s);
      })
      .catch((e) => {
        if (!cancelled) setError((e as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [mesh.id]);

  const resume = async (s: DiscoveredSession) => {
    setBusyId(s.cli_session_id);
    setError(null);
    try {
      const node = await importAndResume(mesh.id, s);
      setBusyId(null);
      onResumed(node);
    } catch (e) {
      setBusyId(null);
      setError((e as Error).message);
    }
  };

  const filtered = (sessions ?? []).filter((s) => {
    if (!filter.trim()) return true;
    const q = filter.toLowerCase();
    return (
      (s.first_message ?? "").toLowerCase().includes(q) ||
      (s.branch ?? "").toLowerCase().includes(q) ||
      (s.worktree_name ?? "").toLowerCase().includes(q)
    );
  });

  return (
    <div
      data-testid="sessions-screen"
      style={{ display: "flex", flexDirection: "column", flex: 1 }}
    >
      <Header title="Previous Sessions" subtitle={mesh.name} onBack={onBack} />
      <input
        placeholder="Search by message, branch, worktree…"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        data-testid="sessions-filter"
        style={{
          margin: "12px",
          background: "#1a1a1a",
          border: "1px solid #333",
          borderRadius: 6,
          padding: "10px 12px",
          color: "#e0e0e0",
          fontSize: 14,
          outline: "none",
        }}
      />
      <div style={{ flex: 1, overflowY: "auto", padding: "0 12px 12px" }}>
        {error && (
          <div style={{ color: "#f44336", padding: 12, fontSize: 13 }}>
            {error}
          </div>
        )}
        {!error && sessions === null && (
          <div style={{ color: "#666", padding: 16, fontSize: 13, textAlign: "center" }}>
            Loading…
          </div>
        )}
        {sessions !== null && filtered.length === 0 && (
          <div
            data-testid="sessions-empty"
            style={{ color: "#555", padding: 24, textAlign: "center", fontSize: 13 }}
          >
            {sessions.length === 0
              ? "No discoverable sessions for this mesh."
              : "No matches."}
          </div>
        )}
        {filtered.map((s) => (
          <button
            key={s.cli_session_id}
            onClick={() => resume(s)}
            disabled={busyId !== null}
            data-testid={`session-${s.cli_session_id}`}
            style={{
              width: "100%",
              textAlign: "left",
              background: "#1a1a1a",
              border: "1px solid transparent",
              borderRadius: 8,
              padding: 12,
              marginBottom: 6,
              cursor: busyId ? "wait" : "pointer",
              color: "inherit",
            }}
          >
            <div
              style={{
                fontSize: 13,
                color: "#fff",
                fontWeight: 500,
                marginBottom: 4,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {s.first_message?.trim() || "(no first message)"}
            </div>
            <div
              style={{
                display: "flex",
                gap: 10,
                fontSize: 11,
                color: "#666",
              }}
            >
              {s.branch && <span>⎇ {s.branch}</span>}
              {s.worktree_name && <span>wt: {s.worktree_name}</span>}
              <span>{s.provider}</span>
              {s.last_active_at && (
                <span style={{ marginLeft: "auto" }}>{timeAgo(s.last_active_at)}</span>
              )}
            </div>
            {busyId === s.cli_session_id && (
              <div style={{ color: "#2196f3", fontSize: 11, marginTop: 6 }}>
                Resuming…
              </div>
            )}
          </button>
        ))}
      </div>
    </div>
  );
}

function Header({
  title,
  subtitle,
  onBack,
}: {
  title: string;
  subtitle: string;
  onBack: () => void;
}) {
  return (
    <div
      style={{
        background: "#1a1a1a",
        padding: "10px 12px",
        display: "flex",
        alignItems: "center",
        gap: 12,
        borderBottom: "1px solid #333",
      }}
    >
      <button
        onClick={onBack}
        aria-label="Back"
        style={{
          background: "transparent",
          border: "none",
          color: "#aaa",
          fontSize: 22,
          cursor: "pointer",
          padding: 4,
          lineHeight: 1,
        }}
      >
        ←
      </button>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 14, fontWeight: 600, color: "#fff" }}>{title}</div>
        <div
          style={{
            fontSize: 11,
            color: "#666",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {subtitle}
        </div>
      </div>
    </div>
  );
}

function timeAgo(iso: string): string {
  const date = new Date(iso);
  const diffMs = Date.now() - date.getTime();
  const min = Math.floor(diffMs / 60000);
  if (min < 1) return "just now";
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h ago`;
  const d = Math.floor(hr / 24);
  return `${d}d ago`;
}
