import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { changedPaths, completion, fingerprint, runGate, scopePaths, verify, waitForVerify } from '../../scripts/harness.mjs';
import { executedTests, gateReads, planGates, touchedFormatDiffs } from '../../scripts/harness-plan.mjs';
import { acquireSlot, heavyGateEnv, heavyGateLimit, runPlan, slotHolders } from '../../scripts/harness-lanes.mjs';
import { conptyPrerequisite, gatePassed, isolatedCommand, isolateTests, knownFlakes, rustFailures, validateFlakeIssues, vitestReport } from '../../scripts/harness-test-failures.mjs';
import { VERSION as CONPTY_VERSION } from '../../scripts/prepare-conpty.mjs';

const script = fileURLToPath(new URL('../../scripts/harness.mjs', import.meta.url));
const root = fileURLToPath(new URL('../../', import.meta.url));

test('Vitest JSON and cargo failure blocks name exact tests without panic prose', () => {
  const report = vitestReport(root, { numPassedTests: 8, numFailedTests: 1, testResults: [{ name: join(root, 'tests/unit/example.test.ts'), status: 'failed', assertionResults: [{ fullName: 'suite a + (b)', status: 'failed' }] }] });
  assert.deepEqual(report.failures, [{ id: 'tests/unit/example.test.ts > suite a + (b)', file: 'tests/unit/example.test.ts', name: 'suite a + (b)' }]);
  assert.equal(report.unattributed, false);
  assert.equal(report.count, 8);
  const cargo = 'running 2 tests\ntest db::broken ... FAILED\n\nfailures:\n\n---- db::broken stdout ----\n    misleading panic detail\n\nfailures:\n\n    db::broken\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored;\n';
  assert.deepEqual(rustFailures(cargo), [{ id: 'db::broken', name: 'db::broken', target: { kind: 'lib' } }]);
  assert.equal(rustFailures(cargo, { kind: 'test', name: 'integration' })[0].id, 'test:integration > db::broken');
  assert.deepEqual(isolatedCommand(report.failures[0], 'vitest', 'report.json'), ['node', 'scripts/harness-vitest-isolate.mjs', 'tests/unit/example.test.ts', '^suite a \\+ \\(b\\)$', 'report.json']);
  assert.deepEqual(isolatedCommand(rustFailures(cargo)[0], 'rust'), ['cargo', 'test', '--locked', '--lib', 'db::broken', '--', '--exact', '--test-threads=1']);
});

test('isolated passes distinguish listed flakes, unlisted flakes and persistent failures', async t => {
  const fixture = repo(t);
  const failures = [{ id: 'rust::known', name: 'rust::known' }, { id: 'rust::new', name: 'rust::new' }];
  let calls = 0;
  const run = async () => { calls += 1; return { exitCode: 0, count: 1, log: 'isolated.log' }; };
  const listed = await isolateTests(fixture.cwd, { failures: [failures[0]] }, run, { 'rust::known': 1833 });
  assert.equal(listed.outcome, 'FLAKY');
  assert.equal(listed.failures[0].issue, 1833);
  assert.equal(gatePassed(listed), true);
  const unlisted = await isolateTests(fixture.cwd, { failures }, run, { 'rust::known': 1833 });
  assert.equal(unlisted.outcome, 'FLAKY');
  assert.equal(gatePassed(unlisted), false);
  assert.match(unlisted.reason, /File an issue/);
  assert.equal(calls, 3);
  for (const rerun of [{ exitCode: 1, count: 0 }, { exitCode: 0, count: 0 }, { exitCode: 0, count: 1, matched: false }, { exitCode: 124, count: 0 }, { exitCode: 127, count: 0 }]) {
    const row = await isolateTests(fixture.cwd, { failures: [failures[0]] }, async () => rerun, { 'rust::known': 1833 });
    assert.equal(row.outcome, rerun.exitCode === 124 ? 'TIMEOUT' : rerun.exitCode === 127 ? 'BLOCKED' : 'FAIL');
    assert.equal(gatePassed(row), false);
  }
  assert.equal((await isolateTests(fixture.cwd, { failures, unattributed: true }, run, {})).outcome, 'FAIL');
});

test('known flakes reference open GitHub issues and reject closed issues or PRs', async () => {
  const entries = knownFlakes(root);
  await validateFlakeIssues(entries);
  await assert.rejects(validateFlakeIssues({ test: 1 }, async () => ({ state: 'closed' })), /open issue/);
  await assert.rejects(validateFlakeIssues({ test: 1 }, async () => ({ state: 'open', pull_request: {} })), /open issue/);
});

test('uncached ConPTY checks its network prerequisite before compilation; cached packages work offline', async t => {
  const fixture = repo(t);
  const unavailable = await conptyPrerequisite(fixture.cwd, async () => { throw new Error('offline'); });
  assert.match(unavailable, /not cached.*unreachable/);
  assert.match(await conptyPrerequisite(fixture.cwd, async () => ({ ok: false, status: 503 })), /HTTP 503/);
  assert.equal(await conptyPrerequisite(fixture.cwd, async () => ({ ok: true })), null);
  fixture.put(`src-tauri/target/conpty/${CONPTY_VERSION}/package.zip`, 'cached fixture');
  assert.equal(await conptyPrerequisite(fixture.cwd, async () => { throw new Error('must not contact network'); }), null);
});

test('real Vitest gate records one exact isolated rerun and keeps unhandled errors red', async t => {
  const fixture = repo(t);
  symlinkSync(join(root, 'node_modules'), join(fixture.cwd, 'node_modules'), 'junction');
  fixture.put('scripts/harness-vitest-isolate.mjs', readFileSync(join(root, 'scripts/harness-vitest-isolate.mjs')));
  fixture.put('package.json', '{"type":"module"}');
  fixture.put('vitest.config.mjs', 'export default { test: { include: ["tests/*.test.js"], environment: "node", passWithNoTests: false } };');
  fixture.put('tests/isolation.test.js', `import { test, expect } from 'vitest';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
test('one + (exact)', () => { mkdirSync('.harness', {recursive:true}); const seen=existsSync('.harness/seen'); writeFileSync('.harness/seen','yes'); expect(seen).toBe(true); });
test('one + (exact) sibling', () => { throw new Error('persistent sibling'); });
`);
  const gate = { id: 'frontend-tests', command: ['node', 'node_modules/vitest/vitest.mjs', 'run'], tests: 'vitest', minutes: 1 };
  const row = await runGate(fixture.cwd, gate, fixture.base);
  assert.equal(row.outcome, 'FAIL', JSON.stringify(row));
  assert.deepEqual(row.failures.map(test => test.outcome), ['FLAKY', 'FAIL']);
  assert.ok(row.failures[0].rerun.command.includes('^one \\+ \\(exact\\)$'));
  assert.equal(row.failures[0].rerun.count, 1);
  assert.equal(row.failures[1].rerun.count, 0);
  fixture.put('tests/isolation.test.js', "import { test } from 'vitest'; test('runtime error', () => { Promise.reject(new Error('unhandled rejection')); });");
  const runtime = await runGate(fixture.cwd, gate, fixture.base);
  assert.equal(runtime.outcome, 'FAIL');
  assert.equal(runtime.unattributed, true);
  assert.deepEqual(runtime.failures, []);
  fixture.put('tests/isolation.test.js', "throw new Error('collection failure');");
  const collection = await runGate(fixture.cwd, gate, fixture.base);
  assert.equal(collection.outcome, 'FAIL');
  assert.equal(collection.failures[0].id, 'tests/isolation.test.js');
  assert.equal(collection.failures[0].rerun, undefined);
  fixture.put('vitest.config.mjs', 'export default { test: { include: ["tests/*.test.*"], environment: "node" } };');
  fixture.put('tests/isolation.test.js', "import { test, expect } from 'vitest'; test.skipIf(process.env.VITEST_MAX_WORKERS === '1')('same name', () => { expect(false).toBe(true); });");
  fixture.put('tests/isolation.test.jsx', "import { test, expect } from 'vitest'; test('same name', () => { expect(true).toBe(true); });");
  const skipped = await runGate(fixture.cwd, gate, fixture.base);
  assert.equal(skipped.outcome, 'FAIL');
  assert.equal(skipped.failures[0].rerun.count, 0);
  assert.match(readFileSync(skipped.failures[0].rerun.log, 'utf8'), /1 skipped/);
  assert.doesNotMatch(readFileSync(skipped.failures[0].rerun.log, 'utf8'), /isolation.test.jsx/);
});

test('real Cargo gate names a suite failure and reruns just that library test', async t => {
  const fixture = repo(t);
  fixture.put('src-tauri/Cargo.toml', '[package]\nname="harness-fixture"\nversion="0.1.0"\nedition="2021"\n');
  fixture.put('src-tauri/src/lib.rs', '#[test] fn transient() { assert!(std::env::args().any(|arg| arg == "--exact")); }\n#[test] fn sibling() {}\n');
  const row = await runGate(fixture.cwd, { id: 'rust-tests', command: ['cargo', 'test'], cwd: 'src-tauri', tests: 'rust', minutes: 1 }, fixture.base);
  assert.equal(row.outcome, 'FLAKY', JSON.stringify(row));
  assert.equal(row.failures[0].id, 'transient');
  assert.equal(row.failures[0].rerun.count, 1);
  assert.deepEqual(row.failures[0].rerun.command.slice(1), ['test', '--locked', '--lib', 'transient', '--', '--exact', '--test-threads=1']);
});

