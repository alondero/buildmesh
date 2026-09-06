import type { ViewMode } from '../stores/uiStore';
import { getNodeGitPath } from './paths';

// Tabs the Probe Panel can show. Kept as a string-literal union (not a
// generated wire enum) because it's a pure UI concern — no backend serialises
// it. `usage` was added in issue #601 as the dedicated glanceable surface
// for Usage Meters (subscription quota + cash balance), reached from a
// meter icon in the sidebar header. `autopilot` was added in wayfinder
// #990 ticket #994 as the dedicated configure + monitor surface for the
// Issue-Driven and Looping Autopilot modes. `circuits` was added for the
// Autopilot Circuits walking skeleton (spec #1205 / issue #1206).
// Lives here (not in the store) so pure domain modules — probeWorkingSet,
// this file — never import from `stores/`; `uiStore` re-exports it.
export type ProbeTab = 'files' | 'review' | 'usage' | 'properties' | 'autopilot' | 'circuits' | 'issues' | 'pulls' | 'sessions' | 'worktrees' | 'scratchpad';

/**
 * The three ownership lenses available to a Probe destination.
 *
 * A lens is the thing a view is about, not merely the store value that
 * happened to be selected when the view rendered. Keeping this vocabulary in
 * one module gives the inspector header, empty states, and destination
 * implementations a shared seam for context decisions.
 */
export type ProbeLens = 'host' | 'mesh' | 'agent';

export type ProbeContextMode = 'fixed' | 'following' | 'pinned';

/** Baseline or data ownership that a destination presents. */
export type ProbeBaseline = 'host' | 'mesh' | 'head' | 'node-base';

export interface ProbeTabDefinition {
  /** Human-readable title used by the inspector header. */
  label: string;
  /** Longer accessible label for destinations whose short title is ambiguous. */
  tooltip?: string;
  /** The primary ownership lens this destination uses. */
  lens: ProbeLens;
  /** Whether an unpinned destination follows the current UI selection. */
  followsSelection: boolean;
  /** Whether the current subject can be captured as a per-tab pin. */
  pinnable: boolean;
  /** The baseline/data family that makes the ownership decision concrete. */
  baseline: ProbeBaseline;
  /** True when the destination contains edits or other stateful actions. */
  stateful: boolean;
  /** Explicit decision for a destination that reads a secondary ownership. */
  mixedOwnership?: string;
}

/**
 * The authoritative ownership map for every current Probe destination.
 *
 * This is deliberately a Record<ProbeTab, ...>, rather than a partial map or
 * a switch in ProbePanel. Adding a new ProbeTab therefore requires an
 * ownership decision before TypeScript will allow the destination to ship.
 */
export const PROBE_TAB_DEFINITIONS: Record<ProbeTab, ProbeTabDefinition> = {
  files: {
    label: 'Project Files',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'head',
    stateful: false,
    mixedOwnership:
      'Mesh-owned repository view; when an Agent Node in that Mesh is focused, its working tree is the displayed path.',
  },
  review: {
    label: 'Agent Changes',
    lens: 'agent',
    followsSelection: true,
    pinnable: true,
    baseline: 'node-base',
    stateful: false,
  },
  usage: {
    label: 'Usage',
    tooltip: 'Provider usage meters',
    lens: 'host',
    followsSelection: false,
    pinnable: false,
    baseline: 'host',
    stateful: false,
  },
  worktrees: {
    label: 'Worktree Manager',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'mesh',
    stateful: true,
  },
  properties: {
    // Issue #1375 — the inspector header and the palette entry must agree on
    // the user-facing task name ("Open Project Settings" in the palette).
    label: 'Project Settings',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'mesh',
    stateful: true,
  },
  autopilot: {
    label: 'Autopilot',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'mesh',
    stateful: true,
  },
  circuits: {
    label: 'Circuits',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'mesh',
    stateful: true,
  },
  issues: {
    // Issue #1375 — the header names the user-facing destination; the palette
    // entry is "Open GitHub Issues".
    label: 'GitHub Issues',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'mesh',
    stateful: true,
  },
  pulls: {
    label: 'Pull Requests',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'mesh',
    stateful: true,
  },
  sessions: {
    // Issue #1375 — "Agent History" is the user-facing destination name (the
    // palette entry matches); the body lists completed, archived, failed, and
    // resumable agent work.
    label: 'Agent History',
    tooltip: 'Completed, archived, failed, and resumable agent work',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'mesh',
    stateful: true,
    mixedOwnership:
      'Mesh-owned Agent Node history index; each listed row is an Agent Node and resume acts on that row.',
  },
  scratchpad: {
    // Issue #1375 — "Notes" is the user-facing destination name; the body it
    // opens is the mesh-aware Scratch Pad (the textarea label keeps that name).
    label: 'Notes',
    tooltip: 'Scratch Pad',
    lens: 'mesh',
    followsSelection: true,
    pinnable: true,
    baseline: 'mesh',
    stateful: true,
  },
};

