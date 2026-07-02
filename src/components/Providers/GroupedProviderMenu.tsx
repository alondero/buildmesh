import { useMemo } from 'react';
import type { SpawnOption } from '../../lib/groups';
import { ProviderIcon } from './ProviderIcon';
import { groupByHarness } from '../../lib/groups';

export interface GroupedProviderMenuProps {
  /** Frontend view of the Spawn Menu (ADR-0016). Already in harness
   *  order; rows with the same `group_key` cluster under their harness
   *  header. `SpawnOption` (issue #583) is the post-`mapBackendProviders`
   *  shape — same fields `ProviderDropdown`/`SpawnButtonCluster` pass in. */
  providers: SpawnOption[];
  /** Called with `(providerId, altKey)` when the user picks a row. */
  onSelect: (providerId: string, altKey: boolean) => void;
  /** Optional filter (e.g. the archived-resume picker filters to
   *  `resumable: true`). Applied before grouping so the harness header
   *  is hidden when *all* its rows are filtered out. */
  filter?: (provider: SpawnOption) => boolean;
  /** Optional CSS class merged onto the root container. */
  className?: string;
}

/**
 * Harness-grouped, always-expanded Spawn Menu (issue #575 / ADR-0016).
 *
 * The backend emits a flat `Vec<ProviderInfo>` already in harness order
 * with `group_key == harness_id`; this component is a pure render — it
 * buckets by `group_key`, keeps the input order within each bucket
 * (so the native row lands first), and renders:
 *
 *   * **Harness header** — the first non-proxied row in each bucket
 *     (`is_proxied === false`) is the clickable native spawn. The whole
 *     row is the button. A `filter` that removes the native row
 *     collapses the bucket to just children (no header) — code-review
 *     finding from issue #575: rendering a Proxied child with the
 *     "harness" badge would mislead the user.
 *   * **Proxied children** — every `is_proxied: true` row in the bucket
 *     is rendered indented as a child button.
 *
 * No hover submenus, no click-to-collapse — the issue calls for an
 * always-expanded flat list so the most common pick (e.g. Claude Code
 * native, or MiniMax via Claude Code) is one click.
 */
export function GroupedProviderMenu({ providers, onSelect, filter, className }: GroupedProviderMenuProps) {
  // Group by `group_key`, preserving the backend's harness order and the
  // stable within-bucket order (native row first, then children in their
  // listed order). The filter is applied per-row BEFORE bucketing so a
  // filter-out row is dropped — and if it was the harness header, the
  // bucket collapses to just children (a valid grouped render).
  // The bucketing is shared with `MeshPropertiesTab` and the mobile
  // `ProviderPicker` via `groupByHarness` (issue #583 cleanup).
  const groups = useMemo(
    () => groupByHarness(providers, { filter }),
    [providers, filter],
  );

  return (
    <div className={className}>
      {groups.map(([groupKey, options]) => {
        // `options[0]` is normally the native harness header (the
        // backend builds it before appending proxied children for
        // the same harness). When a `filter` is provided, the native
        // row may have been filtered out, leaving the first child
        // in its place — that row would then render with a
        // misleading "harness" badge. Find the first non-proxied
        // row explicitly so a filter-out harness header collapses
        // gracefully (all surviving rows render as peers without
        // the harness badge).
        const native = options.find((o) => !o.is_proxied);
        const proxiedChildren = options.filter((o) => o.is_proxied);
        return (
          <div key={groupKey} data-spawn-group={groupKey} className="border-b border-border-subtle last:border-b-0">
            {native && (
              <button
                type="button"
                data-spawn-id={native.id}
                data-spawn-harness={native.harness_id}
                onClick={(e) => { e.stopPropagation(); onSelect(native.id, e.altKey); }}
                className="w-full text-left px-3 py-1.5 text-xs text-text-primary font-medium hover:bg-bg-card flex items-center gap-2"
              >
                <ProviderIcon providerId={native.id} className="h-3.5 w-3.5 shrink-0" />
                <span className="flex-1 truncate">{native.label}</span>
                {/* The "native" badge clarifies the row is a harness
                    launch, not a Proxied child — particularly useful
                    for the bare-Claude subscription in a host with
                    no other Claude-compatible accounts. */}
                <span className="text-2xs uppercase tracking-wider text-text-muted">harness</span>
              </button>
            )}
            {proxiedChildren.length > 0 && (
              <div className="pb-1">
                {proxiedChildren.map((child) => (
                  <button
                    type="button"
                    key={child.id}
                    data-spawn-id={child.id}
                    data-spawn-harness={child.harness_id}
                    onClick={(e) => { e.stopPropagation(); onSelect(child.id, e.altKey); }}
                    className="w-full text-left pl-7 pr-3 py-1 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
                  >
                    <ProviderIcon providerId={child.id} className="h-3.5 w-3.5 shrink-0" />
                    <span className="flex-1 truncate">{child.label}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
