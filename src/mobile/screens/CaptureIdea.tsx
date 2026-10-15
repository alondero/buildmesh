import { useEffect, useRef, useState } from "react";
import {
  AgentNode,
  Mesh,
  Provider,
  createNode,
  isAuthError,
  newOperationKey,
} from "../api";

const DRAFT_KEY = "buildmesh_mobile_idea";
const DESTINATION_KEY = "buildmesh_mobile_idea_destination";
/**
 * Issue #2331 — the retry-safe operation key, stored beside the draft so a
 * retry after a lost response reuses the same key. `fingerprint` records which
 * submission it belongs to: editing the idea or its destination starts a new
 * submission, which must get a new key (the server rejects a key reused with
 * different content as a conflict).
 */
const OPERATION_KEY = "buildmesh_mobile_idea_operation";

type StoredOperation = { fingerprint: string; key: string };

/** The stored key for this exact submission, or `null` for a new one. */
function readOperationKey(fingerprint: string): string | null {
  try {
    const raw = localStorage.getItem(OPERATION_KEY);
    if (!raw) return null;
    const stored = JSON.parse(raw) as StoredOperation;
    return stored?.fingerprint === fingerprint ? stored.key : null;
  } catch {
    return null;
  }
}

function writeOperationKey(fingerprint: string, key: string): void {
  try {
    localStorage.setItem(OPERATION_KEY, JSON.stringify({ fingerprint, key }));
  } catch {
    // A private-mode browser without storage simply loses retry safety; the
    // submission still works.
  }
}

/** Forget the operation key once the submission has landed either way. */
function clearOperationKey(): void {
  try {
    localStorage.removeItem(OPERATION_KEY);
  } catch {
    /* Best-effort: a stale key only affects a later identical submission. */
  }
}

function clearDraft(): void {
  try {
    localStorage.removeItem(DRAFT_KEY);
  } catch {
    /* The submission already landed server-side. */
  }
}

function storedDestination(field: "meshId" | "providerId"): string {
  try {
    const destination = JSON.parse(
      localStorage.getItem(DESTINATION_KEY) ?? "{}",
    );
    return typeof destination?.[field] === "string" ? destination[field] : "";
  } catch {
    return "";
  }
}

