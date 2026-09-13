#!/usr/bin/env node
// Issue #1542 — fixture verifier for the ESLint + React Hooks gate.
//
// The `tests/lint-fixtures/` directory contains files that INTENTIONALLY
// violate the React Hooks rules. They're excluded from `npm run lint` so a
// clean checkout passes, but this verifier runs ESLint against them and
// asserts each violation is caught — proving the rules are live and will
// catch a real regression.
//
// Two fixtures:
//   - conditional-hook.tsx  → must trip `react-hooks/rules-of-hooks`
//   - missing-deps.tsx      → must trip `react-hooks/exhaustive-deps`
//
// Exposed as a pure function so the unit test
// (`tests/agent-infra/check-eslint-fixtures.test.mjs`) can exercise it
// in-process without spawning a child. The CLI wrapper at the bottom
// is what `npm run lint:fixtures` invokes.
//
// Usage:
//   node scripts/check-eslint-fixtures.mjs          # CI / local
//   npm run lint:fixtures                           # npm wrapper

import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ESLint } from 'eslint';
import tseslint from 'typescript-eslint';
import reactHooks from 'eslint-plugin-react-hooks';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
export const repoRoot = resolve(__dirname, '..');
export const fixturesDir = resolve(repoRoot, 'tests/lint-fixtures');

/** Fixture -> expected rule id. Each fixture's whole purpose is to trip
 *  this rule, so the verifier fails if the rule stops firing. */
export const EXPECTED_VIOLATIONS = [
  { file: 'conditional-hook.tsx', ruleId: 'react-hooks/rules-of-hooks' },
  { file: 'missing-deps.tsx', ruleId: 'react-hooks/exhaustive-deps' },
];

/** Run ESLint against the fixtures and return a list of failures.
 *
 *  Two modes:
 *   - `expect: 'violation'` (default — production behavior). Each
 *     fixture MUST trigger its `expected.ruleId`; missing it is a
 *     failure.
 *   - `expect: 'clean'` (test-only). Every fixture must produce zero
 *     messages from the in-scope rule set. Used by unit tests to
 *     assert a clean file doesn't false-trip the rule.
 *
 *  Returns `{ failures, results }` so the unit test can introspect
 *  raw ESLint output. The CLI wrapper at the bottom turns the result
 *  into an exit code.
 */
export async function checkEslintFixtures({ fixturesDir: dir = fixturesDir, eslint, expect = 'violation' } = {}) {
  const lint = eslint ?? new ESLint({
    overrideConfigFile: true,
    overrideConfig: [
      {
        files: ['**/*.{ts,tsx}'],
        languageOptions: {
          // The fixtures use TS `type` aliases; the default ESLint parser
          // bails on those. `typescript-eslint`'s parser handles both
          // `.ts` and `.tsx` so we get past syntax errors and into the
          // actual React Hooks rule evaluation.
          parser: tseslint.parser,
          parserOptions: {
            ecmaVersion: 2022,
            sourceType: 'module',
            ecmaFeatures: { jsx: true },
          },
        },
        plugins: { 'react-hooks': reactHooks },
        rules: {
          'react-hooks/rules-of-hooks': 'error',
          'react-hooks/exhaustive-deps': 'warn',
        },
      },
    ],
    cwd: repoRoot,
  });

  const fixtureFiles = EXPECTED_VIOLATIONS.map(({ file }) => resolve(dir, file));
  const results = await lint.lintFiles(fixtureFiles);

  const failures = [];
  for (let i = 0; i < EXPECTED_VIOLATIONS.length; i++) {
    const expected = EXPECTED_VIOLATIONS[i];
    const result = results[i];
    if (!result) {
      failures.push({
        fixture: expected.file,
        ruleId: expected.ruleId,
        message: `ESLint returned no result for ${expected.file}. Did the file move or get renamed?`,
      });
      continue;
    }

    if (expect === 'clean') {
      // Clean mode: every message in `result.messages` is unexpected.
      const unexpected = result.messages.filter((m) => m.ruleId !== null);
      if (unexpected.length > 0) {
        failures.push({
          fixture: expected.file,
          ruleId: expected.ruleId,
          message:
            `Expected ${expected.file} to be clean but ESLint reported: ` +
            unexpected.map((m) => `${m.ruleId}: ${m.message}`).join('; '),
        });
      }
      continue;
    }

    const matchingMessages = result.messages.filter((m) => m.ruleId === expected.ruleId);
    if (matchingMessages.length === 0) {
      const actualRuleIds = [...new Set(result.messages.map((m) => m.ruleId).filter(Boolean))];
      failures.push({
        fixture: expected.file,
        ruleId: expected.ruleId,
        message:
          `Expected ESLint to report rule "${expected.ruleId}" against ${expected.file} ` +
          `but it reported: [${actualRuleIds.join(', ') || 'no rule ids'}]. ` +
          `The fixture exists to prove the rule is enforced — either the rule was disabled ` +
          `or the fixture stopped violating it.`,
      });
    }
  }

  return { failures, results };
}

// ---------------------------------------------------------------------------
// CLI wrapper. The pure function above is what tests exercise; the wrapper
// translates the structured result into the CI exit-code contract.
// ---------------------------------------------------------------------------

function bail(message) {
  console.error(`::error::${message}`);
  process.exit(1);
}

async function runCli() {
  // Confirm every fixture exists on disk before invoking ESLint — a
  // missing fixture is a structural problem (someone deleted it),
  // not an ESLint failure.
  for (const { file } of EXPECTED_VIOLATIONS) {
    const p = resolve(fixturesDir, file);
    try {
      readFileSync(p, 'utf8');
    } catch (e) {
      bail(`Lint fixture ${file} is missing at ${p}. Restore it from tests/lint-fixtures/ — issue #1542 relies on it to prove the gate is live.`);
    }
  }

  const { failures, results } = await checkEslintFixtures({});

  if (failures.length > 0) {
    console.error(`::error::Lint fixtures gate failed (${failures.length} missing violation${failures.length === 1 ? '' : 's'}):`);
    for (const f of failures) {
      console.error(`  - [${f.fixture}] expected ${f.ruleId}: ${f.message}`);
    }
    console.error('\nThe fixtures exist to prove react-hooks rules are enforced. Restoring them without the violation means the gate is no longer catching the regression class issue #1542 set out to block.');
    process.exit(1);
  }

  // Surface the actual ESLint output too — useful in CI logs.
  const formatter = await new ESLint({ overrideConfigFile: resolve(repoRoot, 'eslint.config.js') }).loadFormatter('stylish');
  const formatted = await formatter.format(results);
  if (formatted.trim()) process.stdout.write(formatted);

  console.log(`Lint fixtures gate passed: ${EXPECTED_VIOLATIONS.length} fixtures each tripped their expected rule.`);
}

const isMain = (() => {
  if (!process.argv[1]) return false;
  try {
    return resolve(fileURLToPath(import.meta.url)) === resolve(process.argv[1]);
  } catch {
    return false;
  }
})();
if (isMain) {
  runCli().catch((err) => {
    console.error(`Lint fixtures check failed: ${err.message}`);
    process.exit(1);
  });
}
