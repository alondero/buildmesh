// Single source of truth for the `ui-shot` mock-render time budgets.
//
// These live in their own module because `scripts/ui-shot.mjs` is a
// top-level CLI script: importing it to read a constant would launch Chromium.
// The script and its server helper import from here, and
// `tests/integration/ui-shot.test.ts` reads the same values, so the wrapper
// deadline it budgets can never silently drift below what the child really
// spends (issue #2049 class: a wrapper tighter than its child reports a bare
// transport error that reads like "start the dev server" when `--serve` already
// started one).

/** Dev-server startup, charged before the browser navigates. */
export const DEV_SERVER_STARTUP_MS = 60000;

/** First `page.goto`. A cold Vite server transforms the app on this request. */
export const NAVIGATION_TIMEOUT_MS = 120000;

/** Waiting for the app to mount something under `#root`, after navigation. */
export const MOUNT_TIMEOUT_MS = 15000;

/** Waiting for a step's selector to become visible. */
export const ELEMENT_VISIBLE_TIMEOUT_MS = 10000;

/**
 * The most a single `ui-shot --serve` run can spend before it is doing
 * unbounded step work: startup, navigation, mount, selector. A supervising
 * wrapper must exceed this.
 */
export const UI_SHOT_STEP_BUDGETS_MS =
  DEV_SERVER_STARTUP_MS +
  NAVIGATION_TIMEOUT_MS +
  MOUNT_TIMEOUT_MS +
  ELEMENT_VISIBLE_TIMEOUT_MS;