export default function CaptureIdea({
  meshes,
  providers,
  onStarted,
  onAuthFailed,
  onBusyChange,
}: {
  meshes: Mesh[];
  providers: Provider[];
  onStarted: (node: AgentNode) => void;
  onAuthFailed: () => void;
  onBusyChange?: (busy: boolean) => void;
}) {
  const [text, setText] = useState(() => {
    try {
      return localStorage.getItem(DRAFT_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [meshId, setMeshId] = useState(() => storedDestination("meshId"));
  const [providerId, setProviderId] = useState(() =>
    storedDestination("providerId"),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /**
   * A node the server committed but could not start (issue #2331). Held rather
   * than navigated to immediately, so the reason stays on screen next to a way
   * through to the node the user actually owns.
   */
  const [committed, setCommitted] = useState<AgentNode | null>(null);
  const [stored, setStored] = useState(true);
  const active = useRef(true);
  const submitting = useRef(false);
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);
  useEffect(() => {
    try {
      localStorage.setItem(
        DESTINATION_KEY,
        JSON.stringify({ meshId, providerId }),
      );
      if (text) localStorage.setItem(DRAFT_KEY, text);
      else localStorage.removeItem(DRAFT_KEY);
      setStored(true);
    } catch {
      setStored(false);
    }
  }, [text, meshId, providerId]);
  const choices = providers.filter(
    (p) =>
      p.capabilities?.supports_prefill &&
      !p.unavailable_reason &&
      (!p.is_proxied || p.configuration),
  );
  const selectedMesh = meshId || String(meshes[0]?.id ?? "");
  const selectedProvider = providerId || choices[0]?.id || "";
  const bytes = new TextEncoder().encode(text).length;
  const valid =
    text.trim().length > 0 &&
    bytes <= 16000 &&
    meshes.some((m) => String(m.id) === selectedMesh) &&
    choices.some((p) => p.id === selectedProvider);

  async function start() {
    if (!valid || submitting.current) return;
    submitting.current = true;
    onBusyChange?.(true);
    setBusy(true);
    setError(null);
    setCommitted(null);
    try {
      const provider = choices.find((p) => p.id === selectedProvider)!;
      // Issue #2331: one operation key per *submission*, reused for every
      // retry of that submission. A retry after a lost response therefore
      // returns the same node instead of creating a second one. The key is
      // regenerated only when the idea or its destination changes, because a
      // key reused with different content is rejected as a conflict.
      const fingerprint = `${selectedMesh}|${selectedProvider}|${text}`;
      const draft = readOperationKey(fingerprint);
      const operationKey = draft ?? newOperationKey();
      writeOperationKey(fingerprint, operationKey);

      const outcome = await createNode({
        mesh_id: Number(selectedMesh),
        provider: provider.configuration?.spawn_option_id ?? provider.id,
        configuration_id: provider.configuration?.id,
        prompt: text,
        operation_key: operationKey,
      });
      if (!active.current) return;

      switch (outcome.outcome) {
        case "active":
          clearOperationKey();
          clearDraft();
          setText("");
          onStarted(outcome.node);
          return;
        case "start_pending":
        case "start_failed": {
          // The node row is committed. `onStarted` unmounts this screen, so
          // navigating immediately would throw away the reason — which is the
          // part the user needs in order to fix it. Stay here, say what
          // happened, and offer the node instead.
          clearOperationKey();
          clearDraft();
          setText("");
          setCommitted(outcome.node);
          setError(
            `Node created but not started: ${
              outcome.outcome === "start_failed"
                ? outcome.start_error
                : outcome.reason
            } Open it to retry.`,
          );
          return;
        }
      }
    } catch (e) {
      if (!active.current) return;
      if (isAuthError(e)) onAuthFailed();
      else
        // Keep both the draft and the operation key: a 4xx/5xx from this
        // handler means nothing was committed, so the same key may safely
        // retry.
        setError(
          `${(e as Error).message} Your idea is still here. Check Work before retrying if the connection was interrupted.`,
        );
    } finally {
      submitting.current = false;
      if (active.current) {
        setBusy(false);
        onBusyChange?.(false);
      }
    }
  }
  return (
    <div className="mobile-page capture-page">
      <h1>New idea</h1>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void start();
        }}
        className="capture-form"
      >
        <label htmlFor="idea-text">Your idea</label>
        <textarea
          id="idea-text"
          className="field idea-text"
          placeholder="What needs fixing, changing, or exploring?"
          value={text}
          disabled={busy}
          onChange={(e) => setText(e.target.value)}
        />
        <p className="field-hint" role="status">
          {stored
            ? text
              ? "Draft saved on this phone"
              : "Your draft saves here as you type"
            : "Storage unavailable. Keep this page open to retain your draft."}
        </p>
        <label htmlFor="idea-mesh">Work in</label>
        <select
          id="idea-mesh"
          className="field"
          value={selectedMesh}
          onChange={(e) => setMeshId(e.target.value)}
          disabled={busy}
        >
          {!meshes.some((m) => String(m.id) === selectedMesh) && (
            <option value={selectedMesh} disabled>
              {meshes.length
                ? "Previous mesh unavailable — choose a mesh"
                : "No meshes available"}
            </option>
          )}
          {meshes.map((m) => (
            <option key={m.id} value={m.id}>
              {m.name}
            </option>
          ))}
        </select>
        <label htmlFor="idea-agent">Agent</label>
        <select
          id="idea-agent"
          className="field"
          value={selectedProvider}
          onChange={(e) => setProviderId(e.target.value)}
          disabled={busy}
        >
          {!choices.some((p) => p.id === selectedProvider) && (
            <option value={selectedProvider} disabled>
              {choices.length
                ? "Previous agent unavailable — choose an agent"
                : "No available agents"}
            </option>
          )}
          {choices.map((p) => (
            <option key={p.id} value={p.id}>
              {p.configuration?.name ?? p.label}
            </option>
          ))}
        </select>
        {!choices.length && (
          <p className="field-hint">
            Configure an agent that accepts an initial prompt on your desktop.
          </p>
        )}
        {bytes > 16000 && (
          <p role="alert" className="inline-error">
            Shorten this idea to 16000 bytes or fewer.
          </p>
        )}
        {error && (
          <p role="alert" className="inline-error">
            {error}
          </p>
        )}
        {committed && (
          <button
            type="button"
            className="btn-ghost"
            onClick={() => onStarted(committed)}
          >
            Open the node
          </button>
        )}
        <button className="btn-primary" disabled={!valid || busy}>
          {busy ? "Starting agent…" : "Start working on this"}
        </button>
        <p className="field-hint">
          Starts a new agent in the selected mesh with your idea as its initial
          prompt.
        </p>
      </form>
    </div>
  );
}
