// Teardown for a browser this run launched.
//
// `browser.close()` takes no timeout argument, so the phase it belongs to can
// only be bounded by racing it (`scripts/ui-shot-deadline.mjs`). Racing it is
// easy; what to do with the losing race is the point of this module.
//
// The close resolves only once Chromium has *exited* and Playwright has removed
// the profile directory it created — neither of which the caller owns. Measured
// on Windows (issue #2245): 2-3s on a quiet host, and 24-50s while a full suite
// saturates the machine, because Playwright force-kills the browser it launched
// 30s into the close and the profile removal happens after that. No budget on
// this side of that point makes the phase deterministic, and none of the
// orderings measured under that load (dev server stopped first, page closed
// first, both) brought the close under the budget either.
//
// So the budget bounds how long the run *waits*, not whether the run passes.
// A slow close tells either caller nothing it still needs: the normal teardown
// has already written the screenshot the run exists to produce, and the
// setup-failure path has already failed for the reason it failed. An overrun is
// reported and handed off: Playwright reaps its own process when this process
// exits, and holding the process open through the grace window is what lets its
// profile cleanup finish instead of racing a `taskkill` (each of the four load
// runs before this change ended that way, each leaving a
// `playwright_chromiumdev_profile-*` directory behind; the four after it left
// none). A close that *errors* is still the run's failure — a slow teardown is
// host contention, a broken one is not.

/** The close finished inside its budget. */
const CLOSED = 'closed';
/** The wait expired before the close finished. */
const SLOW = 'slow';

/**
 * Resolve once `closing` settles, keeping a later rejection handled.
 *
 * `Promise.race` subscribes to this promise, so a close that fails after the
 * wait expired stays an observed rejection rather than an unhandled one.
 */
function onSettled(closing) {
  return closing.then(() => CLOSED);
}

/** Resolve `SLOW` after `ms`, without rejecting anything. */
function afterMs(ms) {
  return new Promise((resolvePromise) => {
    const timer = setTimeout(() => resolvePromise(SLOW), ms);
    // Nothing else in an abandoned close holds the event loop open, so an
    // unref'd timer keeps a run that has already finished from waiting on it.
    timer.unref?.();
  });
}

/** Never settle: the race for an unbudgeted close, which waits on the close. */
function neverSettles() {
  return new Promise(() => {});
}

/**
 * Close a browser this run launched, bounded, and report whether the close ran
 * past its budget.
 *
 * A falsy `timeoutMs` means "this teardown is not budgeted": the real-app modes
 * (`--url`, CDP-attach) have no supervising wrapper and pass null, so they keep
 * the unbounded close they used before this module existed.
 *
 * `graceMs` is how much longer to stay alive after the budget expires for a
 * close that is still running, so Playwright's own cleanup can finish inside
 * this process instead of being cut short by its exit handler.
 *
 * Rejects only when `browser.close()` itself does — a rejected close means the
 * browser could not be closed properly, which the caller still reports as the
 * run's failure.
 */
export async function closeBrowser(browser, {
  timeoutMs,
  graceMs = 0,
  phase = 'Closing the browser',
  warn = message => console.error(message),
} = {}) {
  const startedAt = Date.now();
  // One close, one promise: the grace window re-awaits the promise this call
  // created rather than calling `browser.close()` again, so the hand-off
  // continues the close the run already asked for.
  const closing = browser.close();

  const waited = await Promise.race([onSettled(closing), timeoutMs ? afterMs(timeoutMs) : neverSettles()]);
  if (waited === CLOSED) return { overran: false, reaped: true, waitedMs: Date.now() - startedAt };

  // The close is still running. Stay alive briefly so Playwright can reap its
  // own process and remove its profile directory in-process: an abandoned close
  // loses that cleanup to the exit handler, whose synchronous removal runs
  // against a browser that has not finished dying and fails with EPERM.
  if (graceMs) {
    const reaped = await Promise.race([onSettled(closing), afterMs(graceMs)]);
    if (reaped === CLOSED) {
      warn(`[ui-shot] ${phase} did not finish within ${timeoutMs}ms, but finished during the ${graceMs}ms teardown grace window.`);
      return { overran: true, reaped: true, waitedMs: Date.now() - startedAt };
    }
  }

  warn(
    `[ui-shot] ${phase} did not finish within ${timeoutMs + graceMs}ms; the run is not failed over it. ` +
    'Playwright reaps its own browser process when this process exits.'
  );
  return { overran: true, reaped: false, waitedMs: Date.now() - startedAt };
}
