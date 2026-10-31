import { _invoke } from './_invoke';
import type { AgentNode } from '../../types/generated/AgentNode';
import type { ArchivedAgentNode } from '../../types/generated/ArchivedAgentNode';
import type { WorktreeCloseSafety } from '../worktreeClose';

// Agent Node — renamed from `*Session` to `*AgentNode` in issue #490.
export const createAgentNode = (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean, configurationId?: string, initialPrompt?: string, loginId?: string) => {
  const trimmed = initialPrompt?.trim() ?? '';
  return _invoke<AgentNode>('create_agent_node', {
    meshId, name, path, branch, provider, useWorktree, configurationId,
    ...(trimmed !== '' ? { initialPrompt: trimmed } : {}),
    // The Harness Login picked in the spawn menu (absent = the default).
    ...(loginId ? { loginId } : {}),
  });
};

export const listAgentNodes = () =>
  _invoke<AgentNode[]>('list_agent_nodes');

export const getAgentNode = (nodeId: number) =>
  _invoke<AgentNode>('get_agent_node', { nodeId });

export const getWorktreeCloseSafety = (nodeId: number) =>
  _invoke<WorktreeCloseSafety>('get_worktree_close_safety', { nodeId });

export const deleteAgentNode = (nodeId: number, removeWorktree = false) =>
  _invoke('delete_agent_node', { nodeId, removeWorktree });

export const renameAgentNode = (nodeId: number, name: string) =>
  _invoke('rename_agent_node', { nodeId, name });

/** Persist new grid positions for a set of nodes: `[nodeId, position]` pairs. */
export const updateAgentNodePositions = (updates: [number, number][]) =>
  _invoke('update_agent_node_positions', { updates });

/** Pin / unpin an agent node for the Pinned Grid view (wayfinder #982 /
 * ticket #984). Used by the UI affordance when the user wants a
 * known-good state (e.g. "Pin this node" in a context menu); `toggle`
 * below flips whatever the current value is. Returns the post-write
 * `AgentNode` so the store can patch the local entry directly. */
export const setNodePinned = (nodeId: number, pinned: boolean) =>
  _invoke<AgentNode>('set_node_pinned', { nodeId, pinned });

/** Flip a node's `is_pinned` flag and return the post-write `AgentNode`
 * (wayfinder #982 / ticket #984). The single-action shape the UI's
 * click-to-pin button uses — the user doesn't need to know the current
 * pinned value, just "toggle". */
export const toggleNodePinned = (nodeId: number) =>
  _invoke<AgentNode>('toggle_node_pinned', { nodeId });

// (`listProviders` + `ProviderInfo` re-export moved to `./tauri/provider` —
// see the `export * from './provider'` line near the top of this file.)

// Agent Node Discovery — generated from the Rust struct (issue #359 + #490,
// re-exported here per #404 so call sites that import from `../lib/tauri`
// keep working).
export type { ArchivedAgentNode };

export const discoverAgentNodes = (meshId: number, meshPath: string) =>
  _invoke<ArchivedAgentNode[]>('discover_agent_nodes', { meshId, meshPath });

export const importDiscoveredAgentNode = (
  meshId: number,
  meshPath: string,
  cliSessionId: string,
  branch: string,
  worktreeName: string | null,
  provider?: string,
) =>
  _invoke<AgentNode>('import_discovered_agent_node', {
    meshId, meshPath, cliSessionId, branch, worktreeName, provider
  });

// ── App startup ────────────────────────────────────────────────────────────
//
// Re-attaches the in-process PTY for every node whose previous run was
// interrupted (status === 'suspended'). Returns the ids of the nodes that
// were actually resumed.
export const autoResumeAgentNodes = () =>
  _invoke<number[]>('auto_resume_agent_nodes');
