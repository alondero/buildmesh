import { useEffect, useState } from "react";
import {
  AgentNode,
  DiscoveredSession,
  Mesh,
  discoverSessions,
  importAndResume,
} from "../api";
import { AppBar, CenterNote, PulseDots } from "../ui";

type Props = {
  mesh: Mesh;
  onBack: () => void;
  onResumed: (node: AgentNode) => void;
};

/// Newest activity first; sessions without a timestamp sink to the bottom.
export function sortSessions(sessions: DiscoveredSession[]): DiscoveredSession[] {
  return [...sessions].sort((a, b) =>
    (b.timestamp ?? "").localeCompare(a.timestamp ?? ""),
  );
}

export default function SessionsScreen({ mesh, onBack, onResumed }: Props) {
  const [sessions, setSessions] = useState<DiscoveredSession[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  // Resuming imports the session and spawns a real agent — expensive enough
  // that a stray tap shouldn't trigger it. First tap expands the card,
  // the explicit Resume button commits.
  const [selectedId, setSelectedId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    discoverSessions(mesh.id)
      .then((s) => {
        if (!cancelled) setSessions(sortSessions(s));
      })
      .catch((e) => {
        if (!cancelled) setError((e as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [mesh.id]);

  const resume = async (s: DiscoveredSession) => {
    setBusyId(s.session_id);
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
    <div data-testid="sessions-screen" className="screen">
      <AppBar onBack={onBack} title="Previous Sessions" subtitle={mesh.name} />
      <div style={{ padding: 12, paddingBottom: 0 }}>
        <input
          placeholder="Search by message, branch, worktree…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          data-testid="sessions-filter"
          className="field"
        />
      </div>
      <div style={{ flex: 1, overflowY: "auto", padding: 12 }}>
        {error && (
          <div style={{ color: "var(--red)", padding: 12, fontSize: 13 }}>
            {error}
          </div>
        )}
        {!error && sessions === null && (
          <div style={{ padding: 24, textAlign: "center" }}>
            <PulseDots />
          </div>
        )}
        {sessions !== null && filtered.length === 0 && (
          <CenterNote testId="sessions-empty">
            {sessions.length === 0
              ? "No discoverable sessions for this mesh."
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
              onClick={() => setSelectedId(open ? null : s.session_id)}
              data-testid={`session-${s.session_id}`}
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
                    data-testid={`session-resume-${s.session_id}`}
                    onClick={() => resume(s)}
                  >
                    {busy ? "Resuming…" : "Resume session"}
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
