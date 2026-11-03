import type { DerivedScope } from '../../lib/viewModes';
import { EmptyStateActions } from './EmptyStateActions';

export interface CanvasEmptyStateInput {
  meshCount: number;
  totalNodeCount: number;
  /** How many nodes the scope holds before the active grid filters narrow it. */
  scopedCount: number;
  scope: DerivedScope;
  harnessReady: boolean;
}

export type CanvasEmptyDecision =
  | { branch: 'no-meshes' }
  | { branch: 'no-mesh-selected' }
  | { branch: 'selected-empty'; meshId: number }
  | { branch: 'all-empty' }
  | { branch: 'filters-exclude-all' }
  | { branch: 'pinned-empty' };

export interface CanvasEmptyStateCallbacks {
  onCreateMesh: () => void;
  onOpenSpawnMenu: (meshId: number | null) => void;
  onClearFilters: () => void;
  onOpenSetup: () => void;
  onViewAll: () => void;
  onOpenTerminal?: () => void | Promise<void>;
}

/** Scope owns identity; these counts distinguish missing work from hidden work. */
export function classifyCanvasEmpty(input: CanvasEmptyStateInput): CanvasEmptyDecision {
  if (input.meshCount === 0) return { branch: 'no-meshes' };
  if (input.scope.gridMode === 'pinned' && input.scopedCount === 0) return { branch: 'pinned-empty' };
  if (input.scope.viewMode === 'mesh' && !input.scope.isMeshScoped) return { branch: 'no-mesh-selected' };
  if (input.scope.viewMode === 'mesh' && input.scopedCount === 0 && input.scope.mesh !== null) {
    return { branch: 'selected-empty', meshId: input.scope.mesh.id };
  }
  if (input.totalNodeCount === 0) return { branch: 'all-empty' };
  // The scope holds nodes and the active filters hide all of them. The
  // canvas is empty because of the filters, not because the scope is.
  return { branch: 'filters-exclude-all' };
}

interface EmptyAction {
  heading: string;
  label: string;
  testId: string;
  onClick: () => void;
}

/** One action description feeds one layout for every empty state. */
function emptyAction(
  decision: CanvasEmptyDecision,
  harnessReady: boolean,
  callbacks: CanvasEmptyStateCallbacks,
): EmptyAction {
  switch (decision.branch) {
    case 'no-meshes':
      return {
        heading: 'Add a repository', label: 'New mesh',
        testId: 'canvas-empty-create-mesh', onClick: callbacks.onCreateMesh,
      };
    case 'selected-empty':
    case 'all-empty':
      return {
        heading: harnessReady
          ? decision.branch === 'selected-empty' ? 'No agents in this mesh' : 'No agents yet'
          : 'Set up an agent',
        label: harnessReady ? 'Spawn agent' : 'Open Settings',
        testId: harnessReady ? 'canvas-empty-spawn-agent' : 'canvas-empty-open-setup',
        onClick: harnessReady
          ? () => callbacks.onOpenSpawnMenu(decision.branch === 'selected-empty' ? decision.meshId : null)
          : callbacks.onOpenSetup,
      };
    case 'filters-exclude-all':
      return {
        heading: 'No nodes match', label: 'Clear search & filters',
        testId: 'canvas-empty-clear-filters', onClick: callbacks.onClearFilters,
      };
    case 'pinned-empty':
    case 'no-mesh-selected':
      return {
        heading: decision.branch === 'pinned-empty' ? 'No pinned nodes' : 'No mesh selected',
        label: 'View All Nodes', testId: 'canvas-empty-view-all', onClick: callbacks.onViewAll,
      };
  }
}

export function CanvasEmptyState({ input, callbacks }: {
  input: CanvasEmptyStateInput;
  callbacks: CanvasEmptyStateCallbacks;
}) {
  const decision = classifyCanvasEmpty(input);
  const action = emptyAction(decision, input.harnessReady, callbacks);
  const canStart = decision.branch === 'selected-empty' || decision.branch === 'all-empty';
  const showReadiness = canStart || decision.branch === 'no-meshes';

  return (
    <div className="flex-1 flex min-h-0 overflow-y-auto p-4">
      <div className="m-auto w-full max-w-sm text-center" data-testid="canvas-empty-state">
        <h2 className="mb-5 text-xl font-semibold text-text-primary">{action.heading}</h2>
        <button
          type="button"
          data-testid={action.testId}
          onClick={action.onClick}
          className="min-h-[32px] rounded-md border border-transparent bg-accent-cyan px-4 py-2 text-sm font-medium text-text-inverse transition-colors hover:bg-accent-blue"
        >
          {action.label}
        </button>
        {showReadiness && (
          <EmptyStateActions repositoryReady={canStart} harnessReady={input.harnessReady} callbacks={callbacks} />
        )}
      </div>
    </div>
  );
}
