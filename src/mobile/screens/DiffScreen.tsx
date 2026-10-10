import { useState } from "react";
import { AgentNode, DiffHunk, DiffResult, diffFile, isAuthError } from "../api";
import { AppBar, CenterNote, PulseDots } from "../ui";
import { useAsyncEffect } from "../../hooks/useAsyncEffect";

type Props = {
  node: AgentNode;
  filePath: string;
  onBack: () => void;
  onAuthFailed?: () => void;
};

// Unified-diff rendering on mobile: side-by-side is unreadable on a phone,
// so we synthesize a single column of -/+/  lines from the per-side hunks
// returned by the backend's commands::diff::diff_file_against_head.
export default function DiffScreen({
  node,
  filePath,
  onBack,
  onAuthFailed,
}: Props) {
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
        if (isAuthError(e)) {
          onAuthFailed?.();
          return;
        }
        setError((e as Error).message);
      });
  }, [node.id, filePath, onAuthFailed]);

  return (
    <div data-testid="diff-screen" className="screen">
      <AppBar
        onBack={onBack}
        backTestId="diff-back"
        title={
          <span
            style={{
              fontFamily:
                '"JetBrains Mono", "Fira Code", "Cascadia Code", "Consolas", monospace',
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
  const file = diff.files[0];
  if (file === undefined || file.hunks.length === 0) {
    return (
      <CenterNote testId="diff-empty">{explainHunklessDiff(diff)}</CenterNote>
    );
  }
  return (
    <pre
      data-testid="diff-body"
      style={{
        margin: 0,
        fontFamily: '"JetBrains Mono", "Fira Code", "Cascadia Code", "Consolas", monospace',
        fontSize: 12,
        lineHeight: 1.4,
        color: "var(--text)",
        whiteSpace: "pre",
        // Overflow-wrap intentionally OFF — long lines scroll horizontally
        // so users see exact bytes rather than artificial breaks.
      }}
    >
      {file.hunks.map((h, hi) => (
        <Hunk key={hi} hunk={h} />
      ))}
    </pre>
  );
}

/**
 * Why there is no text diff to show.
 *
 * The backend already distinguishes the cases on the wire (`binary`,
 * `status`, `old_path`, `additions`/`deletions`) — the mobile client used to
 * hand-declare `FileDiff` without those fields, so every one of them looked
 * like "the file matches HEAD". That is wrong for a modified PNG, a
 * rename-only change, or a mode-only change, all of which legitimately
 * produce zero text hunks while being real changes.
 *
 * The baseline is the node's merge base with its mesh `base_ref` (ADR 0005),
 * never "HEAD" — the copy says "since this branch started" so the wording
 * cannot drift away from what `diff_node_file_against_base` actually diffs.
 */
function explainHunklessDiff(diff: DiffResult): string {
  const file = diff.files[0];
  if (file === undefined) {
    return "No changes to this file since the branch point.";
  }
  if (file.binary) {
    return "Binary file changed — no text diff to show.";
  }
  const renamed = file.status === "renamed" || file.old_path !== null;
  if (renamed && file.additions === 0 && file.deletions === 0) {
    return file.old_path
      ? `Renamed from ${file.old_path} — contents unchanged.`
      : "Renamed — contents unchanged.";
  }
  if (file.additions === 0 && file.deletions === 0) {
    return "Changed, but no text lines differ (metadata only).";
  }
  return "No text diff to show for this file.";
}

function Hunk({ hunk }: { hunk: DiffHunk }) {
  return (
    <div style={{ marginBottom: 12 }}>
      {/* Hunk header keeps the reader oriented in the file — without it,
          consecutive hunks run together as one misleading block. */}
      <div
        data-testid="hunk-header"
        style={{
          color: "var(--accent)",
          background: "var(--accent-glow)",
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
            ? "var(--green-dim)"
            : l.line_type === "remove"
            ? "var(--red-dim)"
            : "transparent";
        const prefix =
          l.line_type === "add" ? "+" : l.line_type === "remove" ? "-" : " ";
        const prefixColor =
          l.line_type === "add"
            ? "var(--green)"
            : l.line_type === "remove"
            ? "var(--red)"
            : "var(--text-faint)";
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
