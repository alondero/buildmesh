/**
 * useMinuteClock — a clock that advances once a minute.
 *
 * Some readings are a function of wall-clock time rather than of the data
 * the app fetched: a circuit run that has stopped moving reads as stuck
 * after a fixed number of hours, and nothing sends an event when that
 * threshold is crossed (issue #2831). Without a clock of its own, such a
 * run keeps reading as "running" until the next reload.
 *
 * A minute is the resolution that matters — the threshold is in hours — and
 * it keeps the tab off the one-second clock that belongs to the run cards
 * (issue #1751).
 */

import { useEffect, useState } from 'react';

export function useMinuteClock(enabled = true): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    // No live run means nothing to re-judge: a terminal run's reading is
    // fixed by its own timestamps, and issue #1751 keeps a tab that shows
    // only finished work free of timers.
    if (!enabled) return;
    setNow(new Date());
    const timer = window.setInterval(() => setNow(new Date()), 60_000);
    return () => window.clearInterval(timer);
  }, [enabled]);
  return now;
}
