import type { ProviderInfo } from '../types/generated/ProviderInfo';

/**
 * Tailwind-class map for provider badges (issue #583 cleanup, moved out of
 * `Sidebar/ProviderDropdown.tsx`). Tailwind's purge tool needs static class
 * strings — the backend can't emit these dynamically, and `ProviderInfo.color`
 * (a hex string) is not what the frontend actually renders. Centralised here
 * so the helper below can fill in `color` from `id` without forcing every
 * caller to remember the mapping.
 */
const PROVIDER_COLOR_CLASS: Record<string, string> = {
  anthropic: 'bg-blue-500',
  claude: 'bg-blue-500', // Detected Claude Code profile id (#534).
  minimax: 'bg-indigo-500',
  kimi: 'bg-cyan-500',
  agy: 'bg-emerald-500',
  opencode: 'bg-amber-500',
  terminal: 'bg-gray-500',
};

/** Tailwind class for a provider's badge dot. Kept as a free function so
 *  existing call sites that need a one-off lookup (tests, sparse use)
 *  don't have to instantiate the projection helper. */
export function colorClassForProvider(providerId: string): string {
  return PROVIDER_COLOR_CLASS[providerId] ?? 'bg-gray-500';
}

/**
 * Frontend view of a **Spawn Option** (ADR-0016, issue #575). The full
 * `ProviderInfo` carries backend-only metadata (`default_branch`,
 * `description`, `model`, `resumable`) that no spawn-surface consumer
 * needs — the projection keeps only the fields the UI renders and
 * substitutes a Tailwind class for the backend's hex `color`.
 *
 * `resumable` is omitted on purpose: only the archived-resume picker
 * (`ArchivedNodesTab`) consumes it, and that caller merges it back in
 * via `mapBackendProviders(..).map(o => ({ ...o, resumable: ... }))`.
 * A wider shape would force every other consumer to carry a field it
 * never reads.
 */
export type SpawnOption = Pick<
  ProviderInfo,
  | 'id'
  | 'label'
  | 'icon'
  | 'harness_id'
  | 'provider_id'
  | 'is_proxied'
  | 'group_key'
> & {
  /** Tailwind class (e.g. `bg-blue-500`) derived from `id`. */
  color: string;
};

/**
 * Project the backend `ProviderInfo[]` (returned by `listProviders`)
 * into the frontend's `SpawnOption[]` shape (issue #583 cleanup — was
 * duplicated across `Sidebar`, `ArchivedNodesTab`, `GitIssuesTab`,
 * `GitPullRequestsTab`; #575 introduced the four near-identical
 * 8-field maps and the divergence trap the centralisation closes).
 *
 * Order is preserved (the backend already returns rows in
 * `(is_terminal, rank_of(harness_id))` order — see `order_providers`),
 * so the harness-grouped render keeps its stable header-first grouping.
 */
export function mapBackendProviders(backend: ProviderInfo[]): SpawnOption[] {
  return backend.map((p) => ({
    id: p.id,
    label: p.label,
    icon: p.icon,
    harness_id: p.harness_id,
    provider_id: p.provider_id,
    is_proxied: p.is_proxied,
    group_key: p.group_key,
    color: colorClassForProvider(p.id),
  }));
}

/**
 * True when the spawn menu offers at least one real agent harness — any
 * option that isn't the plain `terminal` shell.
 *
 * On a fresh machine with no agent CLI on `PATH` (`agent/detection.rs`)
 * and no keyed provider account, the backend's `compose_provider_menu`
 * emits only the code-defined Terminal harness
 * (`preferences::default_harness_profiles`) — so the spawn menu is
 * effectively empty. This predicate is that "near-empty" signal: it drives
 * the onboarding empty state in the Spawn Menu (issue #822). A keyed
 * provider always surfaces a Proxied row under a non-terminal harness id
 * (even when the Claude binary is absent — see
 * `preferences::claude_harness_id_from`), so "no CLI AND no key" is exactly
 * the terminal-only case this catches.
 */
export function hasSpawnableAgent(
  providers: Pick<SpawnOption, 'harness_id'>[],
): boolean {
  return providers.some((p) => p.harness_id !== 'terminal');
}

/**
 * Bucket a flat Spawn Option list by `group_key` into
 * `[harness_id, rows]` tuples, preserving input order within each
 * bucket so the native harness row lands first (the backend emits it
 * before its Proxied children, and the harness header badge is
 * load-bearing — see the defensive `find(!is_proxied)` filter in
 * `GroupedProviderMenu`).
 *
 * `ProviderInfo.group_key == harness_id` is a deliberate redundancy
 * (issue #575 / ADR-0016 §6) — the wire carries the grouping field
 * explicitly so the renderer never re-derives it. This helper is
 * the single source of truth for that bucketing (was duplicated in
 * `GroupedProviderMenu`, `MeshPropertiesTab`, and mobile
 * `NodeList`'s `ProviderPicker`).
 *
 * An optional `filter` is applied per-row BEFORE bucketing so a
 * filter that drops the native harness row collapses the bucket
 * gracefully (the renderer then has no header to mislabel with the
 * "harness" badge — see code-review finding B2 in #575).
 *
 * Generic over `T extends { group_key: string }` so the mobile
 * `Provider` shape (which carries `group_key` too) can reuse the
 * same bucketing without an additional projection.
 */
export function groupByHarness<T extends { group_key: string }>(
  providers: T[],
  opts?: { filter?: (provider: T) => boolean },
): [string, T[]][] {
  const filtered = opts?.filter ? providers.filter(opts.filter) : providers;
  const map = new Map<string, T[]>();
  for (const p of filtered) {
    const list = map.get(p.group_key);
    if (list) {
      list.push(p);
    } else {
      map.set(p.group_key, [p]);
    }
  }
  return Array.from(map.entries());
}
