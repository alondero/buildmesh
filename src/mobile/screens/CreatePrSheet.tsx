import { useState } from "react";
import { createPr } from "../api";

type Props = {
  meshId: number;
  currentBranch: string;
  onClose: () => void;
  onCreated: (url: string) => void;
};

export default function CreatePrSheet({
  meshId,
  currentBranch,
  onClose,
  onCreated,
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
      setError((e as Error).message);
    }
  };

  return (
    <div
      data-testid="create-pr-sheet"
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 200,
        display: "flex",
        flexDirection: "column",
      }}
    >
      <div
        onClick={onClose}
        style={{ flex: 1, background: "rgba(0,0,0,0.6)" }}
      />
      <div
        style={{
          background: "#1a1a1a",
          borderRadius: "16px 16px 0 0",
          padding: 20,
          paddingBottom: "max(20px, env(safe-area-inset-bottom))",
        }}
      >
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
            color: "#777",
            margin: 0,
            marginBottom: 12,
          }}
        >
          From <code style={{ color: "#aaa" }}>{currentBranch}</code> into{" "}
          <input
            value={base}
            onChange={(e) => setBase(e.target.value)}
            style={{
              background: "#2a2a2a",
              border: "1px solid #444",
              borderRadius: 4,
              color: "#e0e0e0",
              padding: "2px 6px",
              fontSize: 12,
              width: 80,
              fontFamily:
                '"JetBrains Mono", "Cascadia Code", monospace',
            }}
          />
        </p>
        <input
          placeholder="Title"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          data-testid="pr-title"
          style={{
            width: "100%",
            background: "#2a2a2a",
            border: "1px solid #444",
            borderRadius: 6,
            padding: "10px 12px",
            color: "#e0e0e0",
            fontSize: 14,
            outline: "none",
            marginBottom: 8,
          }}
        />
        <textarea
          placeholder="Body (optional)"
          value={body}
          onChange={(e) => setBody(e.target.value)}
          rows={4}
          data-testid="pr-body"
          style={{
            width: "100%",
            background: "#2a2a2a",
            border: "1px solid #444",
            borderRadius: 6,
            padding: "10px 12px",
            color: "#e0e0e0",
            fontSize: 14,
            outline: "none",
            marginBottom: 12,
            resize: "vertical",
            fontFamily: "inherit",
          }}
        />
        {error && (
          <div
            style={{ color: "#f44336", fontSize: 12, marginBottom: 8 }}
            data-testid="pr-error"
          >
            {error}
          </div>
        )}
        <div style={{ display: "flex", gap: 8 }}>
          <button
            onClick={onClose}
            disabled={submitting}
            style={{
              flex: 1,
              background: "transparent",
              border: "1px solid #444",
              borderRadius: 6,
              padding: "10px 16px",
              color: "#aaa",
              fontSize: 14,
              cursor: "pointer",
            }}
          >
            Cancel
          </button>
          <button
            onClick={submit}
            disabled={submitting}
            data-testid="pr-submit"
            style={{
              flex: 1,
              background: submitting ? "#1565c0" : "#2196f3",
              border: "none",
              borderRadius: 6,
              padding: "10px 16px",
              color: "#fff",
              fontSize: 14,
              cursor: submitting ? "wait" : "pointer",
            }}
          >
            {submitting ? "Creating…" : "Create PR"}
          </button>
        </div>
      </div>
    </div>
  );
}
