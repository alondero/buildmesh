import { useCallback, useEffect, useState } from "react";
import { createPr, isAuthError, prSource } from "../api";
import { Sheet } from "../ui";
import type { PrSource } from "../../types/generated/PrSource";

type Props = {
  meshId: number;
  /**
   * The node whose worktree is the PR source (issue #2024 rank 4 / #1567).
   * The previous mesh-only request could only resolve the mesh root, so it
   * published `main -> main` — or the root's unrelated feature branch.
   */
  nodeId: number;
  /**
   * Identity of THIS sheet instance. `onCreated` is scoped to it so a
   * request that completes after the sheet was dismissed (backdrop, back
   * gesture, auth recovery) cannot navigate the screen underneath.
   */
  sheetId: number;
  /** Branch the Changes screen showed — the fallback while the backend preview loads. */
  currentBranch: string;
  onClose: () => void;
  onCreated: (url: string, sheetId: number) => void;
  onAuthFailed?: () => void;
  /**
   * Publishes "a create request is in flight" to the app shell, which gates
   * the OS/browser Back route. Cancel and the backdrop are gated here and in
   * `Sheet`; without this one, back was a third, ungated escape hatch
   * (issue #2024 rank 9).
   */
  onBusyChange?: (busy: boolean) => void;
};

export default function CreatePrSheet({
  meshId,
  nodeId,
  sheetId,
  currentBranch,
  onClose,
  onCreated,
  onAuthFailed,
  onBusyChange,
}: Props) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [base, setBase] = useState("");
  const [source, setSource] = useState<PrSource | null>(null);
  const [sourceError, setSourceError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Preview from the same resolver the create path uses, so what the sheet
  // shows is what the backend will publish (issue #2024 rank 4).
  useEffect(() => {
    let live = true;
    prSource(meshId, nodeId)
      .then((s) => {
        if (!live) return;
        setSource(s);
        // Seed the editable base from the mesh's own base_ref — the client
        // never assumes `main`.
        setBase((current) => (current ? current : s.base_branch));
      })
      .catch((e) => {
        if (!live) return;
        if (isAuthError(e)) {
          onAuthFailed?.();
          return;
        }
        setSourceError((e as Error).message);
      });
    return () => {
      live = false;
    };
  }, [meshId, nodeId, onAuthFailed]);

  // Keep the shell's dismissal gate in step with the request's lifecycle.
  useEffect(() => {
    onBusyChange?.(submitting);
  }, [submitting, onBusyChange]);

  const submit = async () => {
    if (!title.trim()) {
      setError("Title required");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      const result = await createPr(
        meshId,
        nodeId,
        title.trim(),
        body,
        base.trim() || undefined,
        // Pin the previewed branch: if the worktree moved between preview
        // and submit, fail loudly rather than publish something unseen.
        source?.head_branch,
      );
      setSubmitting(false);
      onCreated(result.url, sheetId);
    } catch (e) {
      setSubmitting(false);
      if (isAuthError(e)) {
        onAuthFailed?.();
        return;
      }
      setError((e as Error).message);
    }
  };

  // Dismissal is owned by one flag so the backdrop, Cancel and the shell's
  // back route cannot disagree (issue #2024 rank 9).
  const requestClose = useCallback(() => {
    if (submitting) return;
    onClose();
  }, [submitting, onClose]);

  const headBranch = source?.head_branch ?? currentBranch;

  return (
    <Sheet
      onClose={requestClose}
      testId="create-pr-sheet"
      label="Create Pull Request"
      dismissible={!submitting}
    >
      <h3
        style={{
          fontSize: 15,
          fontWeight: 600,
          color: "var(--text)",
          margin: 0,
          marginBottom: 12,
        }}
      >
        Create Pull Request
      </h3>
      {sourceError && (
        <div
          style={{ color: "var(--red)", fontSize: 12, marginBottom: 8 }}
          data-testid="pr-source-error"
        >
          {sourceError}
        </div>
      )}
      <p
        style={{
          fontSize: 12,
          color: "var(--text-dim)",
          margin: 0,
          marginBottom: 12,
          display: "flex",
          alignItems: "center",
          gap: 6,
          flexWrap: "wrap",
        }}
      >
        From{" "}
        <code
          data-testid="pr-head-branch"
          style={{ color: "var(--text-dim)", overflowWrap: "anywhere" }}
        >
          {headBranch}
        </code>{" "}
        into
        <input
          value={base}
          onChange={(e) => setBase(e.target.value)}
          aria-label="Base Ref"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          className="field"
          disabled={submitting}
          style={{
            width: 110,
            padding: "4px 8px",
            borderRadius: 6,
            background: "var(--surface-2)",
            fontFamily:
              '"JetBrains Mono", "Fira Code", "Cascadia Code", "Consolas", monospace',
          }}
        />
      </p>
      <input
        placeholder="Title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        disabled={submitting}
        data-testid="pr-title"
        className="field"
        style={{ background: "var(--surface-2)", marginBottom: 8 }}
      />
      <textarea
        placeholder="Body (optional)"
        value={body}
        onChange={(e) => setBody(e.target.value)}
        rows={4}
        disabled={submitting}
        data-testid="pr-body"
        className="field"
        style={{
          background: "var(--surface-2)",
          marginBottom: 12,
          resize: "vertical",
        }}
      />
      {error && (
        <div
          style={{ color: "var(--red)", fontSize: 12, marginBottom: 8 }}
          data-testid="pr-error"
        >
          {error}
        </div>
      )}
      <div style={{ display: "flex", gap: 8 }}>
        <button
          onClick={requestClose}
          disabled={submitting}
          className="btn-ghost"
          style={{ flex: 1 }}
        >
          Cancel
        </button>
        <button
          onClick={submit}
          disabled={submitting || !title.trim()}
          data-testid="pr-submit"
          className="btn-primary"
          style={{ flex: 1 }}
        >
          {submitting ? "Creating…" : "Create PR"}
        </button>
      </div>
    </Sheet>
  );
}