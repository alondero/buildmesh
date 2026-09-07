/** Status badge for a node whose attention signal is unavailable. */
export function SignalHealthBadge({ compact = false }: { compact?: boolean }) {
  return (
    <span
      role="img"
      aria-label="Attention signal unavailable"
      title="Attention signal unavailable — watch this session's terminal directly."
      className="inline-flex shrink-0 items-center gap-1 text-xs text-status-warning"
    >
      <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M8 7v4a4 4 0 0 0 8 0V7" />
        <path d="M6 7h4M14 7h4M4 4l16 16" />
      </svg>
      {!compact && <span>Signal unavailable</span>}
    </span>
  );
}