/** Inspector destination order — feeds the `PROBE_TABS` presentation list
 *  (header chip + placeholder lookups) and, via the omnibar's
 *  `PROBE_TAB_COMMANDS` alias, the palette destination coverage. The
 *  palette's DISPLAY order is `APP_COMMANDS`' array order, not this one. */
export const PROBE_TAB_ORDER: readonly ProbeTab[] = [
  'files',
  'review',
  'usage',
  'worktrees',
  'properties',
  'autopilot',
  'circuits',
  'issues',
  'pulls',
  'sessions',
  'scratchpad',
];

/**
 * A pin is intentionally keyed by destination. Pinning Agent Changes must not
 * make GitHub Issues silently operate on that same mesh after the user changes
 * tabs; each destination gets an independent context decision.
 */
export interface ProbeContextPin {
  tab: ProbeTab;
  lens: Exclude<ProbeLens, 'host'>;
  meshId: number;
  /** Required for an Agent lens; optional for the mixed Project Files view. */
  nodeId: number | null;
}

export interface ProbeContextMesh {
  id: number;
  name: string;
  path: string;
}

export interface ProbeContextNode {
  id: number;
  mesh_id: number;
  name: string;
  path: string;
  use_worktree?: boolean;
  worktree_name?: string | null;
}

export interface ProbeSubject {
  lens: ProbeLens;
  id: number | null;
  name: string | null;
  available: boolean;
}

export interface ProbeContext {
  /** Ownership vocabulary used by the current destination. */
  lens: ProbeLens;
  /** Stable subject the current destination reads or mutates. */
  subject: ProbeSubject;
  /** Formatted subject, e.g. `Host`, `Mesh: buildmesh`, or `Agent: node-a`. */
  subjectLabel: string;
  /** Whether this destination follows live selection or a captured pin. */
  mode: ProbeContextMode;
  /** True only while an unpinned, selection-following context is active. */
  followsSelection: boolean;
  /** The destination permits pinning, and a pin/unpin control can be shown. */
  pinnable: boolean;
  /** True when a valid target is available for this destination. */
  hasRequiredContext: boolean;
  /** Whether the header should offer a pin or unpin action. */
  canPin: boolean;
  /** Candidate captured by the pin button while following selection. */
  pinCandidate: ProbeContextPin | null;
  /** Parent Mesh name for Agent lenses, when the Mesh row is available. */
  activeMeshName: string | null;
  /**
   * For the mixed Project Files view, explains why the path can be a node
   * worktree even though the destination is Mesh-owned. For Agent lenses this
   * identifies the parent Mesh.
   */
  detailLabel: string | null;

  // Existing consumers use these resolved paths/IDs. They remain part of the
  // contract, but are now resolved through the destination's explicit lens.
  activeMeshId: number | null;
  activeNodeId: number | null;
  activePath: string | null;
  activeMeshPath: string | null;
  activeNodeName: string | null;
}

interface ResolveProbeContextInput {
  tab: ProbeTab;
  selectedMeshId: number | null;
  meshesById: ReadonlyMap<number, ProbeContextMesh>;
  activeNodeId: number | null;
  nodesById: Readonly<Record<number, ProbeContextNode | undefined>>;
  viewMode: ViewMode;
  pin: ProbeContextPin | null;
}

function lensLabel(lens: ProbeLens): string {
  if (lens === 'host') return 'Host';
  if (lens === 'mesh') return 'Mesh';
  return 'Agent';
}

export function formatProbeSubject(subject: ProbeSubject): string {
  const prefix = lensLabel(subject.lens);
  if (subject.lens === 'host') return prefix;
  if (subject.name) return `${prefix}: ${subject.name}`;
  return subject.id === null ? prefix : `${prefix}: unavailable`;
}

function currentSelectionMeshId(
  selectedMeshId: number | null,
  activeNode: ProbeContextNode | null,
  viewMode: ViewMode,
): number | null {
  // Single mode is an explicit Agent lens for the canvas. Preserve that
  // behavior when a destination resolves its unpinned selection context; the
  // other modes keep an explicitly selected Mesh authoritative.
  if (viewMode === 'single') return activeNode?.mesh_id ?? selectedMeshId;
  return selectedMeshId ?? activeNode?.mesh_id ?? null;
}

function pinApplies(
  pin: ProbeContextPin | null,
  tab: ProbeTab,
  definition: ProbeTabDefinition,
): pin is ProbeContextPin {
  return (
    pin !== null
    && pin.tab === tab
    && pin.lens === definition.lens
    && definition.pinnable
  );
}

