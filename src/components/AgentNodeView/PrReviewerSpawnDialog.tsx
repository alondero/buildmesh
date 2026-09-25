import { useState } from 'react';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { GroupedProviderMenu } from '../Providers/GroupedProviderMenu';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { createPrNode } from '../../lib/tauri';
import { formatError } from '../../lib/errorUtils';
import type { SpawnOption } from '../../lib/groups';
import type { OpenPr } from '../../types/generated/OpenPr';

interface PrReviewerSpawnDialogProps {
  /** The agent node the pill belongs to — the reviewer is grouped onto its card. */
  nodeId: number;
  meshId: number;
  openPr: OpenPr;
  providers: SpawnOption[];
  onClose: () => void;
}

/**
 * "Spawn reviewer agent" dialog for the agent-node PR pill.
 *
 * Picking a harness does exactly what the Pull Requests probe's `+` does — a
 * `create_pr_node` for the PR's head ref — with one difference: the reviewer is
 * asked for as a *reviewer* (`reviewer: true`), so the backend names it
 * distinctly and it cuts its **own** worktree instead of adopting the
 * implementation node's. Once created, the node is grouped onto the clicked
 * node's card (`groupNodes`) so it opens as another Node Activity tab.
 *
 * The provider picker is the shared Spawn Menu (ADR-0016), rendered inline the
 * way `CanvasSpawnMenu` does it — no `onClose` is forwarded, because
 * `GroupedProviderMenu`'s `useAriaMenu` binds Tab as well as Escape to it, and
 * Tab-in-menu would tear down this dialog instead of cycling focus. The parent
 * `Modal` owns Escape and backdrop close.
 */
export function PrReviewerSpawnDialog({ nodeId, meshId, openPr, providers, onClose }: PrReviewerSpawnDialogProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handlePick = async (providerId: string, _altKey: boolean, configurationId?: string) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const draft = await createPrNode(
        meshId,
        openPr.number,
        openPr.title,
        openPr.head_ref,
        openPr.head_sha,
        providerId,
        openPr.head_repo_owner,
        openPr.head_repo_clone_url,
        configurationId,
        true,
      );
      // Bridge the fresh id into the store before grouping: `groupNodes` bails
      // unless both ids are present, and the `node-created` listener's fetch is
      // async. `adoptAgentNode` is insert-if-absent, so a row the listener
      // already delivered is kept.
      useAgentNodeStore.getState().adoptAgentNode(draft);
      // Merge the reviewer into the clicked node's card and focus it — this is
      // the "another tab in the same view" step.
      useNodeActivityStore.getState().groupNodes(draft.id, nodeId);
      onClose();
    } catch (reason) {
      // Keep the dialog open so the user can retry or pick a different harness.
      setError(formatError(reason));
      setBusy(false);
    }
  };

  return (
    <Modal
      onClose={() => { if (!busy) onClose(); }}
      labelledBy="pr-reviewer-spawn-title"
      maxWidth="max-w-sm"
    >
      <div className="flex items-start justify-between mb-3">
        <h2 id="pr-reviewer-spawn-title" className="text-sm font-semibold text-text-primary">
          Spawn reviewer agent for PR #{openPr.number}
        </h2>
        <ModalCloseButton onClose={() => { if (!busy) onClose(); }} />
      </div>
      <p className="text-2xs text-text-muted mb-3">
        Launches an agent on this PR's head commit with its own worktree, then opens it as
        another tab on this node.
      </p>
      <div className="border border-border-subtle rounded-md bg-bg-overlay max-h-64 overflow-y-auto mb-3">
        <GroupedProviderMenu providers={providers} onSelect={handlePick} />
      </div>
      {error && <p role="alert" className="text-xs text-status-error break-words mb-3">{error}</p>}
      <div className="flex justify-end gap-2">
        <button type="button" disabled={busy} onClick={onClose} className="px-3 py-1.5 text-xs disabled:opacity-40">
          Cancel
        </button>
      </div>
    </Modal>
  );
}
