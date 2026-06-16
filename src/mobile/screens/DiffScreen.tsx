import { useState } from "react";
import { AgentNode, DiffHunk, DiffResult, diffFile } from "../api";
import { AppBar, CenterNote, PulseDots } from "../ui";
import { useAsyncEffect } from "../../hooks/useAsyncEffect";

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

  useAsyncEffect((signal) => {
    diffFile(node.id, filePath)
      .then((d) => {
        if (signal.aborted) return;
        setDiff(d);
      })
      .catch((e) => {
        if (signal.aborted) return;
        setError((e as Error).message);
      });
  }, [node.id, filePath]);

  return (
    <div data-testid="diff-screen" className="screen">
      <AppBar
        onBack={onBack}
        backTestId="diff-back"
        title={
          <span
            style={{
              fontFamily: '"JetBrains Mono", "Cascadia Code", monospace',
              fontSize: 13,
              fontWeight: 400,
            }}
          >
            {filePath}
          </span>
        }
      />

      <div
        style={{
          flex: 1,
          overflow: "auto",
          background: "var(--bg)",
          padding: 8,
        }}
      >
        {error && (
          <div style={{ color: "var(--red)", padding: 16, fontSize: 13 }}>
            {error}
          </div>
        )}
        {!error && diff === null && (
          <div style={{ padding: 24, textAlign: "center" }}>
            <PulseDots />
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
      <CenterNote testId="diff-empty">No diff (file matches HEAD).</CenterNote>
    );
  }
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
      {diff.files[0].hunks.map((h, hi) => (
        <Hunk key={hi} hunk={h} />
      ))}
    </pre>
  );
}

function Hunk({ hunk }: { hunk: DiffHunk }) {
  return (
    <div style={{ marginBottom: 12 }}>
      {/* Hunk header keeps the reader oriented in the file — without it,
          consecutive hunks run together as one misleading block. */}
      <div
        data-testid="hunk-header"
        style={{
          color: "#7aa2c4",
          background: "rgba(33, 150, 243, 0.08)",
          padding: "3px 8px",
          fontSize: 11,
          borderRadius: 4,
          marginBottom: 2,
        }}
      >
        @@ -{hunk.old_start},{hunk.old_lines} +{hunk.new_start},{hunk.new_lines}{" "}
        @@
      </div>
      {hunk.lines.map((l, i) => {
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
    </div>
  );
}
