import { useCallback, useEffect, useRef, useState, type TouchEvent } from "react";

// Pull-to-refresh for the mobile node list (issue #1377, post-review).
// Dragging down on a vertical scroller that is already at its top re-runs
// the list refresh — the same gesture every native mail/twitter app
// trains, and cheaper than a header button when one hand is on the phone.
//
// The browser's own pull-to-refresh stays out of the way: the document
// never scrolls (styles.css pins it), and the scroller gets
// `overscroll-behavior: contain` (see .list-scroll) so the drag is ours
// to interpret. No preventDefault anywhere — React attaches touchmove
// listeners passively, and with the overscroll chain contained a
// top-of-list downward drag scrolls nothing natively anyway.
//
// Axis choice: only *downward* drags (dy > 0) count, and they only count
// while the scroller is actually at scrollTop 0 — scrolling back up
// through history must never look like a refresh pull. The distance is
// damped (DAMPING) so the indicator tracks the finger with a bit of
// rubber-band resistance, capped at MAX_PULL_PX.
//
// Horizontal-axis lock (#1377 review feedback): the attention deck is a
// horizontal carousel — a slight downward diagonal stroke on the deck must
// not hijack the gesture. We require dy > Math.abs(dx) * DOMINANT_RATIO
// before the pull engages (and reset the anchor if a horizontal swipe
// takes over mid-drag).
//
// Performance (#1377 review feedback): the previous implementation called
// `setPull(dy)` on every raw touchmove, which re-rendered the entire
// NodeList tree (mesh sections, node rows, deck cards) at 60–120 Hz. We
// now mirror the pull distance directly into a CSS custom property
// (`--pull-translate`) on the scroll container via ref, and only call
// `setPull` for the indicator's state and label. The indicator uses
// `transform: translateY(...)` so it stays on the GPU compositor and
// skips layout entirely. The list scrolls below it, but the indicator
// floats above — see styles.css `.pull-indicator`.
//
// Unmount safety (#1377 review feedback): the refresh callback's
// `finally` previously called `setRefreshing(false)` against an unmounted
// component if the user navigated away mid-refresh. `mountedRef` guards
// that path.

const THRESHOLD_PX = 64;
const MAX_PULL_PX = 88;
const DAMPING = 0.4;
// Horizontal-lock ratio: dy must exceed |dx| * this before we treat the
// stroke as a pull. Mirrors TerminalScreen's 1.5x so both gestures
// share a consistent "vertical must dominate" feel.
const DOMINANT_RATIO = 1.5;

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
): {
  pull: number;
  refreshing: boolean;
  handlers: PullHandlers;
  /// Imperative API for callers that want to attach the indicator to an
  /// arbitrary element. The hook reads `pull` every animation frame (via
  /// a `requestAnimationFrame` loop that's only active while `pull > 0`)
  /// and mirrors it into `--pull-translate` on the returned element. The
  /// element's styles.css rule is the only place that consumes the
  /// custom property.
  bindIndicator: (el: HTMLElement | null) => void;
} {
  const [pull, setPull] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  // Anchor at gesture start: both x and y so we can apply the horizontal
  // axis lock below.
  const startRef = useRef<{ x: number; y: number } | null>(null);
  // Latest pull distance, mirrored out of state: touchend must read the
  // distance the last touchmove actually reached, not whichever render
  // the handler closure was built from (state updates from move events
  // race the end event under batching).
  const pullRef = useRef(0);
  // Mirrors `refreshing` for the touch handlers — state updates are
  // async, and a second finger landing mid-refresh must not queue a
  // second refresh.
  const refreshingRef = useRef(false);
  const onRefreshRef = useRef(onRefresh);
  onRefreshRef.current = onRefresh;
  // Mounted ref guards the `setRefreshing(false)` call inside the
  // refresh-promise's `.finally` so a component that's already unmounted
  // (user tapped a node row mid-refresh) doesn't warn about state on an
  // unmounted component.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  // rAF loop for mirroring `pullRef` into the indicator's CSS variable.
  // Active only while `pull > 0 || refreshing`, so the steady-state idle
  // has zero rAF cost.
  const indicatorRef = useRef<HTMLElement | null>(null);
  const rafRef = useRef<number | null>(null);

  const mirrorPull = useCallback(() => {
    const el = indicatorRef.current;
    if (el) {
      const px = refreshingRef.current ? MAX_PULL_PX : pullRef.current;
      el.style.setProperty("--pull-translate", `${Math.round(px)}px`);
    }
    if (pullRef.current > 0 || refreshingRef.current) {
      rafRef.current = requestAnimationFrame(mirrorPull);
    } else {
      rafRef.current = null;
      if (el) el.style.setProperty("--pull-translate", "0px");
    }
  }, []);

  const endGesture = useCallback(() => {
    startRef.current = null;
    pullRef.current = 0;
    setPull(0);
    // Drive one final mirror tick so the indicator translates back to 0
    // (the rAF loop is already spinning if pull > 0; if pull is already
    // 0 we still want the CSS variable cleared).
    if (!rafRef.current) {
      rafRef.current = requestAnimationFrame(mirrorPull);
    }
  }, [mirrorPull]);

  const handlers: PullHandlers = {
    onTouchStart(e) {
      if (!enabled || refreshingRef.current) return;
      const t = e.touches[0];
      startRef.current = { x: t.clientX, y: t.clientY };
    },
    onTouchMove(e) {
      const start = startRef.current;
      if (start === null) return;
      const t = e.touches[0];
      const dy = t.clientY - start.y;
      const dx = t.clientX - start.x;
      // Horizontal-axis lock: if a horizontal swipe takes over (deck
      // carousel), drop the pull anchor and let the carousel handle it.
      if (Math.abs(dx) > dy * DOMINANT_RATIO) {
        startRef.current = { x: t.clientX, y: t.clientY };
        pullRef.current = 0;
        setPull(0);
        return;
      }
      if (dy <= 0 || e.currentTarget.scrollTop > 0) {
        // Dragged up (normal scrolling) or the list scrolled away from
        // the top mid-gesture — reset the anchor so a later downward
        // drag inside the same touch re-evaluates from its own start
        // point.
        startRef.current = { x: t.clientX, y: t.clientY };
        pullRef.current = 0;
        setPull(0);
        return;
      }
      const next = Math.min(dy * DAMPING, MAX_PULL_PX);
      pullRef.current = next;
      setPull(next);
      if (!rafRef.current) {
        rafRef.current = requestAnimationFrame(mirrorPull);
      }
    },
    onTouchEnd() {
      const pulled = pullRef.current;
      startRef.current = null;
      pullRef.current = 0;
      setPull(0);
      if (!rafRef.current) {
        rafRef.current = requestAnimationFrame(mirrorPull);
      }
      if (enabled && !refreshingRef.current && pulled >= THRESHOLD_PX) {
        refreshingRef.current = true;
        setRefreshing(true);
        void Promise.resolve(onRefreshRef.current()).finally(() => {
          if (!mountedRef.current) return;
          refreshingRef.current = false;
          setRefreshing(false);
        });
      }
    },
    onTouchCancel: endGesture,
  };

  const bindIndicator = useCallback((el: HTMLElement | null) => {
    indicatorRef.current = el;
    if (el) {
      el.style.setProperty("--pull-translate", "0px");
    }
  }, []);

  return { pull, refreshing, handlers, bindIndicator };
}