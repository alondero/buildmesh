import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { changedPaths, completion, fingerprint, runGate, scopePaths, verify } from '../../scripts/harness.mjs';
import { executedTests, gateReads, planGates, touchedFormatDiffs } from '../../scripts/harness-plan.mjs';

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
  for (const path of ['scripts/harness.mjs', 'scripts/harness-plan.mjs', 'scripts/ci/run-guarded.mjs', 'scripts/ci/changed-scope.mjs']) fixture.put(path, readFileSync(join(root, path)));
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
  assert.deepEqual(docsOnly, ['whitespace', 'staged-content', 'agent-rules', 'docs', 'readme', 'process-spawns', 'docs-tests', 'readme-tests']);

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
  for (const id of ['whitespace', 'staged-content', 'agent-rules', 'docs', 'readme', 'process-spawns', 'docs-tests', 'readme-tests']) assert.equal(byId[id].ignores, undefined, id);
});

test('new Vitest references to src-tauri are audited against the frontend-tests input list', () => {
  // Vitest gate reuse is only sound while its input list covers every src-tauri file a test reads. If this
  // fails, check that file's reads against VITEST_READS_FROM_SRC_TAURI in scripts/harness-plan.mjs, then update the list below.
  const files = execFileSync('git', ['ls-files', 'tests/unit', 'tests/integration', 'tests/e2e', 'tests/setup'], { cwd: root, encoding: 'utf8' })
    .split('\n').filter(file => /\.(?:tsx?|mjs)$/.test(file));
  const referencing = files.filter(file => readFileSync(join(root, file), 'utf8').split('\n').some(line => line.includes('src-tauri') && !/^\s*(?:\/\/|\*|\/\*)/.test(line))).sort();
  assert.deepEqual(referencing, [
    'tests/e2e/app-launch.spec.ts',
    'tests/e2e/utils/buildmesh-launcher.ts',
    'tests/unit/app-version.test.ts',
    'tests/unit/async-command-blocking.test.ts',
    'tests/unit/ci-rust-timeout-guard.test.ts',
    'tests/unit/conpty-runtime.test.ts',
    'tests/unit/event-payloads.test.ts',
    'tests/unit/guard-antipatterns.test.ts',
    'tests/unit/ipc-contract.test.ts',
    'tests/unit/opencode-attention-plugin.test.ts',
    'tests/unit/tauri-capabilities.test.ts',
    'tests/unit/tauri-dev-config.test.ts',
  ]);
});
