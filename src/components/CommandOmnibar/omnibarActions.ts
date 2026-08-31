/**
 * Execution layer for the Command Omnibar (wayfinder #1371, task #1411).
 *
 * The indexers (#1410) emit stable item ids — `node:12`, `mesh:3`,
 * `command:open-settings`, `spawn:<option>:<mesh>`, `issue:<mesh>:<n>`,
 * `pull:<mesh>:<n>` — and this module is the single place that maps an id to
 * the app action it stands for. Keeping the routing out of the component
 * makes it unit-testable without rendering React and keeps the palette a
 * pure "search + select" surface.
 *
 * Modal opens go through `uiStore` (`cheatsheetOpen` / `appSettingsOpen` /
 * `remoteAccessOpen`), the same source of truth App's `?` key and TitleBar's
 * header buttons already use — no window-event side channel.
 */
import type { Mesh } from '../../types/generated/Mesh';
import type { ProbeTab, ViewMode } from '../../stores/uiStore';
import type { SpawnOption } from '../../lib/groups';
import { currentTheme, setTheme, type ThemeName } from '../../lib/theme';
import { mapBackendProviders } from '../../lib/groups';
import { addToast } from '../../stores/toastStore';
import * as api from '../../lib/tauri';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { requestIssueNavigation } from '../../lib/omnibar/issueNavigation';

/** Everything `executeOmnibarItem` needs beyond the stores themselves. */
export interface OmnibarActionContext {
  /** The live mesh list — needed to resolve spawn and git-sync targets. */
  meshes: Mesh[];
  /** The live spawn options (harnesses) — needed to resolve spawn targets. */
  spawnOptions: SpawnOption[];
  setViewMode: (mode: ViewMode) => void;
  openProbeTab: (tab: ProbeTab) => void;
}

/**
 * Run the built-in command behind a `command:<id>` item. Returns false for
 * an unknown id (a catalog/store drift the tests pin) so callers can ignore
 * it deliberately rather than silently.
 */
export function runOmnibarCommand(id: string, ctx: OmnibarActionContext): boolean {
  switch (id) {
    case 'toggle-theme':
      setTheme((currentTheme() === 'light' ? 'dark' : 'light') as ThemeName);
      return true;
    case 'view-single':
    case 'view-pinned':
      ctx.setViewMode(id.slice('view-'.length) as ViewMode);
      return true;
    case 'view-mesh': {
      const meshStore = useMeshStore.getState();
      if (meshStore.selectedMeshId === null) {
        const active = useAgentNodeStore.getState().getActiveNode();
        const meshId = active?.mesh_id ?? ctx.meshes[0]?.id;
        if (meshId !== undefined) {
          meshStore.selectMesh(meshId);
          return true;
        }
      }
      if (useUIStore.getState().viewMode !== 'mesh') ctx.setViewMode('mesh');
      return true;
    }
    case 'view-all':
      if (useMeshStore.getState().selectedMeshId !== null) {
        useMeshStore.getState().selectMesh(null);
      } else if (useUIStore.getState().viewMode !== 'all') {
        ctx.setViewMode('all');
      }
      return true;
    case 'open-settings':
      useUIStore.getState().openAppSettings();
      return true;
    case 'open-remote-access':
      useUIStore.getState().openRemoteAccess();
      return true;
    case 'show-cheatsheet':
      useUIStore.getState().openCheatsheet();
      return true;
    case 'git-sync':
      void gitSyncAllMeshes(ctx.meshes);
      return true;
    default:
      if (id.startsWith('probe-')) {
        ctx.openProbeTab(id.slice('probe-'.length) as ProbeTab);
        return true;
      }
      return false;
  }
}

/**
 * "Git sync" command — fetch and pull every mesh (issue #1410 §1). Runs the
 * `git_sync` IPC SEQUENTIALLY (issue #1411 review): each sync is a real git
 * fetch/pull on the mesh's worktree, and launching a dozen of them in
 * parallel floods the backend and the disk. Failures are collected, don't
 * abort the remaining meshes, and surface as one summary toast — the same
 * feedback channel the per-mesh sync in <MeshItem> uses via mesh-sync
 * toasts.
 */
export async function gitSyncAllMeshes(meshes: Mesh[]): Promise<void> {
  const failed: string[] = [];
  for (const mesh of meshes) {
    try {
      await api.gitSync(mesh.path);
    } catch {
      failed.push(mesh.name);
    }
  }
  if (failed.length > 0) {
    addToast('Git sync', `Sync failed for: ${failed.join(', ')}`, 'warning');
  }
}

