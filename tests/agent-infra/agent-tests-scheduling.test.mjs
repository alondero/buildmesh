// The agent-infrastructure suite's own scheduling (issue #2254).
//
// Node's test runner starts one process per test file, and its default
// concurrency is the machine's core count minus one: a 24-core workstation ran
// 23 of this suite's files at once while a 2-core CI runner ran them one at a
// time. The suite's files are not equally heavy - several compile Rust or
// build a Tauri app - so the suite's demand scaled with the machine, and the
// fixtures that carry a wall-clock budget expired when the machine was busy
// rather than when they were broken.
//
// Two bounds own that now, and these tests are their regression guard:
// the file concurrency is capped, and the suite takes a machine-wide slot so
// an overlapping copy waits instead of doubling the demand.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { FILE_CONCURRENCY_CAP, fileConcurrency, suiteCommand } from '../../scripts/agent-tests.mjs';
import { agentTestLimit, agentTestSlotDirectory } from '../../scripts/harness-lanes.mjs';
import { planGates } from '../../scripts/harness-plan.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const runner = fileURLToPath(new URL('../../scripts/agent-tests.mjs', import.meta.url));

test('the suite bounds its file concurrency instead of taking the whole machine (#2254)', () => {
  // The cap is a property of the suite, not of the machine it runs on: a
  // workstation must not ask for eleven times what CI asks for.
  assert.equal(FILE_CONCURRENCY_CAP, 4);
  assert.equal(fileConcurrency(24), 4);
  assert.equal(fileConcurrency(5), 4);
  // The cap never raises the runner's own default, which is the core count
  // minus one, so a small CI runner keeps running as few files as before.
  assert.equal(fileConcurrency(4), 3);
  assert.equal(fileConcurrency(3), 2);
  assert.equal(fileConcurrency(2), 1);
  assert.equal(fileConcurrency(1), 1);
  assert.deepEqual(suiteCommand(['a.test.mjs'], 24), ['--test', '--test-concurrency=4', 'a.test.mjs']);
});

test('the infrastructure suite has its own slot budget, apart from the heavy-gate slots', () => {
  assert.equal(agentTestLimit({}), 1);
  assert.equal(agentTestLimit({ BUILDMESH_AGENT_TEST_LIMIT: '2' }), 2);
  for (const bad of ['0', '-1', '1.5', 'many', '']) assert.equal(agentTestLimit({ BUILDMESH_AGENT_TEST_LIMIT: bad }), 1, bad);
  // A separate directory, so a suite that holds its slot for minutes cannot
  // starve the product lanes of the slots they overlap in (#2126).
  assert.notEqual(agentTestSlotDirectory({}), agentTestSlotDirectory({ BUILDMESH_AGENT_TEST_SLOTS_DIR: 'x' }));
});