test('the Rust shard runner records targets and reruns library and integration failures separately', async t => {
  const fixture = repo(t);
  for (const path of ['scripts/rust-test-shards.mjs', 'scripts/harness-test-failures.mjs', 'scripts/ci/rust-shards.mjs']) fixture.put(path, readFileSync(join(root, path)));
  fixture.put('.github/workflows/verify.yml', 'jobs:\n  rust-tests:\n    strategy:\n      matrix:\n        shard:\n          - label: fixture\n            args: ""\n    steps:\n');
  fixture.put('src-tauri/Cargo.toml', '[package]\nname="harness-fixture"\nversion="0.1.0"\nedition="2021"\n');
  const tests = '#[test] fn transient() { assert!(std::env::args().any(|arg| arg == "--exact")); }\n#[test] fn sibling() {}\n';
  fixture.put('src-tauri/src/lib.rs', tests);
  fixture.put('src-tauri/tests/contract.rs', tests);
  fixture.put('src-tauri/Cargo.lock', 'version = 4\n[[package]]\nname = "harness-fixture"\nversion = "0.1.0"\n');
  // This std-only crate does not load ConPTY; stage placeholder assets to
  // exercise the runner's prerequisite seam separately from Tauri packaging.
  for (const asset of ['conpty.dll', 'x64/OpenConsole.exe', 'arm64/OpenConsole.exe', 'x86/OpenConsole.exe']) fixture.put(`src-tauri/target/debug/deps/${asset}`, 'fixture');
  const gate = { id: 'rust-tests', command: ['node', 'scripts/rust-test-shards.mjs'], tests: 'rust', minutes: 1 };
  const extraEnv = { CARGO_TARGET_DIR: join(fixture.cwd, 'src-tauri/target') };
  const row = await runGate(fixture.cwd, gate, fixture.base, [], extraEnv);
  assert.equal(row.outcome, 'FLAKY', `${JSON.stringify(row)}\n${readFileSync(row.log, 'utf8')}`);
  assert.deepEqual(row.failures.map(test => test.id).sort(), ['test:contract > transient', 'transient']);
  assert.ok(row.failures.every(test => test.rerun.count === 1));
  const integration = row.failures.find(test => test.target.kind === 'test');
  assert.deepEqual(integration.rerun.command.slice(1), ['test', '--locked', '--test', 'contract', 'transient', '--', '--exact', '--test-threads=1']);
  if (process.platform === 'win32') {
    unlinkSync(join(fixture.cwd, 'src-tauri/target/debug/deps/conpty.dll'));
    const blocked = await runGate(fixture.cwd, gate, fixture.base, [], extraEnv);
    assert.equal(blocked.outcome, 'BLOCKED');
    assert.match(blocked.reason, /ConPTY runtime is not staged.*conpty.dll/);
  }
});

test('test prerequisite checks report missing Git as BLOCKED before launching the command', async t => {
  const fixture = repo(t);
  const row = await runGate(fixture.cwd, { id: 'rust-tests', tests: 'rust', command: ['node', '-e', 'throw new Error("must not start")'], minutes: 1 }, fixture.base, [], { PATH: fixture.cwd });
  assert.equal(row.outcome, 'BLOCKED');
  assert.match(row.reason, /Git is unavailable on PATH/);
  assert.equal(row.log, null);
});

test('completion allows only listed isolated passes and wait prints the names', async t => {
  const fixture = repo(t);
  const task = fixture.start();
  fixture.put('src/owner.ts', 'export const owner = 2;\n');
  passingReceipt(fixture, task);
  const receipt = JSON.parse(readFileSync(join(fixture.cwd, '.harness/receipt.json')));
  const row = receipt.gates.find(gate => gate.id === 'frontend-tests');
  row.outcome = 'FLAKY';
  row.failures = [{ id: 'tests/test.ts > known', outcome: 'FLAKY', issue: 1833, rerun: { count: 1, exitCode: 0 } }];
  fixture.put('.harness/receipt.json', JSON.stringify({ ...receipt, finishedAt: new Date().toISOString(), durationMs: 100 }));
  assert.equal(fixture.cli('finish').status, 0);
  const summary = await waitForVerify(fixture.cwd, { graceMs: 0 });
  assert.match(summary.lines.join('\n'), /FLAKY tests\/test.ts > known.*issues\/1833/);
  delete row.failures[0].issue;
  fixture.put('.harness/receipt.json', JSON.stringify({ ...receipt, outcome: 'FLAKY' }));
  assert.equal(fixture.cli('finish').status, 1);
});

