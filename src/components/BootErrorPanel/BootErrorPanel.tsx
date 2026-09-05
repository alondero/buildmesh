/**
 * `<BootErrorPanel>` — full-panel error UI shown when one of the IPC
 * calls in `App.init()` rejects (issue #1250).
 *
 * Why this exists
 * ---------------
 * Before this panel existed, an init failure left the pulsing splash on
 * screen forever with no user-facing signal — the error went to
 * `console.error` and was otherwise discarded. A single rejected invoke
 * among `initAttentionListeners` / `fetchMeshes` / `fetchAgentNodes`
 * (e.g. corrupted DB) made the app look hung. Now the user gets an
 * honest error message with a Retry button.
 *
 * Visual vocabulary matches the existing `<ErrorBoundary>` (the canonical
 * full-panel error UI in this codebase): centered `bg-bg-base` wrapper,
 * `bg-bg-surface border-status-error/50` card, ⚠ icon + headline,
 * secondary description, raw error in a copyable `<pre>`. The two
 * surfaces stay visually consistent so users learn one error language
 * for the whole app.
 *
 * Accessibility
 * -------------
 * `role="alert"` on the outer wrapper means screen readers announce the
 * failure without the user having to discover the missing UI. The
 * default aria-live="assertive" is the right tone for "boot failed,
 * retry now."
 */
interface Props {
  /** Pre-formatted error string (`formatError(e)` from `lib/errorUtils`). */
  error: string;
  /** Re-runs `App.init()`. Bound to the Retry button. */
  onRetry: () => void;
  /**
   * True while a retry is in flight — disables the Retry button so a
   * panicking backend cannot get hammered with overlapping IPC bursts.
   * Mirrors the busy gate on `AccountCard`'s in-flight remove.
   */
  busy?: boolean;
}

export function BootErrorPanel({ error, onRetry, busy = false }: Props) {
  return (
    <div
      role="alert"
      className="flex h-screen w-screen items-center justify-center bg-bg-base text-text-primary p-8"
    >
      <div className="max-w-2xl w-full bg-bg-surface border border-status-error/50 rounded-md p-6 space-y-4">
        <div className="flex items-center gap-2">
          <div className="text-status-error text-2xl" aria-hidden>
            ⚠
          </div>
          {/* "Couldn't initialize Buildmesh" — distinct from
              ErrorBoundary's "render error" headline so users (and
              logs) can tell a boot failure from a mid-session crash. */}
          <h1 className="text-xl font-semibold">Couldn&apos;t initialize Buildmesh</h1>
        </div>
        <div className="text-sm text-text-secondary">
          Buildmesh failed to load its initial state. The raw error is
          below — you can copy it for a bug report. Details have also been
          written to <code className="text-text-primary font-medium">buildmesh.log</code>.
        </div>
        {/* Raw error in a copyable <pre> block, same pattern as
            ErrorBoundary: a fixed-height scrollable surface so a long
            stack trace can't blow up the layout, and the user can
            copy/paste without opening devtools. */}
        <pre className="text-xs text-text-primary bg-bg-base border border-border-subtle rounded-md p-3 overflow-auto max-h-64">
          {error}
        </pre>
        <button
          type="button"
          onClick={onRetry}
          disabled={busy}
          className="px-4 py-2 bg-accent-cyan text-text-inverse rounded-md hover:bg-accent-cyan/85 font-medium disabled:opacity-50 disabled:cursor-not-allowed"
        >
          Retry
        </button>
      </div>
    </div>
  );
}