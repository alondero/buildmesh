import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { changedPaths, completion, fingerprint, runGate, scopePaths, waitForVerify } from '../../scripts/harness.mjs';
import { executedTests, planGates, touchedFormatDiffs } from '../../scripts/harness-plan.mjs';
import { acquireSlot, heavyGateEnv, heavyGateLimit, runPlan, slotHolders } from '../../scripts/harness-lanes.mjs';

const script = fileURLToPath(new URL('../../scripts/harness.mjs', import.meta.url));
const root = fileURLToPath(new URL('../../', import.meta.url));
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
  for (const path of ['scripts/harness.mjs', 'scripts/harness-plan.mjs', 'scripts/harness-lanes.mjs', 'scripts/ci/run-guarded.mjs', 'scripts/ci/changed-scope.mjs']) fixture.put(path, readFileSync(join(root, path)));
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
  for (const path of ['scripts/harness.mjs', 'scripts/harness-plan.mjs', 'scripts/harness-lanes.mjs', 'scripts/ci/run-guarded.mjs', 'scripts/ci/changed-scope.mjs']) fixture.put(path, readFileSync(join(root, path)));
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
