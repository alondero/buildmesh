import { useState } from "react";
import {
  AgentNode,
  GitStatusEntry,
  GitSummary,
  ghAuthOk,
  gitBranch,
  gitStatus,
  gitSummary,
  isAuthError,
} from "../api";
import { AppBar, CenterNote, PulseDots } from "../ui";
import { useAsyncEffect } from "../../hooks/useAsyncEffect";

type Props = {
  node: AgentNode;
  onBack: () => void;
  onOpenDiff: (filePath: string) => void;
  onOpenPr: (currentBranch: string) => void;
  onAuthFailed?: () => void;
};

export default function ChangesScreen({
  node,
  onBack,
  onOpenDiff,
  onOpenPr,
  onAuthFailed,
}: Props) {
  const [status, setStatus] = useState<GitStatusEntry[] | null>(null);
  const [summary, setSummary] = useState<GitSummary | null>(null);
  const [branch, setBranch] = useState<string | null>(null);
  const [ghOk, setGhOk] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Bump to force the load effect to re-run. The previous run's signal
  // is aborted by useAsyncEffect's cleanup, so any in-flight Promise
  // drops its setState instead of clobbering the new run's result.
  const [reloadKey, setReloadKey] = useState(0);

  useAsyncEffect((signal) => {
    setError(null);
    Promise.all([
      gitStatus(node.id),
      gitSummary(node.id),
      gitBranch(node.id)
        .then((b) => b.branch)
        .catch((e) => {
          if (isAuthError(e)) throw e;
          return "";
        }),
      ghAuthOk().catch((e) => {
        if (isAuthError(e)) throw e;
        return false;
      }),
    ])
      .then(([s, sum, br, gh]) => {
        if (signal.aborted) return;
        setStatus(s);
        setSummary(sum);
        setBranch(br);
        setGhOk(gh);
      })
      .catch((e) => {
        if (signal.aborted) return;
        if (isAuthError(e)) {
          onAuthFailed?.();
          return;
        }
        setError((e as Error).message);
      });
  }, [node.id, onAuthFailed, reloadKey]);

  return (
    <div data-testid="changes-screen" className="screen">
      <AppBar
        onBack={onBack}
        backTestId="changes-back"
        title={node.name}
        subtitle={
          branch !== null ? (
            <>
              {branch || "(detached HEAD)"}
              {summary && (
                <>
                  {" · "}
                  <span style={{ color: "var(--green)" }}>+{summary.added}</span>{" "}
                  <span style={{ color: "var(--accent)" }}>~{summary.modified}</span>{" "}
                  <span style={{ color: "var(--red)" }}>-{summary.deleted}</span>
                </>
              )}
            </>
          ) : undefined
        }
      >
        <button
          onClick={() => setReloadKey((k) => k + 1)}
          aria-label="Refresh changes"
          data-testid="changes-refresh"
          className="chip-btn"
        >
          ↻
        </button>
        <button
          onClick={() => branch && onOpenPr(branch)}
          disabled={ghOk !== true || !branch}
          data-testid="open-create-pr"
          className="btn-primary"
          style={{ padding: "9px 14px", fontSize: 13 }}
        >
          PR
        </button>
      </AppBar>

      <div style={{ flex: 1, overflowY: "auto", padding: "8px 12px" }}>
        {ghOk === false && (
          <div
            className="banner warn"
            data-testid="gh-hint"
            style={{ borderRadius: 8, marginBottom: 8, border: "1px solid #3e3120" }}
          >
            PR creation disabled — the GitHub CLI isn't authenticated on the
            desktop.
          </div>
        )}
        {error && (
          <div
            data-testid="changes-error"
            style={{
              color: "var(--red)",
              fontSize: 13,
              padding: 16,
              textAlign: "center",
            }}
          >
            {error}
          </div>
        )}
        {!error && status === null && (
          <div style={{ padding: 24, textAlign: "center" }}>
            <PulseDots />
          </div>
        )}
        {status !== null && status.length === 0 && (
          <CenterNote testId="changes-empty">
            No changes — tree is clean.
          </CenterNote>
        )}
        {status && status.length > 0 && (
          <ul style={{ listStyle: "none", margin: 0, padding: 0 }}>
            {status.map((entry) => (
              <li key={entry.path}>
                <button
                  onClick={() => onOpenDiff(entry.path)}
                  data-testid={`changes-file-${entry.path}`}
                  className="card"
                  style={{ padding: "10px 12px", gap: 10 }}
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
                      // Keep the filename readable when the path is long —
                      // the tail (basename) is the part that matters.
                      direction: "rtl",
                      textAlign: "left",
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
