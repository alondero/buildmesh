import { useEffect, useRef, useState } from "react";
import { AgentNode, Mesh, Provider, createNode, isAuthError } from "../api";

const DRAFT_KEY = "buildmesh_mobile_idea";
const DESTINATION_KEY = "buildmesh_mobile_idea_destination";

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
    try {
      const provider = choices.find((p) => p.id === selectedProvider)!;
      const node = await createNode({
        mesh_id: Number(selectedMesh),
        provider: provider.configuration?.spawn_option_id ?? provider.id,
        configuration_id: provider.configuration?.id,
        prompt: text,
      });
      if (!active.current) return;
      try {
        localStorage.removeItem(DRAFT_KEY);
      } catch {
        /* The launch still succeeded. */
      }
      setText("");
      onStarted(node);
    } catch (e) {
      if (!active.current) return;
      if (isAuthError(e)) onAuthFailed();
      else
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