test('verify drains both lanes before isolation and resumes only for listed flakes', async t => {
  for (const listed of [true, false]) {
    const fixture = repo(t);
    fixture.put('.gitignore', '.harness/\n.task.json\nnode_modules/\n');
    symlinkSync(join(root, 'node_modules'), join(fixture.cwd, 'node_modules'), 'junction');
    fixture.put('scripts/harness-vitest-isolate.mjs', readFileSync(join(root, 'scripts/harness-vitest-isolate.mjs')));
    fixture.put('package.json', '{"type":"module"}');
    fixture.put('vitest.config.mjs', 'export default { test: { include: ["tests/*.test.js"], environment: "node" } };');
    fixture.put('tests/lane.test.js', `import { test, expect } from 'vitest';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
test('isolated', async () => {
  mkdirSync('.harness', {recursive:true});
  if (existsSync('.harness/failed')) { expect(existsSync('.harness/other-finished')).toBe(true); return; }
  await new Promise(resolve => { const timer=setInterval(() => { if(existsSync('.harness/other-started')) { clearInterval(timer); resolve(); } }, 10); });
  writeFileSync('.harness/failed','yes'); throw new Error('suite-only failure');
});`);
    fixture.put('scripts/known-flakes.json', JSON.stringify(listed ? { 'tests/lane.test.js > isolated': 1833 } : {}));
    const plan = () => [
      { id: 'frontend-tests', lane: 'frontend', tests: 'vitest', minutes: 1, command: ['node', 'node_modules/vitest/vitest.mjs', 'run'] },
      { id: 'other', lane: 'rust', minutes: 1, command: ['node', '-e', "const fs=require('fs'); fs.mkdirSync('.harness',{recursive:true}); fs.writeFileSync('.harness/other-started','yes'); const timer=setInterval(()=>{if(fs.existsSync('.harness/failed')){clearInterval(timer);fs.writeFileSync('.harness/other-finished','yes');}},10);"] },
      { id: 'after', lane: 'frontend', minutes: 1, command: ['node', '-e', "require('fs').writeFileSync('.harness/after','yes')"] },
    ];
    const receipt = await verify(fixture.cwd, { base: fixture.base, plan });
    assert.equal(receipt.outcome, listed ? 'PASS' : 'FLAKY');
    const row = receipt.gates.find(item => item.id === 'frontend-tests');
    assert.equal(row.outcome, 'FLAKY');
    assert.equal(row.failures[0].rerun.count, 1);
    assert.equal(existsSync(join(fixture.cwd, '.harness/after')), listed);
    const stored = JSON.parse(readFileSync(join(fixture.cwd, '.harness/receipt.json')));
    assert.deepEqual(stored.gates.find(item => item.id === row.id).failures, row.failures);
    assert.match((await waitForVerify(fixture.cwd, { graceMs: 0 })).lines.join('\n'), /FLAKY tests\/lane.test.js > isolated/);
    const cache = JSON.parse(readFileSync(join(fixture.cwd, '.harness/gate-cache.json')));
    assert.equal(cache.gates['frontend-tests'], undefined);
  }
});
function repo(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'buildmesh-harness-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  git('init', '-q');
  git('config', 'user.name', 'Harness test');
  git('config', 'user.email', 'harness@example.invalid');
  git('config', 'core.autocrlf', 'false');
  git('config', 'core.hooksPath', join(cwd, 'absent-hooks'));
  const put = (path, data) => { mkdirSync(join(cwd, path, '..'), { recursive: true }); writeFileSync(join(cwd, path), data); };
  const commit = () => { git('add', '.'); git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture'); return git('rev-parse', 'HEAD').trim(); };
  put('.gitignore', '.harness/\n.task.json\n');
  put('src/owner.ts', 'export const owner = 1;\n');
  const base = commit();
  const cli = (...args) => spawnSync(process.execPath, [script, ...args], { cwd, encoding: 'utf8', timeout: 30000 });
  const hook = payload => spawnSync(process.execPath, [script, 'hook'], { cwd, encoding: 'utf8', input: JSON.stringify(payload), timeout: 15000 });
  const start = () => {
    put('.task.json', JSON.stringify({ goal: 'Preserve owner state', criteria: ['Switches preserve state'], plannedEdits: ['src/owner.ts'], base }));
    const result = cli('start', '--spec', '.task.json');
    assert.equal(result.status, 0, result.stderr);
    return JSON.parse(result.stdout);
  };
  return { cwd, git, put, commit, base, cli, hook, start };
}
function passingReceipt(fixture, task) {
  const gatePlan = planGates(changedPaths(fixture.cwd, task.base));
  fixture.put('.harness/receipt.json', JSON.stringify({ root: fixture.cwd, taskId: task.id, base: task.base, tree: fingerprint(fixture.cwd, task.base), full: false, gatePlan, gates: gatePlan.map(gate => ({ id: gate.id, outcome: 'PASS' })), outcome: 'PASS' }));
  const tree = fingerprint(fixture.cwd, task.base);
  fixture.put('.harness/active-task.json', JSON.stringify({ ...task, evidence: ['Observed state after owner switch'], evidenceTree: tree, reviewTree: tree, review: { reviewer: 'independent fixture', verdict: 'APPROVE', findings: [], summary: 'Approved current change' } }));
}

test('scope includes committed, staged, unstaged, untracked, deletion and Unicode paths', t => {
  const fixture = repo(t);
  fixture.put('src/owner.ts', 'export const owner = 2;\n');
  fixture.commit();
  fixture.put('src/staged.ts', 'export {};\n');
  fixture.git('add', '.');
  fixture.put('src/staged.ts', 'export const edited = true;\n');
  fixture.put('src/space λ.ts', 'export {};\n');
  fixture.git('rm', 'src/owner.ts');
  assert.deepEqual(changedPaths(fixture.cwd, fixture.base), ['src/owner.ts', 'src/space λ.ts', 'src/staged.ts']);
  assert.throws(() => changedPaths(fixture.cwd, 'missing-base'));
});
test('scope preserves generated-binding Rust gates and conservative unknowns', () => {
  const ids = paths => planGates(paths).map(row => row.id);
  assert.ok(ids(['src/types/generated/Example.ts']).includes('rust-tests'));
  assert.ok(ids(['unknown/input']).includes('browser-smoke'));
  assert.ok(ids(['unknown/input']).includes('rust-tests'));
  assert.ok(!ids(['docs/page.md', '.claude/hooks/example.mjs']).includes('rust-tests'));
  assert.ok(ids(['.claude/hooks/example.mjs']).includes('agent-tests'));
  assert.ok(ids(['src/owner.ts']).includes('browser-smoke'));
  assert.ok(!ids(['src/owner.ts']).includes('rust-tests'));
  assert.ok(ids(['docs/page.md']).includes('lint'));
});
test('Rust tests run as the CI shards in concurrent processes and still count executed tests', () => {
  const rust = planGates(['src-tauri/src/lib.rs']).find(row => row.id === 'rust-tests');
  assert.deepEqual(rust.command, ['node', 'scripts/rust-test-shards.mjs']);
  assert.equal(rust.tests, 'rust');
  // The runner prints each passing process's summary lines; they still sum.
  assert.equal(executedTests('rust', '[db] passed in 27s\ntest result: ok. 300 passed; 0 failed;\n[agent] passed in 47s\ntest result: ok. 764 passed; 0 failed;'), 1064);
});
test('rustfmt baseline debt fails only touched files, like the Clippy gate', () => {
  const output = [
    'Diff in \\\\?\\F:\\repo\\src-tauri\\src\\agent\\background.rs:104:',
    ' fn untouched() {}',
    'Diff in /home/runner/work/repo/src-tauri/src/db/mod.rs:12:',
    'Diff in F:\\repo\\src-tauri\\src\\db\\mod.rs:40:',
  ].join('\n');
  assert.deepEqual(touchedFormatDiffs(output, ['src-tauri/src/lib.rs']), { touched: [], total: 3 });
  assert.deepEqual(touchedFormatDiffs(output, ['src-tauri/src/db/mod.rs', 'docs/page.md']), { touched: ['src-tauri/src/db/mod.rs'], total: 3 });
  // A path that merely ends with a touched file name is a different file.
  assert.deepEqual(touchedFormatDiffs('Diff in /r/src-tauri/src/xmod.rs:1:', ['src-tauri/src/mod.rs']).touched, []);
  // No recognisable diff means rustfmt failed some other way: never a pass.
  assert.equal(touchedFormatDiffs('error: expected item, found `}`', ['src-tauri/src/lib.rs']).total, 0);
  const format = planGates(['src-tauri/src/lib.rs']).find(row => row.id === 'rust-format');
  assert.equal(format.touchedFormat, true);
});
test('opposite staged and working edits cannot disappear or reuse prior evidence', t => {
  const fixture = repo(t);
  const original = fingerprint(fixture.cwd, fixture.base);
  fixture.put('src/owner.ts', 'throw new Error("bad staged code");\n');
  fixture.git('add', 'src/owner.ts');
  fixture.put('src/owner.ts', 'export const owner = 1;\n');
  assert.deepEqual(changedPaths(fixture.cwd, fixture.base), ['src/owner.ts']);
  assert.notEqual(fingerprint(fixture.cwd, fixture.base), original);
  const result = fixture.cli('gate', 'staging');
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Staged content differs/);
});
test('amending a commit message invalidates receipts consumed by the docs gate', t => {
  const fixture = repo(t);
  const before = fingerprint(fixture.cwd, fixture.base);
  fixture.git('-c', 'commit.gpgsign=false', 'commit', '--amend', '-qm', 'Different documentation decision');
  assert.notEqual(fingerprint(fixture.cwd, fixture.base), before);
  const docs = planGates(['docs/page.md']).find(row => row.id === 'docs');
  assert.deepEqual(docs.command.slice(-2), ['--base', '$BASE']);
});
test('staged deletion with an untracked replacement cannot certify a different commit', t => {
  const fixture = repo(t);
  fixture.git('rm', 'src/owner.ts');
  fixture.put('src/owner.ts', 'export const owner = 1;\n');
  assert.deepEqual(changedPaths(fixture.cwd, fixture.base), ['src/owner.ts']);
  const result = fixture.cli('gate', 'staging');
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Staged content differs/);
});
test('a rejected independent review cannot complete a green task', t => {
  const fixture = repo(t);
  const task = fixture.start();
  passingReceipt(fixture, task);
  fixture.put('.task.json', JSON.stringify({ review: { reviewer: 'adversarial', verdict: 'REQUEST_CHANGES', findings: ['State leaks across meshes'], summary: 'Rejected' } }));
  assert.equal(fixture.cli('update', '--spec', '.task.json').status, 0);
  assert.equal(fixture.cli('finish').status, 2);
  assert.match(completion(fixture.cwd).reason, /APPROVE/);
});
test('package scope narrows harness commands but never dependencies or product scripts', t => {
  const fixture = repo(t);
  const initial = { scripts: { build: 'tsc', 'test:agent': 'node --test original.test.mjs' }, dependencies: { react: '19' } };
  fixture.put('package.json', JSON.stringify(initial));
  const base = fixture.commit();
  fixture.put('package.json', JSON.stringify({ ...initial, scripts: { ...initial.scripts, verify: 'node scripts/harness.mjs verify' } }));
  assert.deepEqual(scopePaths(fixture.cwd, base, ['package.json']), []);
  fixture.put('package.json', JSON.stringify({ ...initial, dependencies: { react: '20' } }));
  assert.deepEqual(scopePaths(fixture.cwd, base, ['package.json']), ['package.json']);
  fixture.put('package.json', JSON.stringify({ ...initial, scripts: { ...initial.scripts, build: 'echo skipped' } }));
  assert.deepEqual(scopePaths(fixture.cwd, base, ['package.json']), ['package.json']);
});
test('task start cannot replace an unfinished task or update its immutable base', t => {
  const fixture = repo(t);
  fixture.start();
  assert.equal(fixture.cli('start', '--spec', '.task.json').status, 2);
  fixture.put('.task.json', JSON.stringify({ base: 'HEAD' }));
  assert.equal(fixture.cli('update', '--spec', '.task.json').status, 2);
  assert.equal(fixture.cli('finish').status, 2);
  assert.match(fixture.cli('status').stdout, /Preserve owner state/);
});
test('progress transitions persist phase timing and continuity', t => {
  const fixture = repo(t);
  const task = fixture.start();
  fixture.put('.harness/active-task.json', JSON.stringify({ ...task, phaseStartedAt: '2020-01-01T00:00:00Z' }));
  fixture.put('.task.json', JSON.stringify({ phase: 'implement', nextAction: 'Exercise the owning transition' }));
  const result = fixture.cli('update', '--spec', '.task.json');
  assert.equal(result.status, 0, result.stderr);
  const updated = JSON.parse(result.stdout);
  assert.equal(updated.phase, 'implement');
  assert.ok(updated.phaseDurationsMs.understand > 0);
  assert.equal(updated.nextAction, 'Exercise the owning transition');
});
test('receipts reject missing acceptance, source edits and partial gates', t => {
  const fixture = repo(t);
  const task = fixture.start();
  passingReceipt(fixture, task);
  assert.equal(completion(fixture.cwd).outcome, 'PASS');
  const receipt = JSON.parse(readFileSync(join(fixture.cwd, '.harness/receipt.json')));
  receipt.gates.pop();
  fixture.put('.harness/receipt.json', JSON.stringify(receipt));
  assert.match(completion(fixture.cwd).reason, /every required gate/);
  passingReceipt(fixture, task);
  fixture.put('src/owner.ts', 'export const owner = 3;\n');
  assert.match(completion(fixture.cwd).reason, /stale/);
  fixture.put('src/owner.ts', 'export const owner = 1;\n');
  fixture.put('.harness/active-task.json', JSON.stringify(task));
  assert.match(completion(fixture.cwd).reason, /acceptance criterion/);
});
test('fingerprint ignores local logs but sees tests, lockfile, deletions and untracked data', t => {
  const fixture = repo(t);
  const original = fingerprint(fixture.cwd, fixture.base);
  fixture.put('.harness/logs/temp.log', 'runtime only');
  assert.equal(fingerprint(fixture.cwd, fixture.base), original);
  fixture.put('package-lock.json', '{}');
  assert.notEqual(fingerprint(fixture.cwd, fixture.base), original);
  assert.notEqual(fingerprint(fixture.cwd, 'different-base'), original);
});
test('two real worktrees have isolated task state and cannot reuse receipts', t => {
  const fixture = repo(t);
  const task = fixture.start();
  passingReceipt(fixture, task);
  const other = join(fixture.cwd, '..', `${task.id}-worktree`);
  fixture.git('worktree', 'add', '--detach', other, fixture.base);
  t.after(() => rmSync(other, { recursive: true, force: true }));
  assert.equal(completion(other).outcome, 'BLOCKED');
  mkdirSync(join(other, '.harness'));
  writeFileSync(join(other, '.harness/active-task.json'), JSON.stringify(task));
  assert.throws(() => completion(other), /different worktree/);
});
test('checkpoint references belong to the goal and restore use is verified', t => {
  const fixture = repo(t);
  const task = fixture.start();
  const result = fixture.cli('checkpoint');
  assert.equal(result.status, 0, result.stderr);
  const checkpoint = JSON.parse(result.stdout);
  assert.ok(checkpoint.ref.includes(task.id));
  assert.equal(fixture.git('rev-parse', checkpoint.ref).trim(), fixture.base);
  fixture.put('src/owner.ts', 'export const owner = 4;\n');
  fixture.commit();
  assert.equal(fixture.cli('record-rollback', '--ref', checkpoint.ref).status, 2);
  // Recovery is an operator action in this disposable fixture, never implicit.
  fixture.git('reset', '--hard', checkpoint.ref);
  assert.equal(fixture.cli('record-rollback', '--ref', checkpoint.ref).status, 0);
  assert.equal(readFileSync(join(fixture.cwd, 'src/owner.ts'), 'utf8'), 'export const owner = 1;\n');
  assert.match(readFileSync(join(fixture.cwd, '.harness/events.jsonl'), 'utf8'), /"type":"rollback"/);
});
test('real gate execution separates assertion failure, unavailable tool and timeout', async t => {
  const fixture = repo(t);
  const gate = (id, command, minutes = 1) => runGate(fixture.cwd, { id, command, minutes }, fixture.base);
  assert.equal((await gate('pass', ['node', '-e', 'process.exit(0)'])).outcome, 'PASS');
  assert.equal((await gate('fail', ['node', '-e', 'process.exit(3)'])).outcome, 'FAIL');
  assert.equal((await gate('missing', ['buildmesh-tool-does-not-exist'])).outcome, 'BLOCKED');
  assert.equal((await gate('timeout', ['node', '-e', 'setInterval(() => {}, 1000)'], 0.01)).outcome, 'TIMEOUT');
});
test('the format gate passes baseline rustfmt debt and fails touched or unexplained failures', async t => {
  const fixture = repo(t);
  const formatter = output => ['node', '-e', `console.log(${JSON.stringify(output)}); process.exit(1)`];
  const gate = (output, paths) => runGate(fixture.cwd, { id: 'rust-format', command: formatter(output), minutes: 1, touchedFormat: true }, fixture.base, paths);
  const debt = await gate('Diff in /r/src-tauri/src/old.rs:3:', ['src-tauri/src/new.rs']);
  assert.equal(debt.outcome, 'PASS');
  assert.equal(debt.formatDiffCount, 1);
  const touched = await gate('Diff in /r/src-tauri/src/new.rs:3:', ['src-tauri/src/new.rs']);
  assert.equal(touched.outcome, 'FAIL');
  assert.match(touched.reason, /src-tauri\/src\/new\.rs/);
  // Bare `rustfmt <file>` also rewrites child modules, so the gate must not recommend it.
  assert.match(touched.reason, /node scripts\/rustfmt-touched\.mjs "src-tauri\/src\/new\.rs"/);
  assert.doesNotMatch(touched.reason, /rustfmt --edition 2021/);
  // A worktree path can contain spaces; the suggested command must stay copy-pasteable.
  const spaced = await gate('Diff in /r/src-tauri/src/new module.rs:3:', ['src-tauri/src/new module.rs']);
  assert.match(spaced.reason, /node scripts\/rustfmt-touched\.mjs "src-tauri\/src\/new module\.rs"/);
  assert.equal((await gate('error: unexpected token', ['src-tauri/src/new.rs'])).outcome, 'FAIL');
});
test('behavior gates reject zero tests and preserve executed counts', async t => {
  const fixture = repo(t);
  const zero = await runGate(fixture.cwd, { id: 'zero', command: ['node', '-e', 'console.log("# pass 0")'], minutes: 1, tests: 'node' }, fixture.base);
  assert.equal(zero.outcome, 'FAIL');
  assert.equal(zero.count, 0);
  fixture.put('behavior.test.mjs', 'import { test } from "node:test"; import assert from "node:assert/strict"; test("literal output", () => assert.equal(2 + 2, 4));\n');
  const executed = await runGate(fixture.cwd, { id: 'test', command: ['node', '--test', 'behavior.test.mjs'], minutes: 1, tests: 'node' }, fixture.base);
  assert.equal(executed.outcome, 'PASS', executed.log ? readFileSync(executed.log, 'utf8') : executed.reason);
  assert.equal(executed.count, 1);
  assert.equal(executedTests('rust', 'test result: ok. 2 passed;\ntest result: ok. 3 passed;'), 5);
});
test('build gates use production while test gates retain the test environment', async t => {
  const fixture = repo(t);
  for (const [id, expected] of [['frontend-build', 'production'], ['mobile-build', 'production'], ['frontend-tests', 'test']]) {
    const result = await runGate(fixture.cwd, {
      id, minutes: 1,
      command: ['node', '-e', `if (process.env.NODE_ENV !== '${expected}') throw new Error(process.env.NODE_ENV);`],
    }, fixture.base);
    assert.equal(result.outcome, 'PASS', result.log ? readFileSync(result.log, 'utf8') : result.reason);
  }
});
test('missing dependencies preflight BLOCKED without launching tests', async t => {
  const fixture = repo(t);
  const result = await runGate(fixture.cwd, { id: 'frontend', command: ['npm', 'run', 'test'], minutes: 1, tests: 'vitest' }, fixture.base);
  assert.equal(result.outcome, 'BLOCKED');
  assert.equal(result.log, null);
  assert.match(result.reason, /node_modules/);
});
test('canonical verification rejects a source mutation even when every child check passes', t => {
  const fixture = repo(t);
  // Stand-in executables keep this a process/orchestration regression, without
  // coupling it to a full Tauri or frontend installation.
  for (const path of ['scripts/harness.mjs', 'scripts/harness-plan.mjs', 'scripts/harness-lanes.mjs', 'scripts/harness-test-failures.mjs', 'scripts/ci/run-guarded.mjs', 'scripts/ci/changed-scope.mjs']) fixture.put(path, readFileSync(join(root, path)));
  for (const path of ['scripts/check-agent-diff.mjs', 'scripts/check-docs.mjs', 'scripts/check-readme-drift.mjs', 'scripts/check-process-spawn-discipline.mjs']) fixture.put(path, 'process.exit(0);\n');
  fixture.put('.gitignore', '.harness/\n.task.json\nnode_modules/\n');
  fixture.put('node_modules/eslint/package.json', '{"version":"fixture"}');
  fixture.put('fixture.test.mjs', 'import { test } from "node:test"; import assert from "node:assert/strict"; test("child runs", () => assert.equal(process.env.NODE_ENV, "test"));\n');
  fixture.put('npm-cli.mjs', `import { spawnSync } from 'node:child_process'; import { writeFileSync } from 'node:fs';
const script = process.argv[3];
if (script.endsWith('tests') || script.startsWith('test:')) {
  const child = spawnSync(process.execPath, ['--test', '--test-reporter=tap', 'fixture.test.mjs'], { stdio: 'inherit', env: process.env });
  process.exitCode = child.status;
} else if (script === 'lint') writeFileSync('src/owner.ts', 'export const owner = 999;\\n');
`);
  const base = fixture.commit();
  const result = spawnSync(process.execPath, [script, 'verify', '--base', base], { cwd: fixture.cwd, encoding: 'utf8', timeout: 30000, env: { ...process.env, npm_execpath: join(fixture.cwd, 'npm-cli.mjs') } });
  assert.equal(result.status, 1, result.stderr);
  const receipt = JSON.parse(readFileSync(join(fixture.cwd, '.harness/receipt.json')));
  assert.ok(receipt.gates.every(gate => gate.outcome === 'PASS'), result.stdout);
  assert.equal(receipt.outcome, 'FAIL');
  assert.match(receipt.reason, /Source changed during verification/);
});
test('hook stdin restores context, guards state writes and rejects stale completion', t => {
  const fixture = repo(t);
  fixture.start();
  const context = fixture.hook({ hook_event_name: 'SessionStart' });
  assert.equal(context.status, 0, context.stderr);
  assert.match(JSON.parse(context.stdout).hookSpecificOutput.additionalContext, /Preserve owner state/);
  const denied = fixture.hook({ hook_event_name: 'PreToolUse', tool_input: { file_path: '.harness/receipt.json' } });
  assert.equal(JSON.parse(denied.stdout).hookSpecificOutput.permissionDecision, 'deny');
  const stop = fixture.hook({ hook_event_name: 'Stop' });
  assert.equal(JSON.parse(stop.stdout).decision, 'block');
  assert.equal(fixture.hook({ hook_event_name: 'Stop', stop_hook_active: true }).stdout, '');
  assert.equal(JSON.parse(readFileSync(join(fixture.cwd, '.harness/active-task.json'))).phase, 'understand');
});
test('a failed gate keeps blocking Stop, names the way out, and releases once update records a blocked handoff', t => {
  const fixture = repo(t);
  const task = fixture.start();
  const gatePlan = planGates(changedPaths(fixture.cwd, task.base));
  fixture.put('.harness/receipt.json', JSON.stringify({ root: fixture.cwd, taskId: task.id, base: task.base, tree: fingerprint(fixture.cwd, task.base), full: false, gatePlan, gates: [], outcome: 'FAIL', reason: 'Command failed. See the gate log; failure attribution is unverified.' }));
  // A written report alone must not release a FAIL; only recorded blockers do.
  for (const payload of [{ hook_event_name: 'Stop' }, { hook_event_name: 'Stop', stop_hook_active: true }]) {
    const stop = JSON.parse(fixture.hook(payload).stdout);
    assert.equal(stop.decision, 'block');
    assert.match(stop.reason, /npm run harness -- update --spec/);
    assert.match(stop.reason, /"phase":\s*"blocked"/);
    assert.match(stop.reason, /blockers/);
  }
  fixture.put('.task.json', JSON.stringify({ phase: 'blocked', blockers: ['rust-tests fails on a pre-existing host-dependent test'] }));
  const update = fixture.cli('update', '--spec', '.task.json');
  assert.equal(update.status, 0, update.stderr);
  // The recorded handoff is released on the first stop of every later turn, not
  // only on a recursive one, and it still never marks the task complete.
  for (const payload of [{ hook_event_name: 'Stop' }, { hook_event_name: 'Stop', stop_hook_active: true }]) {
    assert.equal(fixture.hook(payload).stdout, '', JSON.stringify(payload));
  }
  const recorded = JSON.parse(readFileSync(join(fixture.cwd, '.harness/active-task.json')));
  assert.equal(recorded.phase, 'blocked');
  assert.equal(fixture.cli('finish').status, 1, 'a blocked handoff still fails finish on the failing receipt');
  // Clearing the blockers restores the guard instead of leaving a silent pass.
  fixture.put('.task.json', JSON.stringify({ phase: 'verify', blockers: [] }));
  assert.equal(fixture.cli('update', '--spec', '.task.json').status, 0);
  assert.equal(JSON.parse(fixture.hook({ hook_event_name: 'Stop' }).stdout).decision, 'block');
});
test('the operation lock blocks a live owner, and a lock left by a dead process is reclaimed', t => {
  const fixture = repo(t);
  fixture.start();
  fixture.put('.harness/lock', JSON.stringify({ pid: process.pid }));
  const held = fixture.cli('finish');
  assert.equal(held.status, 2);
  assert.match(held.stderr, /another harness operation/);
  // An interrupted process leaves the lock behind; the next operation takes it.
  const dead = spawnSync(process.execPath, ['-e', ''], { encoding: 'utf8' }).pid;
  fixture.put('.harness/lock', JSON.stringify({ pid: dead, startedAt: '2026-01-01T00:00:00.000Z' }));
  const reclaimed = fixture.cli('finish');
  assert.equal(reclaimed.status, 2);
  assert.doesNotMatch(reclaimed.stderr, /another harness operation/);
  assert.equal(existsSync(join(fixture.cwd, '.harness/lock')), false, 'the reclaiming run must leave no lock behind');
  assert.match(readFileSync(join(fixture.cwd, '.harness/events.jsonl'), 'utf8'), new RegExp(`"type":"lock-reclaimed","lock":"lock","pid":${dead}`));
  // A lock that cannot be attributed to a dead owner stays put for the operator.
  fixture.put('.harness/lock', 'not json');
  assert.match(fixture.cli('finish').stderr, /another harness operation/);
});
test('read-only sessions and unrelated hook events do not require a task', t => {
  const fixture = repo(t);
  assert.equal(fixture.hook({ hook_event_name: 'Stop' }).stdout, '');
  assert.equal(fixture.hook({ hook_event_name: 'OtherEvent' }).stdout, '');
});
test('evaluation corpus uses present tests and names remaining runtime gaps', () => {
  const corpus = JSON.parse(readFileSync(join(root, 'scripts/harness-corpus.json')));
  assert.equal(new Set(corpus.map(row => row.id)).size, 7);
  for (const row of corpus) {
    assert.ok(row.promise && row.boundary && row.remaining);
    for (const path of row.gate.command.filter(arg => arg.startsWith('tests/'))) assert.ok(readFileSync(join(root, path)).length);
  }
});

