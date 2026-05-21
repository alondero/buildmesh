import { useEffect, useState } from "react";
import {
  AgentNode,
  GitHubIssue,
  Mesh,
  listIssues,
  spawnFromIssue,
} from "../api";

type Props = {
  mesh: Mesh;
  onBack: () => void;
  onSpawned: (node: AgentNode) => void;
};

export default function IssuesScreen({ mesh, onBack, onSpawned }: Props) {
  const [issues, setIssues] = useState<GitHubIssue[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyIssue, setBusyIssue] = useState<number | null>(null);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    let cancelled = false;
    listIssues(mesh.id)
      .then((i) => {
        if (!cancelled) setIssues(i);
      })
      .catch((e) => {
        if (!cancelled) setError((e as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [mesh.id]);

  const spawn = async (issue: GitHubIssue) => {
    setBusyIssue(issue.number);
    setError(null);
    try {
      const node = await spawnFromIssue(mesh.id, issue);
      setBusyIssue(null);
      onSpawned(node);
    } catch (e) {
      setBusyIssue(null);
      setError((e as Error).message);
    }
  };

  const filtered = (issues ?? []).filter((i) =>
    !filter.trim() || i.title.toLowerCase().includes(filter.toLowerCase()),
  );

  return (
    <div
      data-testid="issues-screen"
      style={{ display: "flex", flexDirection: "column", flex: 1 }}
    >
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
          <div style={{ fontSize: 14, fontWeight: 600, color: "#fff" }}>
            GitHub Issues
          </div>
          <div style={{ fontSize: 11, color: "#666" }}>{mesh.name}</div>
        </div>
      </div>
      <input
        placeholder="Filter by title…"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        data-testid="issues-filter"
        style={{
          margin: 12,
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
        {!error && issues === null && (
          <div style={{ color: "#666", padding: 16, fontSize: 13, textAlign: "center" }}>
            Loading…
          </div>
        )}
        {issues !== null && filtered.length === 0 && (
          <div
            data-testid="issues-empty"
            style={{ color: "#555", padding: 24, textAlign: "center", fontSize: 13 }}
          >
            {issues.length === 0
              ? "No open issues — or this mesh has no GitHub remote."
              : "No matches."}
          </div>
        )}
        {filtered.map((issue) => (
          <button
            key={issue.number}
            onClick={() => spawn(issue)}
            disabled={busyIssue !== null}
            data-testid={`issue-${issue.number}`}
            style={{
              width: "100%",
              textAlign: "left",
              background: "#1a1a1a",
              border: "1px solid transparent",
              borderRadius: 8,
              padding: 12,
              marginBottom: 6,
              cursor: busyIssue ? "wait" : "pointer",
              color: "inherit",
            }}
          >
            <div
              style={{
                display: "flex",
                gap: 8,
                alignItems: "center",
                marginBottom: 4,
              }}
            >
              <span style={{ color: "#888", fontSize: 12 }}>#{issue.number}</span>
              <span
                style={{
                  fontSize: 13,
                  color: "#fff",
                  fontWeight: 500,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  flex: 1,
                }}
              >
                {issue.title}
              </span>
            </div>
            {issue.labels.length > 0 && (
              <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                {issue.labels.map((l) => (
                  <span
                    key={l}
                    style={{
                      fontSize: 10,
                      color: "#aaa",
                      background: "#2a2a2a",
                      padding: "2px 6px",
                      borderRadius: 4,
                    }}
                  >
                    {l}
                  </span>
                ))}
              </div>
            )}
            {busyIssue === issue.number && (
              <div style={{ color: "#2196f3", fontSize: 11, marginTop: 6 }}>
                Spawning agent…
              </div>
            )}
          </button>
        ))}
      </div>
    </div>
  );
}
