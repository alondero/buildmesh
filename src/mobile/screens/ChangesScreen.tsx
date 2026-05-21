import { useEffect, useState } from "react";
import {
  AgentNode,
  GitStatusEntry,
  GitSummary,
  ghAuthOk,
  gitBranch,
  gitStatus,
  gitSummary,
} from "../api";

type Props = {
  node: AgentNode;
  onBack: () => void;
  onOpenDiff: (filePath: string) => void;
  onOpenPr: (currentBranch: string) => void;
};

export default function ChangesScreen({
  node,
  onBack,
  onOpenDiff,
  onOpenPr,
}: Props) {
  const [status, setStatus] = useState<GitStatusEntry[] | null>(null);
  const [summary, setSummary] = useState<GitSummary | null>(null);
  const [branch, setBranch] = useState<string | null>(null);
  const [ghOk, setGhOk] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      gitStatus(node.id),
      gitSummary(node.id),
      gitBranch(node.id).then((b) => b.branch).catch(() => ""),
      ghAuthOk().catch(() => false),
    ])
      .then(([s, sum, br, gh]) => {
        if (cancelled) return;
        setStatus(s);
        setSummary(sum);
        setBranch(br);
        setGhOk(gh);
      })
      .catch((e) => {
        if (!cancelled) setError((e as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [node.id]);

  return (
    <div
      data-testid="changes-screen"
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
          data-testid="changes-back"
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
          <div
            style={{
              fontSize: 14,
              fontWeight: 600,
              color: "#fff",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {node.name}
          </div>
          {branch !== null && (
            <div
              style={{
                fontSize: 11,
                color: "#666",
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {branch || "(detached HEAD)"}
              {summary && (
                <>
                  {" · "}
                  <span style={{ color: "#4caf50" }}>+{summary.added}</span>{" "}
                  <span style={{ color: "#2196f3" }}>~{summary.modified}</span>{" "}
                  <span style={{ color: "#f44336" }}>-{summary.deleted}</span>
                </>
              )}
            </div>
          )}
        </div>
        <button
          onClick={() => branch && onOpenPr(branch)}
          disabled={ghOk !== true || !branch}
          data-testid="open-create-pr"
          style={{
            background: ghOk ? "#2196f3" : "#333",
            border: "none",
            borderRadius: 6,
            padding: "8px 12px",
            color: ghOk ? "#fff" : "#666",
            fontSize: 13,
            cursor: ghOk ? "pointer" : "not-allowed",
          }}
          title={ghOk ? "" : "gh CLI not authenticated on host"}
        >
          PR
        </button>
      </div>

      <div style={{ flex: 1, overflowY: "auto", padding: "8px 12px" }}>
        {error && (
          <div
            data-testid="changes-error"
            style={{
              color: "#f44336",
              fontSize: 13,
              padding: 16,
              textAlign: "center",
            }}
          >
            {error}
          </div>
        )}
        {!error && status === null && (
          <div style={{ color: "#666", padding: 16, textAlign: "center" }}>
            Loading changes…
          </div>
        )}
        {status !== null && status.length === 0 && (
          <div
            data-testid="changes-empty"
            style={{ color: "#555", padding: 24, textAlign: "center", fontSize: 13 }}
          >
            No changes — tree is clean.
          </div>
        )}
        {status && status.length > 0 && (
          <ul style={{ listStyle: "none", margin: 0, padding: 0 }}>
            {status.map((entry) => (
              <li key={entry.path} style={{ marginBottom: 4 }}>
                <button
                  onClick={() => onOpenDiff(entry.path)}
                  data-testid={`changes-file-${entry.path}`}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 10,
                    width: "100%",
                    padding: "10px 12px",
                    background: "#1a1a1a",
                    border: "1px solid transparent",
                    borderRadius: 8,
                    cursor: "pointer",
                    textAlign: "left",
                    color: "inherit",
                  }}
                >
                  <StatusBadge code={entry.status} />
                  <span
                    style={{
                      fontFamily:
                        '"JetBrains Mono", "Cascadia Code", monospace',
                      fontSize: 13,
                      color: "#ddd",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                      flex: 1,
                    }}
                  >
                    {entry.path}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function StatusBadge({ code }: { code: string }) {
  const first = code.trim().charAt(0).toUpperCase();
  const color =
    first === "A" || first === "?"
      ? "#4caf50"
      : first === "D"
      ? "#f44336"
      : first === "R"
      ? "#9c27b0"
      : "#2196f3";
  const letter = first === "?" ? "U" : first; // "U"ntracked reads better
  return (
    <span
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        width: 22,
        height: 22,
        borderRadius: 4,
        background: color,
        color: "#fff",
        fontSize: 11,
        fontWeight: 700,
        flexShrink: 0,
      }}
    >
      {letter}
    </span>
  );
}