// --- Parallel lanes, machine-wide heavy-gate slots and `harness wait` (#2104) ---
const settle = () => new Promise(done => setImmediate(done));
// Fake gates that run until the test finishes them, so overlap and ordering are
// observed directly instead of inferred from timings.
function controlledGates() {
  const log = [];
  const open = new Map();
  const execute = gate => new Promise(resolve => {
    log.push(`start ${gate.id}`);
    open.set(gate.id, outcome => { log.push(`end ${gate.id}`); resolve({ id: gate.id, outcome }); });
  });
  const started = async id => { for (let i = 0; i < 1000 && !open.has(id); i += 1) await settle(); assert.ok(open.has(id), `${id} never started: ${log}`); };
  const finish = async (id, outcome = 'PASS') => { await started(id); const resolve = open.get(id); open.delete(id); resolve(outcome); };
  const idle = async () => { for (let i = 0; i < 20; i += 1) await settle(); };
  return { log, execute, started, finish, idle };
}
const lanePlan = () => [
  { id: 'infra' },
  { id: 'build', lane: 'frontend' }, { id: 'ui-tests', lane: 'frontend' },
  { id: 'fmt', lane: 'rust' }, { id: 'compile', lane: 'rust', after: ['build'] }, { id: 'rust-tests', lane: 'rust' },
];

