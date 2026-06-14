import { useEffect, useState } from "react";
import {
  AgentNode,
  GitHubIssue,
  Mesh,
  listIssues,
  spawnFromIssue,
} from "../api";
import { AppBar, CenterNote, PulseDots } from "../ui";

type Props = {
  mesh: Mesh;
  onBack: () => void;
  onSpawned: (node: AgentNode) => void;
};

const BODY_PREVIEW_LIMIT = 600;

export default function IssuesScreen({ mesh, onBack, onSpawned }: Props) {
  const [issues, setIssues] = useState<GitHubIssue[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyIssue, setBusyIssue] = useState<number | null>(null);
  const [filter, setFilter] = useState("");
  // Spawning creates a worktree and starts a real agent — first tap expands
  // the issue (body preview, GitHub link), the explicit button commits.
  const [selectedIssue, setSelectedIssue] = useState<number | null>(null);

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

  const filtered = (issues ?? []).filter(
    (i) => !filter.trim() || i.title.toLowerCase().includes(filter.toLowerCase()),
  );

  return (
    <div data-testid="issues-screen" className="screen">
      <AppBar onBack={onBack} title="GitHub Issues" subtitle={mesh.name} />
      <div style={{ padding: 12, paddingBottom: 0 }}>
        <input
          placeholder="Filter by title…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          data-testid="issues-filter"
          className="field"
        />
      </div>
      <div style={{ flex: 1, overflowY: "auto", padding: 12 }}>
        {error && (
          <div style={{ color: "var(--red)", padding: 12, fontSize: 13 }}>
            {error}
          </div>
        )}
        {!error && issues === null && (
          <div style={{ padding: 24, textAlign: "center" }}>
            <PulseDots />
          </div>
        )}
        {issues !== null && filtered.length === 0 && (
          <CenterNote testId="issues-empty">
            {issues.length === 0
              ? "No open issues — or this mesh has no GitHub remote."
              : "No matches."}
          </CenterNote>
        )}
        {filtered.map((issue) => {
          const open = selectedIssue === issue.number;
          const busy = busyIssue === issue.number;
          const body = (issue.body ?? "").trim();
          return (
            <div
              key={issue.number}
              role="button"
              tabIndex={0}
              onClick={() => setSelectedIssue(open ? null : issue.number)}
              data-testid={`issue-${issue.number}`}
              className="card"
              style={{
                display: "block",
                borderColor: open ? "var(--border-strong)" : "transparent",
              }}
            >
              <div
                style={{
                  display: "flex",
                  gap: 8,
                  alignItems: "baseline",
                  marginBottom: 4,
                }}
              >
                <span style={{ color: "var(--text-dim)", fontSize: 12, flexShrink: 0 }}>
                  #{issue.number}
                </span>
                <span
                  style={{
                    fontSize: 13,
                    color: "#fff",
                    fontWeight: 500,
                    overflow: "hidden",
                    flex: 1,
                    ...(open
                      ? { whiteSpace: "normal" as const }
                      : { textOverflow: "ellipsis", whiteSpace: "nowrap" as const }),
                  }}
                >
                  {issue.title}
                </span>
              </div>
              {open && (
                <div onClick={(e) => e.stopPropagation()}>
                  {body && (
                    <div
                      data-testid={`issue-body-${issue.number}`}
                      style={{
                        marginTop: 10,
                        fontSize: 12,
                        lineHeight: 1.5,
                        color: "var(--text-dim)",
                        whiteSpace: "pre-wrap",
                        overflowWrap: "anywhere",
                        maxHeight: 180,
                        overflowY: "auto",
                        background: "var(--bg)",
                        borderRadius: 8,
                        padding: 10,
                      }}
                    >
                      {body.length > BODY_PREVIEW_LIMIT
                        ? body.slice(0, BODY_PREVIEW_LIMIT) + "…"
                        : body}
                    </div>
                  )}
                  <div
                    style={{
                      marginTop: 10,
                      display: "flex",
                      gap: 8,
                      alignItems: "center",
                    }}
                  >
                    <button
                      className="btn-primary"
                      style={{ flex: 1 }}
                      disabled={busyIssue !== null}
                      data-testid={`issue-spawn-${issue.number}`}
                      onClick={() => spawn(issue)}
                    >
                      {busy ? "Spawning agent…" : "Start agent on this issue"}
                    </button>
                    {/* No "View ↗" link: the Rust `GitHubIssue` struct doesn't
                        serialise an issue URL, so the generated type has no
                        `url` field. It returns automatically once the backend
                        widens the struct (see src-tauri/src/commands/pr.rs). */}
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
