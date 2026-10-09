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

/** The gap between readiness probes, likewise clipped to what remains. */
export const DEV_READY_POLL_MS = 250;

/**
 * The TCP connect used to decide whether the URL is already served.
 *
 * Deliberately short: a refusal means nothing is listening, and that question
 * should not wait on a slow HTTP response, which says nothing about whether a
 * server owns the port.
 */
export const DEV_LISTEN_PROBE_MS = 1000;

/**
 * Waiting for a dev server this module started to exit, after a screenshot or a
 * failure. Charged because it is real teardown time a supervising wrapper has to
 * outlast.
 */
export const DEV_SERVER_STOP_MS = 2000;

/**
 * Launching Chromium itself, before any navigation.
 *
 * The fallback in `launchChromium` retries the launch against a system Chromium
 * when the bundled one is missing, so the phase can be charged twice; the sum
 * prices both attempts rather than one.
 */
export const BROWSER_LAUNCH_TIMEOUT_MS = 30000;

/**
 * Creating a page and installing the mock IPC script, after the launch.
 *
 * Neither `browser.newPage` nor `page.addInitScript` accepts a `timeout`, so
 * each is raced against this. The sum charges it twice because there are two
 * such calls — creating the page, then installing the init script — not because
 * of a launch fallback: setup itself runs once, after whichever launch
 * succeeded.
 */
export const BROWSER_SETUP_TIMEOUT_MS = 15000;

/**
 * Reading the mock fixtures file, and importing a `--steps` module.
 *
 * Both are awaited before the work they feed is reached — the fixtures before
 * `addInitScript`, the steps module before the step script runs — so neither is
 * covered by the phase it precedes. Each carries its own budget, and each is
 * charged once in the sum (#2063).
 */
export const FIXTURES_LOAD_TIMEOUT_MS = 15000;
export const STEP_MODULE_LOAD_TIMEOUT_MS = 15000;

/** First `page.goto`. A cold Vite server transforms the app on this request. */
export const NAVIGATION_TIMEOUT_MS = 120000;

/** Waiting for the app to mount something under `#root`, after navigation. */
export const MOUNT_TIMEOUT_MS = 15000;

/** Waiting for a step's selector to become visible. */
export const ELEMENT_VISIBLE_TIMEOUT_MS = 10000;

/**
 * Taking the screenshot, after the selector wait.
 *
 * Priced because `page.screenshot` and `locator.screenshot` otherwise fall back
 * to Playwright's 30s default, and an unpriced pair of default-timeout calls
 * can exceed a supervising wrapper's whole slack (#2063).
 */
export const SCREENSHOT_TIMEOUT_MS = 30000;

/**
 * Shutting the browser down in the CLI's `finally`.
 *
 * Priced for the same reason, but it is enforced by racing the close rather than
 * by a timeout argument: `browser.close()` accepts no timeout, so a hung close
 * would otherwise be the one phase nothing bounds.
 */
export const BROWSER_CLOSE_TIMEOUT_MS = 15000;

/**
 * The whole step script, spent after the app has mounted.
 *
 * Bounding this phase is the point of `STEP_SCRIPT_TIMEOUT_MS`: without it the
 * phase is unbounded, because step files call `click`, `waitFor` and
 * `waitForFunction` with no explicit timeout and each inherits Playwright's 30s
 * default. A supervising wrapper then kills the child mid-flight and reports a
 * bare transport error instead of the child's real diagnostic (issue #2063,
 * the #2049 failure class). `scripts/ui-shot-steps.mjs` applies this as the
 * deadline.
 *
 * The cap is a total for the whole script, not a per-call budget. It is sized
 * above what the scripts actually spend — the largest step file
 * (`ui-shot-node-status`) performs ~56 awaited actions — so that a slow but
 * successful run still passes. It was previously 120s, which *reduced* headroom:
 * uncapped scripts had the whole 235s wrapper, so a 150s run passed before this
 * change and failed after it. 240s keeps the cap well clear of a loaded run while
 * still bounding a hang; raising it widens the wrapper automatically.
 *
 * It is not a measured guarantee, and under CPU contention a script that is
 * merely slow can still be cut off. Raise it here rather than duplicating the
 * number.
 */
export const STEP_SCRIPT_TIMEOUT_MS = 240000;

/**
 * The most a single `ui-shot --serve` run can spend, phase by phase:
 * dev-server startup, browser launch, navigation, mount, step script, selector
 * wait, screenshot, browser close. A supervising wrapper must exceed this.
 *
 * Every phase the child can spend is priced here, including the ones that would
 * otherwise fall back to a Playwright default, because an unpriced phase is
 * exactly how this sum came to sit below the real worst case (#2063).
 *
 * `tests/integration/ui-shot.test.ts` audits `scripts/ui-shot.mjs` against this
 * sum, so a phase that stops passing its budget is caught rather than quietly
 * reverting to Playwright's own default.
 */
export const UI_SHOT_STEP_BUDGETS_MS =
  DEV_SERVER_STARTUP_MS +
  BROWSER_LAUNCH_TIMEOUT_MS * 2 +
  BROWSER_SETUP_TIMEOUT_MS * 2 +
  FIXTURES_LOAD_TIMEOUT_MS +
  NAVIGATION_TIMEOUT_MS +
  MOUNT_TIMEOUT_MS +
  STEP_SCRIPT_TIMEOUT_MS +
  STEP_MODULE_LOAD_TIMEOUT_MS +
  ELEMENT_VISIBLE_TIMEOUT_MS +
  SCREENSHOT_TIMEOUT_MS +
  BROWSER_CLOSE_TIMEOUT_MS +
  DEV_SERVER_STOP_MS;
