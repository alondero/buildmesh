// Issue #1542 — unit test for the ESLint fixtures gate.
//
// `scripts/check-eslint-fixtures.mjs` exposes pure `checkEslintFixtures` and
// `checkEslintConfigDrift` functions. The CLI wrapper (the script's bottom
// block) just turns the result into an exit code; this test exercises the
// functions directly so it runs in-process without spawning a child.
//
// What we assert:
//   1. The two REAL fixtures (conditional-hook.tsx, missing-deps.tsx)
//      each trip their expected rule — proving the React Hooks gate is
//      live and would catch the regression class #1242 was.
//   2. The prod `eslint.config.js` declares both rules at severity
//      >= the minimum — proving the gate is actually enabled (not just
//      that the plugin can fire). This is the drift hole that #2 in
//      PR #1542 review caught.
//   3. A synthetic "clean" fixture (a valid component with no rule
//      violations) reports zero failures — proving the gate isn't just
//      noisy-by-default.
//   4. A synthetic fixture that trips `rules-of-hooks` is REJECTED when
//      checked against the `exhaustive-deps` expectation — proving the
//      check is rule-specific, not "anything fires".

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  checkEslintFixtures,
  checkEslintConfigDrift,
  EXPECTED_VIOLATIONS,
} from '../../scripts/check-eslint-fixtures.mjs';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const worktreeRoot = join(__dirname, '..', '..');

// Synthetic fixtures — these are NOT the production fixtures; they live
// in a temp dir so a contributor editing the real fixtures doesn't
// accidentally break this test. Each fixture is a tiny TSX file
// engineered to either pass or fail in a specific way.

const CLEAN_FIXTURE = `
import { useEffect, useState } from 'react';

export function CleanComponent() {
  const [count, setCount] = useState(0);

  // Fully deps-listed — no exhaustive-deps violation.
  useEffect(() => {
    setCount((c) => c + 1);
  }, [setCount]);

  return <div>{count}</div>;
}
`.trim();

// A fixture that trips `react-hooks/rules-of-hooks` (hook called after an
// early return). Used to prove the verifier is rule-specific: when this
// content is fed in for `missing-deps.tsx` (whose expected rule is
// `exhaustive-deps`), the verifier must report that the expected rule is
// missing AND name the wrong rule that DID fire.
const RULES_OF_HOOKS_FIXTURE = `
import { useState } from 'react';

type Props = { ready: boolean };

export function WrongRuleFixture({ ready }: Props) {
  if (!ready) {
    return null;
  }
  const [count, setCount] = useState(0);
  return <button onClick={() => setCount(0)}>{count}</button>;
}
`.trim();

test('real fixtures each trip their expected React Hooks rule', async () => {
  // Run against the REAL fixtures (the production files in
  // tests/lint-fixtures/). This is the integration-level proof that
  // the gate would catch a regression introduced into real source.
  const { failures, driftFailures } = await checkEslintFixtures();
  assert.equal(
    driftFailures.length,
    0,
    `prod eslint.config.js has rule-disablement drift: ${JSON.stringify(driftFailures)}`,
  );
  assert.equal(
    failures.length,
    0,
    `expected zero failures (each fixture should trip its rule), got: ${JSON.stringify(failures)}`,
  );
  // EXPECTED_VIOLATIONS lists the canonical rule expectations.
  assert.equal(EXPECTED_VIOLATIONS.length, 2);
  assert.deepEqual(
    EXPECTED_VIOLATIONS.map(({ ruleId }) => ruleId).sort(),
    ['react-hooks/exhaustive-deps', 'react-hooks/rules-of-hooks'].sort(),
  );
});

test('prod config keeps both React Hooks rules enabled above the minimum severity', async () => {
  // The drift check is the second half of the verifier: even if the
  // fixtures trip, the prod gate must actually be enabled.
  const driftFailures = await checkEslintConfigDrift();
  assert.equal(
    driftFailures.length,
    0,
    `prod config drift detected: ${JSON.stringify(driftFailures)}`,
  );
});

test('synthetic clean fixture reports zero failures', async () => {
  // Build the temp dir INSIDE the worktree (not in `os.tmpdir()`) so
  // ESLint's "outside of base path" guard considers it in-scope.
  const dir = mkdtempSync(join(worktreeRoot, 'eslint-fixtures-test-'));
  try {
    for (const { file } of EXPECTED_VIOLATIONS) {
      writeFileSync(join(dir, file), CLEAN_FIXTURE, 'utf8');
    }
    // `expect: 'clean'` flips the function into "every message is
    // unexpected" mode — production behavior asserts violations ARE
    // present; this test asserts they ARE NOT.
    const { failures } = await checkEslintFixtures({ fixturesDir: dir, expect: 'clean' });
    assert.equal(failures.length, 0, JSON.stringify(failures));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('synthetic wrong-rule fixture is rejected by the rule-specific check', async () => {
  // Specificity test: a fixture that trips `rules-of-hooks` should
  // PASS the `conditional-hook.tsx` check (which expects that rule)
  // and FAIL the `missing-deps.tsx` check (which expects
  // `exhaustive-deps`). The failure message MUST name the wrong rule
  // — proving the verifier is checking for the expected rule, not
  // "any violation".
  const dir = mkdtempSync(join(worktreeRoot, 'eslint-fixtures-test-'));
  try {
    for (const { file } of EXPECTED_VIOLATIONS) {
      writeFileSync(join(dir, file), RULES_OF_HOOKS_FIXTURE, 'utf8');
    }
    const { failures } = await checkEslintFixtures({ fixturesDir: dir });

    // Both fixtures are checked. `conditional-hook.tsx` expects
    // `rules-of-hooks`, which the fixture DOES trip → no failure.
    // `missing-deps.tsx` expects `exhaustive-deps`, which the fixture
    // does NOT trip → one failure.
    assert.equal(failures.length, 1, `expected 1 failure (missing-deps), got ${failures.length}: ${JSON.stringify(failures)}`);

    const missingDepsFailure = failures.find((f) => f.fixture === 'missing-deps.tsx');
    assert.ok(missingDepsFailure, `expected failure on missing-deps.tsx, got: ${JSON.stringify(failures.map((f) => f.fixture))}`);
    assert.equal(missingDepsFailure.ruleId, 'react-hooks/exhaustive-deps', 'failure should name the EXPECTED rule, not the one that actually fired');

    // The message must report the rule that DID fire so a contributor
    // sees the mismatch — this is the specificity signal.
    assert.match(
      missingDepsFailure.message,
      /react-hooks\/rules-of-hooks/,
      `failure message should name the wrong rule that fired, got: ${missingDepsFailure.message}`,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
