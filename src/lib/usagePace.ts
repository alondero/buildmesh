/**
 * Period-pace helper for the UsageBar tick. The wire type (`UsageWindow`)
 * carries only `label` + `resetsAt` — no period start — so "how far
 * through the period am I" is inferred from the label's nominal duration.
 * Anything unknowable returns null so the caller hides the tick instead
 * of inventing one.
 *
 * Supported shapes (each evidenced under `src-tauri/src/services/usage*`):
 *   - bare quota labels: `5-hour`, `1-hour`, `Weekly`, `Monthly`, Codex
 *     dynamic `Nh`/`Nd`/`Ns`, Anthropic `N-day` (`7-day`, `7-day Sonnet`)
 *   - trailing segment after ` — ` (Antigravity `Display — Weekly`) or
 *     ` · ` (Codex additional-bucket `name · 5-hour`)
 *   - Grok `Weekly Pool` / `Monthly Limit`
 * Deliberately unmatched (stay hidden): model-name labels (`Claude Sonnet
 * 4.6 (Thinking)`, `Gemini (all models)`), `Grok Build Quota` (unknown
 * period), Cursor `Fast Requests` (monthly grid, not a fixed duration).
 *
 * Pure — `nowMs` is an arg so renderers and tests pin the clock.
 */

import type { UsageAmount, UsageWindow } from './tauri';

const HOUR_MS = 3600_000;
const DAY_MS = 24 * HOUR_MS;

type WindowPeriod = Pick<UsageWindow, 'label' | 'resetsAt'>;
type AmountPeriod = Pick<UsageAmount, 'resetsAt'>;

type Period = { durationMs: number; calendarMonth: boolean };

/** Nominal period length for a bare period label, or null when unknown. */
function inferBarePeriod(text: string): Period | null {
  switch (text) {
    case '5-hour': return { durationMs: 5 * HOUR_MS, calendarMonth: false };
    case '1-hour': return { durationMs: HOUR_MS, calendarMonth: false };
    case 'weekly':
    case 'weekly pool': return { durationMs: 7 * DAY_MS, calendarMonth: false };
    case 'monthly':
    case 'monthly limit': return { durationMs: 30 * DAY_MS, calendarMonth: true };
    default: break;
  }
  const compact = /^(?<value>\d+)\s*(?<unit>h|d|s)$/.exec(text);
  if (compact?.groups) {
    const value = Number(compact.groups.value);
    switch (compact.groups.unit) {
      case 'h': return { durationMs: value * HOUR_MS, calendarMonth: false };
      case 'd': return { durationMs: value * DAY_MS, calendarMonth: false };
      case 's': return { durationMs: value * 1000, calendarMonth: false };
      default: return null;
    }
  }
  const days = /^(?<value>\d+)\s*-\s*day\b/.exec(text);
  if (days?.groups) return { durationMs: Number(days.groups.value) * DAY_MS, calendarMonth: false };
  return null;
}

function inferWindowPeriod(label: string): Period | null {
  const text = label.trim().toLowerCase();
  // "Display — Weekly" (Antigravity quota summary) and "name · 5-hour"
  // (Codex additional buckets) carry the period as the trailing segment —
  // infer from whatever follows the last separator of either kind.
  const cuts = [' — ', ' · '].map((sep) => {
    const i = text.lastIndexOf(sep);
    return i === -1 ? -1 : i + sep.length;
  });
  const cut = Math.max(...cuts);
  return inferBarePeriod(cut >= 0 ? text.slice(cut) : text);
}

/** Nominal period length for a window label, or null when unknown. */
export function inferUsageWindowDurationMs(label: string): number | null {
  return inferWindowPeriod(label)?.durationMs ?? null;
}

function parseResetMs(resetsAt: string | null): number | null {
  if (resetsAt == null) return null;
  const resetsAtMs = Date.parse(resetsAt);
  return Number.isFinite(resetsAtMs) ? resetsAtMs : null;
}

function previousMonthInstant(resetsAtMs: number): number {
  const reset = new Date(resetsAtMs);
  const year = reset.getUTCFullYear();
  const month = reset.getUTCMonth();
  const lastDayOfPreviousMonth = new Date(Date.UTC(year, month, 0)).getUTCDate();
  return Date.UTC(
    year,
    month - 1,
    Math.min(reset.getUTCDate(), lastDayOfPreviousMonth),
    reset.getUTCHours(),
    reset.getUTCMinutes(),
    reset.getUTCSeconds(),
    reset.getUTCMilliseconds(),
  );
}

function getMonthlyPacePercent(resetsAtMs: number, nowMs: number): number | null {
  const startMs = previousMonthInstant(resetsAtMs);
  if (nowMs < startMs || nowMs > resetsAtMs) return null;
  return ((nowMs - startMs) / (resetsAtMs - startMs)) * 100;
}

/**
 * Share (0–100) of the period elapsed at `nowMs`, or null when the
 * period can't be established. A reset in the past or further out
 * than the nominal duration means the reading is stale, not that
 * the user is 0% or 100% through — also null.
 */
export function getUsageWindowPacePercent(
  window: WindowPeriod,
  nowMs: number = Date.now(),
): number | null {
  const period = inferWindowPeriod(window.label);
  const resetsAtMs = parseResetMs(window.resetsAt);
  if (period == null || resetsAtMs == null) return null;
  if (period.calendarMonth) return getMonthlyPacePercent(resetsAtMs, nowMs);
  const remaining = resetsAtMs - nowMs;
  if (remaining < 0 || remaining > period.durationMs) return null;
  return ((period.durationMs - remaining) / period.durationMs) * 100;
}

export function getUsageAmountPacePercent(
  amount: AmountPeriod,
  nowMs: number = Date.now(),
): number | null {
  const resetsAtMs = parseResetMs(amount.resetsAt);
  return resetsAtMs == null ? null : getMonthlyPacePercent(resetsAtMs, nowMs);
}
