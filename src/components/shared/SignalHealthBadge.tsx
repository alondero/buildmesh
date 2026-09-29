import type { SignalHealth } from '../../types/generated/SignalHealth';

/** Installation and observed delivery are different evidence levels. */
export function SignalHealthBadge({ compact = false, health = 'unavailable' }: { compact?: boolean; health?: SignalHealth }) {
  if (health === 'ok') return null;
  const label = health === 'unverified' ? 'Status unverified' : health === 'degraded' ? 'Signal degraded' : 'Signal unavailable';
  return (
    <span
      role="img"
      aria-label={health === 'unavailable' ? 'Attention signal unavailable' : label}
      title={`${label}. Check the terminal; configuration alone does not prove lifecycle delivery.`}
      className="inline-flex shrink-0 items-center gap-1 text-xs text-status-warning"
    >
      <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M8 7v4a4 4 0 0 0 8 0V7" />
        <path d="M6 7h4M14 7h4M4 4l16 16" />
      </svg>
      {!compact && <span>{label}</span>}
    </span>
  );
}
