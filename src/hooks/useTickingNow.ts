import { useEffect, useState } from 'react';

/**
 * A clock that ticks once a second while `ticking` is true, so an elapsed
 * time on screen advances instead of freezing at the moment it was
 * rendered. The interval only runs while there is something to measure, so
 * a settled operation costs nothing.
 *
 * The clock starts when the ticking begins, not when the component
 * mounted: a surface that opens while a clone is ALREADY running must show
 * the time since the clone started, not the time since the page loaded.
 */
export function useTickingNow(ticking: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!ticking) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [ticking]);
  return now;
}