test('an overlapping copy of the infrastructure suite waits for the machine-wide slot (#2254)', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'buildmesh-agent-slot-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const slots = join(dir, 'slots');
  const env = { ...process.env, BUILDMESH_AGENT_TEST_SLOTS_DIR: slots, BUILDMESH_AGENT_TEST_LIMIT: '1' };
  // Node refuses to run a test runner inside a test runner, and the marker it
  // uses to detect that is NODE_TEST_CONTEXT. The harness scrubs it the same
  // way before it starts a gate (scripts/harness.mjs).
  delete env.NODE_TEST_CONTEXT;
  // These copies are independent callers, not a nested run: the slot flag is
  // dropped so they each take the slot the way `scripts/check.ps1` and CI do.
  delete env.BUILDMESH_AGENT_TEST_SLOT;

  // The first copy holds the slot until it is released; the second must not
  // start its file while the first one holds it.
  const firstFile = join(dir, 'first.test.mjs');
  writeFileSync(firstFile, `import { test } from 'node:test';
import { writeFileSync, existsSync } from 'node:fs';
test('first', async () => {
  writeFileSync(${JSON.stringify(join(dir, 'first-started'))}, 'yes');
  const deadline = Date.now() + 60000;
  while (!existsSync(${JSON.stringify(join(dir, 'first-release'))}) && Date.now() < deadline) await new Promise(done => setTimeout(done, 50));
  writeFileSync(${JSON.stringify(join(dir, 'first-done'))}, 'yes');
});
`);
  const secondFile = join(dir, 'second.test.mjs');
  writeFileSync(secondFile, `import { test } from 'node:test';
import { writeFileSync } from 'node:fs';
test('second', () => { writeFileSync(${JSON.stringify(join(dir, 'second-started'))}, 'yes'); });
`);

  const children = [];
  const spawnCopy = file => {
    const child = spawn(process.execPath, [runner, file], { env, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '';
    child.stdout.on('data', chunk => { output += chunk; });
    child.stderr.on('data', chunk => { output += chunk; });
    children.push(child);
    return { child, output: () => output };
  };
  t.after(() => { for (const child of children) child.kill(); });

  const first = spawnCopy(firstFile);
  const started = Date.now();
  while (!existsSync(join(dir, 'first-started')) && Date.now() - started < 60000) await new Promise(done => setTimeout(done, 50));
  assert.ok(existsSync(join(dir, 'first-started')), `the first copy must start its file\n${first.output()}`);

  const second = spawnCopy(secondFile);
  await new Promise(done => setTimeout(done, 3000));
  assert.equal(existsSync(join(dir, 'second-started')), false, 'the second copy must wait for the slot');
  assert.match(second.output(), /QUEUED agent-tests/, 'the waiting copy must say so');

  writeFileSync(join(dir, 'first-release'), 'yes');
  const exited = Date.now();
  while (first.child.exitCode === null && Date.now() - exited < 60000) await new Promise(done => setTimeout(done, 50));
  assert.notEqual(first.child.exitCode, null, 'the first copy must exit');

  const startedAgain = Date.now();
  while (!existsSync(join(dir, 'second-started')) && Date.now() - startedAgain < 60000) await new Promise(done => setTimeout(done, 50));
  assert.ok(existsSync(join(dir, 'second-started')), 'the second copy must run once the slot is free');
});

test('the harness takes the infrastructure slot before it starts the gate (#2254)', () => {
  const gates = planGates(['scripts/harness.mjs']);
  const agentTests = gates.find(gate => gate.id === 'agent-tests');
  assert.ok(agentTests, 'the agent-tests gate must be in the plan');
  assert.equal(agentTests.agentTest, true);
  // The suite must not take a product heavy-gate slot: the product lanes need
  // both of those to overlap (#2126).
  assert.equal(agentTests.heavy, undefined);
});

test('a caller that already holds the slot is not made to wait for itself (#2254)', t => {
  const dir = mkdtempSync(join(tmpdir(), 'buildmesh-agent-held-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const slots = join(dir, 'slots');
  mkdirSync(slots, { recursive: true });
  // A live owner holds the only slot, so a caller that took it itself would
  // wait forever.
  writeFileSync(join(slots, 'slot-0.json'), JSON.stringify({ pid: process.pid, token: 'held', gate: 'agent-tests', root: 'other' }));
  const file = join(dir, 'held.test.mjs');
  writeFileSync(file, `import { test } from 'node:test';
import { writeFileSync } from 'node:fs';
test('held', () => { writeFileSync(${JSON.stringify(join(dir, 'held-started'))}, 'yes'); });
`);
  const env = { ...process.env, BUILDMESH_AGENT_TEST_SLOTS_DIR: slots, BUILDMESH_AGENT_TEST_LIMIT: '1', BUILDMESH_AGENT_TEST_SLOT: 'held' };
  delete env.NODE_TEST_CONTEXT;
  const result = spawnSync(process.execPath, [runner, file], { env, encoding: 'utf8', timeout: 60000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.ok(existsSync(join(dir, 'held-started')), 'the holder must run its file');
});

test('the repository runs the infrastructure suite through its scheduling owner', () => {
  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
  assert.match(pkg.scripts['test:agent'], /^node scripts\/agent-tests\.mjs tests\/agent-infra\//);
  assert.ok(pkg.scripts['test:agent'].includes('tests/agent-infra/harness.test.mjs'));
  // The runner owns the concurrency, so the npm script must not also pass a
  // flag that would override it.
  assert.ok(!pkg.scripts['test:agent'].includes('--test-concurrency'));
});
