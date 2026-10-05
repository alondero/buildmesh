import type { InputStall } from '../../lib/terminalInputQueue';

/**
 * "Your input is not reaching the agent" (issue #1530).
 *
 * Renders only when the transport's ordered retry buffer has been holding a
 * session's bytes *past* its own stall threshold — a brief refusal is a blip
 * the writer thread drains on its own, and a badge for that would be noise in
 * an already-dense node header. By the time this shows, the agent has genuinely
 * stopped reading its input.
 *
 * Deliberately the smallest thing that conveys it: a bare warning glyph in the
 * existing badge slot beside `SignalHealthBadge`, with the words in the tooltip
 * and `aria-label` rather than in pixels. No new row, no new panel, no new
 * colour — `--color-status-warning` is already in the theme. The buffer retries
 * on its own, so the badge clears itself the moment the write lands; it is not
 * an error state the user has to dismiss.
 */
export function StalledInputBadge({ stall, compact = false }: { stall: InputStall | null; compact?: boolean }) {
  if (!stall) return null;
  const label = 'Input queued';
  const detail = `${label}: the agent is not reading its input, so ${stall.pendingBytes} ${
    stall.pendingBytes === 1 ? 'byte is' : 'bytes are'
  } waiting to be sent. It retries automatically — resend if the agent stays stuck.`;
  return (
    <span
      role="img"
      aria-label={detail}
      title={detail}
      data-testid="stalled-input-badge"
      className="inline-flex shrink-0 items-center gap-1 text-xs text-status-warning"
    >
      <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M12 7v5l3 2" />
        <circle cx="12" cy="12" r="9" />
      </svg>
      {!compact && <span>{label}</span>}
    </span>
  );
}
