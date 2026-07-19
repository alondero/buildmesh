/**
 * The async-state vocabulary for full-tab fetch / empty / error
 * surfaces (issue #813). Inline banner errors and toolbar action
 * feedback are intentionally NOT here — those have different
 * typography and live next to their trigger, not centred in a body.
 */
import type { ReactNode } from 'react';

export function Spinner({ className = 'w-5 h-5' }: { className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={`inline-block animate-spin border border-accent-cyan border-t-transparent rounded-full ${className}`}
    />
  );
}

/** Centered spinner + caption for a panel/tab body while it fetches. */
export function LoadingState({ label = 'Loading…' }: { label?: string }) {
  return (
    <div role="status" className="flex flex-col items-center justify-center gap-2 py-8 animate-fade-in">
      <Spinner />
      <span className="text-xs text-text-muted">{label}</span>
    </div>
  );
}

interface EmptyStateProps {
  /** The headline (e.g. "No open issues"). Shown muted, primary copy. */
  label: string;
  /** Optional smaller hint line below the label (kept short — full
   *  paragraphs belong in the loading/error variants). */
  hint?: string;
  /** Optional glyph rendered inside the soft circular badge above the
   *  label, in place of the default `i`-glyph. Accepts any ReactNode —
   *  the probe passes its Lucide-style SVG icons (`w-5 h-5`, coloured by
   *  the badge's `text-text-muted`); a plain string (e.g. an emoji) is
   *  rendered at badge size for legacy callers. */
  icon?: ReactNode;
  /** Fill the parent's height (host-shell case where the empty state is
   *  the only thing in the body); the default `py-8` only sizes the
   *  content. When false the empty state sits at natural content size and
   *  centers within whatever flex/scroll context the caller provides. */
  fill?: boolean;
  /** `data-testid` for tests that want to assert presence. The same label
   *  text is also reachable via `getByText`/`findByText`. */
  testId?: string;
}

/**
 * Soft-badge glyph + label for the "fetched successfully but the list is
 * empty" case. Pairs with `LoadingState` and `ErrorState`. The glyph sits
 * in a quiet circular chip so the state reads as intentional rather than
 * "content missing", and the iconography stays consistent across tabs.
 */
export function EmptyState({ label, hint, icon, fill, testId }: EmptyStateProps) {
  return (
    <div
      role="status"
      data-testid={testId}
      className={`flex flex-col items-center justify-center text-center animate-fade-in ${fill ? 'h-full' : 'py-8'}`}
    >
      <div
        aria-hidden="true"
        className="w-11 h-11 rounded-full bg-bg-card border border-border-subtle flex items-center justify-center mb-3 text-text-muted"
      >
        {icon ? (
          typeof icon === 'string' ? (
            <span className="text-lg leading-none">{icon}</span>
          ) : (
            icon
          )
        ) : (
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            className="w-5 h-5"
          >
            <circle cx="12" cy="12" r="10" />
            <line x1="12" y1="8" x2="12" y2="12" />
            <line x1="12" y1="16" x2="12.01" y2="16" />
          </svg>
        )}
      </div>
      <span className="text-xs text-text-muted">{label}</span>
      {hint && (
        <span className="text-2xs text-text-muted/80 mt-1 max-w-[280px]">{hint}</span>
      )}
    </div>
  );
}

interface ErrorStateProps {
  /** A short headline naming the failure in user terms (e.g. "Failed to
   *  load issues"). The raw error message goes in `detail`, not here. */
  title: string;
  /** The raw rejection message from the IPC. Optional — some callers
   *  (e.g. a permission gate) have a structured error that does not
   *  need to surface verbatim. Truncated visually to a max-width so
   *  a runaway stack-trace string doesn't blow up the tab body. */
  detail?: string | null;
  /** `data-testid` for tests that want to assert presence. The title text
   *  is also reachable via `getByText`/`findByText`. */
  testId?: string;
}

