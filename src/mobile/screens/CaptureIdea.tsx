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

/**
 * The stored destination field, or `null` when it was never chosen.
 *
 * `null` and `""` mean different things now: `""` is the mesh default — a
 * launch that names no provider at all — while `null` means the user has
 * not picked one, so the first available agent is used (issue #2385).
 */
function storedDestination(field: "meshId" | "providerId"): string | null {
  try {
    const destination = JSON.parse(
      localStorage.getItem(DESTINATION_KEY) ?? "{}",
    );
    return typeof destination?.[field] === "string" ? destination[field] : null;
  } catch {
    return null;
  }
}

export default function CaptureIdea({
  meshes,
  providers,
  onStarted,
  onAuthFailed,
  onBusyChange,
  preset,
}: {
  meshes: Mesh[];
  providers: Provider[];
  onStarted: (node: AgentNode) => void;
  onAuthFailed: () => void;
  onBusyChange?: (busy: boolean) => void;
  /**
   * The mesh and launch the surface pre-selected (issue #2385). The mesh
   * card's split button opens Capture with both already chosen, so the user
   * writes the idea against the launch they picked rather than re-picking it.
   */
  preset?: { meshId: number; launchId?: string | null };
}) {
  const [text, setText] = useState(() => {
    try {
      return localStorage.getItem(DRAFT_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [meshId, setMeshId] = useState(() =>
    // A preset is the surface's own choice, not the user's stored one, so it
    // wins from the very first render — the stored destination would
    // otherwise flash on screen before the preset effect replaced it.
    preset ? String(preset.meshId) : storedDestination("meshId") ?? "",
  );
  const [providerId, setProviderId] = useState<string | null>(() =>
    preset ? preset.launchId ?? "" : storedDestination("providerId"),
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
  // The mesh card's split button opens Capture with the mesh and the launch
  // it chose already selected (issue #2385). The primitives are the deps so a
  // parent that rebuilds the object on every render cannot reset the user's
  // own picks.
  const presetMeshId = preset?.meshId;
  const presetLaunchId = preset?.launchId;
  /**
   * Whether the destination on screen came from the preset rather than from
   * the user. A preset is a one-shot instruction from the mesh card, so while
   * it is in force the destination must not be written to `localStorage`:
   * that would overwrite the user's own stored destination with the mesh they
   * happened to tap, and the stored pair would be lost for good. The flag
   * clears the moment the user picks a mesh or an agent, because from then
   * on the pair on screen is theirs to store.
   */
  const presetApplied = useRef(Boolean(preset));
  useEffect(() => {
    if (presetMeshId === undefined) return;
    presetApplied.current = true;
    setMeshId(String(presetMeshId));
    setProviderId(presetLaunchId ?? "");
  }, [presetMeshId, presetLaunchId]);
  useEffect(() => {
    try {
      if (!presetApplied.current) {
        localStorage.setItem(
          DESTINATION_KEY,
          JSON.stringify({ meshId, providerId }),
        );
      }
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
  // `null` means the user has not chosen an agent yet, so the first available
  // one is used; `""` is the mesh default, which names no provider at all and
  // lets the backend resolve its own default chain (issue #2385).
  const selectedProvider =
    providerId === null ? (choices[0]?.id ?? "") : providerId;
  // A named agent that is no longer on offer — kept visible so the user sees
  // what happened instead of silently landing on a different agent.
  const staleProvider =
    selectedProvider !== "" && !choices.some((p) => p.id === selectedProvider)
      ? selectedProvider
      : null;
  const bytes = new TextEncoder().encode(text).length;
  const valid =
    text.trim().length > 0 &&
    bytes <= 16000 &&
    meshes.some((m) => String(m.id) === selectedMesh) &&
    (selectedProvider === "" || choices.some((p) => p.id === selectedProvider));

  async function start() {
    if (!valid || submitting.current) return;
    submitting.current = true;
    onBusyChange?.(true);
    setBusy(true);
    setError(null);
    setCommitted(null);
    try {
      // The mesh default names no provider at all: the request then carries
      // neither `provider` nor `configuration_id`, and the backend resolves
      // the mesh's own default chain (issue #2385).
      const provider = choices.find((p) => p.id === selectedProvider);
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
        ...(provider
          ? {
              provider:
                provider.configuration?.spawn_option_id ?? provider.id,
              configuration_id: provider.configuration?.id,
            }
          : {}),
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
          onChange={(e) => {
            // The user has now chosen this mesh, so the pair on screen is
            // theirs and may be stored (issue #2385).
            presetApplied.current = false;
            setMeshId(e.target.value);
          }}
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
          onChange={(e) => {
            // The user has now chosen this agent, so the pair on screen is
            // theirs and may be stored (issue #2385).
            presetApplied.current = false;
            setProviderId(e.target.value);
          }}
          disabled={busy}
        >
          {staleProvider && (
            <option value={staleProvider} disabled>
              {choices.length
                ? "Previous agent unavailable — choose an agent"
                : "No available agents"}
            </option>
          )}
          <option value="">Mesh default</option>
          {choices.map((p) => (
            <option key={p.id} value={p.id}>
              {p.configuration?.name ?? p.label}
            </option>
          ))}
        </select>
        {!choices.length && (
          <p className="field-hint">
            No agent on this desktop accepts an initial prompt — the mesh
            default will be used.
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
