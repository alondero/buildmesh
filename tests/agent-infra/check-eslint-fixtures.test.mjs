// Issue #1542 — unit test for the ESLint fixtures gate.
//
// `scripts/check-eslint-fixtures.mjs` exposes a pure `checkEslintFixtures`
// function. The CLI wrapper (the script's bottom block) just turns the
// result into an exit code; this test exercises the function directly so
// it runs in-process without spawning a child.
//
// What we assert:
//   1. The two REAL fixtures (conditional-hook.tsx, missing-deps.tsx)
//      each trip their expected rule — proving the React Hooks gate is
//      live and would catch the regression class #1242 was.
//   2. A synthetic "clean" fixture (a valid component with no rule
//      violations) reports zero failures — proving the gate isn't just
//      noisy-by-default.
//   3. A synthetic "wrong rule" fixture (a `no-unused-vars` violation
//      instead of a hooks violation) does NOT trip the expected hooks
//      rule — proving the check is rule-specific, not "anything fires".

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  checkEslintFixtures,
  EXPECTED_VIOLATIONS,
} from '../../scripts/check-eslint-fixtures.mjs';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const worktreeRoot = join(__dirname, '..', '..');

// Synthetic fixtures — these are NOT the production fixtures; they live
// in a temp dir so a contributor editing the real fixtures doesn't
// accidentally break this test. Each fixture is a tiny TSX file
// engineered to either pass or fail in a specific way.
function writeFixtureSync(dir, name, body) {
  const p = join(dir, name);
  writeFileSync(p, body, 'utf8');
  return p;
}

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

const WRONG_RULE_FIXTURE = `
// Intentionally violates 'no-unused-vars' instead of a hooks rule.
// The check must NOT report this as a hooks violation.
export function UnusedVarComponent() {
  const unused = 42;
  return <div>{unused}</div>;
}
`.trim();

test('real fixtures each trip their expected React Hooks rule', async () => {
  // Run against the REAL fixtures (the production files in
  // tests/lint-fixtures/). This is the integration-level proof that
  // the gate would catch a regression introduced into real source.
  const { failures } = await checkEslintFixtures();
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

test('synthetic wrong-rule fixture reports the expected rule as missing', async () => {
  // Same trick as the clean-fixture test — temp dir lives inside the
  // worktree so ESLint sees it as in-base-path.
  const dir = mkdtempSync(join(worktreeRoot, '.eslint-fixtures-test-'));
  try {
    for (const { file } of EXPECTED_VIOLATIONS) {
      writeFileSync(join(dir, file), WRONG_RULE_FIXTURE, 'utf8');
    }
    const { failures } = await checkEslintFixtures({ fixturesDir: dir });
    assert.equal(failures.length, 2, `expected 2 failures, got ${failures.length}: ${JSON.stringify(failures)}`);
    for (const f of failures) {
      assert.match(f.message, /Expected ESLint to report rule/, 'failure message should name the missing rule');
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