export function resolveProbeContext({
  tab,
  selectedMeshId,
  meshesById,
  activeNodeId,
  nodesById,
  viewMode,
  pin,
}: ResolveProbeContextInput): ProbeContext {
  const definition = PROBE_TAB_DEFINITIONS[tab];
  const selectedNode = activeNodeId === null
    ? null
    : nodesById[activeNodeId] ?? null;
  const selectedMeshIdForContext = currentSelectionMeshId(
    selectedMeshId,
    selectedNode,
    viewMode,
  );
  const pinned = pinApplies(pin, tab, definition);
  const mode: ProbeContextMode = definition.lens === 'host'
    ? 'fixed'
    : pinned
      ? 'pinned'
      : 'following';

  if (definition.lens === 'host') {
    const subject: ProbeSubject = {
      lens: 'host',
      id: null,
      name: null,
      available: true,
    };
    return {
      lens: 'host',
      subject,
      subjectLabel: formatProbeSubject(subject),
      mode,
      followsSelection: false,
      pinnable: false,
      hasRequiredContext: true,
      canPin: false,
      pinCandidate: null,
      activeMeshName: null,
      detailLabel: null,
      activeMeshId: null,
      activeNodeId: null,
      activePath: null,
      activeMeshPath: null,
      activeNodeName: null,
    };
  }

  const targetMeshId = pinned ? pin.meshId : selectedMeshIdForContext;
  const mesh = targetMeshId === null ? null : meshesById.get(targetMeshId) ?? null;

  if (definition.lens === 'agent') {
    const targetNodeId = pinned ? pin.nodeId : activeNodeId;
    const node = targetNodeId === null ? null : nodesById[targetNodeId] ?? null;
    const agentMeshId = node?.mesh_id ?? targetMeshId;
    const agentMesh = agentMeshId === null ? null : meshesById.get(agentMeshId) ?? null;
    const subject: ProbeSubject = {
      lens: 'agent',
      id: targetNodeId,
      name: node?.name ?? null,
      available: node !== null,
    };
    const pinCandidate = node === null || agentMeshId === null
      ? null
      : {
          tab,
          lens: 'agent' as const,
          meshId: agentMeshId,
          nodeId: node.id,
        };

    return {
      lens: 'agent',
      subject,
      subjectLabel: formatProbeSubject(subject),
      mode,
      followsSelection: mode === 'following' && definition.followsSelection,
      pinnable: definition.pinnable,
      hasRequiredContext: subject.available,
      canPin: mode === 'pinned' || pinCandidate !== null,
      pinCandidate,
      activeMeshName: agentMesh?.name ?? null,
      detailLabel: agentMesh ? `Mesh: ${agentMesh.name}` : null,
      activeMeshId: agentMeshId,
      activeNodeId: targetNodeId,
      activePath: node ? getNodeGitPath(node) : null,
      activeMeshPath: agentMesh?.path ?? null,
      activeNodeName: node?.name ?? null,
    };
  }

  // Mesh destinations own the Mesh even when a destination has a secondary
  // node-aware presentation. Only Project Files uses the focused Agent
  // working directory; other Mesh destinations deliberately ignore node
  // selection so their target cannot drift with an unrelated card.
  const meshNodeId = tab !== 'files'
    ? null
    : pinned
      ? pin.nodeId
      : selectedNode?.mesh_id === targetMeshId
        ? selectedNode.id
        : null;
  const candidateMeshNode = meshNodeId === null ? null : nodesById[meshNodeId] ?? null;
  const meshNode = candidateMeshNode?.mesh_id === targetMeshId ? candidateMeshNode : null;
  const pinnedFilesNodeUnavailable = pinned
    && tab === 'files'
    && pin.nodeId !== null
    && meshNode === null;
  const subject: ProbeSubject = {
    lens: 'mesh',
    id: targetMeshId,
    name: mesh?.name ?? null,
    available: mesh !== null,
  };
  const pinCandidate = targetMeshId === null || mesh === null
    ? null
    : {
        tab,
        lens: 'mesh' as const,
        meshId: targetMeshId,
        nodeId: tab === 'files' ? meshNode?.id ?? null : null,
      };

  return {
    lens: 'mesh',
    subject,
    subjectLabel: formatProbeSubject(subject),
    mode,
    followsSelection: mode === 'following' && definition.followsSelection,
    pinnable: definition.pinnable,
    hasRequiredContext: subject.available && !pinnedFilesNodeUnavailable,
    canPin: mode === 'pinned' || pinCandidate !== null,
    pinCandidate,
    activeMeshName: mesh?.name ?? null,
    detailLabel: mesh === null
      ? null
      : pinnedFilesNodeUnavailable
        ? 'Pinned working tree unavailable'
        : tab === 'files'
          ? meshNode
            ? `Working tree: ${meshNode.name}`
            : 'Repository root'
          : null,
    activeMeshId: targetMeshId,
    activeNodeId: meshNode?.id ?? null,
    activePath: pinnedFilesNodeUnavailable
      ? null
      : meshNode
        ? getNodeGitPath(meshNode)
        : mesh?.path ?? null,
    activeMeshPath: mesh?.path ?? null,
    activeNodeName: meshNode?.name ?? null,
  };
}
