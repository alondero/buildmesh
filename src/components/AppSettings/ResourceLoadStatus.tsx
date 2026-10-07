/**
 * Per-resource load status surface (issue #1534), extracted from
 * `AppSettingsModal` in issue #1880 so every pane can render its own
 * failure banner without importing the modal. Renders a loading message
 * while in flight, and on failure an accessible error banner with a Retry
 * button that rehydrates ONLY this resource (siblings stay where they
 * are). Replaces the previous shape where the global `loaded` flag flipped
 * to `true` even after a failure, leaving the user staring with placeholder
 * default state and no signal that anything went wrong.
 *
 * Test-IDs follow the `resource-load-<key>` / `resource-load-<key>-retry`
 * convention so tests can target a specific resource's banner without
 * relying on prose that may change. */
import { Spinner } from '../shared/Spinner';
import { humanResourceName } from './settingsUtils';
import type { ResourceKey, ResourceState } from './useSettingsResources';

export function ResourceLoadStatus({
  resource,
  state,
  onRetry,
}: {
  resource: ResourceKey;
  state: ResourceState;
  /** Required for the `failed` banner (renders the Retry button);
   *  unused by the `loading` branch. If `onRetry` is undefined on
   *  a failed banner, we render an alert WITHOUT a Retry button
   *  rather than a button that no-ops — round-5 review caught the
   *  earlier "render the button unconditionally" hazard. */
  onRetry?: () => void;
}) {
  if (state.status === 'failed') {
    return (
      <div
        className="flex items-start gap-2 bg-bg-card border border-status-warning/40 rounded-md px-3 py-2"
        data-testid={`resource-load-${resource}`}
        // Issue #1534 (review round 2) — the failure banner is an
        // alert: screen readers announce it as soon as it appears
        // (`role="alert"` implicitly maps to `aria-live="assertive"`)
        // and a descriptive label gives the message a name beyond
        // "alert". Without these, a user relying on assistive tech
        // would never learn that the resource failed to load.
        role="alert"
        aria-live="assertive"
        aria-label={`Couldn't load ${humanResourceName(resource)}`}
      >
        <span className="flex-1 text-base text-text-primary">
          <span className="font-medium">Couldn’t load {humanResourceName(resource)}.</span>{' '}
          <span className="text-text-muted">{state.error ?? 'Unknown error.'}</span>
        </span>
        {onRetry && (
          <button
            type="button"
            onClick={onRetry}
            className="px-3 py-1 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-sm rounded-md hover:bg-bg-card-hover"
            data-testid={`resource-load-${resource}-retry`}
            // Scoped aria-label so a screen reader user hears which
            // resource this Retry button belongs to — multiple banners
            // on the same pane (e.g. preferences + providers both
            // failing) would otherwise announce two indistinguishable
            // "Retry" buttons.
            aria-label={`Retry loading ${humanResourceName(resource)}`}
          >
            Retry
          </button>
        )}
      </div>
    );
  }
  // loading (default branch — `idle` shouldn't reach the UI yet because
  // the dependent resource hasn't fired; the only resource that starts
  // at `idle` is pairings, which is rendered conditionally by the pane).
  return (
    <p
      className="flex items-center gap-2 text-base text-text-muted"
      data-testid={`resource-load-${resource}-loading`}
      // Polite live region so a screen reader announces the
      // "Loading…" state change without interrupting whatever's
      // mid-utterance. `role="status"` carries the implicit
      // `aria-live="polite"`.
      role="status"
      aria-live="polite"
    >
      {/* The provider list is dominated by subprocess probes for a
          routed Codex install (see `codex::discover_supported_install`),
          so the providers resource can sit in this state for seconds.
          A bare text line read as a hang — every control gated on it
          was disabled with no explanation of why. The shared `Spinner`
          keeps this banner in the same visual family as the probe
          tabs' `LoadingState` (issue #813); it is `aria-hidden` so the
          live region announces the text once, not the glyph. */}
      <Spinner className="w-3.5 h-3.5 shrink-0" />
      Loading {humanResourceName(resource)}…
    </p>
  );
}