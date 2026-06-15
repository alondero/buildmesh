/**
 * useProbeContext — derive the active (mesh, node, path) triple that the
 * Probe Panel (issue #373) is showing. Pure derivation over the existing
 * stores; no state of its own.
 *
 * Why this is a hook and not another store
 * ----------------------------------------
 * The three values are *fully determined* by `meshStore.selectedMeshId`,
 * `agentNodeStore.activeNodeId`, and the two stores' data. Caching them in
 * a third store would just invite drift — every writer would have to keep
 * the cache in sync. A selector-style hook means there's exactly one
 * source of truth for each input and the answer is recomputed on read.
 *
 * Resolution rules
 * ----------------
 *   activeMeshId    =  selectedMeshId ?? (activeNode?.mesh_id ?? null)
 *   activeNodeId    =  activeNodeId from agentNodeStore (the focused card,
 *                      independent of which mesh the sidebar is on)
 *   activePath      =  when a node is focused, the node's working
 *                      directory (`node.path`, which the backend already
 *                      resolves to the worktree dir for a Worktree Node
 *                      or the mesh root for a Root Node — see
 *                      `env::node_working_path`). When no node is
 *                      focused but a mesh is, the mesh root. Otherwise null.
 *   activeMeshPath  =  the mesh row's own path (NOT the focused node's
 *                      worktree). Mesh-scoped tabs (issues, sessions,
 *                      future worktree manager) need the *mesh root* to
 *                      walk the repo (e.g. `discover_sessions` reads
 *                      `.claude/projects/...` from the mesh root, not
 *                      from a worktree subdir). Use this instead of
 *                      reaching into the mesh store directly.
 *
 * The "global view" case (sidebar shows all meshes, no mesh selected, but
 * the user just clicked an agent card) is the one the explicit fallback
 * branch handles: `selectedMeshId === null && activeNodeId !== null` ⇒ the
 * hook re-derives `activeMeshId` from the focused node's `mesh_id`, so the
 * probe panel can still show files/review for that card.
 */

import { useMemo } from 'react';
import { useMeshStore } from '../stores/meshStore';
import { useAgentNodeStore } from '../stores/agentNodeStore';

export interface ProbeContext {
  activeMeshId: number | null;
  activeNodeId: number | null;
  /** Working directory the probe should treat as its root — node path if a
   *  node is focused, else the mesh root, else null. */
  activePath: string | null;
  /** The mesh row's own path (independent of any focused worktree). Mesh-
   *  scoped tabs that walk the repo (GitHub Issues, Session Discovery,
   *  etc.) should prefer this over `activePath`. Null when no mesh is
   *  resolvable. */
  activeMeshPath: string | null;
}

const EMPTY_CONTEXT: ProbeContext = {
  activeMeshId: null,
  activeNodeId: null,
  activePath: null,
  activeMeshPath: null,
};

export function useProbeContext(): ProbeContext {
  const selectedMeshId = useMeshStore((s) => s.selectedMeshId);
  const meshesById = useMeshStore((s) => s.meshesById);
  const activeNodeId = useAgentNodeStore((s) => s.activeNodeId);
  const agentNodes = useAgentNodeStore((s) => s.agentNodes);

  return useMemo<ProbeContext>(() => {
    const activeNode =
      activeNodeId !== null
        ? agentNodes.find((n) => n.id === activeNodeId) ?? null
        : null;

    // Explicit mesh selection wins; the global-view fallback derives mesh
    // from whichever node card the user last focused.
    const activeMeshId =
      selectedMeshId !== null
        ? selectedMeshId
        : activeNode?.mesh_id ?? null;

    if (activeMeshId === null) return EMPTY_CONTEXT;

    const mesh = meshesById.get(activeMeshId) ?? null;

    // The backend populates `AgentNode.path` with the resolved working
    // directory (worktree subdir for a Worktree Node, mesh root for a Root
    // Node). When no node is focused, fall back to the mesh root itself so
    // the probe's "files" tab still has a place to anchor.
    const activePath = activeNode
      ? activeNode.path
      : mesh?.path ?? null;

    // The mesh's own path is independent of any focused worktree. The
    // "global view" fallback (`selectedMeshId === null`, mesh derived
    // from the focused node) keeps `activeMeshPath` populated so the
    // mesh-scoped tabs still have a repo root to walk.
    const activeMeshPath = mesh?.path ?? null;

    return { activeMeshId, activeNodeId, activePath, activeMeshPath };
  }, [selectedMeshId, meshesById, activeNodeId, agentNodes]);
}
