/**
 * The one loading vocabulary. Three of the probe tabs had already converged
 * on this border-spinner independently; the rest showed bare "Loading…" text.
 * Every async surface should render one of these instead of hand-rolling.
 */
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
