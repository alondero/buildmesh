import * as api from '../../lib/tauri';
import type { ProviderInfo } from '../../lib/tauri';

/**
 * Memoised lookups for the "Handover to new Node [<provider>]" context-menu
 * label.
 *
 * The label is two pieces of data: the mesh's default provider id
 * (`getDefaultProvider`, mesh-scoped) and the static provider list
 * (`listProviders`, process-global). Every `AgentTerminal` used to fetch both
 * on mount, so switching to a mesh with N open nodes fired 2×N identical IPC
 * calls just to label one hidden menu item — a burst that competed with the
 * navigation it accompanied.
 *
 * These helpers cache each call so the provider list is fetched at most once
 * per process and the default provider at most once per mesh, shared across
 * every node. A rejected fetch is evicted so the next caller retries rather
 * than inheriting a permanently-failed promise.
 */

let providerListPromise: Promise<ProviderInfo[]> | null = null;
const defaultProviderByMesh = new Map<number, Promise<string>>();

function fetchProviderList(): Promise<ProviderInfo[]> {
  if (!providerListPromise) {
    providerListPromise = api.listProviders();
    providerListPromise.catch(() => { providerListPromise = null; });
  }
  return providerListPromise;
}

function fetchDefaultProvider(meshId: number): Promise<string> {
  let p = defaultProviderByMesh.get(meshId);
  if (!p) {
    p = api.getDefaultProvider(meshId);
    p.catch(() => defaultProviderByMesh.delete(meshId));
    defaultProviderByMesh.set(meshId, p);
  }
  return p;
}

/**
 * Resolve the display label for a mesh's default provider, deduping the
 * underlying IPC across every node in that mesh. Falls back to `'Default'` if
 * either lookup fails.
 */
export async function getHandoverProviderLabel(meshId: number): Promise<string> {
  try {
    const [defProvider, providers] = await Promise.all([
      fetchDefaultProvider(meshId),
      fetchProviderList(),
    ]);
    return providers.find(p => p.id === defProvider)?.label ?? defProvider;
  } catch {
    return 'Default';
  }
}

/** Test-only: clear the module-level caches between cases. */
export function __resetHandoverProviderCacheForTests(): void {
  providerListPromise = null;
  defaultProviderByMesh.clear();
}
