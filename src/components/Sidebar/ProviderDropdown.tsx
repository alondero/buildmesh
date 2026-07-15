import { GroupedProviderMenu } from '../Providers/GroupedProviderMenu';
import { SafeLink } from '../shared/SafeLink';
import { hasSpawnableAgent, type SpawnOption } from '../../lib/groups';

// `SpawnOption` is the frontend view of the Spawn Option wire shape
// (issue #583) — produced by `mapBackendProviders` and consumed by the
// harness-grouped render. `GroupedProviderMenu` now consumes `SpawnOption`
// directly (not `ProviderInfo`) so the four desktop spawn surfaces
// share one frontend view and the boundary no longer needs a cast.

/** GitHub README anchor for the install prerequisites (issue #822). */
const PREREQUISITES_URL = 'https://github.com/alondero/buildmesh#prerequisites';

interface ProviderDropdownProps {
  meshId: number;
  providers: SpawnOption[];
  onSelect: (providerId: string, altKey: boolean) => void;
}

export function ProviderDropdown({ meshId, providers, onSelect }: ProviderDropdownProps) {
  // Issue #575 / ADR-0016 — render the harness-grouped, always-expanded
  // Spawn Menu. The single backend-derived list (issue #538 retired the
  // legacy enum-backed rows) is now grouped by `group_key` (== `harness_id`):
  // each harness's native row lands as a clickable header, every Proxied
  // child renders indented below it. Terminal is pinned last by the
  // backend's `order_providers` sort, so the visual order is identical
  // to the stored harness order.
  //
  // Issue #822 — first-run onboarding. On a fresh machine with no agent CLI
  // detected and no keyed provider, the backend emits only the Terminal
  // harness, so the menu is effectively empty with no hint about what to
  // install. Surface an empty-state panel above the (Terminal-only) menu
  // explaining the fix. Terminal stays clickable below — it's still a valid,
  // if limited, spawn — so this augments rather than replaces the menu.
  const noAgent = !hasSpawnableAgent(providers);
  return (
    <div
      data-dropdown-for={meshId}
      className="absolute right-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded-md shadow-md min-w-[200px] max-h-[400px] overflow-y-auto animate-scale-in origin-top-right"
    >
      {noAgent && (
        <div
          data-testid="spawn-menu-empty-state"
          className="px-3 py-2.5 border-b border-border-subtle"
          // Keep the panel from bubbling to the row's click-outside/toggle
          // handlers (the link has its own stopPropagation, but a click on
          // the surrounding text must not close the dropdown either).
          onClick={(e) => e.stopPropagation()}
        >
          <p className="text-xs font-medium text-text-primary">No agent CLIs found</p>
          <p className="mt-1 text-2xs text-text-muted leading-relaxed">
            Install one of Claude Code, Codex, Antigravity, or OpenCode, or add a
            provider key in Settings&nbsp;&rarr;&nbsp;Providers.
          </p>
          <SafeLink
            url={PREREQUISITES_URL}
            className="mt-1.5 inline-block text-2xs text-accent-cyan hover:underline"
            title="Open the setup instructions on GitHub"
          >
            View setup instructions&nbsp;&#8599;
          </SafeLink>
        </div>
      )}
      <GroupedProviderMenu providers={providers} onSelect={onSelect} />
    </div>
  );
}
