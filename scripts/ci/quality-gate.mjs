#!/usr/bin/env node
// quality-gate.mjs — the merge-gate decision behind the required
// `Verification / Quality (Linux)` status check, extracted from the workflow so
// the rules are testable rather than buried in a shell step.
//
// Why this exists: `Quality (Linux)` runs no check of its own. It is an
// aggregate over `Detect changes`, `Quality gates (Linux)` and the three
// `Quality vitest (<leg>)` legs, and a required status check that GitHub
// reports as *skipped* counts as satisfied. So the aggregate must never skip
// itself — a skip is indistinguishable from a pass to branch protection, and
// two separate regressions came out of trying to use one (issue #2046 review):
//
//   * Skipping on `frontend == 'false'` bricked every Rust-only pull request.
//     `Rust tests + TS bindings` requires `Quality (Linux)` == success, so a
//     skipped aggregate turned that required check red with the message
//     "the Rust suite is not fully green" for a suite that had never run.
//   * Skipping on `frontend == 'false'` also meant a docs-only pull request
//     could merge with failing static gates: the aggregate skipped, the
//     non-required `Quality gates (Linux)` failed, and all three required
//     checks reported skipped.
//
// The rule is therefore: always run, and decide here, where the reason for an
// absent branch is visible. A branch is allowed to be absent only when the
// change-scope classification says it has nothing to prove.
//
// Usage: node scripts/ci/quality-gate.mjs   (reads the GitHub Actions env)

/**
 * @param {{ changesResult: string, frontendChanged: string, gatesResult: string, testsResult: string }} input
 * @returns {{ ok: boolean, error: string | null }}
 */
export function evaluateQualityGate({ changesResult = '', frontendChanged = '', gatesResult = '', testsResult = '' }) {
  // No green frontend gate without a successful classification behind it. A
  // failed `Detect changes` leaves every output empty, which would otherwise
  // read as "no frontend changed" and wave the vitest branch through.
  if (changesResult !== 'success' || frontendChanged === '') {
    return {
      ok: false,
      error: `change-scope result=${changesResult} frontend='${frontendChanged}' - refusing to certify the frontend suite without a successful classification.`,
    };
  }

  // The static gates run on every pull request, whatever the classification, so
  // this is where a lint, docs, README-drift, build or bundle-budget failure
  // stops a pull request — including a docs-only one, which has no other
  // required check left to catch it.
  if (gatesResult !== 'success') {
    return { ok: false, error: `quality-gates=${gatesResult} - the static gates are not green.` };
  }

  // The vitest legs skip themselves when the classification says the frontend
  // did not change, and that is the only acceptable absence: nothing for a
  // browser to prove, and `Quality (Linux)` reports success exactly as a skip
  // would. A failed or cancelled leg is never acceptable.
  if (testsResult === 'success') return { ok: true, error: null };
  if (testsResult === 'skipped' && frontendChanged === 'false') return { ok: true, error: null };
  return {
    ok: false,
    error: `quality-tests=${testsResult} with frontend='${frontendChanged}' - the vitest legs must run and pass whenever the diff touched the frontend.`,
  };
}

export function describeDecision(input) {
  const { ok, error } = evaluateQualityGate(input);
  return ok
    ? `frontend=${input.frontendChanged} quality-gates=${input.gatesResult} quality-tests=${input.testsResult}`
    : error;
}

function main(env) {
  const input = {
    changesResult: env.CHANGES_RESULT ?? '',
    frontendChanged: env.FRONTEND_CHANGED ?? '',
    gatesResult: env.GATES_RESULT ?? '',
    testsResult: env.TESTS_RESULT ?? '',
  };
  const { ok, error } = evaluateQualityGate(input);
  if (!ok) {
    process.stdout.write(`::error::${error}\n`);
    return 1;
  }
  process.stdout.write(`${describeDecision(input)}\n`);
  return 0;
}

// Only act when run as a program, so importing this for tests is side-effect
// free. Windows needs the `.cmd` shim for `process.argv[1]`.
if (process.argv[1] && /quality-gate\.mjs$/.test(process.argv[1])) {
  process.exitCode = main(process.env);
}
