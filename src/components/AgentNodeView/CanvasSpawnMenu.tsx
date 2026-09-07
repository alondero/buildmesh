/**
 * Issue #1536 — canvas-level Spawn Menu dialog.
 *
 * The Sidebar's per-mesh `+ ▾` cluster already opens a Spawn Menu for
 * the mesh it's anchored to. The canvas empty state isn't in that
 * tree, so it needs a sibling dialog at App scope. This component
 * owns the spawn dialog mount and reads the requested mesh id from
 * `uiStore.canvasSpawnMenuMeshId`.
 *
 * `meshId === null` means "any mesh" — the canvas "All Nodes empty"
 * branch fires this when no specific mesh is selected; we resolve to
 * the most recently selected mesh, falling back to the first mesh in
 * the list (matches the sidebar's "default" target behaviour).
 *
 * The actual create→activate→select-mesh invariant (issue #283) lives
 * in `agentNodeStore.selectProviderForMesh`; this dialog is a thin
 * shell that picks a provider and delegates. The harness-ready empty
 * state is the same onboarding panel the Sidebar's spawn dropdown
 * surfaces (issue #822) — duplicated here verbatim because the two
 * surfaces can be open independently and the panel needs its own
 * click-outside scope.
 */
import { useMemo } from 'react';
import { Modal } from '../shared/Modal';
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

export function CanvasSpawnMenu() {
  const meshId = useUIStore((s) => s.canvasSpawnMenuMeshId);
  const close = useUIStore((s) => s.closeCanvasSpawnMenu);
  const meshes = useMeshStore((s) => s.meshes);
  const meshesById = useMeshStore((s) => s.meshesById);
  const selectProviderForMesh = useAgentNodeStore((s) => s.selectProviderForMesh);
  const selectMesh = useMeshStore((s) => s.selectMesh);
  const providerList = useProviderList();

  // Resolve the requested mesh id: explicit id wins, else fall back to
  // the sidebar's `selectedMeshId`, else to the first mesh in the
  // list. The last two cases preserve the user's "I'm in a mesh"
  // context when the canvas branch fires without an id (the "All
  // Nodes empty" branch).
  const targetMesh = useMemo(() => {
    if (meshId !== null && meshesById.has(meshId)) return meshesById.get(meshId) ?? null;
    const selected = useMeshStore.getState().selectedMeshId;
    if (selected !== null && meshesById.has(selected)) return meshesById.get(selected) ?? null;
    return meshes[0] ?? null;
  }, [meshId, meshes, meshesById]);

  // Wait for the next render after closing — closing the modal via the
  // backdrop / Escape / × unmounts us, so no need to clear here.
  if (targetMesh === null) {
    if (meshId !== null) close();
    return null;
  }

  const handleSelect = async (providerId: string, altKey: boolean) => {
    close();
    // The create→activate→select-mesh invariant (issue #283) lives in
    // the store action; the canvas surface delegates so the same
    // ordering holds whether the spawn came from the sidebar's `+ ▾`
    // or the canvas empty state.
    await selectProviderForMesh(
      targetMesh.id,
      targetMesh.name,
      targetMesh.path,
      providerId,
      altKey,
    );
    selectMesh(targetMesh.id);
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
