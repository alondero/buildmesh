// Issue #1542 — asserts the ESLint gate is wired AND configured to catch
// the high-signal React Hooks rules. The fixture file
// `tests/agent-infra/fixtures/react-hooks-violations.tsx` contains two
// intentional violations; this test fails if ESLint no longer flags them.
//
// We drive ESLint through its Node API rather than shelling out. Two
// reasons: (a) the Windows `eslint.cmd` shim does not reliably propagate
// stdout/stderr through `execFileSync` (a known platform quirk — even
// when it does, the parsed ESLint JSON lives in stderr); and (b) using
// the API gives us structured results, so we can assert rule names by
// ID rather than text scraping the formatter output.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { readFileSync, existsSync } from 'node:fs';
import { ESLint } from 'eslint';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');

const configPath = join(root, 'eslint.config.js');
const fixturePath = join(root, 'tests/agent-infra/fixtures/react-hooks-violations.tsx');

test('eslint.config.js exists and the lint script is wired', () => {
  assert.ok(existsSync(configPath), `Missing ${configPath} — issue #1542 acceptance requires an ESLint flat config.`);
  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
  // The script must invoke ESLint and pass `--max-warnings 0` so any
  // introduced warning fails the step (issue #1542 acceptance: "CI
  // fails on introduced violations"). We split on whitespace to be
  // robust against future flag additions.
  const lintScript = pkg.scripts?.lint ?? '';
  assert.ok(/eslint/.test(lintScript), `package.json must invoke ESLint in its lint script; got ${JSON.stringify(lintScript)}.`);
  assert.ok(
    /--max-warnings\s+0\b/.test(lintScript),
    `package.json's lint script must pass --max-warnings 0; got ${JSON.stringify(lintScript)}.`,
  );
});

test('fixture file still carries the intentional violations', () => {
  // Guard against a well-meaning future edit silently turning the test
  // green by deleting the violations. The rule names asserted below
  // rely on both being present in the source.
  const text = readFileSync(fixturePath, 'utf8');
  assert.match(text, /rules-of-hooks/, 'Fixture must mention rules-of-hooks so the matching assertion below is meaningful.');
  assert.match(text, /exhaustive-deps/, 'Fixture must mention exhaustive-deps so the matching assertion below is meaningful.');
});

test('ESLint flags both react-hooks violations in the fixture', async () => {
  // `ignore: false` is the only documented way to lint a file that
  // the config's `ignores` list excludes — see the comment in
  // eslint.config.js's `ignores` block. The fixture exists to PROVE
  // the gate catches what it claims to catch (issue #1542 acceptance);
  // `npm run lint` skips it on purpose so the gate can stay green,
  // but this test exercises the same rules against the same source.
  const eslint = new ESLint({ cwd: root, ignore: false });
  const results = await eslint.lintFiles([fixturePath]);
  assert.equal(results.length, 1, 'ESLint should report on exactly one file.');
  const messages = results[0].messages.map(m => m.ruleId);
  assert.ok(
    messages.includes('react-hooks/rules-of-hooks'),
    `Expected react-hooks/rules-of-hooks in ESLint output; got ${JSON.stringify(messages)}`,
  );
  assert.ok(
    messages.includes('react-hooks/exhaustive-deps'),
    `Expected react-hooks/exhaustive-deps in ESLint output; got ${JSON.stringify(messages)}`,
  );
});
