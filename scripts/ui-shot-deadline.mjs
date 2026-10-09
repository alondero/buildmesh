// A deadline primitive for phases `ui-shot` cannot hand a timeout to.
//
// Playwright accepts `timeout` on most calls but not all: `browser.close()` takes
// only a `reason`, so the browser-close phase can only be bounded by racing it.
// This is that race, and it lives in its own module because it is not
// step-specific — `runSteps` in `ui-shot-steps.mjs` uses it for the same reason.
//
// Bounding a phase and pricing it are one concern: the constant that prices the
// phase in `ui-shot-budgets.mjs` is the constant passed here, so a phase the
// sum budgets is a phase something enforces (issue #2063).

/**
 * Reject with a named phase diagnostic if `work` has not settled within
 * `timeoutMs`.
 *
 * A falsy budget means "this phase is not budgeted": callers with no
 * supervising wrapper (real-app `--url` and CDP modes) pass null rather than
 * inheriting a limit nothing accounts for.
 *
 * `work` stays subscribed through `Promise.race`, so a rejection arriving after
 * the deadline cannot become unhandled.
 */
export async function withDeadline(work, timeoutMs, phase) {
  if (!timeoutMs) return work;
  let timer;
  return Promise.race([
    work,
    new Promise((_, rejectPhase) => {
      timer = setTimeout(() => rejectPhase(new Error(`${phase} did not finish within ${timeoutMs}ms.`)), timeoutMs);
    }),
  ]).finally(() => clearTimeout(timer));
}
