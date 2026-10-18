/**
 * `formatListTimestamp` — the label a Probe list row shows for a stored
 * timestamp (issue #2461).
 *
 * It owns two behaviours that used to live inside `ArchivedNodesTab` and are
 * easy to regress when the next tab asks for the same label: the historical
 * "days never reset" cut to an absolute date at 30 days, and passing an
 * unparseable value through instead of formatting NaN.
 */

import { describe, it, expect } from 'vitest';
import { formatListTimestamp } from '../../src/lib/time';

const NOW = new Date('2026-10-18T12:00:00.000Z');
const ago = (ms: number) => new Date(NOW.getTime() - ms).toISOString();

describe('formatListTimestamp', () => {
  it('reads as a relative age while the timestamp is young', () => {
    expect(formatListTimestamp(ago(5 * 60_000), NOW)).toBe('5m ago');
    expect(formatListTimestamp(ago(3 * 3_600_000), NOW)).toBe('3h ago');
    expect(formatListTimestamp(ago(4 * 86_400_000), NOW)).toBe('4d ago');
  });

  it('cuts over to an absolute date past 30 days', () => {
    const old = '2026-08-01T09:30:00.000Z';
    expect(formatListTimestamp(old, NOW)).toBe(new Date(old).toLocaleDateString());
    expect(formatListTimestamp(old, NOW)).not.toMatch(/ago$/);
  });

  it('parses the nanosecond precision the backend stores', () => {
    // Rust serialises `DateTime<Utc>` with nine fractional digits; `Date`
    // keeps milliseconds, so the label must not echo the raw digits.
    const nanos = '2026-10-18T11:55:00.000596700Z';
    expect(formatListTimestamp(nanos, NOW)).toBe('5m ago');
  });

  it('passes an unparseable value through instead of formatting NaN', () => {
    expect(formatListTimestamp('not-a-timestamp', NOW)).toBe('not-a-timestamp');
  });
});