#!/usr/bin/env node
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { appendFileSync, closeSync, existsSync, lstatSync, mkdirSync, openSync, readFileSync, readlinkSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { runGuarded } from './ci/run-guarded.mjs';
import { availableParallelism } from 'node:os';
import { executedTests, gateReads, planGates, touchedFormatDiffs } from './harness-plan.mjs';
import { acquireSlot, heavyGateEnv, heavyGateLimit, runPlan } from './harness-lanes.mjs';
import { conptyPrerequisite, failureLines, gatePassed, isolatedCommand, isolateTests, knownFlakes, readTestReport, validateFlakeIssues } from './harness-test-failures.mjs';

const USAGE = 'harness start --spec <json> | update --spec <json> | status | metrics | verify [--base <commit>] [--full] | wait [--max-seconds <n>] | finish | evaluate [--case <id>] | checkpoint | record-rollback --ref <commit>';
const EXIT = { PASS: 0, FAIL: 1, FLAKY: 1, BLOCKED: 2, TIMEOUT: 124 };
const statePath = (root, name) => join(root, '.harness', name);
const git = (root, ...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });
const readJson = (path) => existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : null;
function saveJson(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.${randomUUID()}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`);
  renameSync(temporary, path);
}
function alive(pid) {
  try { process.kill(pid, 0); return true; } catch (error) { return error.code === 'EPERM'; }
}
function lock(root, name = 'lock', action = null) {
  mkdirSync(statePath(root, ''), { recursive: true });
  const path = statePath(root, name);
  let fd;
  try { fd = openSync(path, 'wx'); } catch {
    // An interrupted process leaves the lock behind. Reclaim it when the recorded
    // PID is gone, but never when the owner is still running, or when the file
    // is unreadable and therefore cannot be attributed to a dead owner.
    let owner = null;
    try { owner = readJson(path); } catch { owner = null; }
    if (!owner || typeof owner.pid !== 'number' || alive(owner.pid)) throw new Error(`BLOCKED: another harness operation owns .harness/${name}. If interrupted, inspect its PID before removing the lock.`);
    unlinkSync(path);
    try { fd = openSync(path, 'wx'); } catch { throw new Error(`BLOCKED: another harness operation owns .harness/${name}.`); }
    event(root, { type: 'lock-reclaimed', lock: name, pid: owner.pid, startedAt: owner.startedAt ?? null });
  }
  writeFileSync(fd, JSON.stringify({ pid: process.pid, action, startedAt: new Date().toISOString() }));
  return () => { closeSync(fd); unlinkSync(path); };
}
function event(root, value) {
  appendFileSync(statePath(root, 'events.jsonl'), `${JSON.stringify({ at: new Date().toISOString(), ...value })}\n`);
}
function setPhase(task, phase) {
  if (task.phase === phase) return;
  const now = Date.now();
  task.phaseDurationsMs ??= {};
  task.phaseDurationsMs[task.phase] = (task.phaseDurationsMs[task.phase] ?? 0) + Math.max(0, now - Date.parse(task.phaseStartedAt ?? task.startedAt));
  task.phase = phase;
  task.phaseStartedAt = new Date(now).toISOString();
}
export function changedPaths(root, base) {
  return [...new Set([
    ...git(root, 'diff', '--no-renames', '--name-only', '-z', base, '--').split('\0'),
    ...git(root, 'diff', '--cached', '--no-renames', '--name-only', '-z', base, '--').split('\0'),
    ...git(root, 'diff', '--no-renames', '--name-only', '-z', '--').split('\0'),
    ...git(root, 'ls-files', '--others', '--exclude-standard', '-z').split('\0'),
  ].filter(Boolean))].sort();
}
export function scopePaths(root, base, paths) {
  // Adding harness commands does not change product dependencies or build
  // recipes. Narrow only this proven case; arbitrary package edits stay broad.
  return paths.filter(path => {
    if (path === 'package.json') {
      try {
        const before = JSON.parse(git(root, 'show', `${base}:package.json`));
        const after = readJson(join(root, path));
        const allowed = ['harness', 'verify', 'eval:harness', 'eval:behaviour'];
        for (const name of allowed) { delete before.scripts[name]; delete after.scripts[name]; }
        const addedTest = ' tests/agent-infra/harness.test.mjs';
        if (after.scripts['test:agent'] === `${before.scripts['test:agent']}${addedTest}`) after.scripts['test:agent'] = before.scripts['test:agent'];
        return JSON.stringify(before) !== JSON.stringify(after);
      } catch { return true; }
    }
    if (path === 'eslint.config.js') {
      try {
        const before = git(root, 'show', `${base}:${path}`).replaceAll('\r\n', '\n');
        const after = readFileSync(join(root, path), 'utf8').replaceAll('\r\n', '\n').replace("      '.harness/**',\n", '');
        return before !== after;
      } catch { return true; }
    }
    return true;
  });
}
// What can change a result without touching a file: the base and the toolchain.
function runIdentity(root, base) {
  return JSON.stringify({ base, node: process.version, platform: process.platform, tools: toolIdentity(root), environment: ['NODE_OPTIONS', 'RUSTFLAGS', 'CARGO_TARGET_DIR', 'CC', 'CXX', 'TS_RS_EXPORT_DIR'].map(key => [key, process.env[key] ?? null]) });
}
function entryOf(root, path) {
  const full = join(root, path);
  const stat = existsSync(full) ? lstatSync(full) : null;
  const data = !stat ? 'deleted' : stat.isSymbolicLink() ? `link:${readlinkSync(full)}` : stat.isFile() ? readFileSync(full) : 'directory';
  return { mode: stat?.mode ?? 0, data };
}
function treePaths(root) {
  return [...new Set(git(root, 'ls-files', '--cached', '--others', '--exclude-standard', '-z').split('\0').filter(Boolean))].sort();
}
export function fingerprint(root, base, prefix = '') {
  const hash = createHash('sha256');
  hash.update(runIdentity(root, base));
  hash.update(git(root, 'rev-parse', 'HEAD'));
  hash.update(git(root, 'ls-files', '--stage', '-z', '--', prefix || '.'));
  for (const path of treePaths(root)) {
    if (path.startsWith('.harness/') || !path.startsWith(prefix)) continue;
    const { mode, data } = entryOf(root, path);
    hash.update(`${path.length}:${path}\0${mode}\0${data.length}:`);
    hash.update(data);
  }
  return hash.digest('hex');
}
// One content digest per working-tree file, read once so every gate's input hash
// shares a single pass over the tree.
function treeSnapshot(root) {
  const entries = [];
  for (const path of treePaths(root)) {
    if (path.startsWith('.harness/')) continue;
    const { mode, data } = entryOf(root, path);
    entries.push({ path, line: `${path.length}:${path}\0${mode}\0${createHash('sha256').update(data).digest('hex')}\n` });
  }
  return entries;
}
// Hash of everything a gate reads. A gate without declared `ignores` reads the
// whole tree, so it keys on the whole-tree fingerprint exactly as before; a
// gate with them keys on the toolchain identity and only the files it reads
// (index state and HEAD do not matter to it). The gate definition is part of
// the key, so a changed command, deadline or input list never reuses a PASS.
function gateInputs(gate, snapshot, identity, tree) {
  const hash = createHash('sha256');
  hash.update(JSON.stringify(gate));
  if (!gate.ignores) hash.update(`\0tree:${tree}`);
  else {
    hash.update(`\0${identity}\0`);
    for (const entry of snapshot) if (gateReads(gate, entry.path)) hash.update(entry.line);
  }
  return hash.digest('hex');
}
function toolIdentity(root) {
  const versions = {};
  for (const name of ['eslint', 'typescript', 'vite', 'vitest', '@playwright/test']) {
    const path = join(root, 'node_modules', name, 'package.json');
    versions[name] = existsSync(path) ? readJson(path).version : null;
  }
  const cargo = spawnSync('cargo', ['--version'], { encoding: 'utf8', windowsHide: true, timeout: 5000 });
  versions.cargo = cargo.status === 0 ? cargo.stdout.trim() : null;
  try {
    const require = createRequire(join(root, 'package.json'));
    versions.chromiumAvailable = existsSync(require('@playwright/test').chromium.executablePath());
  } catch { versions.chromiumAvailable = false; }
  return versions;
}
function taskAt(root) {
  const task = readJson(statePath(root, 'active-task.json'));
  if (task && task.root !== root) throw new Error('BLOCKED: task belongs to a different worktree.');
  return task;
}
function stringList(value, name) {
  if (!Array.isArray(value) || !value.length || value.some(item => typeof item !== 'string' || !item.trim())) throw new Error(`${name} must be a nonempty array of strings.`);
  return value;
}
function resolveBase(root, ref) {
  return git(root, 'rev-parse', '--verify', '--end-of-options', `${ref}^{commit}`).trim();
}
function start(root, spec) {
  const previous = taskAt(root);
  if (previous && previous.phase !== 'complete') throw new Error('BLOCKED: an unfinished task exists. Resume it with status/update instead of replacing it.');
  if (typeof spec.goal !== 'string' || !spec.goal.trim()) throw new Error('Task goal is required.');
  const task = {
    id: randomUUID(), root, base: resolveBase(root, spec.base ?? 'HEAD'), goal: spec.goal,
    criteria: stringList(spec.criteria, 'criteria'), plannedEdits: stringList(spec.plannedEdits, 'plannedEdits'),
    phase: 'understand', nextAction: spec.nextAction ?? 'Read the relevant engineering contract and production module.',
    decisions: [], blockers: [], evidence: [], review: null, checkpoints: [],
    agent: spec.agent ?? null, model: spec.model ?? null, startedAt: new Date().toISOString(), verificationAttempts: 0,
    phaseStartedAt: new Date().toISOString(), phaseDurationsMs: {},
  };
  saveJson(statePath(root, 'active-task.json'), task);
  event(root, { type: 'task-start', taskId: task.id, agent: task.agent, model: task.model });
  return task;
}
function update(root, spec) {
  const task = taskAt(root);
  if (!task || task.phase === 'complete') throw new Error('An active unfinished task is required.');
  const allowed = ['phase', 'nextAction', 'decisions', 'blockers', 'evidence', 'review'];
  if (Object.keys(spec).some(key => !allowed.includes(key))) throw new Error(`Only progress fields can be updated: ${allowed.join(', ')}.`);
  if (spec.phase && !['understand', 'plan', 'implement', 'verify', 'review', 'blocked'].includes(spec.phase)) throw new Error('Invalid task phase. Use finish for completion.');
  for (const key of ['decisions', 'blockers', 'evidence']) {
    if (spec[key] !== undefined && (!Array.isArray(spec[key]) || spec[key].some(item => typeof item !== 'string'))) throw new Error(`${key} must be an array of strings.`);
  }
  if (spec.nextAction !== undefined && (typeof spec.nextAction !== 'string' || !spec.nextAction.trim())) throw new Error('nextAction must be a nonempty string.');
  if (spec.review !== undefined) {
    const review = spec.review;
    if (!review || typeof review.reviewer !== 'string' || !review.reviewer.trim() || typeof review.summary !== 'string' || !review.summary.trim() || !['APPROVE', 'REQUEST_CHANGES', 'BLOCKED'].includes(review.verdict) || !Array.isArray(review.findings) || review.findings.some(item => typeof item !== 'string')) throw new Error('review requires reviewer, summary, verdict (APPROVE/REQUEST_CHANGES/BLOCKED), and findings array.');
  }
  if (spec.phase) setPhase(task, spec.phase);
  Object.assign(task, spec);
  if (spec.evidence) task.evidenceTree = fingerprint(root, task.base);
  if (spec.review) task.reviewTree = fingerprint(root, task.base);
  saveJson(statePath(root, 'active-task.json'), task);
  event(root, { type: 'progress', taskId: task.id, phase: task.phase, nextAction: task.nextAction });
  return task;
}
function npmCommand(root, args) {
  // npm supplies the real JS entrypoint; direct Node invocation avoids Windows
  // .cmd shell quoting, and cannot interpret an argument as shell source.
  const cli = process.env.npm_execpath;
  if (cli && existsSync(cli)) return [process.execPath, cli, ...args];
  const candidates = [join(dirname(process.execPath), 'node_modules/npm/bin/npm-cli.js'), join(root, 'node_modules/npm/bin/npm-cli.js')];
  const found = candidates.find(existsSync);
  if (!found) throw new Error('BLOCKED: npm CLI is unavailable. Invoke through npm run verify.');
  return [process.execPath, found, ...args];
}
function commandFor(root, gate, base) {
  const args = gate.command.slice(1).map(value => value === '$BASE' ? base : value);
  if (gate.command[0] === 'npm') return npmCommand(root, args);
  return [gate.command[0] === 'node' ? process.execPath : gate.command[0], ...args];
}
async function preflight(root, gate, extraEnv) {
  if (gate.tests === 'vitest' || gate.tests === 'rust') {
    const probe = spawnSync('git', ['--version'], { encoding: 'utf8', windowsHide: true, timeout: 5000, env: { ...process.env, ...extraEnv } });
    if (probe.error || probe.status !== 0) return 'Git is unavailable on PATH; these tests spawn Git.';
  }
  if ((gate.command[0] === 'npm' || gate.browser || gate.command.some(arg => arg.startsWith('node_modules/'))) && !existsSync(join(root, 'node_modules/eslint/package.json'))) return 'node_modules is missing; install with npm ci (or the documented worktree junction).';
  if (gate.rust) {
    const probe = spawnSync('cargo', ['--version'], { encoding: 'utf8', windowsHide: true, timeout: 5000 });
    if (probe.error || probe.status !== 0) return 'Rust/Cargo is unavailable; install the Tauri platform prerequisites.';
    const compiler = spawnSync('rustc', ['-vV'], { encoding: 'utf8', windowsHide: true, timeout: 5000 });
    if (compiler.status !== 0) return 'Rust compiler is unavailable.';
    if (gate.id === 'rust-format' || gate.id === 'rust-clippy') {
      const component = spawnSync('cargo', [gate.id === 'rust-format' ? 'fmt' : 'clippy', '--version'], { encoding: 'utf8', windowsHide: true, timeout: 5000 });
      if (component.status !== 0) return `${gate.id === 'rust-format' ? 'rustfmt' : 'Clippy'} is unavailable; install the Rust toolchain component.`;
    }
    if (gate.id !== 'rust-format') {
      if (process.platform === 'linux') {
        const libraries = spawnSync('pkg-config', ['--exists', 'gtk+-3.0', 'webkit2gtk-4.1'], { windowsHide: true, timeout: 5000 });
        if (libraries.error || libraries.status !== 0) return 'Tauri GTK/WebKit development libraries or pkg-config are unavailable; install the platform prerequisites.';
      } else if (process.platform === 'darwin') {
        const tools = spawnSync('xcrun', ['--find', 'clang'], { encoding: 'utf8', timeout: 5000 });
        if (tools.error || tools.status !== 0) return 'Xcode command-line tools are unavailable; install the Tauri platform prerequisites.';
      } else if (process.platform === 'win32') {
        const unavailable = await conptyPrerequisite(root);
        if (unavailable) return unavailable;
        if (compiler.stdout.includes('windows-msvc')) {
          const linker = spawnSync('where.exe', ['link.exe'], { encoding: 'utf8', windowsHide: true, timeout: 5000 });
          const installer = join(process.env['ProgramFiles(x86)'] ?? 'C:/Program Files (x86)', 'Microsoft Visual Studio/Installer/vswhere.exe');
          const installed = existsSync(installer) ? spawnSync(installer, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-find', 'VC/Tools/MSVC/**/bin/**/link.exe'], { encoding: 'utf8', windowsHide: true, timeout: 5000 }) : null;
          const nativeLinker = linker.status === 0 && linker.stdout.split(/\r?\n/).filter(Boolean).some(path => {
            const help = spawnSync(path, ['/?'], { encoding: 'utf8', windowsHide: true, timeout: 5000 });
            return /Microsoft.*(?:Incremental Linker|Linker Version)/i.test(`${help.stdout ?? ''}${help.stderr ?? ''}`);
          });
          if (!nativeLinker && !installed?.stdout?.trim()) return 'MSVC linker/build tools are unavailable; install the Tauri Windows prerequisites.';
        }
      }
    }
  }
  if (gate.browser) {
    try {
      const require = createRequire(join(root, 'package.json'));
      const { chromium } = require('@playwright/test');
      if (!existsSync(chromium.executablePath())) return 'Playwright Chromium is missing; run npx playwright install chromium.';
    } catch { return 'Playwright is unavailable; run npm ci and npx playwright install chromium.'; }
  }
  return null;
}
export async function runGate(root, gate, base, paths = [], extraEnv = {}, { deferIsolation = false, isolationBudget = {} } = {}) {
  mkdirSync(statePath(root, 'logs'), { recursive: true });
  const log = statePath(root, `logs/${Date.now()}-${gate.id}.log`);
  const started = Date.now();
  let result;
  const unavailable = await preflight(root, gate, extraEnv);
  if (unavailable) result = { outcome: 'BLOCKED', reason: unavailable };
  else {
    const command = commandFor(root, gate, base);
    const reportPath = `${log}.json`;
    if (gate.tests === 'vitest') command.push(...(gate.command[0] === 'npm' ? ['--'] : []), '--reporter=default', '--reporter=json', `--outputFile.json=${reportPath}`);
    const nodeEnv = gate.id === 'frontend-build' || gate.id === 'mobile-build' ? 'production' : 'test';
    const env = { ...process.env, NODE_ENV: nodeEnv, NO_COLOR: '1', ...extraEnv };
    delete env.FORCE_COLOR;
    delete env.BUILDMESH_PREFILL;
    delete env.NODE_TEST_CONTEXT;
    if (gate.tests === 'rust') env.BUILDMESH_TEST_REPORT = reportPath;
    const code = await runGuarded({ minutes: gate.minutes, killGraceSeconds: 2, log, label: gate.id, command, cwd: gate.cwd ? join(root, gate.cwd) : root, env, output: () => {} });
    result = { ...classify(gate, code, readFileSync(log, 'utf8'), paths), exitCode: code };
    if (result.outcome === 'FAIL' && ['vitest', 'rust'].includes(gate.tests)) {
      const report = readTestReport(root, gate.tests, reportPath, readFileSync(log, 'utf8'));
      result = { ...result, failures: report.failures, count: report.count, unattributed: report.unattributed };
    }
  }
  const row = { id: gate.id, command: gate.command, durationMs: Date.now() - started, log: existsSync(log) ? log : null, ...result };
  if (!deferIsolation && row.failures?.length) await diagnoseGate(root, gate, row, base, extraEnv, isolationBudget);
  return row;
}
async function diagnoseGate(root, gate, row, base, extraEnv = {}, isolationBudget = {}) {
  const started = Date.now();
  const result = await isolateTests(root, row, async (test, remainingMs) => {
    const log = `${row.log}.rerun-${row.failures.indexOf(test) + 1}.log`;
    const reportPath = `${log}.json`;
    const command = commandFor(root, { command: isolatedCommand(test, gate.tests, reportPath) }, base);
    const env = { ...process.env, ...extraEnv, NODE_ENV: 'test', NO_COLOR: '1', RUST_TEST_THREADS: '1', VITEST_MAX_WORKERS: '1' };
    for (const key of ['FORCE_COLOR', 'BUILDMESH_PREFILL', 'NODE_TEST_CONTEXT', 'BUILDMESH_TEST_REPORT']) delete env[key];
    const began = Date.now();
    const exitCode = await runGuarded({ minutes: Math.min(gate.minutes, 5, remainingMs / 60000), killGraceSeconds: 2, log, label: `${gate.id}: ${test.id}`, command, cwd: gate.tests === 'rust' ? join(root, 'src-tauri') : root, env, output: () => {} });
    const output = readFileSync(log, 'utf8');
    if (gate.tests === 'vitest') {
      const report = readTestReport(root, 'vitest', reportPath, output);
      const matches = report.report?.testResults?.filter(suite => resolve(root, suite.name) === resolve(root, test.file)).flatMap(suite => suite.assertionResults ?? []).filter(item => item.fullName === test.name) ?? [];
      const reason = matches.length > 1 ? `Ambiguous test name: ${matches.length} matches in the original file; cannot isolate one test.` : null;
      return { command, log, exitCode, durationMs: Date.now() - began, count: report.count, matched: matches.length === 1 && matches[0].status === 'passed' && report.count === 1 && !report.unattributed, ...(reason ? { reason } : {}) };
    }
    return { command, log, exitCode, durationMs: Date.now() - began, count: executedTests('rust', output) };
  }, knownFlakes(root), isolationBudget);
  Object.assign(row, result);
  row.diagnosed = true;
  row.durationMs += Date.now() - started;
}
function classify(gate, code, output, paths) {
  if (code === 124) return { outcome: 'TIMEOUT', reason: 'Deadline exceeded; investigate a hang or resource contention before retrying.' };
  if (code === 127) return { outcome: 'BLOCKED', reason: 'The command could not be started.' };
  if (code === 2 && (gate.id === 'known-flakes' || gate.tests === 'rust')) {
    const blocked = output.match(/^BLOCKED: (.+)$/m);
    if (blocked) return { outcome: 'BLOCKED', reason: blocked[1] };
    const invalidFlake = gate.id === 'known-flakes' && output.match(/^Known-flake issue #\d+ must be an open issue\.\r?$/m);
    if (invalidFlake) return { outcome: 'FAIL', reason: invalidFlake[0].trim() };
  }
  if (gate.touchedFormat && code !== 0) {
    const { touched, total } = touchedFormatDiffs(output, paths);
    if (!total) return { outcome: 'FAIL', reason: 'rustfmt failed without reporting a formatting diff. See the gate log.' };
    if (touched.length) return { outcome: 'FAIL', reason: `rustfmt diffs in touched files: ${touched.join(', ')}. Format them with node scripts/rustfmt-touched.mjs ${touched.map(path => `"${path}"`).join(' ')}, which restores every other Rust file; bare "rustfmt <file>" also rewrites child modules and "cargo fmt" rewrites the whole crate (#2022).`, formatDiffCount: total };
    return { outcome: 'PASS', count: null, formatDiffCount: total };
  }
  if (code !== 0) return { outcome: 'FAIL', reason: 'Command failed. See the gate log; failure attribution is unverified.' };
  const count = gate.tests ? executedTests(gate.tests, output) : null;
  if (gate.tests && !count) return { outcome: 'FAIL', count: 0, reason: 'No passing tests were executed (or the reporter format is unrecognised).' };
  if (gate.warnings) {
    const diagnostics = output.split('\n').flatMap(line => {
      try { const message = JSON.parse(line); return message.reason === 'compiler-message' && message.message.level === 'warning' ? [message.message] : []; } catch { return []; }
    });
    // The repository has an acknowledged Clippy backlog. Cached dependencies
    // need not replay warnings; Cargo JSON reports current crate diagnostics.
    const touched = diagnostics.filter(message => message.spans.some(span => paths.some(path => path === span.file_name || path === `src-tauri/${span.file_name}`)));
    if (touched.length) return { outcome: 'FAIL', reason: `${touched.length} Rust warnings in touched files.`, warningCount: diagnostics.length };
    return { outcome: 'PASS', count, warningCount: diagnostics.length };
  }
  return { outcome: 'PASS', count };
}
// `plan` lets tests substitute fake gates for the real npm/cargo plan.
export async function verify(root, { base: requestedBase, full = false, plan = planGates } = {}) {
  const task = taskAt(root);
  if (!task && !requestedBase) throw new Error('BLOCKED: start a task or provide --base <commit>; HEAD alone can miss committed work.');
  const base = resolveBase(root, requestedBase ?? task.base);
  if (task && base !== task.base) throw new Error('BLOCKED: verification base must match the active task.');
  const paths = changedPaths(root, base);
  const gates = plan(scopePaths(root, base, paths), { full });
  const tree = fingerprint(root, base);
  const snapshot = treeSnapshot(root);
  const identity = runIdentity(root, base);
  const receipt = { taskId: task?.id ?? null, root, base, head: git(root, 'rev-parse', 'HEAD').trim(), tree, full, paths, gatePlan: gates, gates: [], outcome: 'BLOCKED', startedAt: new Date().toISOString() };
  // Passed gates survive across attempts of one task, so a failure part-way
  // through the plan does not forget the gates it never reached. A cache that
  // is missing, corrupt or another task's only costs time, never correctness.
  const cachePath = statePath(root, 'gate-cache.json');
  let stored = null;
  try { stored = readJson(cachePath); } catch { stored = null; }
  const reusable = stored?.taskId === receipt.taskId && stored.gates && typeof stored.gates === 'object' ? stored.gates : {};
  const cache = { taskId: receipt.taskId, gates: { ...reusable } };
  if (task) {
    setPhase(task, 'verify');
    task.verificationAttempts += 1;
    saveJson(statePath(root, 'active-task.json'), task);
  }
  const started = Date.now();
  mkdirSync(statePath(root, 'logs'), { recursive: true });
  // Persist a nonpassing receipt first so an interrupted attempt cannot leave
  // yesterday's green receipt authorizing completion.
  saveJson(statePath(root, 'receipt.json'), receipt);
  const execute = async (gate, isStopped) => {
    const inputs = gateInputs(gate, snapshot, identity, tree);
    const cached = cache.gates[gate.id];
    if (gate.id !== 'known-flakes' && cached?.outcome === 'PASS' && cached.inputs === inputs) {
      const log = cached.log && existsSync(cached.log) ? cached.log : null;
      console.log(`PASS ${gate.id} (cached, inputs ${inputs.slice(0, 12)})`);
      return { ...cached, cached: true, log };
    }
    let slot = null;
    if (gate.heavy) {
      slot = await acquireSlot({ gate: gate.id, root, onQueued: holders => console.log(`QUEUED ${gate.id}: waiting for a heavy-gate slot (limit ${heavyGateLimit()}; held by ${holders.map(held => `${held.gate} in ${held.root}`).join(', ') || 'a process that just exited'})`) });
    }
    try {
      if (slot && isStopped?.()) return null;
      const row = { ...await runGate(root, gate, base, paths, heavyGateEnv(gate, gates, availableParallelism()), { deferIsolation: true }), inputs };
      if (slot?.queuedMs) row.queuedMs = slot.queuedMs;
      event(root, { type: 'gate', taskId: receipt.taskId, attempt: task?.verificationAttempts ?? null, ...row });
      return row;
    } finally { slot?.release(); }
  };
  const reportRow = row => {
    if (!receipt.gates.includes(row)) receipt.gates.push(row);
    saveJson(statePath(root, 'receipt.json'), receipt);
    console.log(`${row.outcome} ${row.id}${row.count != null ? ` (${row.count} ${row.outcome === 'PASS' ? 'tests' : 'passed'})` : ''}${row.reason ? `: ${row.reason}` : ''}${row.outcome !== 'PASS' && row.log ? `\n  ${row.log}` : ''}`);
    for (const line of failureLines(row)) console.log(line);
  };
  const isolationBudget = {};
  for (;;) {
    await runPlan(gates, {
      execute: (gate, stopped) => receipt.gates.find(row => row.id === gate.id) ?? execute(gate, stopped),
      onResult: row => {
        if (receipt.gates.includes(row)) return;
        if (row.cached) { receipt.gates.push(row); saveJson(statePath(root, 'receipt.json'), receipt); return; }
        if (row.outcome === 'PASS') {
          cache.gates[row.id] = row;
          saveJson(cachePath, cache);
        }
        reportRow(row);
      },
    });
    // runPlan has drained both lanes. Isolated reruns cannot compete with a
    // still-running suite, and the suite itself is never repeated here.
    for (const row of receipt.gates.filter(item => item.outcome === 'FAIL' && item.failures?.length && !item.diagnosed)) {
      await diagnoseGate(root, gates.find(gate => gate.id === row.id), row, base, {}, isolationBudget);
      event(root, { type: 'gate-diagnosis', taskId: receipt.taskId, ...row });
      reportRow(row);
    }
    if (receipt.gates.some(row => !gatePassed(row)) || receipt.gates.length === gates.length) break;
  }
  // Lanes finish in completion order; the receipt reads in plan order.
  receipt.gates.sort((a, b) => gates.findIndex(gate => gate.id === a.id) - gates.findIndex(gate => gate.id === b.id));
  receipt.durationMs = Date.now() - started;
  receipt.finishedAt = new Date().toISOString();
  receipt.outcome = receipt.gates.find(row => !gatePassed(row))?.outcome ?? (receipt.gates.length === gates.length ? 'PASS' : 'BLOCKED');
  if (fingerprint(root, base) !== tree) {
    receipt.outcome = 'FAIL';
    receipt.reason = 'Source changed during verification, including possible generated binding drift. Inspect the diff and rerun.';
    // A gate may have run against the changed source while its key was taken
    // before the change, so nothing this attempt passed may be reused.
    saveJson(cachePath, { taskId: receipt.taskId, gates: reusable });
  }
  saveJson(statePath(root, 'receipt.json'), receipt);
  event(root, { type: 'verification', taskId: receipt.taskId, outcome: receipt.outcome, durationMs: receipt.durationMs, attempt: task?.verificationAttempts ?? null });
  console.log(`${receipt.outcome}: ${receipt.gates.length}/${gates.length} gates${receipt.reason ? `; ${receipt.reason}` : ''}. Receipt: .harness/receipt.json`);
  return receipt;
}
async function evaluate(root, selected) {
  const corpus = readJson(join(root, 'scripts/harness-corpus.json'));
  const cases = selected ? corpus.filter(row => row.id === selected) : corpus;
  if (!cases.length) throw new Error('Unknown evaluation case.');
  const base = git(root, 'rev-parse', 'HEAD').trim();
  const before = fingerprint(root, base);
  const results = [];
  const isolationBudget = {};
  for (const row of cases) {
    const result = row.gate ? await runGate(root, { minutes: 10, ...row.gate, id: row.id }, base, [], {}, { isolationBudget })
      : { id: row.id, outcome: 'BLOCKED', reason: row.gap };
    results.push({ ...result, boundary: row.boundary, remaining: row.remaining });
    event(root, { type: 'evaluation', ...results.at(-1) });
    console.log(`${result.outcome} ${row.id}: ${row.boundary}${result.reason ? `; ${result.reason}` : ''}`);
  }
  const receipt = { results, tree: before, outcome: results.some(row => row.outcome === 'FAIL') ? 'FAIL' : results.find(row => !gatePassed(row))?.outcome ?? 'PASS' };
  if (fingerprint(root, base) !== before) { receipt.outcome = 'FAIL'; receipt.reason = 'Source changed during evaluation.'; }
  saveJson(statePath(root, 'evaluation.json'), receipt);
  return receipt;
}
function checkpoint(root, rollbackRef) {
  const task = taskAt(root);
  if (!task || task.phase === 'complete') throw new Error('An unfinished task is required.');
  const head = resolveBase(root, 'HEAD');
  if (git(root, 'status', '--porcelain').trim()) throw new Error('BLOCKED: checkpoint/rollback recording requires a clean worktree; commit intended changes first.');
  if (rollbackRef) {
    const commit = resolveBase(root, rollbackRef);
    if (!task.checkpoints.some(row => row.commit === commit) || head !== commit) throw new Error('BLOCKED: first restore a recorded checkpoint and confirm HEAD matches it; this command records recovery only.');
    event(root, { type: 'rollback', taskId: task.id, commit });
    return { commit, outcome: 'PASS', reason: 'Rollback use recorded; no Git operation was performed.' };
  }
  const ref = `refs/buildmesh/tasks/${task.id}/${Date.now()}`;
  git(root, 'update-ref', ref, head);
  task.checkpoints.push({ ref, commit: head, at: new Date().toISOString() });
  saveJson(statePath(root, 'active-task.json'), task);
  event(root, { type: 'checkpoint', taskId: task.id, ref, commit: head });
  return { ref, commit: head, reason: 'Committed code checkpoint recorded; task state stays in .harness/.' };
}
export function completion(root) {
  const task = taskAt(root);
  const receipt = readJson(statePath(root, 'receipt.json'));
  if (!task) return { outcome: 'BLOCKED', reason: 'Start a task with npm run harness -- start --spec <json>.' };
  if (!receipt || receipt.taskId !== task.id || receipt.root !== root || receipt.base !== task.base || receipt.tree !== fingerprint(root, task.base)) return { outcome: 'BLOCKED', reason: 'Verification is missing or stale. Run npm run verify.' };
  const expected = planGates(scopePaths(root, task.base, changedPaths(root, task.base)), { full: receipt.full });
  if (JSON.stringify(expected) !== JSON.stringify(receipt.gatePlan)) return { outcome: 'BLOCKED', reason: 'Required gate plan changed. Run npm run verify.' };
  if (receipt.outcome !== 'PASS') return { outcome: receipt.outcome, reason: receipt.reason ?? receipt.gates.find(row => row.outcome !== 'PASS')?.reason ?? 'Incomplete verification.' };
  if (receipt.gates.length !== expected.length || expected.some(gate => !receipt.gates.some(row => row.id === gate.id && gatePassed(row)))) return { outcome: 'BLOCKED', reason: 'Receipt does not include every required gate.' };
  if (task.evidence.length !== task.criteria.length || task.evidence.some(value => !value.trim()) || task.review?.verdict !== 'APPROVE' || task.review.findings.length || task.evidenceTree !== receipt.tree || task.reviewTree !== receipt.tree || task.blockers.length) return { outcome: 'BLOCKED', reason: 'Record one evidence entry per acceptance criterion, independent APPROVE review with no unresolved findings for this tree, and resolve blockers with harness update.' };
  return { outcome: 'PASS', reason: 'Required gates are current; acceptance and review evidence are recorded.' };
}
const MAX_SUMMARY_GATES = 8;
const oneLine = (text, limit = 300) => String(text ?? '').replace(/\s+/g, ' ').trim().slice(0, limit);
// At most 2 lines per failing gate plus a header, whatever the plan size, so a
// waiting agent reads a bounded result instead of the whole receipt.
function receiptSummary(root, held) {
  const receipt = readJson(statePath(root, 'receipt.json'));
  if (!receipt) return { outcome: 'BLOCKED', lines: ['BLOCKED: no receipt exists. Run npm run verify.'] };
  const progress = receipt.gates.map(row => `${row.id}:${row.outcome}`).join(' ');
  // A verify that died before writing its first receipt leaves yesterday's file.
  if (held && Date.parse(receipt.startedAt) < Date.parse(held.startedAt)) return { outcome: 'BLOCKED', lines: ['BLOCKED: the verify run ended without writing a receipt; read its own output for the reason.'] };
  if (!receipt.finishedAt) return { outcome: 'BLOCKED', lines: [`BLOCKED: no verify is running and the last one never finished (interrupted?). Gates recorded: ${progress || 'none'}. Rerun npm run verify.`] };
  const failed = receipt.gates.filter(row => row.outcome !== 'PASS');
  const lines = [`${receipt.outcome}: ${receipt.gates.length}/${receipt.gatePlan.length} gates in ${Math.round(receipt.durationMs / 1000)}s${receipt.reason ? `; ${oneLine(receipt.reason)}` : ''}. Receipt: .harness/receipt.json`];
  for (const row of failed.slice(0, MAX_SUMMARY_GATES)) {
    lines.push(`${row.outcome} ${row.id}: ${oneLine(row.reason)}`);
    if (row.log) lines.push(`  ${row.log}`);
  }
  let named = 0;
  for (const row of failed) {
    if (named === 10) break;
    lines.push(...failureLines(row, 10 - named));
    named += Math.min(row.failures?.length ?? 0, 10 - named);
  }
  if (failed.length > MAX_SUMMARY_GATES) lines.push(`(+${failed.length - MAX_SUMMARY_GATES} more nonpassing gates in the receipt)`);
  return { outcome: receipt.outcome, lines };
}
// Block until no harness operation holds .harness/lock (a verify holds it for
// its whole run), then summarise the receipt. Read-only, so it never competes
// for the lock. `graceMs` covers a verify launched in the background a moment
// before this call, which has not taken the lock yet.
export async function waitForVerify(root, { maxMs = 540000, graceMs = 2000, pollMs = 1000, sleep = ms => new Promise(done => setTimeout(done, ms)) } = {}) {
  const readLock = () => {
    const path = statePath(root, 'lock');
    if (!existsSync(path)) return null;
    // A lock being written is momentarily empty; treat it as held.
    try { return readJson(path) ?? { pid: null }; } catch { return { pid: null }; }
  };
  const started = Date.now();
  let held = readLock();
  for (let until = started + graceMs; !held && Date.now() < until; held = readLock()) await sleep(Math.min(200, graceMs));
  let seen = held && typeof held.startedAt === 'string' ? held : null;
  while (held) {
    if (typeof held.pid === 'number' && !alive(held.pid)) break;
    if (Date.now() - started >= maxMs) {
      const receipt = readJson(statePath(root, 'receipt.json'));
      const progress = receipt?.gates.map(row => `${row.id}:${row.outcome}`).join(' ');
      return { outcome: 'TIMEOUT', lines: [`TIMEOUT: still running after ${Math.round((Date.now() - started) / 1000)}s (${held.action ?? 'harness operation'}, pid ${held.pid ?? 'unknown'}). Gates so far: ${progress || 'none'}. Run npm run harness -- wait again.`] };
    }
    await sleep(Math.min(pollMs, Math.max(1, maxMs - (Date.now() - started))));
    held = readLock();
    if (held && typeof held.startedAt === 'string') seen = held;
  }
  return receiptSummary(root, seen);
}
function status(root) {
  const task = taskAt(root);
  const receipt = readJson(statePath(root, 'receipt.json'));
  return { branch: git(root, 'branch', '--show-current').trim(), root, gitStatus: git(root, 'status', '--short').trim(), task, verification: receipt ? { outcome: receipt.outcome, current: receipt.tree === fingerprint(root, receipt.base), gates: receipt.gates.map(row => `${row.id}:${row.outcome}`) } : null };
}
function metrics(root) {
  const path = statePath(root, 'events.jsonl');
  const events = existsSync(path) ? readFileSync(path, 'utf8').split('\n').filter(Boolean).map(JSON.parse) : [];
  const outcomes = { PASS: 0, FAIL: 0, FLAKY: 0, BLOCKED: 0, TIMEOUT: 0 };
  for (const row of events.filter(row => row.type === 'verification')) outcomes[row.outcome] += 1;
  return {
    verificationOutcomes: outcomes,
    verificationMs: events.filter(row => row.type === 'verification').reduce((sum, row) => sum + row.durationMs, 0),
    failedGates: events.filter(row => row.type === 'gate' && row.outcome !== 'PASS').map(({ id, outcome, attempt }) => ({ id, outcome, attempt })),
    checkpoints: events.filter(row => row.type === 'checkpoint').length,
    rollbacks: events.filter(row => row.type === 'rollback').length,
    tasks: events.filter(row => row.type === 'task-start').map(({ taskId, agent, model }) => ({ taskId, agent, model })),
    completedPhaseDurationsMs: events.filter(row => row.type === 'task-finish').map(({ taskId, phaseDurationsMs }) => ({ taskId, phaseDurationsMs })),
  };
}
function finish(root) {
  const result = completion(root);
  if (result.outcome === 'PASS') {
    const task = taskAt(root);
    setPhase(task, 'complete');
    task.finishedAt = new Date().toISOString();
    saveJson(statePath(root, 'active-task.json'), task);
    event(root, { type: 'task-finish', taskId: task.id, elapsedMs: Date.now() - Date.parse(task.startedAt), verificationAttempts: task.verificationAttempts, phaseDurationsMs: task.phaseDurationsMs });
  }
  return result;
}
async function hook(root, payload) {
  if (payload.hook_event_name === 'SessionStart') {
    const current = status(root);
    console.log(JSON.stringify({ hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: `Buildmesh task context: ${JSON.stringify(current).slice(0, 5000)}\nRecent commits:\n${git(root, 'log', '-3', '--oneline')}\nUse npm run harness -- status to resume; npm run verify before finish. Read only the relevant architecture sections.` } }));
  } else if (payload.hook_event_name === 'PostToolUse' && taskAt(root)?.phase !== 'complete' && taskAt(root)) {
    let release;
    try { release = lock(root, 'fast-lock'); } catch { return; }
    try {
      const task = taskAt(root);
      const tree = fingerprint(root, task.base);
      if (readJson(statePath(root, 'fast.json'))?.tree === tree) return;
      const paths = changedPaths(root, task.base);
      const gates = [{ id: 'fast-agent-rules', command: ['node', 'scripts/check-agent-diff.mjs', '--base', '$BASE'], minutes: 1 }];
      const lintable = paths.filter(path => /^(?:src|tests|scripts)\/.+\.(?:tsx?|m?js)$/.test(path) && !path.startsWith('src/types/generated/') && !path.startsWith('tests/lint-fixtures/') && existsSync(join(root, path)));
      if (lintable.length) gates.push({ id: 'fast-lint', command: ['node', 'node_modules/eslint/bin/eslint.js', '--max-warnings', '0', ...lintable], minutes: 2 });
      if (paths.some(path => /^src\/.+\.tsx?$/.test(path))) gates.push({ id: 'fast-types', command: ['node', 'node_modules/typescript/bin/tsc', '--noEmit'], minutes: 2 });
      const results = [];
      for (const gate of gates) {
        const result = await runGate(root, gate, task.base, paths);
        results.push(result);
        if (result.outcome !== 'PASS') break;
      }
      if (fingerprint(root, task.base) !== tree) return;
      saveJson(statePath(root, 'fast.json'), { tree, results });
      const failed = results.find(row => row.outcome !== 'PASS');
      if (failed) console.log(JSON.stringify({ hookSpecificOutput: { hookEventName: 'PostToolUse', additionalContext: `Fast check ${failed.outcome}: ${failed.id}; ${failed.reason}; log ${failed.log}. Full verification is still required.` } }));
    } finally { release(); }
  } else if (payload.hook_event_name === 'PreToolUse') {
    const path = payload.tool_input?.file_path;
    const full = path ? resolve(root, path).replaceAll('\\', '/').toLowerCase() : '';
    const protectedRoot = `${statePath(root, '').replaceAll('\\', '/').toLowerCase()}/`;
    if (full.startsWith(protectedRoot)) {
      console.log(JSON.stringify({ hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: 'Update task state through npm run harness -- update; verification receipts are written by npm run verify.' } }));
    }
  } else if (payload.hook_event_name === 'Stop' && taskAt(root)) {
    const result = completion(root);
    if (result.outcome !== 'PASS') {
      const task = taskAt(root);
      // A recorded blocked handoff is released on the very first stop, so the
      // agent is not re-blocked once per later turn. It never updates
      // phase=complete, and finish still returns nonzero.
      if (task.phase === 'blocked' && task.blockers.length) return;
      if (payload.stop_hook_active && ['BLOCKED', 'TIMEOUT'].includes(result.outcome)) return;
      // FAIL is never released by a written report: only recorded blockers permit a handoff, so say how to record them.
      const escape = result.outcome === 'FAIL' ? ' If the failure cannot be repaired here, write {"phase": "blocked", "blockers": ["<why>"]} to a JSON file and run npm run harness -- update --spec <file>; that permits an incomplete handoff.' : '';
      console.log(JSON.stringify({ decision: 'block', reason: `${result.outcome}: ${result.reason} Complete verification and evidence, or report an incomplete handoff with this diagnostic.${escape} Do not claim success.` }));
    }
  }
}
async function internalGate(root, name) {
  if (name === 'flakes') {
    const entries = knownFlakes(root);
    try { await validateFlakeIssues(entries); }
    catch (error) {
      if (!error.message.includes('must be an open issue')) throw new Error(`BLOCKED: Cannot verify known-flake issues: ${error.message}`);
      throw error;
    }
    return;
  }
  if (name === 'whitespace') git(root, 'diff', '--check', taskAt(root)?.base ?? process.env.BUILDMESH_VERIFY_BASE, '--');
  else if (name === 'staging') {
    const staged = new Set(git(root, 'diff', '--cached', '--no-renames', '--name-only', '-z', '--').split('\0').filter(Boolean));
    const working = [...git(root, 'diff', '--no-renames', '--name-only', '-z', '--').split('\0'), ...git(root, 'ls-files', '--others', '--exclude-standard', '-z').split('\0')];
    const mismatches = working.filter(path => staged.has(path));
    if (mismatches.length) throw new Error(`Staged content differs from the tested working tree: ${mismatches.join(', ')}. Stage the intended final content before verification.`);
  }
  else if (name === 'bindings') {
    const expected = process.env.BUILDMESH_BINDINGS_FINGERPRINT;
    if (!expected || fingerprint(root, process.env.BUILDMESH_VERIFY_BASE, 'src/types/generated/') !== expected) throw new Error('Generated bindings changed during verification; inspect the diff and rerun.');
  } else throw new Error('Unknown internal gate.');
}
function parse(argv) {
  const options = { action: argv[0] };
  for (let i = 1; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--full') options.full = true;
    else if (flag === '--max-seconds' && Number(argv[i + 1]) > 0) options.maxSeconds = Number(argv[++i]);
    else if (['--spec', '--base', '--case', '--ref'].includes(flag) && argv[i + 1]) options[flag.slice(2)] = argv[++i];
    else throw new Error(USAGE);
  }
  return options;
}
async function main() {
  let release;
  const env = { ...process.env };
  try {
    const root = resolve(git(process.cwd(), 'rev-parse', '--show-toplevel').trim());
    process.chdir(root);
    const args = process.argv.slice(2);
    if (args[0] === 'hook') {
      let payload;
      try { payload = JSON.parse(readFileSync(0, 'utf8')); } catch { return; }
      if (payload && typeof payload === 'object') await hook(root, payload);
      return;
    }
    if (args[0] === 'gate') { await internalGate(root, args[1]); return; }
    const options = parse(args);
    if (options.action === 'status') { console.log(JSON.stringify(status(root), null, 2)); return; }
    if (options.action === 'metrics') { console.log(JSON.stringify(metrics(root), null, 2)); return; }
    if (options.action === 'wait') {
      const result = await waitForVerify(root, { maxMs: (options.maxSeconds ?? 540) * 1000 });
      console.log(result.lines.join('\n'));
      process.exitCode = EXIT[result.outcome];
      return;
    }
    release = lock(root, 'lock', options.action);
    process.env.NODE_ENV = 'test';
    process.env.NO_COLOR = '1';
    delete process.env.FORCE_COLOR;
    delete process.env.BUILDMESH_PREFILL;
    // Match the existing Windows wrapper's native tool selection.
    if (process.platform === 'win32' && existsSync('C:/Program Files/Git/cmd/git.exe')) process.env.PATH = `C:/Program Files/Git/cmd;C:/Program Files/Git/usr/bin;${process.env.PATH}`;
    let result;
    if (options.action === 'start' || options.action === 'update') {
      if (!options.spec) throw new Error(USAGE);
      result = (options.action === 'start' ? start : update)(root, readJson(resolve(options.spec)));
    } else if (options.action === 'verify') {
      process.env.BUILDMESH_VERIFY_BASE = options.base ?? taskAt(root)?.base ?? '';
      if (!process.env.BUILDMESH_VERIFY_BASE) throw new Error('BLOCKED: start a task or provide --base <commit>.');
      process.env.BUILDMESH_BINDINGS_FINGERPRINT = fingerprint(root, resolveBase(root, process.env.BUILDMESH_VERIFY_BASE), 'src/types/generated/');
      result = await verify(root, options);
      process.exitCode = EXIT[result.outcome];
    } else if (options.action === 'finish') {
      result = finish(root);
      process.exitCode = EXIT[result.outcome];
    } else if (options.action === 'evaluate') {
      result = await evaluate(root, options.case);
      process.exitCode = EXIT[result.outcome];
    } else if (options.action === 'checkpoint' || options.action === 'record-rollback') {
      if (options.action === 'record-rollback' && !options.ref) throw new Error(USAGE);
      result = checkpoint(root, options.ref);
    } else throw new Error(USAGE);
    if (options.action !== 'verify') console.log(JSON.stringify(result, null, 2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  } finally {
    if (release) release();
    for (const key of Object.keys(process.env)) if (!(key in env)) delete process.env[key];
    Object.assign(process.env, env);
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
