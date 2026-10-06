import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { readVitestLegs, COVERED_SUITES, repoRoot } from '../../scripts/ci/vitest-legs.mjs';

/**
 * The vitest legs in verify.yml split the frontend suite across parallel jobs
 * (issue #2046). That is only safe while the legs still partition the suite:
 * vitest gives every file to exactly one `--shard`, so the shard indices must
 * be 1..N over a single N, and the suite directories must cover exactly the two
 * the one combined invocation used to name. Drop or renumber a shard and a test
 * suite stops running with nothing failing — the same failure mode the Rust
 * shard-coverage gate exists for, and the reason this gate reads the matrix
 * rather than trusting it.
 *
 * The runtime alternative (`npx vitest list` per leg) was measured and rejected:
 * one boot costs ~57s, so four boots would cost more wall-clock than the split
 * saves.
 */
const workflow = fs.readFileSync(path.join(repoRoot, '.github', 'workflows', 'verify.yml'), 'utf8');

/** The body of one top-level `job:` block, so one job's steps are not read as another's. */
function jobBlock(jobId) {
  const marker = `\n  ${jobId}:\n`;
  const start = workflow.indexOf(marker);
  assert.notEqual(start, -1, `verify.yml has no \`${jobId}\` job`);
  const rest = workflow.slice(start + marker.length);
  const end = rest.search(/\n {2}[a-z][\w-]*:\n/);
  return end === -1 ? workflow.slice(start) : rest.slice(0, end);
}

test('the legs are read from the real verify workflow', () => {
  const legs = readVitestLegs();
  assert.deepEqual(legs.map((leg) => leg.label), ['unit 1/2', 'unit 2/2', 'integration']);
  assert.equal(legs[0].args, '--pool=threads --shard=1/2 tests/unit');
  assert.equal(legs[2].args, '--pool=threads tests/integration');
});

test('every suite the combined invocation ran is owned, and none is dropped or invented', () => {
  const legs = readVitestLegs();
  const named = legs.flatMap((leg) => leg.suites);
  assert.deepEqual([...new Set(named)].sort(), [...COVERED_SUITES].sort());
  for (const suite of COVERED_SUITES) {
    const owners = legs.filter((leg) => leg.suites.includes(suite));
    assert.ok(owners.length > 0, `no leg runs ${suite}`);
    // A suite may be named by several legs only when those legs shard it —
    // otherwise a `--shard` on the only leg that owns it silently drops the
    // rest of the suite.
    if (owners.length > 1) {
      assert.ok(
        owners.every((leg) => leg.shard),
        `${suite} is split across ${owners.length} legs, so every one of them must shard it`,
      );
    } else {
      assert.equal(owners[0].shard, null, `${suite} has one leg, so that leg must run all of it`);
    }
  }
});

test('the shard legs are 1..N over one N, so no file can fall between them', () => {
  const sharded = readVitestLegs().filter((leg) => leg.shard);
  assert.ok(sharded.length >= 2, 'expected at least two sharded legs');
  const totals = new Set(sharded.map((leg) => leg.shard.of));
  assert.equal(totals.size, 1, `every shard leg must use the same denominator, got ${[...totals].join(', ')}`);
  const indexes = sharded.map((leg) => leg.shard.index).sort((a, b) => a - b);
  assert.deepEqual(indexes, Array.from({ length: sharded.length }, (_, at) => at + 1));
});

test('the legs keep the threads pool CI needs, and the browser belongs to the integration leg', () => {
  const body = jobBlock('quality-tests');
  for (const leg of readVitestLegs()) {
    assert.match(leg.args, /--pool=threads/, `leg "${leg.label}" drops the threads pool (#1257)`);
    // `ui-shot.test.ts` is the only suite that calls `launchChromium`, and it
    // lives in tests/integration. A leg naming that directory has to install
    // the browser; a leg naming only tests/unit must not pay for one.
    const wantsBrowser = leg.suites.includes('tests/integration');
    assert.equal(leg.browser, String(wantsBrowser), `leg "${leg.label}" declares the wrong browser flag`);
  }
  assert.match(body, /if: \$\{\{ matrix\.leg\.browser == 'true' \}\}/);
  assert.match(body, /npx playwright install --with-deps chromium/);
});

test('the required Quality check aggregates both branches and fails closed', () => {
  // `Quality (Linux)` is a required status check, so renaming it is a
  // branch-protection change and skipping it counts as passing. The aggregate
  // must therefore need both branches and must not rely on the implicit skip.
  const aggregate = jobBlock('quality');
  assert.match(aggregate, /needs: \[changes, quality-gates, quality-tests\]/);
  assert.match(aggregate, /if: always\(\) &&/);
  assert.match(aggregate, /quality-gates=\$GATES_RESULT quality-tests=\$TESTS_RESULT - the frontend gate is not fully green/);
});

test('the legs check out full history, because a unit test reads the release tags', () => {
  // `tests/unit/app-version.test.ts` resolves the latest release with
  // `git describe --abbrev=0 --tags`. The suite used to run in the job that
  // had `fetch-depth: 0`; a leg that checks out shallow has no tags and that
  // test fails. The first split run of this matrix proved it.
  assert.match(jobBlock('quality-tests'), /actions\/checkout@v7\n {8}with:\n {10}fetch-depth: 0/);
});

test('a matrix line the parser does not understand fails loudly instead of dropping a leg', () => {
  const broken = [
    '  quality-tests:',
    '    strategy:',
    '      matrix:',
    '        leg:',
    '          - label: unit 1/2',
    '            args: "--pool=threads --shard=1/2 tests/unit"',
    '            browser: "true"',
    '          - label: broken',
    '            filter: "tests/unit"',
    '    steps:',
  ].join('\n');
  assert.throws(() => readVitestLegs(broken), /does not understand/);
  assert.throws(() => readVitestLegs('jobs:\n  quality:\n'), /no `quality-tests` job/);
  assert.throws(
    () => readVitestLegs(['  quality-tests:', '    strategy:', '      matrix:', '        leg:', '          - label: leg one'].join('\n')),
    /has no args line/,
  );
});