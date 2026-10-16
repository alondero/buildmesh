// Single source of truth for the `ui-shot` mock-render time budgets.
//
// These live in their own module because `scripts/ui-shot.mjs` is a
// top-level CLI script: importing it to read a constant would launch Chromium.
// The script and its server helper import from here, and
// `tests/integration/ui-shot.test.ts` reads the same values, so the deadline
// the wrapper budgets can never silently drift below what the child really
// spends (issue #2049 class: a wrapper tighter than its child reports a bare
// transport error that reads like "start the dev server" when `--serve` already
// started one).
//
// The wrapper's deadline is derived from these budgets below
// (`UI_SHOT_WATCHDOG_DEADLINE_MS`), not hand-summed: see the note there for
// why the 12-term sum this replaced was itself the maintenance hazard
// (issue #2168).

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
 * How long the run waits for a browser it launched to close.
 *
 * `browser.close()` accepts no timeout argument, so this is enforced by racing
 * the close rather than by handing Playwright a number — the same reason the
 * page-setup calls are raced (#2063).
 *
 * What it bounds is the *wait*, not the run's verdict. The close resolves only
 * once Chromium has exited and Playwright has removed the profile directory it
 * created, and on a Windows host running a full suite that takes 2-3s quietly
 * and 24-50s under load, because Playwright force-kills the browser 30s into the
 * close and the profile removal follows it (issue #2245). Racing that at 15s and
 * failing the run reported host contention as a product failure, after the
 * screenshot had already been written. An overrun is now reported on stderr and
 * handed off; see `BROWSER_CLOSE_GRACE_MS` and `scripts/ui-shot-close.mjs`.
 *
 * Not a budget the run is expected to consume: on a quiet host the close
 * resolves in a fraction of it.
 */
export const BROWSER_CLOSE_TIMEOUT_MS = 15000;

/**
 * How much longer to stay alive after `BROWSER_CLOSE_TIMEOUT_MS` expires, for a
 * close that is still running.
 *
 * The two sum to 30s, which is the point at which Playwright gives up waiting
 * for its own browser process and force-kills the tree
 * (`DEFAULT_PLAYWRIGHT_TIMEOUT` in playwright-core). Staying alive that long
 * lets Playwright finish reaping that process *and* remove its profile
 * directory in-process, which is the whole reason for the window: a close
 * abandoned at the first budget leaves the cleanup to the exit handler, whose
 * synchronous removal runs against a browser that has not finished dying and
 * fails with EPERM. In the load reproduction for #2245 each such occurrence
 * left a `playwright_chromiumdev_profile-*` directory behind, and the four runs
 * re-run after this change left none.
 *
 * A run whose close settles pays no wall-clock time for it: the grace timer is
 * unref'd, so it never holds the process open, and the worst case for a run that
 * does overrun is the 30s Playwright would have spent inside the close anyway.
 */
export const BROWSER_CLOSE_GRACE_MS = 15000;

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
 * The slowest single phase a `--mock` run can be in.
 *
 * Every phase above is a budget, and each is enforced at its own call site, so
 * the child fails with a *named* phase long before any wrapper needs to act.
 * What a wrapper still wants is one deadline above every phase, so a child that
 * somehow exceeds all of them is caught rather than running forever.
 *
 * The browser close is priced as its wait plus its grace window, because that
 * is the span one call site can be in; a single term is not the whole phase.
 */
export const UI_SHOT_SLOWEST_PHASE_MS = Math.max(
  DEV_SERVER_STARTUP_MS,
  BROWSER_LAUNCH_TIMEOUT_MS,
  BROWSER_SETUP_TIMEOUT_MS,
  FIXTURES_LOAD_TIMEOUT_MS,
  NAVIGATION_TIMEOUT_MS,
  MOUNT_TIMEOUT_MS,
  STEP_SCRIPT_TIMEOUT_MS,
  STEP_MODULE_LOAD_TIMEOUT_MS,
  ELEMENT_VISIBLE_TIMEOUT_MS,
  SCREENSHOT_TIMEOUT_MS,
  BROWSER_CLOSE_TIMEOUT_MS + BROWSER_CLOSE_GRACE_MS,
  DEV_SERVER_STOP_MS,
);

/**
 * How many times the slowest phase a supervising wrapper must outlast.
 *
 * This replaces a 12-term hand-priced sum of every phase (#2168). The sum was a
 * second copy of the phase list, and the copy was the invariant: nothing
 * detected a phase present in neither copy, and #2140's eight review rounds were
 * spent keeping them in step. A multiple of the slowest phase is derived from
 * the budgets themselves, so raising any of them widens this with it and no
 * list has to be maintained in two places.
 *
 * Known limit: a *newly added* phase whose budget exceeds every one listed
 * above has to be added to `UI_SHOT_SLOWEST_PHASE_MS` too. That is one list of
 * constants, not the old list of names-with-call-counts plus its test mirror,
 * and getting it wrong errs safe — the deadline stays generous while the new
 * phase's own timeout still fires first and reports by name.
 *
 * Four is chosen because a run that is slow in several phases at once is the
 * case this covers, and 4x the slowest (240s -> 960s) still clears the old
 * 612s sum. It is a backstop, not a budget: the child's own per-phase timeouts
 * fire first and report the phase by name, and the wrapper's job on a kill is
 * only to name that phase too (`scripts/phase-watchdog.mjs`).
 */
export const UI_SHOT_WATCHDOG_MULTIPLE = 4;

/**
 * The deadline a supervising wrapper gives a `--mock` run: one number, derived
 * from the per-phase budgets rather than summed by hand.
 */
export const UI_SHOT_WATCHDOG_DEADLINE_MS =
  UI_SHOT_SLOWEST_PHASE_MS * UI_SHOT_WATCHDOG_MULTIPLE;
