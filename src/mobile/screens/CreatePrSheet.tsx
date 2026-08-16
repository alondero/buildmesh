import { useState } from "react";
import { createPr, isAuthError } from "../api";
import { Sheet } from "../ui";

type Props = {
  meshId: number;
  currentBranch: string;
  onClose: () => void;
  onCreated: (url: string) => void;
  onAuthFailed?: () => void;
};

export default function CreatePrSheet({
  meshId,
  currentBranch,
  onClose,
  onCreated,
  onAuthFailed,
}: Props) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [base, setBase] = useState("main");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    if (!title.trim()) {
      setError("Title required");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      const { url } = await createPr(meshId, title.trim(), body, base.trim());
      setSubmitting(false);
      onCreated(url);
    } catch (e) {
      setSubmitting(false);
      if (isAuthError(e)) {
        onAuthFailed?.();
        return;
      }
      setError((e as Error).message);
    }
  };

  return (
    <Sheet onClose={onClose} testId="create-pr-sheet">
      <h3
        style={{
          fontSize: 15,
          fontWeight: 600,
          color: "#fff",
          margin: 0,
          marginBottom: 12,
        }}
      >
        Create Pull Request
      </h3>
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
        <code style={{ color: "#ccc", overflowWrap: "anywhere" }}>
          {currentBranch}
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
          style={{
            width: 110,
            padding: "4px 8px",
            borderRadius: 6,
            background: "var(--surface-2)",
            fontFamily: '"JetBrains Mono", "Cascadia Code", monospace',
          }}
        />
      </p>
      <input
        placeholder="Title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        autoFocus
        data-testid="pr-title"
        className="field"
        style={{ background: "var(--surface-2)", marginBottom: 8 }}
      />
      <textarea
        placeholder="Body (optional)"
        value={body}
        onChange={(e) => setBody(e.target.value)}
        rows={4}
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
          onClick={onClose}
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