test('the product plan splits into frontend and Rust lanes that wait for the build before compiling', () => {
  const both = planGates(['unknown/input']);
  const byId = id => both.find(row => row.id === id);
  assert.deepEqual(both.filter(row => !row.lane).map(row => row.id).slice(0, 3), ['whitespace', 'staged-content', 'agent-rules']);
  assert.deepEqual(both.filter(row => row.lane === 'frontend').map(row => row.id), ['frontend-build', 'bundle', 'frontend-tests', 'browser-smoke']);
  assert.deepEqual(both.filter(row => row.lane === 'rust').map(row => row.id), ['rust-format', 'rust-clippy', 'rust-tests', 'binding-drift']);
  // vite build empties dist/mobile, which the Rust crate embeds.
  assert.deepEqual(byId('rust-clippy').after, ['frontend-build']);
  assert.deepEqual(byId('rust-tests').after, ['frontend-build']);
  assert.equal(byId('rust-format').after, undefined);
  assert.equal(byId('browser-smoke').after, undefined);
  assert.deepEqual(both.filter(row => row.heavy).map(row => row.id), ['frontend-tests', 'rust-tests']);
  // Without a frontend build the mobile build opens the Rust lane, so nothing needs to wait.
  const rustOnly = planGates(['src-tauri/src/lib.rs']);
  assert.deepEqual(rustOnly.filter(row => row.lane === 'rust').map(row => row.id).slice(0, 2), ['mobile-build', 'rust-format']);
  assert.ok(rustOnly.filter(row => row.after).every(row => row.after.length === 0));
  assert.ok(planGates(['docs/page.md']).every(row => !row.lane));
});
test('lanes run concurrently, and a gate starts only after the gates it names have passed', async () => {
  const fake = controlledGates();
  const run = runPlan(lanePlan(), { execute: fake.execute });
  await fake.finish('infra');
  // Both lanes are in flight at once.
  await fake.started('build');
  await fake.started('fmt');
  await fake.finish('fmt');
  await fake.idle();
  assert.ok(!fake.log.includes('start compile'), 'compile must wait for the build');
  await fake.finish('build');
  await fake.finish('compile');
  await fake.finish('ui-tests');
  await fake.finish('rust-tests');
  await run;
  assert.ok(fake.log.indexOf('end build') < fake.log.indexOf('start compile'));
});
test('a failed gate stops further gates from starting in every lane but lets running ones finish', async () => {
  const fake = controlledGates();
  const rows = [];
  const run = runPlan(lanePlan(), { execute: fake.execute, onResult: row => rows.push(`${row.id}:${row.outcome}`) });
  await fake.finish('infra');
  await fake.started('build');
  await fake.started('fmt');
  await fake.finish('build', 'FAIL');
  await fake.finish('fmt');
  await fake.idle();
  // Nothing after the failure started, including the gate that depended on the build.
  assert.deepEqual(fake.log.filter(entry => entry.startsWith('start')).sort(), ['start build', 'start fmt', 'start infra']);
  await run;
  assert.deepEqual(rows.sort(), ['build:FAIL', 'fmt:PASS', 'infra:PASS']);
});
test('a failing infrastructure gate keeps the lanes from starting', async () => {
  const fake = controlledGates();
  const run = runPlan(lanePlan(), { execute: fake.execute });
  await fake.finish('infra', 'TIMEOUT');
  await fake.idle();
  assert.deepEqual(fake.log, ['start infra', 'end infra']);
  await run;
});
test('a queued gate that finds the plan already failed leaves no row and starts nothing after it', async () => {
  const fake = controlledGates();
  const rows = [];
  let leaveQueue;
  const queue = new Promise(done => { leaveQueue = done; });
  const run = runPlan([{ id: 'a', lane: 'x' }, { id: 'b', lane: 'y' }, { id: 'c', lane: 'y' }, { id: 'd', lane: 'z', after: ['b'] }], {
    // b models a heavy gate waiting for a machine-wide slot.
    execute: async (gate, isStopped) => gate.id === 'b' ? (await queue, isStopped() ? null : { id: 'b', outcome: 'PASS' }) : fake.execute(gate),
    onResult: row => rows.push(`${row.id}:${row.outcome}`),
  });
  await fake.finish('a', 'FAIL');
  await fake.idle();
  leaveQueue();
  await run;
  assert.deepEqual(rows, ['a:FAIL']);
  assert.deepEqual(fake.log, ['start a', 'end a']);
});
test('a gate whose dependency did not pass is not run', async () => {
  const fake = controlledGates();
  const run = runPlan([{ id: 'a', lane: 'x' }, { id: 'b', lane: 'y', after: ['a'] }], { execute: fake.execute });
  await fake.finish('a', 'BLOCKED');
  await fake.idle();
  assert.deepEqual(fake.log, ['start a', 'end a']);
  await run;
});

test('overlapping heavy suites split the cores, and a lone one keeps the whole machine', () => {
  const both = planGates(['unknown/input']);
  const byId = id => both.find(row => row.id === id);
  assert.deepEqual(heavyGateEnv(byId('frontend-tests'), both, 24), { VITEST_MAX_WORKERS: '12' });
  assert.deepEqual(heavyGateEnv(byId('rust-tests'), both, 24), { RUST_TEST_THREADS: '12', BUILDMESH_RUST_TEST_JOBS: '2' });
  assert.deepEqual(heavyGateEnv(byId('frontend-tests'), both, 1), { VITEST_MAX_WORKERS: '2' });
  assert.deepEqual(heavyGateEnv(byId('lint'), both, 24), {});
  for (const paths of [['src/owner.ts'], ['src-tauri/src/lib.rs']]) {
    const lone = planGates(paths);
    for (const row of lone.filter(item => item.heavy)) assert.deepEqual(heavyGateEnv(row, lone, 24), {}, row.id);
  }
});

