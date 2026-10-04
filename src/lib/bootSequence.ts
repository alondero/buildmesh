/**
 * `runBoot` — the startup failure contract (issue #1524).
 *
 * Why this exists
 * ---------------
 * `App.init()` used to treat a `Promise.allSettled` outcome as the whole
 * story: only a *rejected* boot promise counted as a failure. Both
 * authoritative snapshot loaders — `meshStore.fetchMeshes` and
 * `agentNodeStore.fetchAgentNodes` — catch their IPC failure, write it to
 * store error state, and resolve. So a startup whose Mesh or Agent Node
 * snapshot never loaded looked like a clean boot: `isReady` flipped true
 * and the app painted an empty workspace, which reads as data loss rather
 * than as the backend being unreachable.
 *
 * Two signals, one verdict
 * ------------------------
 * `runBoot` combines both channels a loader can report through:
 *
 *   1. Rejection — the loader refused to lie. `attachListeners` rethrows
 *      after rolling back a partial registration; `refreshMeshes`
 *      rethrows after storing the error.
 *   2. Store error state — the loader absorbed the failure and resolved.
 *      Both snapshot stores clear `error` when a load *starts*, so a
 *      non-null value read after the loaders settle belongs to this
 *      attempt and no earlier one.
 *
 * Only a fully clean run reports `ok`, so the caller can gate "the
 * workspace is authoritative" on the outcome rather than on a promise
 * that happened to settle.
 *
 * Errors are reported per source (`Meshes: …`, `Agent Nodes: …`, `Event
 * listeners: …`) and deduplicated by source+text, because a rejecting
 * loader and its own store error describe the same failure twice.
 */
import { formatError } from './errorUtils';

/** The two authoritative snapshots, keyed to their store fields. */
export interface BootSnapshotErrors {
  meshes: string | null;
  agentNodes: string | null;
}

export interface BootLoaders {
  /** `agentNodeStore.initAttentionListeners` — rejects on a failed registration. */
  attachListeners: () => Promise<void>;
  /** Loads the Mesh snapshot (`meshStore.refreshMeshes` at the call site). */
  loadMeshes: () => Promise<void>;
  /** Loads the Agent Node snapshot (`agentNodeStore.fetchAgentNodes`). */
  loadAgentNodes: () => Promise<void>;
  /**
   * Read both stores' `error` fields after the loaders settle. Called once
   * per attempt, so a caller that reads a stale value cannot be mistaken
   * for a failure of this attempt.
   */
  readSnapshotErrors: () => BootSnapshotErrors;
}

export interface BootOutcome {
  /** True only when every loader succeeded AND both snapshots are clean. */
  ok: boolean;
  /** One pre-formatted line per failed source; empty when `ok`. */
  errors: string[];
}

/// Human-facing source labels. The vocabulary matches the domain
/// (`Meshes` / `Agent Nodes` are the two authoritative snapshots), so the
/// Boot Error Panel names what failed instead of showing a bare message.
const MESH_LABEL = 'Meshes';
const AGENT_NODE_LABEL = 'Agent Nodes';
const LISTENER_LABEL = 'Event listeners';

/**
 * Run the boot loaders concurrently and report whether the workspace is
 * authoritative. Never rejects for a loader failure: every failure comes
 * back in `BootOutcome.errors` so the caller renders one panel listing all
 * of them. A synchronous throw from a loader is converted to a rejection
 * (`Promise.resolve().then(run)`) so one bad loader cannot abort its
 * siblings — they still get a chance to report.
 */
export async function runBoot(loaders: BootLoaders): Promise<BootOutcome> {
  const sources: { label: string; run: () => Promise<void> }[] = [
    { label: LISTENER_LABEL, run: loaders.attachListeners },
    { label: MESH_LABEL, run: loaders.loadMeshes },
    { label: AGENT_NODE_LABEL, run: loaders.loadAgentNodes },
  ];

  const settled = await Promise.allSettled(
    sources.map(({ run }) => Promise.resolve().then(run)),
  );

  const errors: string[] = [];
  const addError = (label: string, message: string) => {
    const line = `${label}: ${message}`;
    if (!errors.includes(line)) errors.push(line);
  };

  settled.forEach((result, index) => {
    if (result.status === 'rejected') {
      addError(sources[index].label, formatError(result.reason));
    }
  });

  // Backstop for the loaders that store the failure and resolve.
  const { meshes, agentNodes } = loaders.readSnapshotErrors();
  if (meshes) addError(MESH_LABEL, meshes);
  if (agentNodes) addError(AGENT_NODE_LABEL, agentNodes);

  return { ok: errors.length === 0, errors };
}
