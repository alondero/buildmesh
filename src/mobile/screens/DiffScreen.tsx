import { useEffect, useState } from "react";
import { AgentNode, DiffResult, diffFile } from "../api";

type Props = {
  node: AgentNode;
  filePath: string;
  onBack: () => void;
};

// Unified-diff rendering on mobile: side-by-side is unreadable on a phone,
// so we synthesize a single column of -/+/  lines from the per-side hunks
// returned by the backend's commands::diff::diff_file_against_head.
export default function DiffScreen({ node, filePath, onBack }: Props) {
  const [diff, setDiff] = useState<DiffResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    diffFile(node.id, filePath)
      .then((d) => {
        if (!cancelled) setDiff(d);
      })
      .catch((e) => {
        if (!cancelled) setError((e as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [node.id, filePath]);

  return (
    <div
      data-testid="diff-screen"
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
          data-testid="diff-back"
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
        <span
          style={{
            fontFamily: '"JetBrains Mono", "Cascadia Code", monospace',
            fontSize: 13,
            color: "#fff",
            flex: 1,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {filePath}
        </span>
      </div>

      <div
        style={{
          flex: 1,
          overflow: "auto",
          background: "#0f0f0f",
          padding: 8,
        }}
      >
        {error && (
          <div style={{ color: "#f44336", padding: 16, fontSize: 13 }}>
            {error}
          </div>
        )}
        {!error && diff === null && (
          <div style={{ color: "#666", padding: 16, fontSize: 13 }}>
            Loading diff…
          </div>
        )}
        {diff && <DiffBody diff={diff} />}
      </div>
    </div>
  );
}

function DiffBody({ diff }: { diff: DiffResult }) {
  if (diff.files.length === 0 || diff.files[0].hunks.length === 0) {
    return (
      <div
        data-testid="diff-empty"
        style={{ color: "#666", padding: 16, fontSize: 13 }}
      >
        No diff (file matches HEAD).
      </div>
    );
  }
  const lines = diff.files[0].hunks.flatMap((h) => h.lines);
  return (
    <pre
      data-testid="diff-body"
      style={{
        margin: 0,
        fontFamily: '"JetBrains Mono", "Cascadia Code", monospace',
        fontSize: 12,
        lineHeight: 1.4,
        color: "#ddd",
        whiteSpace: "pre",
        // Overflow-wrap intentionally OFF — long lines scroll horizontally
        // so users see exact bytes rather than artificial breaks.
      }}
    >
      {lines.map((l, i) => {
        const bg =
          l.line_type === "add"
            ? "rgba(76, 175, 80, 0.12)"
            : l.line_type === "remove"
            ? "rgba(244, 67, 54, 0.12)"
            : "transparent";
        const prefix =
          l.line_type === "add" ? "+" : l.line_type === "remove" ? "-" : " ";
        const prefixColor =
          l.line_type === "add"
            ? "#4caf50"
            : l.line_type === "remove"
            ? "#f44336"
            : "#666";
        return (
          <div
            key={i}
            style={{
              background: bg,
              padding: "0 4px",
              display: "flex",
              gap: 6,
            }}
          >
            <span style={{ color: prefixColor, width: 12, flexShrink: 0 }}>
              {prefix}
            </span>
            <span>{l.content}</span>
          </div>
        );
      })}
    </pre>
  );
}
