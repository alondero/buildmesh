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
// Three drift holes this verifier closes (review feedback on PR #1542):
//   1. The verifier loads the PRODUCTION `eslint.config.js` and asserts
//      that the React Hooks rules remain enabled at the right severity.
//      A future PR that sets `react-hooks/rules-of-hooks: 'off'` in prod
//      passes `npm run lint` silently; this verifier fails instead.
//   2. The verifier uses `overrideConfigFile: true` + a minimal inline
//      config so the production `tests/lint-fixtures/**` ignore doesn't
//      skip the fixtures. The rule severities are inherited from the prod
//      config check above; if the prod rules are off, the lint result is
//      irrelevant.
//   3. `expect: 'clean'` mode (used by unit tests) now reports parse
//      errors as failures — previously a `ruleId: null` fatal was filtered
//      out and a syntactically broken fixture would pass as "clean".
//
// Usage:
//   node scripts/check-eslint-fixtures.mjs          # CI / local
//   npm run lint:fixtures                           # npm wrapper

import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
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
  { file: 'conditional-hook.tsx', ruleId: 'react-hooks/rules-of-hooks', minSeverity: 2 },
  { file: 'missing-deps.tsx', ruleId: 'react-hooks/exhaustive-deps', minSeverity: 1 },
];

/** Severity numbers ESLint uses internally:
 *   0 = off
 *   1 = warn
 *   2 = error
 */
function severityNumber(level) {
  if (level === 'error' || level === 2) return 2;
  if (level === 'warn' || level === 1) return 1;
  if (level === 'off' || level === 0) return 0;
  // Some rule entries are arrays like ['warn', { ... }].
  if (Array.isArray(level) && level.length > 0) return severityNumber(level[0]);
  return 0;
}

/** Load the production `eslint.config.js` and resolve each `EXPECTED_VIOLATIONS`
 *  rule to the configured severity under the TS/TSX files block. Returns a
 *  list of `{ ruleId, configured, minSeverity, ok }`. `ok` is false when the
 *  prod severity has dropped below `minSeverity` — that's the drift hole.
 *
 *  Implementation note: flat-config blocks don't merge rule entries across
 *  blocks by default (later blocks override earlier ones for the same rule),
 *  but we only care about the final configured level under the TS files block.
 *  ESLint's flat config can resolve this via `ESLint#calculateConfigForFile`,
 *  but that's an extra plugin dependency. The simpler approach: walk the
 *  exported config array and take the LAST entry that has a `rules[ruleId]`
 *  (later blocks override earlier ones per the flat-config spec). For our
 *  config that's exactly the TS block's `rules`.
 */
export async function checkEslintConfigDrift({ configPath } = {}) {
  const cfgPath = configPath ?? resolve(repoRoot, 'eslint.config.js');
  const prodConfig = (await import(pathToFileURL(cfgPath).href)).default;
  if (!Array.isArray(prodConfig)) {
    return [{ ok: false, message: `eslint.config.js default export is not an array (got ${typeof prodConfig})` }];
  }

  const failures = [];
  for (const expected of EXPECTED_VIOLATIONS) {
    let configured = null;
    // Walk in reverse so the LAST (most-specific) block wins — matches
    // flat-config's later-blocks-override semantics.
    for (let i = prodConfig.length - 1; i >= 0; i--) {
      const block = prodConfig[i];
      const ruleEntry = block?.rules?.[expected.ruleId];
      if (ruleEntry !== undefined) {
        configured = severityNumber(ruleEntry);
        break;
      }
    }
    if (configured === null) {
      failures.push({
        ruleId: expected.ruleId,
        configured,
        minSeverity: expected.minSeverity,
        ok: false,
        message: `eslint.config.js does not declare rule "${expected.ruleId}" in any block. ` +
          `The verifier cannot trust this gate while the rule is unset.`,
      });
      continue;
    }
    if (configured < expected.minSeverity) {
      failures.push({
        ruleId: expected.ruleId,
        configured,
        minSeverity: expected.minSeverity,
        ok: false,
        message:
          `eslint.config.js sets "${expected.ruleId}" to severity ${configured} ` +
          `(need >= ${expected.minSeverity}). ` +
          `The fixture verifier proves the plugin can fire on violations, ` +
          `but not that the gate enables it. CI must fail if a future tweak silences the rule.`,
      });
    }
  }
  return failures;
}

/** Run ESLint against the fixtures and return a list of failures.
 *
 *  Two modes:
 *   - `expect: 'violation'` (default — production behavior). Each
 *     fixture MUST trigger its `expected.ruleId`; missing it is a
 *     failure.
 *   - `expect: 'clean'` (test-only). Every fixture must produce zero
 *     messages from the in-scope rule set, and zero fatal parse
 *     errors. Used by unit tests to assert a clean file doesn't
 *     false-trip the rule.
 *
 *  Returns `{ failures, results, driftFailures }`:
 *    - `failures`: per-fixture failures from lint output.
 *    - `results`: raw ESLint results.
 *    - `driftFailures`: failures from `checkEslintConfigDrift` — run
 *      BEFORE the lint so a rule-disablement surfaces immediately and
 *      is not masked by the lint result.
 */
export async function checkEslintFixtures({ fixturesDir: dir = fixturesDir, eslint, expect = 'violation' } = {}) {
  // Drift check FIRST — if the prod config has dropped the rule severity,
  // there's no point linting (the result is meaningless). Fail loudly.
  const driftFailures = await checkEslintConfigDrift();
  if (driftFailures.length > 0) {
    return { failures: [], results: [], driftFailures };
  }

  const lint = eslint ?? new ESLint({
    // `overrideConfigFile: true` (per ESLint 9) disables the auto-loaded
    // flat-config lookup so the only config ESLint sees is the inline
    // rule set passed via `overrideConfig` below. Without this, ESLint
    // would also load `eslint.config.js` from `cwd`, whose
    // `tests/lint-fixtures/**` ignore would silently skip the fixtures.
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
      // Clean mode: every message AND every fatal parse error is unexpected.
      // The previous filter `m.ruleId !== null` silently dropped parse
      // errors, so a syntactically broken fixture would pass as "clean".
      const offending = result.messages.filter(
        (m) => m.ruleId !== null || m.fatal === true || result.errorCount > 0,
      );
      if (offending.length > 0) {
        failures.push({
          fixture: expected.file,
          ruleId: expected.ruleId,
          message:
            `Expected ${expected.file} to be clean but ESLint reported: ` +
            offending.map((m) => `${m.ruleId ?? 'fatal'}: ${m.message}`).join('; '),
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

  return { failures, results, driftFailures: [] };
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

  const { failures, results, driftFailures } = await checkEslintFixtures({});

  if (driftFailures.length > 0) {
    console.error(`::error::Lint fixtures drift gate failed (${driftFailures.length}):`);
    for (const d of driftFailures) {
      console.error(`  - ${d.message}`);
    }
    console.error('\nThe gate must keep these rules enabled at the configured severity. Bumping a rule to "off" or "warn" (where "error" is required) turns the gate into a silent pass.');
    process.exit(1);
  }

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

  console.log(`Lint fixtures gate passed: ${EXPECTED_VIOLATIONS.length} fixtures each tripped their expected rule (drift check OK).`);
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