const slotsDir = t => {
  const dir = mkdtempSync(join(tmpdir(), 'buildmesh-slots-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
};
test('heavy-gate slots admit up to the limit, queue the rest with a notice, and free on release', async t => {
  const dir = slotsDir(t);
  const options = { dir, limit: 2, pollMs: 10, root: 'wt' };
  const first = await acquireSlot({ ...options, gate: 'a' });
  const second = await acquireSlot({ ...options, gate: 'b' });
  const queued = [];
  const third = acquireSlot({ ...options, gate: 'c', onQueued: holders => queued.push(holders.map(held => held.gate).sort()) });
  let admitted = false;
  third.then(() => { admitted = true; });
  for (let i = 0; i < 400 && !queued.length; i += 1) await new Promise(done => setTimeout(done, 5));
  assert.deepEqual(queued[0], ['a', 'b']);
  await new Promise(done => setTimeout(done, 60));
  assert.equal(admitted, false, 'a third gate must not run while two hold slots');
  first.release();
  const slot = await third;
  assert.ok(slot.queuedMs > 0);
  second.release();
  slot.release();
  assert.equal(slotHolders(dir, 2).length, 0);
});
test('a slot left by a dead process is reclaimed, and a stale release never frees a newer owner', async t => {
  const dir = slotsDir(t);
  const dead = spawnSync(process.execPath, ['-e', ''], { encoding: 'utf8' }).pid;
  writeFileSync(join(dir, 'slot-0.json'), JSON.stringify({ pid: dead, token: 'old', gate: 'crashed' }));
  const mine = await acquireSlot({ dir, limit: 1, pollMs: 10, gate: 'a', root: 'wt' });
  assert.equal(mine.queuedMs, 0, 'a dead owner must not queue anyone');
  // Someone else takes over this slot (e.g. after we were reclaimed while paused).
  writeFileSync(join(dir, 'slot-0.json'), JSON.stringify({ pid: process.pid, token: 'newer', gate: 'b' }));
  mine.release();
  assert.equal(JSON.parse(readFileSync(join(dir, 'slot-0.json'), 'utf8')).token, 'newer');
});
test('the heavy-gate limit defaults to two and rejects nonsense values', () => {
  assert.equal(heavyGateLimit({}), 2);
  assert.equal(heavyGateLimit({ BUILDMESH_HEAVY_GATE_LIMIT: '3' }), 3);
  for (const bad of ['0', '-1', '1.5', 'many', '']) assert.equal(heavyGateLimit({ BUILDMESH_HEAVY_GATE_LIMIT: bad }), 2, bad);
});
test('separate processes never hold more heavy-gate slots than the limit', async t => {
  const dir = slotsDir(t);
  const marks = mkdtempSync(join(tmpdir(), 'buildmesh-marks-'));
  t.after(() => rmSync(marks, { recursive: true, force: true }));
  const lanes = pathToFileURL(join(root, 'scripts/harness-lanes.mjs')).href;
  // Each child is a stand-in for a worktree's heavy gate: it counts the markers
  // present while it holds a slot, so a leak above the limit shows as a count.
  const child = [
    "import { appendFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';",
    "import { join } from 'node:path';",
    `import { acquireSlot } from ${JSON.stringify(lanes)};`,
    "const slot = await acquireSlot({ dir: process.argv[1], limit: 2, pollMs: 15, gate: 'fake', root: 'wt' });",
    "const mark = join(process.argv[2], String(process.pid));",
    "writeFileSync(mark, '');",
    "appendFileSync(join(process.argv[2], 'counts.log'), readdirSync(process.argv[2]).filter(name => name !== 'counts.log').length + '\\n');",
    "await new Promise(done => setTimeout(done, 300));",
    "rmSync(mark);",
    "slot.release();",
  ].join('\n');
  const runs = Array.from({ length: 5 }, () => new Promise((resolve, reject) => {
    const proc = spawn(process.execPath, ['--input-type=module', '-e', child, dir, marks], { stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    proc.stderr.on('data', chunk => { stderr += chunk; });
    proc.on('close', code => code === 0 ? resolve() : reject(new Error(stderr)));
  }));
  await Promise.all(runs);
  const counts = readFileSync(join(marks, 'counts.log'), 'utf8').trim().split('\n').map(Number);
  assert.equal(counts.length, 5);
  assert.ok(Math.max(...counts) <= 2, `concurrent heavy gates: ${counts}`);
});

function writeReceipt(fixture, overrides = {}) {
  fixture.put('.harness/receipt.json', JSON.stringify({
    startedAt: '2026-01-01T00:00:00.000Z', finishedAt: '2026-01-01T00:05:00.000Z', durationMs: 300000, outcome: 'PASS',
    gatePlan: [{ id: 'a' }, { id: 'b' }, { id: 'c' }], gates: [{ id: 'a', outcome: 'PASS' }, { id: 'b', outcome: 'PASS' }, { id: 'c', outcome: 'PASS' }], ...overrides,
  }));
}
const spawnWait = (fixture, ...args) => new Promise(resolve => {
  const proc = spawn(process.execPath, [script, 'wait', ...args], { cwd: fixture.cwd, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '';
  proc.stdout.on('data', chunk => { stdout += chunk; });
  proc.on('close', status => resolve({ status, stdout }));
});
test('wait reports a finished PASS receipt and exits 0', t => {
  const fixture = repo(t);
  writeReceipt(fixture);
  const result = fixture.cli('wait');
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^PASS: 3\/3 gates in 300s\. Receipt: \.harness\/receipt\.json/);
});
test('wait on FAIL names the failed gates with their logs in at most 30 lines and exits 1', async t => {
  const fixture = repo(t);
  const gates = Array.from({ length: 20 }, (_, index) => ({ id: `gate-${index}`, outcome: 'FAIL', reason: `broke\n${'x'.repeat(2000)}`, log: `.harness/logs/${index}.log` }));
  writeReceipt(fixture, { outcome: 'FAIL', gates, gatePlan: gates.map(({ id }) => ({ id })) });
  const summary = await waitForVerify(fixture.cwd, { graceMs: 0 });
  assert.equal(summary.outcome, 'FAIL');
  assert.ok(summary.lines.length <= 30, `${summary.lines.length} lines`);
  assert.ok(summary.lines.every(line => line.length < 400));
  assert.match(summary.lines.join('\n'), /FAIL gate-0: broke/);
  assert.match(summary.lines.join('\n'), /\.harness\/logs\/0\.log/);
  assert.match(summary.lines.at(-1), /\+12 more/);
  assert.equal(fixture.cli('wait').status, 1);
});
test('wait blocks while a verify holds the lock, then reports the receipt that verify wrote', async t => {
  const fixture = repo(t);
  writeReceipt(fixture, { startedAt: '2020-01-01T00:00:00.000Z', outcome: 'PASS' });
  fixture.put('.harness/lock', JSON.stringify({ pid: process.pid, action: 'verify', startedAt: '2026-06-01T00:00:00.000Z' }));
  const waiting = spawnWait(fixture, '--max-seconds', '30');
  await new Promise(done => setTimeout(done, 1500));
  // The old green receipt must not be reported while the run is still going.
  writeReceipt(fixture, { startedAt: '2026-06-01T00:00:01.000Z', outcome: 'FAIL', gates: [{ id: 'a', outcome: 'FAIL', reason: 'broke', log: '.harness/logs/a.log' }] });
  unlinkSync(join(fixture.cwd, '.harness/lock'));
  const result = await waiting;
  assert.equal(result.status, 1, result.stdout);
  assert.match(result.stdout, /FAIL a: broke/);
});
test('wait times out with exit 124 and the gates finished so far while the run is still going', t => {
  const fixture = repo(t);
  writeReceipt(fixture, { finishedAt: undefined, outcome: 'BLOCKED', gates: [{ id: 'a', outcome: 'PASS' }] });
  fixture.put('.harness/lock', JSON.stringify({ pid: process.pid, action: 'verify', startedAt: '2020-01-01T00:00:00.000Z' }));
  const result = fixture.cli('wait', '--max-seconds', '1');
  assert.equal(result.status, 124, result.stderr);
  assert.match(result.stdout, /^TIMEOUT: still running after \d+s.*a:PASS.*wait again/);
});
test('wait never reports a stale or missing result as success', async t => {
  const fixture = repo(t);
  assert.equal((await waitForVerify(fixture.cwd, { graceMs: 0 })).outcome, 'BLOCKED');
  // A verify that was killed leaves an unfinished receipt and a dead owner's lock.
  writeReceipt(fixture, { finishedAt: undefined, outcome: 'BLOCKED' });
  const dead = spawnSync(process.execPath, ['-e', ''], { encoding: 'utf8' }).pid;
  fixture.put('.harness/lock', JSON.stringify({ pid: dead, action: 'verify', startedAt: '2026-01-01T00:00:00.000Z' }));
  const interrupted = await waitForVerify(fixture.cwd, { graceMs: 0 });
  assert.equal(interrupted.outcome, 'BLOCKED');
  assert.match(interrupted.lines[0], /never finished/);
  // A verify that ended before it wrote any receipt leaves the previous run's PASS.
  writeReceipt(fixture, { startedAt: '2020-01-01T00:00:00.000Z' });
  const stale = await waitForVerify(fixture.cwd, { graceMs: 0 });
  assert.equal(stale.outcome, 'BLOCKED');
  assert.match(stale.lines[0], /without writing a receipt/);
});
test('verify queues a heavy gate behind held machine-wide slots and runs it once one frees', async t => {
  const fixture = repo(t);
  for (const path of ['scripts/harness.mjs', 'scripts/harness-plan.mjs', 'scripts/harness-lanes.mjs', 'scripts/harness-test-failures.mjs', 'scripts/ci/run-guarded.mjs', 'scripts/ci/changed-scope.mjs']) fixture.put(path, readFileSync(join(root, path)));
  for (const path of ['scripts/check-agent-diff.mjs', 'scripts/check-docs.mjs', 'scripts/check-readme-drift.mjs', 'scripts/check-process-spawn-discipline.mjs']) fixture.put(path, 'process.exit(0);\n');
  fixture.put('.gitignore', '.harness/\n.task.json\nnode_modules/\n');
  fixture.put('node_modules/eslint/package.json', '{"version":"fixture"}');
  fixture.put('npm-cli.mjs', [
    'const script = process.argv[3];',
    "if (script === 'test') console.log('Tests  1 passed (1)');",
    "else if (script.startsWith('test:')) console.log('# pass 1');",
  ].join('\n'));
  const base = fixture.commit();
  fixture.put('src/owner.ts', 'export const owner = 2;\n');
  const slots = slotsDir(t);
  // Another worktree's heavy gate (a live process: this one) holds the only slot.
  writeFileSync(join(slots, 'slot-0.json'), JSON.stringify({ pid: process.pid, token: 'other-worktree', gate: 'rust-tests', root: 'elsewhere' }));
  const proc = spawn(process.execPath, [script, 'verify', '--base', base], { cwd: fixture.cwd, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, npm_execpath: join(fixture.cwd, 'npm-cli.mjs'), BUILDMESH_GATE_SLOTS_DIR: slots, BUILDMESH_HEAVY_GATE_LIMIT: '1' } });
  t.after(() => proc.kill());
  let stdout = '';
  proc.stdout.on('data', chunk => { stdout += chunk; });
  const closed = new Promise(resolve => proc.on('close', resolve));
  for (let i = 0; i < 600 && !/QUEUED frontend-tests/.test(stdout); i += 1) await new Promise(done => setTimeout(done, 100));
  assert.match(stdout, /QUEUED frontend-tests: waiting for a heavy-gate slot \(limit 1; held by rust-tests in elsewhere\)/);
  assert.doesNotMatch(stdout, /PASS frontend-tests/, 'the gate must not run while the slot is held');
  unlinkSync(join(slots, 'slot-0.json'));
  await closed;
  const receipt = JSON.parse(readFileSync(join(fixture.cwd, '.harness/receipt.json')));
  const rows = Object.fromEntries(receipt.gates.map(row => [row.id, row]));
  assert.equal(rows['frontend-tests'].outcome, 'PASS', stdout);
  assert.ok(rows['frontend-tests'].queuedMs > 0);
  assert.equal(rows['frontend-build'].outcome, 'PASS');
  // The receipt reads in plan order even though lanes finish in any order.
  const order = receipt.gates.map(row => row.id);
  assert.deepEqual(order, planGates(['src/owner.ts']).map(gate => gate.id).filter(id => order.includes(id)));
});

// Fake gates append their id to an untracked log, so a test can tell which gates
// really ran. `ignores` is the same input declaration the real plan uses.
function fakeGates(fixture) {
  const log = join(fixture.cwd, '.harness/ran.log');
  const marker = name => {
    mkdirSync(join(fixture.cwd, '.harness'), { recursive: true });
    return join(fixture.cwd, '.harness', name);
  };
  const gate = (id, ignores, body = '') => ({
    id, minutes: 1, ...(ignores ? { ignores } : {}),
    command: ['node', '-e', `const fs=require('node:fs');fs.appendFileSync(${JSON.stringify(log)},${JSON.stringify(`${id}\n`)});${body}`],
  });
  const failWhen = name => `if(fs.existsSync(${JSON.stringify(marker(name))}))process.exit(1);`;
  const mutateWhen = name => `if(fs.existsSync(${JSON.stringify(marker(name))}))fs.writeFileSync(${JSON.stringify(join(fixture.cwd, 'src/owner.ts'))},'export const owner = 999;\\n');`;
  const ran = () => {
    const lines = existsSync(log) ? readFileSync(log, 'utf8').split('\n').filter(Boolean) : [];
    rmSync(log, { force: true });
    return lines;
  };
  return { gate, failWhen, mutateWhen, ran, marker };
}
const rows = receipt => Object.fromEntries(receipt.gates.map(row => [row.id, row]));

test('a passed gate is reused until a file it reads changes, not until anything changes', async t => {
  const fixture = repo(t);
  const { gate, ran } = fakeGates(fixture);
  const plan = () => [gate('frontend-fake', ['^src-tauri/', '^docs/']), gate('rust-fake', ['^src/', '^docs/']), gate('docs-fake')];
  const run = () => verify(fixture.cwd, { base: fixture.base, plan });
  const first = await run();
  assert.equal(first.outcome, 'PASS');
  assert.deepEqual(ran(), ['frontend-fake', 'rust-fake', 'docs-fake']);
  assert.ok(first.gates.every(row => !row.cached && /^[0-9a-f]{64}$/.test(row.inputs)));

  // A Rust-only edit re-runs the Rust gate and the whole-tree gate; the frontend gate is cached against the same hash.
  fixture.put('src-tauri/src/secret_scrubber.rs', 'pub fn scrub() {}\n');
  const rust = await run();
  assert.deepEqual(ran(), ['rust-fake', 'docs-fake']);
  assert.equal(rust.outcome, 'PASS');
  assert.equal(rows(rust)['frontend-fake'].cached, true);
  assert.equal(rows(rust)['frontend-fake'].inputs, rows(first)['frontend-fake'].inputs);
  assert.notEqual(rows(rust)['rust-fake'].inputs, rows(first)['rust-fake'].inputs);

  // A docs-only edit re-runs only the gate that reads docs.
  fixture.put('docs/page.md', '# Page\n');
  const docs = await run();
  assert.deepEqual(ran(), ['docs-fake']);
  assert.deepEqual(docs.gates.map(row => !!row.cached), [true, true, false]);

  // A frontend edit re-runs the frontend gate and leaves the Rust gate cached.
  fixture.put('src/owner.ts', 'export const owner = 2;\n');
  await run();
  assert.deepEqual(ran(), ['frontend-fake', 'docs-fake']);

  // Nothing changed: everything is cached, and committing does not invalidate the gates that never read the index or HEAD.
  assert.equal((await run()).gates.every(row => row.cached), true);
  assert.deepEqual(ran(), []);
  fixture.commit();
  await run();
  assert.deepEqual(ran(), ['docs-fake']);
});

test('a failed attempt keeps the gates it passed across attempts', async t => {
  const fixture = repo(t);
  const { gate, failWhen, ran, marker } = fakeGates(fixture);
  const plan = () => [gate('first', ['^docs/']), gate('flaky', ['^docs/'], failWhen('fail')), gate('last', ['^docs/'])];
  const run = () => verify(fixture.cwd, { base: fixture.base, plan });
  writeFileSync(marker('fail'), '');
  const failed = await run();
  assert.equal(failed.outcome, 'FAIL');
  assert.deepEqual(ran(), ['first', 'flaky']);
  rmSync(marker('fail'));
  const recovered = await run();
  assert.equal(recovered.outcome, 'PASS');
  // `first` kept its PASS across the failed attempt; the gates after the failure had never run.
  assert.deepEqual(ran(), ['flaky', 'last']);
  assert.equal(rows(recovered).first.cached, true);
});

test('reuse is keyed on the toolchain, the gate definition and the task, and a corrupt cache only costs time', async t => {
  const fixture = repo(t);
  const { gate, ran } = fakeGates(fixture);
  let plan = () => [gate('scoped', ['^docs/'])];
  const run = () => verify(fixture.cwd, { base: fixture.base, plan });
  const cache = join(fixture.cwd, '.harness/gate-cache.json');
  await run();
  ran();
  await run();
  assert.deepEqual(ran(), [], 'identical inputs are reused');

  // Same files, different toolchain environment: the PASS no longer applies.
  const before = process.env.RUSTFLAGS;
  t.after(() => { if (before === undefined) delete process.env.RUSTFLAGS; else process.env.RUSTFLAGS = before; });
  process.env.RUSTFLAGS = '-D warnings';
  await run();
  assert.deepEqual(ran(), ['scoped']);
  await run();
  assert.deepEqual(ran(), []);

  // Same files, edited gate definition (here: its input list) never reuses a PASS from the old definition.
  plan = () => [gate('scoped', ['^docs/', '^android/'])];
  await run();
  assert.deepEqual(ran(), ['scoped']);

  // Another task's cache is ignored.
  writeFileSync(cache, JSON.stringify({ ...JSON.parse(readFileSync(cache, 'utf8')), taskId: 'some-other-task' }));
  await run();
  assert.deepEqual(ran(), ['scoped']);

  writeFileSync(cache, 'not json');
  assert.equal((await run()).outcome, 'PASS');
  assert.deepEqual(ran(), ['scoped']);
});

test('a source change during verification fails the attempt and nothing it passed is reused', async t => {
  const fixture = repo(t);
  const { gate, mutateWhen, ran, marker } = fakeGates(fixture);
  const plan = () => [gate('reads-src', ['^docs/']), gate('mutator', ['^docs/'], mutateWhen('mutate'))];
  const run = () => verify(fixture.cwd, { base: fixture.base, plan });
  writeFileSync(marker('mutate'), '');
  const mutated = await run();
  assert.equal(mutated.outcome, 'FAIL');
  assert.match(mutated.reason, /Source changed during verification/);
  assert.ok(mutated.gates.every(row => row.outcome === 'PASS'));
  ran();
  // The tree is back to what `reads-src` was keyed on, but it ran while the source was being rewritten.
  rmSync(marker('mutate'));
  fixture.put('src/owner.ts', 'export const owner = 1;\n');
  const clean = await run();
  assert.equal(clean.outcome, 'PASS');
  assert.deepEqual(ran(), ['reads-src', 'mutator']);
});

test('completion needs every gate PASS on the current tree, cached or fresh', t => {
  const fixture = repo(t);
  const task = fixture.start();
  const cachedReceipt = () => {
    passingReceipt(fixture, task);
    const receipt = JSON.parse(readFileSync(join(fixture.cwd, '.harness/receipt.json')));
    receipt.gates = receipt.gates.map(row => ({ ...row, cached: true, inputs: 'a'.repeat(64) }));
    fixture.put('.harness/receipt.json', JSON.stringify(receipt));
    return receipt;
  };
  cachedReceipt();
  assert.equal(completion(fixture.cwd).outcome, 'PASS');
  // A cached row still has to be PASS, and the receipt still has to list every planned gate.
  const failing = cachedReceipt();
  failing.gates[0].outcome = 'FAIL';
  failing.outcome = 'FAIL';
  fixture.put('.harness/receipt.json', JSON.stringify(failing));
  assert.equal(completion(fixture.cwd).outcome, 'FAIL');
  const partial = cachedReceipt();
  partial.gates.pop();
  fixture.put('.harness/receipt.json', JSON.stringify(partial));
  assert.match(completion(fixture.cwd).reason, /every required gate/);
  // Cached rows do not carry a receipt across an edit: the receipt itself is bound to the current tree.
  cachedReceipt();
  fixture.put('src/owner.ts', 'export const owner = 5;\n');
  assert.match(completion(fixture.cwd).reason, /stale/);
});

test('the real plan reuses frontend gates after a Rust-only edit and docs-only edits skip code gates', () => {
  const plan = planGates(['src/owner.ts', 'src-tauri/src/lib.rs']);
  const rerun = path => plan.filter(row => gateReads(row, path)).map(row => row.id);
  const frontend = ['frontend-build', 'bundle', 'frontend-tests', 'browser-smoke'];
  const rust = ['rust-format', 'rust-clippy', 'rust-tests', 'binding-drift'];

  const rustOnly = rerun('src-tauri/src/secret_scrubber.rs');
  for (const id of rust) assert.ok(rustOnly.includes(id), id);
  for (const id of [...frontend, 'lint', 'lint-fixtures', 'lint-tests']) assert.ok(!rustOnly.includes(id), `${id} must be reused after a Rust-only edit`);
  // Whole-tree gates and repo-wide agent tests still look at it.
  for (const id of ['whitespace', 'staged-content', 'agent-rules', 'docs', 'docs-tests', 'agent-tests']) assert.ok(rustOnly.includes(id), id);

  const docsOnly = rerun('docs/agents/development-harness.md');
  assert.deepEqual(docsOnly, ['whitespace', 'staged-content', 'agent-rules', 'docs', 'readme', 'process-spawns', 'known-flakes', 'docs-tests', 'readme-tests']);

  // Frontend edits re-run the Rust gates: Rust tests read src/ and embed the built mobile bundle.
  const frontendEdit = rerun('src/App.tsx');
  for (const id of [...frontend, ...rust]) assert.ok(frontendEdit.includes(id), id);
  // Unknown and tooling paths invalidate everything that does not provably ignore them.
  assert.deepEqual(rerun('some-new-top-level-dir/file'), plan.map(row => row.id));
});

test('Vitest keeps re-running for the src-tauri files it reads, and every Rust gate shares one input list', () => {
  const plan = planGates(['src/owner.ts', 'src-tauri/src/lib.rs']);
  const byId = Object.fromEntries(plan.map(row => [row.id, row]));
  for (const path of ['src-tauri/Cargo.toml', 'src-tauri/Cargo.lock', 'src-tauri/tauri.conf.json', 'src-tauri/tauri.dev.conf.json', 'src-tauri/tauri.windows.conf.json', 'src-tauri/capabilities/default.json', 'src-tauri/src/lib.rs', 'src-tauri/src/commands/file_tree.rs', 'src-tauri/src/http/routes/issues.rs', 'src-tauri/src/agent/provider/adapters/opencode_attention_plugin.js', 'docs/brand/b3-relay-icon.svg', 'src/types/generated/Example.ts']) {
    assert.ok(gateReads(byId['frontend-tests'], path), path);
  }
  for (const path of ['src-tauri/src/secret_scrubber.rs', 'src-tauri/src/db/mod.rs', 'src-tauri/tests/fixtures/transcripts/muse/muse_transcript.jsonl', 'docs/page.md']) {
    assert.ok(!gateReads(byId['frontend-tests'], path), path);
  }
  assert.ok(gateReads(byId['frontend-build'], 'package.json') && gateReads(byId['frontend-build'], 'vite.config.ts'));
  for (const path of ['src/types/generated/Example.ts', 'mobile/index.html', 'package-lock.json', 'src-tauri/tests/fixtures/x.json', 'scripts/rust-test-shards.mjs']) {
    assert.ok(gateReads(byId['rust-tests'], path), path);
  }
  const rustInputs = ['rust-format', 'rust-clippy', 'rust-tests', 'binding-drift'].map(id => JSON.stringify(byId[id].ignores));
  assert.equal(new Set(rustInputs).size, 1, 'binding-drift must never be reused while rust-tests re-runs');
  assert.ok(rustInputs[0] !== undefined);
  // Gates the audit did not cover keep reading the whole tree.
  for (const id of ['whitespace', 'staged-content', 'agent-rules', 'docs', 'readme', 'process-spawns', 'known-flakes', 'docs-tests', 'readme-tests']) assert.equal(byId[id].ignores, undefined, id);
});

test('new Vitest references to src-tauri are audited against the frontend-tests input list', () => {
  // Vitest gate reuse is only sound while its input list covers every src-tauri file a test reads.
  // We pin a snapshot of every referencing line so that any new or modified reference across any
  // test file (including files already referencing src-tauri) forces re-audit against
  // VITEST_READS_FROM_SRC_TAURI in scripts/harness-plan.mjs.
  const files = execFileSync('git', ['ls-files', 'tests/unit', 'tests/integration', 'tests/e2e', 'tests/setup'], { cwd: root, encoding: 'utf8' })
    .split('\n').filter(file => /\.(?:tsx?|mjs)$/.test(file));
  const referencingLines = files.flatMap(file =>
    readFileSync(join(root, file), 'utf8')
      .split(/\r?\n/)
      .map(line => line.trim())
      .filter(line => line.includes('src-tauri') && !/^\s*(?:\/\/|\*|\/\*)/.test(line))
      .map(line => `${file}: ${line}`)
  ).sort();
  assert.deepEqual(referencingLines, [
    "tests/e2e/app-launch.spec.ts: const EXE_PATH = 'X:/src/buildmesh/src-tauri/target/release/buildmesh.exe';",
    "tests/e2e/app-launch.spec.ts: const debugExe = 'X:/src/buildmesh/src-tauri/target/debug/buildmesh.exe';",
    "tests/e2e/utils/buildmesh-launcher.ts: 'src-tauri', 'target', 'release', 'buildmesh.exe',",
    'tests/unit/app-version.test.ts: ["src-tauri", "Cargo.lock"],',
    'tests/unit/app-version.test.ts: ["src-tauri", "Cargo.toml"],',
    'tests/unit/app-version.test.ts: ["src-tauri", "tauri.conf.json"],',
    'tests/unit/app-version.test.ts: cargo: read(["src-tauri", "Cargo.toml"]).match(/^version\\s*=\\s*"([^"]+)"/m)?.[1],',
    'tests/unit/app-version.test.ts: const cargo = readFileSync(path.join(root, "src-tauri", "Cargo.toml"), "utf8");',
    'tests/unit/app-version.test.ts: const lock = readFileSync(path.join(root, "src-tauri", "Cargo.lock"), "utf8");',
    'tests/unit/app-version.test.ts: lock: read(["src-tauri", "Cargo.lock"]).match(',
    'tests/unit/app-version.test.ts: mkdirSync(path.join(dir, "src-tauri"), { recursive: true });',
    'tests/unit/app-version.test.ts: readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),',
    'tests/unit/app-version.test.ts: tauri: JSON.parse(read(["src-tauri", "tauri.conf.json"])).version,',
    "tests/unit/async-command-blocking.test.ts: const COMMANDS_DIR = join(REPO_ROOT, 'src-tauri', 'src', 'commands');",
    "tests/unit/async-command-blocking.test.ts: const HTTP_ROUTES_DIR = join(REPO_ROOT, 'src-tauri', 'src', 'http', 'routes');",
    "tests/unit/async-command-blocking.test.ts: it('walks src-tauri/src/commands and finds Rust files', () => {",
    "tests/unit/async-command-blocking.test.ts: it('walks src-tauri/src/http/routes and finds Rust files', () => {",
    'tests/unit/ci-rust-timeout-guard.test.ts: const path = `path: src-tauri/${logFile(script)}`;',
    "tests/unit/conpty-runtime.test.ts: const common = JSON.parse(await readFile('src-tauri/tauri.conf.json', 'utf8'));",
    "tests/unit/conpty-runtime.test.ts: const windows = JSON.parse(await readFile('src-tauri/tauri.windows.conf.json', 'utf8'));",
    'tests/unit/event-payloads.test.ts: `  2. Run \\`cargo test\\` in src-tauri/ to regenerate the .ts file.\\n` +',
    'tests/unit/event-payloads.test.ts: `payload to be a struct in src-tauri/src/ that derives #[derive(TS)] and is generated\\n` +',
    'tests/unit/guard-antipatterns.test.ts: "X:\\\\src\\\\buildmesh\\\\.claude\\\\worktrees\\\\red-rare-hedge\\\\src-tauri\\\\src\\\\db\\\\mod.rs",',
    'tests/unit/guard-antipatterns.test.ts: "X:\\\\src\\\\buildmesh\\\\src-tauri\\\\src\\\\db\\\\mod.rs",',
    'tests/unit/guard-antipatterns.test.ts: "X:\\\\src\\\\buildmesh\\\\src-tauri\\\\src\\\\db\\\\mod.rs",',
    'tests/unit/guard-antipatterns.test.ts: "src-tauri/src/agent/spawn.rs",',
    'tests/unit/guard-antipatterns.test.ts: "src-tauri/src/agent/spawn.rs",',
    'tests/unit/guard-antipatterns.test.ts: "src-tauri/src/agent/spawn.rs",',
    'tests/unit/guard-antipatterns.test.ts: "src-tauri/src/env/mod.rs",',
    'tests/unit/guard-antipatterns.test.ts: expect(checkWorktreeEscape("src-tauri/src/db/mod.rs", CWD_WORKTREE)).toBeNull();',
    'tests/unit/guard-antipatterns.test.ts: it("allows \\\\\\\\wsl$ inside src-tauri/src/env/", () => {',
    'tests/unit/ipc-contract.test.ts: `\\n  1. Add the command to tauri::generate_handler![ ... ] in src-tauri/src/lib.rs` +',
    "tests/unit/ipc-contract.test.ts: const LIB_RS = join(REPO_ROOT, 'src-tauri', 'src', 'lib.rs');",
    'tests/unit/opencode-attention-plugin.test.ts: const source = readFileSync(resolve("src-tauri/src/agent/provider/adapters/opencode_attention_plugin.js"), "utf8");',
    "tests/unit/tauri-capabilities.test.ts: describe('src-tauri/capabilities/default.json', () => {",
    "tests/unit/tauri-capabilities.test.ts: readFileSync(resolve(process.cwd(), 'src-tauri/capabilities/default.json'), 'utf8'),",
    "tests/unit/tauri-dev-config.test.ts: readFileSync(resolve(process.cwd(), 'src-tauri/tauri.conf.json'), 'utf8'),",
    "tests/unit/tauri-dev-config.test.ts: readFileSync(resolve(process.cwd(), 'src-tauri/tauri.dev.conf.json'), 'utf8'),",
  ]);

  // Every actual read of a src-tauri file/subpath identified across those lines must be covered by frontend-tests:
  const plan = planGates(['src/owner.ts', 'src-tauri/src/lib.rs']);
  const frontendGate = plan.find(row => row.id === 'frontend-tests');
  for (const readPath of [
    'src-tauri/Cargo.toml',
    'src-tauri/Cargo.lock',
    'src-tauri/tauri.conf.json',
    'src-tauri/tauri.dev.conf.json',
    'src-tauri/tauri.windows.conf.json',
    'src-tauri/capabilities/default.json',
    'src-tauri/src/lib.rs',
    'src-tauri/src/commands/file_tree.rs',
    'src-tauri/src/http/routes/issues.rs',
    'src-tauri/src/agent/provider/adapters/opencode_attention_plugin.js',
  ]) {
    assert.ok(gateReads(frontendGate, readPath), `frontend-tests must read ${readPath}`);
  }
  // Pure string-literal mentions in tests that do not read the filesystem (guard-antipatterns) are not inputs:
  for (const unread of ['src-tauri/src/db/mod.rs', 'src-tauri/src/agent/spawn.rs', 'src-tauri/src/env/mod.rs']) {
    assert.ok(!gateReads(frontendGate, unread), `frontend-tests must NOT read unread path ${unread}`);
  }
});
