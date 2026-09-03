import { useState } from 'react';
import { useAgentNodeStore, type AgentNode } from '../stores/agentNodeStore';
import { REGENERATE_DISABLED_STATUSES } from '../lib/regenerate';
import { addToast } from '../stores/toastStore';
import { formatError } from '../lib/errorUtils';
import type { SpawnOption } from '../lib/groups';

export interface PendingRegenerate {
  providerId: string;
  providerLabel: string;
}

/**
 * Issue #1502 — the shared Regenerate action (issue #778 contract).
 *
 * Owns everything both Regenerate surfaces need identically so the two
 * can never drift: the status gate, the confirm state machine for
 * `running` nodes (interrupting drops in-flight PTY output, so the
 * picker opens a dialog instead of firing the IPC), and the shared
 * toast pipeline for backend rejections (issue #1001 — never a silent
 * menu close).
 *
 * Menu chrome stays with the callers — each closes its own menu around
 * `pickRegenerateProvider` and renders its own `ConfirmDialog` from
 * `pendingRegenerate` — because the sidebar context menu and the
 * header toolbar/kebab close differently. `node` may be `undefined`
 * (the header subscribes per-id and renders through a not-yet-loaded
 * pass); every entry point guards it, so the hook call itself stays
 * unconditional under the early return (Rules of Hooks).
 */
export function useRegenerateAction(
  node: Pick<AgentNode, 'id' | 'provider' | 'status'> | undefined,
  providerList: SpawnOption[] | undefined,
) {
  const regenerateAgentNode = useAgentNodeStore((s) => s.regenerateAgentNode);
  const [pendingRegenerate, setPendingRegenerate] = useState<PendingRegenerate | null>(null);

  const isRegenerateDisabled = !node || REGENERATE_DISABLED_STATUSES.includes(node.status);
  // In-place kick-start (#1502): the current provider alone is enough to
  // enable the action — only a completely empty list disables it.
  const hasRegenerateTargets = (providerList ?? []).length > 0;

  const fireRegenerate = (providerId: string) => {
    if (!node) return;
    regenerateAgentNode(node.id, providerId).catch((err) => {
      addToast('Regenerate failed', formatError(err), 'error');
    });
  };

  const pickRegenerateProvider = (providerId: string, providerLabel: string) => {
    if (!node || isRegenerateDisabled || !hasRegenerateTargets) return;
    if (node.status === 'running') {
      setPendingRegenerate({ providerId, providerLabel });
      return;
    }
    fireRegenerate(providerId);
  };

  const cancelRegenerate = () => setPendingRegenerate(null);

  const confirmRegenerate = () => {
    if (!node || !pendingRegenerate) return;
    const { providerId } = pendingRegenerate;
    setPendingRegenerate(null);
    fireRegenerate(providerId);
  };

  return {
    pendingRegenerate,
    isRegenerateDisabled,
    hasRegenerateTargets,
    pickRegenerateProvider,
    confirmRegenerate,
    cancelRegenerate,
  };
}