/**
 * Soft red badge + title + raw IPC message. `role="alert"` so AT users
 * hear the failure; the icon is `aria-hidden` so the "X circle" is
 * not announced on its own. Shares the `EmptyState` badge shape so the
 * three async states read as one family.
 */
export function ErrorState({ title, detail, testId }: ErrorStateProps) {
  return (
    <div
      role="alert"
      data-testid={testId}
      className="flex flex-col items-center justify-center py-8 animate-fade-in"
    >
      <div
        aria-hidden="true"
        className="w-11 h-11 rounded-full bg-status-error-bg border border-status-error/20 flex items-center justify-center mb-3 text-status-error"
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          className="w-5 h-5"
        >
          <circle cx="12" cy="12" r="10" />
          <line x1="15" y1="9" x2="9" y2="15" />
          <line x1="9" y1="9" x2="15" y2="15" />
        </svg>
      </div>
      <span className="text-xs text-status-error">{title}</span>
      {detail && (
        <span className="text-2xs text-text-muted mt-1 max-w-[280px] text-center">
          {detail}
        </span>
      )}
    </div>
  );
}

interface RefreshControlProps {
  /** Click handler — usually a `() => void reload()` closure that also
   *  flips an `isRefreshing` flag. Safe to be async; the button
   *  controls its own busy state via `isRefreshing`, but the caller
   *  must flip the flag in a `try/finally` so a rejected fetch can't
   *  leave the button stuck disabled (mirrors `UsageTab`'s pattern). */
  onRefresh: () => void;
  /** True while the refresh is in flight. Renders an inline spinner
   *  ahead of the label and sets `aria-busy`/`disabled`. */
  isRefreshing: boolean;
  /** Visual treatment. `'default'` is the cyan accent used by fetch-
   *  driven tabs (Issues, PRs, Archive, Usage); `'muted'` sits visually
   *  quieter so it doesn't compete with adjacent destructive actions
   *  (e.g. WorktreeManagerTab's Refresh next to Delete Selected). */
  variant?: 'default' | 'muted';
  /** External disable independent of `isRefreshing`. Used when a sibling
   *  long-running operation (e.g. WorktreeManagerTab's deletion pass)
   *  would race the refresh's `load()` — the inline button this replaces
   *  honored `loading || deleting` and we preserve that contract here. */
  disabled?: boolean;
  /** Visible label on the button (e.g. "Refresh", "Rescan"). */
  label?: string;
  /** Accessible name. Defaults to the visible label — pass an explicit
   *  value when the label is short or context-specific ("Refresh
   *  issues", "Refresh usage") so AT users get the same specificity
   *  the icon-only render provides. The existing UsageTab test asserts
   *  `aria-label="Refresh usage"`, so consumers can preserve the
   *  selector by passing an explicit string here. */
  ariaLabel?: string;
}

/**
 * Manual-refresh affordance shared by the fetch-driven probe tabs
 * (issue #813). Composes the shared `<Spinner>` so the in-flight
 * glyph stays in lockstep with `LoadingState`.
 */
export function RefreshControl({
  onRefresh,
  isRefreshing,
  variant = 'default',
  disabled = false,
  label = 'Refresh',
  ariaLabel,
}: RefreshControlProps) {
  const colourClass =
    variant === 'muted'
      ? 'text-text-muted hover:text-text-primary'
      : 'text-accent-cyan hover:text-accent-cyan/80';
  const disabledClass =
    variant === 'muted'
      ? 'disabled:cursor-not-allowed disabled:opacity-50'
      : 'disabled:cursor-not-allowed disabled:hover:text-accent-cyan';
  return (
    <button
      type="button"
      onClick={onRefresh}
      disabled={disabled || isRefreshing}
      aria-busy={isRefreshing}
      aria-label={ariaLabel ?? label}
      className={`text-xs ${colourClass} ${disabledClass} inline-flex items-center gap-1.5`}
    >
      {isRefreshing && <Spinner className="w-3 h-3" />}
      {label}
    </button>
  );
}
