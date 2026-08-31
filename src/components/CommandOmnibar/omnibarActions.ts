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
 * Surfaces that already own their own open-state (Settings and Remote Access
 * modals in <TitleBar>, the cheatsheet in <App>) are reached with window
 * CustomEvents rather than new store fields: the events reuse the existing
 * `window.dispatchEvent` pattern App.tsx already uses for
 * `shortcut-triggered`, and avoid growing `uiStore` with modal flags that
 * only one consumer reads.
 */
import type { Mesh } from '../../types/generated/Mesh';
import type { ProbeTab, ViewMode } from '../../stores/uiStore';
import type { SpawnOption } from '../../lib/groups';
import { currentTheme, setTheme, type ThemeName } from '../../lib/theme';
import { mapBackendProviders } from '../../lib/groups';
import * as api from '../../lib/tauri';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';

/** Window event that asks <App> to open the keyboard cheatsheet. */
export const OPEN_CHEATSHEET_EVENT = 'buildmesh:open-cheatsheet';
/** Window event that asks <TitleBar> to open the Settings modal. */
export const OPEN_SETTINGS_EVENT = 'buildmesh:open-settings';
/** Window event that asks <TitleBar> to open the Remote Access modal. */
export const OPEN_REMOTE_ACCESS_EVENT = 'buildmesh:open-remote-access';

/** Everything `executeOmnibarItem` needs beyond the stores themselves. */
export interface OmnibarActionContext {
  /** The live mesh list — needed to resolve spawn and git-sync targets. */
  meshes: Mesh[];
  /** The live spawn options (harnesses) — needed to resolve spawn targets. */
  spawnOptions: SpawnOption[];
  setViewMode: (mode: ViewMode) => void;
  openProbeTab: (tab: ProbeTab) => void;
}

function dispatchWindowEvent(name: string): void {
  window.dispatchEvent(new CustomEvent(name));
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
    case 'view-mesh':
    case 'view-pinned':
    case 'view-all':
      ctx.setViewMode(id.slice('view-'.length) as ViewMode);
      return true;
    case 'open-settings':
      dispatchWindowEvent(OPEN_SETTINGS_EVENT);
      return true;
    case 'open-remote-access':
      dispatchWindowEvent(OPEN_REMOTE_ACCESS_EVENT);
      return true;
    case 'show-cheatsheet':
      dispatchWindowEvent(OPEN_CHEATSHEET_EVENT);
      return true;
    case 'git-sync':
      // "Fetch and pull all meshes" (issue #1410 §1). Fire-and-forget per
      // mesh — the palette closes immediately; failures surface through the
      // existing mesh-sync-warning toasts the backend emits.
      for (const mesh of ctx.meshes) {
        void api.gitSync(mesh.path).catch(() => {
          // The backend already toasts sync failures; swallow here so an
          // unhandled rejection can't escape the palette's Enter handler.
        });
      }
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
 * Execute the item the user selected. `id` is the indexer's stable id.
 * Unknown / unresolvable ids (e.g. a node deleted between indexing and
 * selection) are a no-op — the palette has already closed by the time this
 * runs, so there is nothing to surface an error on.
 */
export function executeOmnibarItem(id: string, ctx: OmnibarActionContext): void {
  if (id.startsWith('node:')) {
    const nodeId = Number(id.slice('node:'.length));
    if (!Number.isFinite(nodeId)) return;
    useAgentNodeStore.getState().setActiveNode(nodeId);
    return;
  }
  if (id.startsWith('mesh:')) {
    const meshId = Number(id.slice('mesh:'.length));
    if (!Number.isFinite(meshId)) return;
    useMeshStore.getState().selectMesh(meshId);
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
    // Navigation entry point: jump the Probe to the mesh's GitHub tab. The
    // row itself doesn't carry enough context to deep-link into a single
    // issue/PR view — the tab is the discoverable surface for it.
    ctx.openProbeTab(id.startsWith('pull:') ? 'pulls' : 'issues');
    return;
  }
}

/**
 * The harness menu for the spawn domain. The provider list has no store —
 * every surface (Sidebar, Probe tabs, Settings) fetches `listProviders()`
 * on demand — so the palette caches its first successful projection in a
 * module-level variable: one IPC round-trip per app run, taken lazily on
 * the first palette open. A failed fetch resolves to `[]` (the spawn domain
 * simply shows nothing) rather than poisoning the cache, so the next open
 * retries.
 */
let spawnOptionsCache: SpawnOption[] | null = null;
export async function loadSpawnOptions(): Promise<SpawnOption[]> {
  if (spawnOptionsCache !== null) return spawnOptionsCache;
  try {
    const backend = await api.listProviders();
    if (Array.isArray(backend)) {
      spawnOptionsCache = mapBackendProviders(backend);
    } else {
      spawnOptionsCache = [];
    }
  } catch {
    return [];
  }
  return spawnOptionsCache;
}

/** Test-only: drop the cached spawn menu so a test's next open re-fetches. */
export function resetSpawnOptionsCacheForTests(): void {
  spawnOptionsCache = null;
}