/**
 * Execute the item the user selected. `id` is the indexer's stable id.
 * Unknown / unresolvable ids (e.g. a node deleted between indexing and
 * selection) are a no-op — the palette has already closed by the time this
 * runs, so there is nothing to surface an error on.
 */
export function executeOmnibarItem(id: string, ctx: OmnibarActionContext): void {
  if (id.startsWith('node:')) {
    const nodeId = Number(id.slice('node:'.length));
    if (!Number.isFinite(nodeId)) return;
    const node = useAgentNodeStore.getState().agentNodes.find((item) => item.id === nodeId);
    if (!node) return;
    const wasSingle = useUIStore.getState().viewMode === 'single';
    useAgentNodeStore.getState().setActiveNode(node.id);
    useMeshStore.getState().selectMesh(node.mesh_id);
    // Mesh selection drives Mesh mode through the shared subscription. Only
    // restore Single when that was the source lens.
    if (wasSingle) ctx.setViewMode('single');
    return;
  }
  if (id.startsWith('mesh:')) {
    const meshId = Number(id.slice('mesh:'.length));
    if (!Number.isFinite(meshId)) return;
    if (!ctx.meshes.some((mesh) => mesh.id === meshId)) return;
    const changed = useMeshStore.getState().selectedMeshId !== meshId;
    useMeshStore.getState().selectMesh(meshId);
    if (!changed) ctx.setViewMode('mesh');
    return;
  }
  if (id.startsWith('command:')) {
    runOmnibarCommand(id.slice('command:'.length), ctx);
    return;
  }
  if (id.startsWith('spawn:')) {
    // Id shape is `spawn:<optionId>:<meshId>`; the option id is an opaque
    // spawn-option id (could itself contain a colon), so split from the
    // RIGHT and treat the last segment as the mesh id.
    const body = id.slice('spawn:'.length);
    const sep = body.lastIndexOf(':');
    if (sep === -1) return;
    const optionId = body.slice(0, sep);
    const meshId = Number(body.slice(sep + 1));
    const option = ctx.spawnOptions.find((o) => o.id === optionId);
    const mesh = ctx.meshes.find((m) => m.id === meshId);
    if (!option || !mesh) return;
    // Route through the same store action the sidebar spawn menu uses
    // (#283 invariant: create → activate → select mesh, atomically).
    void useAgentNodeStore
      .getState()
      .selectProviderForMesh(mesh.id, mesh.name, mesh.path, option.id)
      .catch(() => {
        // Spawn failures already toast via the store's error surface.
      });
    return;
  }
  if (id.startsWith('issue:') || id.startsWith('pull:')) {
    // Id shape is `issue:<meshId>:<number>` / `pull:<meshId>:<number>`.
    // The Probe's GitHub tabs read their mesh from `meshStore`, so an item
    // belonging to a mesh other than the currently selected one must
    // select its mesh first — otherwise the user lands on the tab showing
    // a DIFFERENT mesh's issues (issue #1411 review).
    const [, meshPart, numberPart] = id.split(':');
    const meshId = Number(meshPart);
    const number = Number(numberPart);
    const mesh = ctx.meshes.find((item) => item.id === meshId);
    if (!mesh || !Number.isFinite(number)) return;
    const changed = useMeshStore.getState().selectedMeshId !== mesh.id;
    useMeshStore.getState().selectMesh(mesh.id);
    if (!changed && useUIStore.getState().viewMode !== 'mesh') ctx.setViewMode('mesh');
    if (id.startsWith('issue:')) {
      requestIssueNavigation({ meshId: mesh.id, issueNumber: number });
      ctx.openProbeTab('issues');
    } else {
      ctx.openProbeTab('pulls');
      useUIStore.getState().openDiff({
        filePath: '',
        rootPath: mesh.path,
        nodeId: null,
        meshId: mesh.id,
        source: 'pr',
        prNumber: number,
      });
    }
    return;
  }
}

/**
 * The harness menu for the spawn domain. The provider list has no store —
 * every surface (Sidebar, Probe tabs, Settings) fetches `listProviders()`
 * on demand — so the palette fetches it fresh on EVERY open (issue #1411
 * review): one IPC per open keeps providers added in Settings (or newly
 * installed harnesses) appearing without an app restart, and there is no
 * module-level cache to invalidate. A failed fetch resolves to `[]` (the
 * spawn domain simply shows nothing).
 */
export async function loadSpawnOptions(): Promise<SpawnOption[]> {
  try {
    const backend = await api.listProviders();
    if (Array.isArray(backend)) return mapBackendProviders(backend);
  } catch {
    // Provider list unavailable (backend not ready, test env) — empty menu.
  }
  return [];
}
