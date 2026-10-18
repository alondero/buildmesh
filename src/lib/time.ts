/**
 * Shared relative-time formatter. Three callers (UsageTab,
 * ArchivedNodesTab, ArchivedNodesScreen) had drifted into three
 * near-identical copies of `timeAgo(...)`; this module is the
 * single source.
 *
 * Granularity option. `UsageTab` ticks every second and shows
 * "Ns ago"; `ArchivedNodesTab` only renders once per list load
 * and shows "Nm ago" from the first minute. Forced parity on
 * either side is a regression — Usage users would lose the
 * real-second grain they watch, Archive users would pay an
 * unmount/remount cadence for nothing. Each caller passes the
 * grain it wants; the helper itself stays dumb.
 */
export type RelativeAgeGranularity = 'minute' | 'second';

interface FormatOptions {
  /** Smallest unit the formatter will break out. Default `'minute'`
   * (matches the historical `timeAgo` shape). `'second'` adds an
   * extra tier for the 30s-60s range and floor-floors everything
   * else the same way. */
  granularity?: RelativeAgeGranularity;
  /** Seconds below which the helper prints "just now" instead of
   * a numeric label. Default 30. The chosen floor so the label
   * doesn't churn in the first half-minute even at second
   * granularity. */
  justNowBelowSeconds?: number;
}

const DEFAULT_JUST_NOW_BELOW = 30;

/**
 * Format the gap between `then` and `now` as a relative-age label.
 *
 * Pure — `now` is taken as an arg so React renderers (and tests)
 * can pin the clock without freezing `Date.now()`.
 *
 * Floor-based grain — the label never feels jumpy because each
 * tier cuts to a single number:
 *   < floor       → "just now"         (no churn below the floor)
 *   < 60s (sec)   → "Ns ago"           (only when granularity='second')
 *   < 60m         → "Nm ago"           (minute-floor — 5m30s reads as "5m ago")
 *   < 24h         → "Nh ago"
 *   else          → "Nd ago"           (callers pair with an absolute `title`
 *                                       for sub-day precision on old items)
 *
 * Negative diffs clamp to 0 (clock skew shouldn't read as "future").
 */
export function formatRelativeAge(
  then: Date,
  now: Date,
  options: FormatOptions = {},
): string {
  const granularity = options.granularity ?? 'minute';
  const justNowBelow = options.justNowBelowSeconds ?? DEFAULT_JUST_NOW_BELOW;
  const seconds = Math.max(0, Math.floor((now.getTime() - then.getTime()) / 1000));
  if (seconds < justNowBelow) return 'just now';
  if (granularity === 'second' && seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

/**
 * The timestamp label a *list row* shows: relative age while the row is
 * young, an absolute date once "Nd ago" stops meaning anything.
 *
 * The 30-day cut is the historical "days never reset" convention the Archive
 * list already rendered. `formatRelativeAge` floors at 24h + `days`
 * indefinitely, so the older-of-the-two tier has to be handled here.
 *
 * Lives beside `formatRelativeAge` rather than in one tab because the raw
 * value the backend stores (`created_at`, `lifecycle.timestamp`) is an
 * RFC 3339 string with Rust nanosecond precision — `2026-10-17T22:43:58.211596700Z`.
 * Printing it verbatim is what the Probe agent-history cards did until
 * issue #2461, and it is unreadable at 240px.
 *
 * An unparseable value passes through unchanged rather than becoming
 * "NaNd ago", so this formatter yields a usable label on every path.
 *
 * Deliberately not the single timestamp renderer: the status tooltip in
 * `lib/status.ts` quotes the stored value on purpose (full precision under a
 * hover), the mobile Archive screen keeps its own no-date-cut relative label,
 * and the recovery log prints a file-snapshot stamp. Route a new list row
 * here; leave those as they are.
 */
export function formatListTimestamp(iso: string, now: Date = new Date()): string {
  const then = new Date(iso);
  if (Number.isNaN(then.getTime())) return iso;
  const days = Math.floor((now.getTime() - then.getTime()) / (24 * 60 * 60 * 1000));
  if (days >= 30) return then.toLocaleDateString();
  return formatRelativeAge(then, now);
}
