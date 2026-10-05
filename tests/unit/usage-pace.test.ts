/**
 * Pure-helper tests for the UsageBar pace tick (`src/lib/usagePace.ts`).
 *
 * The wire type (`UsageWindow`) carries only `label` + `resetsAt` — no
 * period start — so "how far through the period" is inferred from the
 * label's nominal duration. These pin the label table and the
 * hide-instead-of-invent rule (unknown label, missing/unparsable/stale
 * reset → null, never a guessed tick).
 */

import { describe, it, expect } from 'vitest';
import { inferUsageWindowDurationMs, getUsageWindowPacePercent } from '../../src/lib/usagePace';

const HOUR = 3600_000;
const DAY = 24 * HOUR;
const NOW = Date.parse('2026-10-05T12:00:00Z');
const isoAfter = (ms: number) => new Date(NOW + ms).toISOString();

describe('inferUsageWindowDurationMs', () => {
  it.each([
    ['5-hour', 5 * HOUR],
    ['1-hour', HOUR],
    ['24h', 24 * HOUR],
    ['Weekly', 7 * DAY],
    ['Monthly', 30 * DAY],
    ['3h', 3 * HOUR],
    ['7d', 7 * DAY],
    ['90s', 90_000],
  ])('maps %s to its nominal duration', (label, expected) => {
    expect(inferUsageWindowDurationMs(label)).toBe(expected);
  });

  it.each(['  Weekly ', 'WEEKLY', '5-HOUR'])('is trim- and case-insensitive (%s)', (label) => {
    expect(inferUsageWindowDurationMs(label)).not.toBeNull();
  });

  it.each(['Fortnightly', '', 'hour', '5-hours'])('returns null for %s', (label) => {
    expect(inferUsageWindowDurationMs(label)).toBeNull();
  });
});

describe('getUsageWindowPacePercent', () => {
  it('returns the elapsed share mid-period', () => {
    const pace = getUsageWindowPacePercent({ label: '5-hour', usedPercent: 0, resetsAt: isoAfter(3.9 * HOUR) }, NOW);
    expect(pace).toBeCloseTo(22, 1);
  });

  it('returns 0 at the period start and 100 at the reset instant', () => {
    expect(getUsageWindowPacePercent({ label: '5-hour', usedPercent: 0, resetsAt: isoAfter(5 * HOUR) }, NOW)).toBe(0);
    expect(getUsageWindowPacePercent({ label: '5-hour', usedPercent: 0, resetsAt: isoAfter(0) }, NOW)).toBe(100);
  });

  it.each([
    ['missing reset', { label: '5-hour', usedPercent: 0, resetsAt: null }],
    ['unparsable reset', { label: '5-hour', usedPercent: 0, resetsAt: 'not-a-date' }],
    ['unknown label', { label: 'Fortnightly', usedPercent: 0, resetsAt: isoAfter(HOUR) }],
    ['reset in the past', { label: '5-hour', usedPercent: 0, resetsAt: isoAfter(-HOUR) }],
    ['reset beyond the duration (stale)', { label: '5-hour', usedPercent: 0, resetsAt: isoAfter(6 * HOUR) }],
  ])('returns null for %s', (_case, window) => {
    expect(getUsageWindowPacePercent(window, NOW)).toBeNull();
  });
});
