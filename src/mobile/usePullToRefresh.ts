import { useRef, useState, type TouchEvent } from "react";

// Pull-to-refresh for the mobile node list (issue #1377). Dragging down on a
// vertical scroller that is already at its top re-runs the list refresh —
// the same gesture every native mail/twitter app trains, and cheaper than a
// header button when one hand is on the phone.
//
// The browser's own pull-to-refresh stays out of the way: the document never
// scrolls (styles.css pins it), and the scroller gets
// `overscroll-behavior: contain` (see .list-scroll) so the drag is ours to
// interpret. No preventDefault anywhere — React attaches touchmove listeners
// passively, and with the overscroll chain contained a top-of-list downward
// drag scrolls nothing natively anyway.
//
// Axis choice: only *downward* drags (dy > 0) count, and they only count
// while the scroller is actually at scrollTop 0 — scrolling back up through
// history must never look like a refresh pull. The distance is damped
// (DAMPING) so the indicator tracks the finger with a bit of rubber-band
// resistance, capped at MAX_PULL_PX.

const THRESHOLD_PX = 64;
const MAX_PULL_PX = 88;
const DAMPING = 0.4;

export const PULL_REFRESH_THRESHOLD_PX = THRESHOLD_PX;

type PullHandlers = {
  onTouchStart: (e: TouchEvent) => void;
  onTouchMove: (e: TouchEvent) => void;
  onTouchEnd: (e: TouchEvent) => void;
  onTouchCancel: (e: TouchEvent) => void;
};

export function usePullToRefresh(
  onRefresh: () => Promise<void> | void,
  enabled: boolean,
): { pull: number; refreshing: boolean; handlers: PullHandlers } {
  const [pull, setPull] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  const startYRef = useRef<number | null>(null);
  // Latest pull distance, mirrored out of state: touchend must read the
  // distance the last touchmove actually reached, not whichever render the
  // handler closure was built from (state updates from move events race the
  // end event under batching).
  const pullRef = useRef(0);
  // Mirrors `refreshing` for the touch handlers — state updates are async,
  // and a second finger landing mid-refresh must not queue a second refresh.
  const refreshingRef = useRef(false);
  const onRefreshRef = useRef(onRefresh);
  onRefreshRef.current = onRefresh;

  const endGesture = () => {
    startYRef.current = null;
    pullRef.current = 0;
    setPull(0);
  };

  const handlers: PullHandlers = {
    onTouchStart(e) {
      if (!enabled || refreshingRef.current) return;
      startYRef.current = e.touches[0].clientY;
    },
    onTouchMove(e) {
      const startY = startYRef.current;
      if (startY === null) return;
      const dy = e.touches[0].clientY - startY;
      if (dy <= 0 || e.currentTarget.scrollTop > 0) {
        // Dragged up (normal scrolling) or the list scrolled away from the
        // top mid-gesture — reset the anchor so a later downward drag inside
        // the same touch re-evaluates from its own start point.
        startYRef.current = e.touches[0].clientY;
        pullRef.current = 0;
        setPull(0);
        return;
      }
      const next = Math.min(dy * DAMPING, MAX_PULL_PX);
      pullRef.current = next;
      setPull(next);
    },
    onTouchEnd() {
      const pulled = pullRef.current;
      startYRef.current = null;
      pullRef.current = 0;
      setPull(0);
      if (enabled && !refreshingRef.current && pulled >= THRESHOLD_PX) {
        refreshingRef.current = true;
        setRefreshing(true);
        void Promise.resolve(onRefreshRef.current()).finally(() => {
          refreshingRef.current = false;
          setRefreshing(false);
        });
      }
    },
    onTouchCancel: endGesture,
  };

  return { pull, refreshing, handlers };
}
