/**
 * Issue #1536 — canvas-level Spawn Menu dialog.
 *
 * The Sidebar's per-mesh `+ ▾` cluster already opens a Spawn Menu for
 * the mesh it's anchored to. The canvas empty state isn't in that
 * tree, so it needs a sibling dialog at App scope. This component
 * owns the spawn dialog mount.
 *
 * Two critical contracts (caught in senior review):
 *
 *   1. **Modal only mounts when explicitly opened.** The mesh id
 *      resolved against the store (`canvasSpawnMenuMeshId`) drives
 *      the conditional mount at the call site (App.tsx). The modal
 *      itself is a pure renderer; an internal "fall back to
 *      sidebar selection / first mesh" resolution would mount the
 *      modal on every app startup with meshes present — the review
 *      caught this as a UI lockout trap (closing the modal via
 *      Escape or backdrop sets the id back to null, which the
 *      fallback would re-resolve to a real mesh, re-opening the
 *      modal in an inescapable cycle).
 *
 *   2. **The mesh is resolved by the CALLER, not here.** When the
 *      canvas empty state's `onOpenSpawnMenu` callback fires, it
 *      has already picked a concrete mesh id (sidebar selection,
 *      active-node mesh, or first mesh in the list). The id is
 *      stale the moment it lands in the store if the user deletes
 *      the mesh mid-render, so the modal renders `null` and closes
 *      the store signal if the lookup misses.
 *
 * The actual create→activate→select-mesh invariant (issue #283) lives
 * in `agentNodeStore.selectProviderForMesh`; this dialog is a thin
 * shell that picks a provider and delegates. The harness-ready empty
 * state is the same onboarding panel the Sidebar's spawn dropdown
 * surfaces (issue #822) — duplicated here verbatim because the two
 * surfaces can be open independently and the panel needs its own
 * click-outside scope.
 */
import { useEffect } from 'react';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { GroupedProviderMenu } from '../Providers/GroupedProviderMenu';
import { SafeLink } from '../shared/SafeLink';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import { useProviderList } from '../../hooks/useProviderList';
import { hasSpawnableAgent } from '../../lib/groups';

/** README anchor for the install prerequisites — mirrors the sidebar
 *  spawn dropdown's onboarding panel (issue #822). */
const PREREQUISITES_URL = 'https://github.com/alondero/buildmesh#prerequisites';

interface CanvasSpawnMenuProps {
  /** The mesh id the canvas empty state requested the spawn menu
   *  for. The component renders nothing when this resolves to a
   *  mesh that's no longer in the store (e.g. deleted between the
   *  open request and the render) — the close action resets the
   *  store flag in that case so a retry isn't a silent no-op. */
  meshId: number;
}

export function CanvasSpawnMenu({ meshId }: CanvasSpawnMenuProps) {
  const close = useUIStore((s) => s.closeCanvasSpawnMenu);
  // Hook selector, NOT `useMeshStore.getState()` inside the body —
  // a `getState()` read here wouldn't re-subscribe when the mesh
  // row mutates between open and render (senior-review finding).
  const targetMesh = useMeshStore((s) => s.meshesById.get(meshId) ?? null);
  const selectProviderForMesh = useAgentNodeStore((s) => s.selectProviderForMesh);
  const providerList = useProviderList();

  // The mesh disappeared between open request and render (delete /
  // refetch race). Reset the store flag and bail — the next open
  // call from the canvas empty state will re-resolve the mesh.
  // MUST run in an effect: calling `close()` synchronously inside
  // the render body is a store write during render, which React 19
  // flags as a side-effect (and warns under StrictMode). Effect
  // runs after commit, so the first paint still shows nothing
  // (the early return below) and the close happens before the
  // next render.
  useEffect(() => {
    if (targetMesh === null) close();
  }, [targetMesh, close]);

  if (targetMesh === null) return null;

  const handleSelect = async (providerId: string, altKey: boolean) => {
    close();
    // The `altKey` modifier on a spawn pick toggles the worktree
    // override — Alt+click inverts the mesh's configured default,
    // plain click honours it. Passing `altKey` directly would
    // force every non-alt spawn to `useWorktree: false`, which
    // silently spawns in the mesh root regardless of the user's
    // repository configuration. Senior-review finding.
    const useWorktree = altKey ? !targetMesh.use_worktree : targetMesh.use_worktree;
    // The create→activate→select-mesh invariant (issue #283) lives
    // entirely inside `selectProviderForMesh`: it creates the node,
    // sets it active, then selects the mesh. The canvas surface
    // delegates so the same ordering holds whether the spawn came
    // from the sidebar's `+ ▾` or the canvas empty state. No
    // separate `selectMesh` here — that would re-enter the
    // invariant and race the action's own ordering.
    try {
      await selectProviderForMesh(
        targetMesh.id,
        targetMesh.name,
        targetMesh.path,
        providerId,
        useWorktree,
      );
    } catch (error) {
      // The agentNodeStore selector swallows errors and surfaces
      // them via `state.error` (per the IPC wrapper contract), but
      // an awaited rejection here would still bubble as an
      // unhandled promise — log via console.error so a test or a
      // dev-tools surface can catch the failure mode without
      // crashing the modal (it's already closed by the `close()`
      // call above).
      console.error('[CanvasSpawnMenu] selectProviderForMesh failed:', error);
    }
  };

  const noAgent = !hasSpawnableAgent(providerList);

  return (
    <Modal
      onClose={close}
      labelledBy="canvas-spawn-menu-title"
      maxWidth="max-w-sm"
    >
      <div className="flex items-start justify-between mb-3">
        <h2 id="canvas-spawn-menu-title" className="text-sm font-semibold text-text-primary">
          Spawn agent in {targetMesh.name}
        </h2>
        <ModalCloseButton onClose={close} />
      </div>
      <p className="text-2xs text-text-muted mb-3">
        Pick a harness to launch. Native CLIs run when the binary is on PATH; proxied providers use a keyed account.
      </p>
      {noAgent && (
        <div
          data-testid="canvas-spawn-menu-empty-state"
          className="px-3 py-2.5 mb-3 border border-border-subtle rounded-md bg-bg-card/60"
        >
          <p className="text-xs font-medium text-text-primary">No agent CLIs found</p>
          <p className="mt-1 text-2xs text-text-muted leading-relaxed">
            Install one of Claude Code, Codex, Antigravity, or OpenCode, or add a
            provider key in Settings&nbsp;&rarr;&nbsp;Providers.
          </p>
          <SafeLink
            url={PREREQUISITES_URL}
            className="mt-1.5 inline-block text-2xs text-text-secondary hover:text-text-primary hover:underline"
            title="Open the setup instructions on GitHub"
          >
            View setup instructions&nbsp;&#8599;
          </SafeLink>
        </div>
      )}
      <GroupedProviderMenu
        providers={providerList}
        onSelect={handleSelect}
        // `Modal` already owns Escape / backdrop close — disable the
        // menu's own Escape path so we don't double-fire (the menu's
        // hook also has Tab-leave behaviour that conflicts with the
        // Modal's focus trap).
        onClose={close}
      />
    </Modal>
  );
}
