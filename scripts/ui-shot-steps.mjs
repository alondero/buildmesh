// Runs a `ui-shot --steps` script under an explicit deadline.
//
// This lives in its own module because `scripts/ui-shot.mjs` is a top-level
// CLI script: importing it to reach this behaviour would launch Chromium.
//
// Bounding the phase is the point: step files call `click`, `waitFor` and
// `waitForFunction` with no explicit timeout, and each inherits Playwright's
// default, so an unbounded script can outlive the supervising wrapper, which
// then kills the child and reports a bare transport error instead of the real
// diagnostic (issue #2063, the #2049 failure class). The cap is a total for the
// whole script, and `STEP_SCRIPT_TIMEOUT_MS` carries its rationale.

import { resolve } from 'path';
import { pathToFileURL } from 'url';
import { STEP_SCRIPT_TIMEOUT_MS, STEP_MODULE_LOAD_TIMEOUT_MS } from './ui-shot-budgets.mjs';
import { withDeadline } from './ui-shot-deadline.mjs';

/**
 * Import and run a steps file, rejecting with a named phase diagnostic when
 * either phase does not settle within its own budget.
 *
 * `timeoutMs` bounds running the script; `moduleLoadTimeoutMs` bounds importing
 * it. They are separate because they are separate phases priced separately, and
 * because only `--mock` runs are supervised: a real-app caller passes null for
 * both and keeps both unbounded.
 */
export async function runSteps(stepsFile, deps, { timeoutMs = STEP_SCRIPT_TIMEOUT_MS, moduleLoadTimeoutMs = null } = {}) {
  // A dynamic import of a caller-supplied absolute path, deliberately opaque to
  // bundler/test-runner transforms (`@vite-ignore`): the steps file lives outside
  // the module graph, so a transformed import specifier would fail to resolve it
  // instead of loading it. The steps file is caller-supplied test/verify data,
  // never a build-time dependency of this module.
  //
  // The import is bounded by `moduleLoadTimeoutMs` alone. Deriving it from
  // `timeoutMs` would hand the step budget's 120s to the load phase, and
  // defaulting it would cap the real-app modes that deliberately pass null.
  const mod = await withDeadline(
    import(/* @vite-ignore */ pathToFileURL(resolve(stepsFile)).href),
    moduleLoadTimeoutMs,
    `Loading the step script ${stepsFile}`,
  );
  if (typeof mod.default !== 'function') throw new Error(`${stepsFile} must default-export an async function`);

  return withDeadline(mod.default(deps), timeoutMs, `Steps in ${stepsFile} (step script phase)`);
}
