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
// Performance (#1377 review feedback, round 2): the previous build STILL
// called `setPull(dy)` on every raw touchmove (despite the comment that
// said it didn't) AND animated inline `height` on the indicator — both
// forced a full NodeList re-render at 60–120 Hz AND a main-thread layout
// reflow per pixel. The fixed path:
//
//   1. **No React state on the pull distance.** The handler writes the
//      pull distance directly into a CSS custom property (`--pull-y`) on
//      a child indicator element via ref, at requestAnimationFrame
//      cadence. React state only holds `isPastThreshold` — a boolean
//      that flips once per threshold crossing, used solely for the
//      "Pull to refresh" / "Release to refresh" label.
//
//   2. **GPU-accelerated translation.** The indicator uses
//      `transform: translateY(var(--pull-y))` with `will-change: transform`,
//      not animated height. The element is `position: absolute` over the
//      list so the list's height doesn't reflow during the drag — the
//      indicator floats over the top of the list, anchored to the top
//      edge of the scroller.
//
// Unmount safety (#1377 review feedback): the refresh callback's
// `finally` previously called `setRefreshing(false)` against an unmounted
// component if the user navigated away mid-refresh. `mountedRef` guards
// that.

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
  /// `isPastThreshold` flips true while the user's pull has crossed the
  /// 64px release threshold — used ONLY for the "Pull" / "Release" label
  /// (a single render per crossing, not per pixel). The visual pull
  /// distance is NOT a render signal.
  isPastThreshold: boolean;
  refreshing: boolean;
  handlers: PullHandlers;
  /// Imperative API for the indicator element. The hook reads `pullRef`
  /// every animation frame (while `pullRef > 0 || refreshingRef`) and
  /// mirrors it into the `--pull-y` custom property on the returned
  /// element. The element's styles.css rule is the only place that
  /// consumes the custom property — the indicator does NOT live in the
  /// document flow (it's `position: absolute`), so the list below it
  /// doesn't reflow during the drag.
  bindIndicator: (el: HTMLElement | null) => void;
} {
  const [isPastThreshold, setIsPastThreshold] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  // Anchor at gesture start: both x and y so we can apply the horizontal
  // axis lock below.
  const startRef = useRef<{ x: number; y: number } | null>(null);
  // Latest pull distance, mirrored out of state: touchend must read the
  // distance the last touchmove actually reached, not whichever render
  // the handler closure was built from (state updates from move events
  // race the end event under batching). Also the source-of-truth for
  // the rAF mirror into --pull-y.
  const pullRef = useRef(0);
  // `isPastThresholdRef` mirrors the boolean React state so the touchmove
  // handler can detect threshold crossings without reading stale state
  // (React state updates are async).
  const isPastThresholdRef = useRef(false);
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
  // rAF loop for mirroring `pullRef` into the indicator's `--pull-y` CSS
  // variable. Active only while `pull > 0 || refreshing`, so the
  // steady-state idle has zero rAF cost.
  const indicatorRef = useRef<HTMLElement | null>(null);
  const rafRef = useRef<number | null>(null);

  const mirrorPull = useCallback(() => {
    const el = indicatorRef.current;
    if (el) {
      const px = refreshingRef.current ? MAX_PULL_PX : pullRef.current;
      el.style.setProperty("--pull-y", `${Math.round(px)}px`);
    }
    if (pullRef.current > 0 || refreshingRef.current) {
      rafRef.current = requestAnimationFrame(mirrorPull);
    } else {
      rafRef.current = null;
      if (el) el.style.setProperty("--pull-y", "0px");
    }
  }, []);

  const endGesture = useCallback(() => {
    startRef.current = null;
    pullRef.current = 0;
    isPastThresholdRef.current = false;
    setIsPastThreshold(false);
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
      // The check is on |dx| > |dy| * RATIO — NO direction qualifier on
      // dy, because a horizontal stroke with a 1-2px downward drift is
      // still horizontal (review feedback #3). Dropping the anchor also
      // resets the indicator visual so the user doesn't see a residual
      // downward translation.
      if (Math.abs(dx) > Math.abs(dy) * DOMINANT_RATIO) {
        startRef.current = { x: t.clientX, y: t.clientY };
        pullRef.current = 0;
        isPastThresholdRef.current = false;
        setIsPastThreshold(false);
        if (!rafRef.current) {
          rafRef.current = requestAnimationFrame(mirrorPull);
        }
        return;
      }
      if (dy <= 0 || e.currentTarget.scrollTop > 0) {
        // Dragged up (normal scrolling) or the list scrolled away from
        // the top mid-gesture — reset the anchor so a later downward
        // drag inside the same touch re-evaluates from its own start
        // point.
        startRef.current = { x: t.clientX, y: t.clientY };
        pullRef.current = 0;
        isPastThresholdRef.current = false;
        setIsPastThreshold(false);
        if (!rafRef.current) {
          rafRef.current = requestAnimationFrame(mirrorPull);
        }
        return;
      }
      const next = Math.min(dy * DAMPING, MAX_PULL_PX);
      pullRef.current = next;
      // Threshold-crossing detection: ONLY flip the React state when we
      // actually cross 64px (per release-affordance label), not every
      // pixel. The `prev !== next` check skips the setState call entirely
      // for the 63 frames of in-flight drag between crossings.
      const pastThreshold = next >= THRESHOLD_PX;
      if (pastThreshold !== isPastThresholdRef.current) {
        isPastThresholdRef.current = pastThreshold;
        setIsPastThreshold(pastThreshold);
      }
      if (!rafRef.current) {
        rafRef.current = requestAnimationFrame(mirrorPull);
      }
    },
    onTouchEnd() {
      const pulled = pullRef.current;
      startRef.current = null;
      pullRef.current = 0;
      isPastThresholdRef.current = false;
      setIsPastThreshold(false);
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
      el.style.setProperty("--pull-y", "0px");
    }
  }, []);

  return { isPastThreshold, refreshing, handlers, bindIndicator };
}