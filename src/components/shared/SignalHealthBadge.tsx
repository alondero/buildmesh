import type { SignalHealth } from '../../types/generated/SignalHealth';
import { isSignalHealthProblem, signalHealthNote } from '../../lib/status';

/**
 * Node-level status-reporting fault (issue #1364 §3).
 *
 * Renders only for a health the user can act on: `degraded` (a callback arrived
 * but could not be interpreted) and `unavailable` (setup failed, or the harness
 * declares no observer). `unverified` is not a fault — it means hooks are
 * installed and nothing has gone wrong yet — so it stays out of the scarce
 * title-bar space and is carried by the status tooltip instead.
 */
export function SignalHealthBadge({ compact = false, health }: { compact?: boolean; health?: SignalHealth | null }) {
  if (!isSignalHealthProblem(health)) return null;
  const label = health === 'degraded' ? 'Signal degraded' : 'Signal unavailable';
  return (
    <span
      role="img"
      aria-label={health === 'unavailable' ? 'Attention signal unavailable' : label}
      title={`${label}. ${signalHealthNote(health)}`}
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
