/**
 * useProbeContext — derive the active (mesh, node, path) triple that the
 * Probe Panel (issue #373) is showing. Pure derivation over the existing
 * stores; no state of its own.
 *
 * Why this is a hook and not another store
 * ----------------------------------------
 * The values are *fully determined* by `meshStore.selectedMeshId`,
 * `agentNodeStore.activeNodeId`, `uiStore.viewMode`, and the stores' data.
 * Caching this derived context in
 * a third store would just invite drift — every writer would have to keep
 * the cache in sync. A selector-style hook means there's exactly one
 * source of truth for each input and the answer is recomputed on read.
 *
 * Resolution rules
 * ----------------
 *   activeMeshId    =  in Single mode, activeNode?.mesh_id ?? selectedMeshId
 *                     otherwise, selectedMeshId ?? (activeNode?.mesh_id ?? null)
 *   activeNodeId    =  activeNodeId from agentNodeStore (the focused card,
 *                      independent of which mesh the sidebar is on)
 *   activePath      =  when a node is focused, the node's working
 *                      directory (resolved by `getNodeGitPath` from the
 *                      node's `path` and `worktree_name`, mirroring
 *                      the Rust `env::node_working_path`). When no node
 *                      is focused but a mesh is, the mesh root. Otherwise null.
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
import { useUIStore } from '../stores/uiStore';
import { getNodeGitPath } from '../lib/paths';

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
  /** The mesh's display name. Surfaced in the shared dock header as a
   *  subheading so the user always knows which project the dock is
   *  anchored to, without needing the directory path strip that the
   *  Issues / PRs tabs used to render. Follows the same resolution rule
   *  as `activeMeshPath` (independent of any focused worktree). */
  activeMeshName: string | null;
}

const EMPTY_CONTEXT: ProbeContext = {
  activeMeshId: null,
  activeNodeId: null,
  activePath: null,
  activeMeshPath: null,
  activeMeshName: null,
};

export function useProbeContext(): ProbeContext {
  const selectedMeshId = useMeshStore((s) => s.selectedMeshId);
  const meshesById = useMeshStore((s) => s.meshesById);
  const activeNodeId = useAgentNodeStore((s) => s.activeNodeId);
  // Issue #1384 — subscribe to the normalized `nodesById` directly. We
  // only need a single-node read here (the active node); the old
  // array-subscriber pattern re-rendered on every node's status flip,
  // including unrelated agents, which cascaded into every consumer of
  // `useProbeContext` (the dock header, the GitHub Issues/PR tabs, etc.).
  const nodesById = useAgentNodeStore((s) => s.nodesById);
  const viewMode = useUIStore((s) => s.viewMode);

  return useMemo<ProbeContext>(() => {
    const activeNode =
      activeNodeId !== null
        ? nodesById[activeNodeId] ?? null
        : null;

    // Single mode is an explicit node lens: the focused node can belong to a
    // different mesh than the sidebar selection because selecting a sidebar
    // node deliberately does not leave Single mode. Outside Single mode,
    // explicit mesh selection remains authoritative and the global-view
    // fallback derives mesh from whichever node card the user last focused.
    const activeMeshId =
      viewMode === 'single'
        ? activeNode?.mesh_id ?? selectedMeshId
        : selectedMeshId ?? activeNode?.mesh_id ?? null;

    if (activeMeshId === null) return EMPTY_CONTEXT;

    const mesh = meshesById.get(activeMeshId) ?? null;

    // The frontend resolves the working directory from the node's mesh
    // path and worktree metadata via `getNodeGitPath` (mirrors the
    // Rust-side `env::node_working_path`). When no node is focused, fall
    // back to the mesh root itself so the probe's "files" tab still has a
    // place to anchor.
    const activePath = activeNode
      ? getNodeGitPath(activeNode)
      : mesh?.path ?? null;

    // The mesh's own path is independent of any focused worktree. The
    // "global view" fallback (`selectedMeshId === null`, mesh derived
    // from the focused node) keeps `activeMeshPath` populated so the
    // mesh-scoped tabs still have a repo root to walk.
    const activeMeshPath = mesh?.path ?? null;

    // Display name follows the same resolution as the path — sourced
    // from the mesh row, independent of any focused worktree. Null when
    // no mesh is resolvable so the dock header can omit the subheading.
    const activeMeshName = mesh?.name ?? null;

    return { activeMeshId, activeNodeId, activePath, activeMeshPath, activeMeshName };
  }, [selectedMeshId, meshesById, activeNodeId, nodesById, viewMode]);
}
