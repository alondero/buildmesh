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

import type { UsageWindow } from './tauri';

const HOUR_MS = 3600_000;
const DAY_MS = 24 * HOUR_MS;

type WindowPeriod = Pick<UsageWindow, 'label' | 'resetsAt'>;

/** Nominal period length for a bare period label, or null when unknown. */
function inferBareDurationMs(text: string): number | null {
  switch (text) {
    case '5-hour': return 5 * HOUR_MS;
    case '1-hour': return HOUR_MS;
    case 'weekly':
    case 'weekly pool': return 7 * DAY_MS;
    case 'monthly':
    case 'monthly limit': return 30 * DAY_MS;
    default: break;
  }
  const compact = /^(?<value>\d+)\s*(?<unit>h|d|s)$/.exec(text);
  if (compact?.groups) {
    const value = Number(compact.groups.value);
    switch (compact.groups.unit) {
      case 'h': return value * HOUR_MS;
      case 'd': return value * DAY_MS;
      case 's': return value * 1000;
      default: return null;
    }
  }
  const days = /^(?<value>\d+)\s*-\s*day\b/.exec(text);
  if (days?.groups) return Number(days.groups.value) * DAY_MS;
  return null;
}

/** Nominal period length for a window label, or null when unknown. */
export function inferUsageWindowDurationMs(label: string): number | null {
  const text = label.trim().toLowerCase();
  // "Display — Weekly" (Antigravity quota summary) and "name · 5-hour"
  // (Codex additional buckets) carry the period as the trailing segment —
  // infer from whatever follows the last separator of either kind.
  const cuts = [' — ', ' · '].map((sep) => {
    const i = text.lastIndexOf(sep);
    return i === -1 ? -1 : i + sep.length;
  });
  const cut = Math.max(...cuts);
  return inferBareDurationMs(cut >= 0 ? text.slice(cut) : text);
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
  const duration = inferUsageWindowDurationMs(window.label);
  if (duration == null || window.resetsAt == null) return null;
  const resetsAt = Date.parse(window.resetsAt);
  if (!Number.isFinite(resetsAt)) return null;
  const remaining = resetsAt - nowMs;
  if (remaining < 0 || remaining > duration) return null;
  return ((duration - remaining) / duration) * 100;
}
