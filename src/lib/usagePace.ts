/**
 * Period-pace helper for the UsageBar tick. The wire type (`UsageWindow`)
 * carries only `label` + `resetsAt` — no period start — so "how far
 * through the period am I" is inferred from the label's nominal duration.
 * Anything unknowable (unknown label, missing/unparsable/stale reset)
 * returns null so the caller hides the tick instead of inventing one.
 *
 * Pure — `nowMs` is an arg so renderers and tests pin the clock.
 */

const HOUR_MS = 3600_000;
const DAY_MS = 24 * HOUR_MS;

/** Nominal period length for a window label, or null when unknown. */
export function inferUsageWindowDurationMs(label: string): number | null {
  const text = label.trim().toLowerCase();
  switch (text) {
    case '5-hour': return 5 * HOUR_MS;
    case '1-hour': return HOUR_MS;
    case '24h': return 24 * HOUR_MS;
    case 'weekly': return 7 * DAY_MS;
    case 'monthly': return 30 * DAY_MS;
    default: break;
  }
  const amount = /^(?<value>\d+)\s*(?<unit>h|d|s)$/.exec(text);
  if (!amount?.groups) return null;
  const value = Number(amount.groups.value);
  switch (amount.groups.unit) {
    case 'h': return value * HOUR_MS;
    case 'd': return value * DAY_MS;
    case 's': return value * 1000;
    default: return null;
  }
}

/**
 * Share (0–100) of the period elapsed at `nowMs`, or null when the
 * period can't be established. A reset in the past or further out
 * than the nominal duration means the reading is stale, not that
 * the user is 0% or 100% through — also null.
 */
export function getUsageWindowPacePercent(
  window: { label: string; resetsAt: string | null },
  nowMs: number = Date.now(),
): number | null {
  const duration = inferUsageWindowDurationMs(window.label);
  if (duration == null || window.resetsAt == null) return null;
  const resetsAt = Date.parse(window.resetsAt);
  if (!Number.isFinite(resetsAt)) return null;
  const remaining = resetsAt - nowMs;
  if (remaining < 0 || remaining > duration) return null;
  return ((duration - remaining) / duration) * 100;
}
