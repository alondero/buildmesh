/**
 * Settings tab chrome vocabulary and small helpers shared by every pane
 * module (issue #1880). These live apart from `AppSettingsModal` so the
 * modal holds only the chrome that composes panes, and so a pane can
 * resolve its own dirty site / resource label without importing the
 * modal (which would be a cycle).
 */
import * as api from '../../lib/tauri';
import { isWindows } from '../../lib/platform';
import type { ResourceKey } from './useSettingsResources';

/** Sentinel for "no override" in the routing pickers. Distinct from an
 *  empty string, which is the stored form of "off". */
export const NO_OVERRIDE = '__no_override__';

/** The Settings sub-panes. Each pane groups settings by concern and owns the
 *  settings that belong to it: General = app behaviour + appearance + runtime
 *  defaults; Providers = provider routing defaults + credentials; Harnesses =
 *  spawn-menu composition + per-harness defaults; Remote Access = network
 *  reachability; Data & Diagnostics = the profile's durable state (snapshot,
 *  integrity check, export, restore — issue #1537). All panes stay MOUNTED
 *  (inactive ones get the `hidden` attribute) — the modal's dirty tracking
 *  (issue #730) lives in child component state, so unmounting a pane on
 *  tab-switch would destroy half-typed credentials while the modal still
 *  reports itself dirty. */
export const SETTINGS_TABS = [
  { id: 'general', label: 'General' },
  { id: 'providers', label: 'Providers' },
  { id: 'harnesses', label: 'Launch Configurations' },
  { id: 'remote', label: 'Remote Access' },
  { id: 'data', label: 'Data & Diagnostics' },
] as const;

export type SettingsTabId = (typeof SETTINGS_TABS)[number]['id'];

/** Which pane a dirty site belongs to, so its nav item can show the
 *  unsaved-changes dot. Site keys: `circuit-agent-pool` + `worktree-dir` +
 *  `probe-prompts` (General), `harness-defaults` and the prefixed
 *  `harness-*` sites wired by HarnessConfigList (Harnesses), and
 *  `account-*` / `add-custom-form` (Providers). The default-provider /
 *  reviewer / auto-naming selects save immediately and are never dirty. */
export function paneForDirtySite(site: string): SettingsTabId {
  if (site === 'circuit-agent-pool' || site === 'worktree-dir' || site === 'probe-prompts') return 'general';
  if (site.startsWith('harness-')) return 'harnesses';
  return 'providers';
}

/** Normalise empty-string api_key to null on the way in. The Rust side
 *  sometimes serialises empty keys as `""` rather than `null`; treating
 *  those as the same value lets the smart collapse actually collapse
 *  when the user backspaces to empty. */
export function normalizeApiKey(value: string | null | undefined): string | null {
  return value ? value : null;
}

/** Friendly pane name for a resource (issue #1534). Used in the
 *  per-resource banner so the message reads naturally ("Couldn't load
 *  paired devices" rather than "Couldn't load devices"). Kept
 *  here because every pane that surfaces a `ResourceLoadStatus` needs it,
 *  and duplicating the switch per pane would let the labels drift. */
export function humanResourceName(resource: ResourceKey): string {
  switch (resource) {
    case 'preferences':
      return 'preferences';
    case 'providers':
      return 'providers';
    case 'routing':
      return 'routing choices';
    case 'accounts':
      return 'provider accounts';
    case 'pairings':
      return 'harness pairings';
    case 'coordinator':
      return 'coordinator status';
    case 'devices':
      return 'paired devices';
    case 'network':
      return 'network status';
  }
}

/** Pairings load needs a runtime-specific helper for fetching
 *  verification state across Windows + WSL. Shared by the resource hook
 *  (issue #1534) and the Harnesses pane's live-verification listener. */
export async function getHostPairingVerifications(): Promise<api.PairingVerification[]> {
  const runtimes: api.EnvType[] = isWindows ? ['windows', 'wsl'] : ['windows'];
  return (await Promise.all(runtimes.map((runtime) => api.getPairingVerifications(runtime)))).flat();
}