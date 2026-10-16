#!/usr/bin/env node
// Run the whole Rust suite locally as concurrent processes, one per CI shard.
//
// The CI shard split (`.github/workflows/verify.yml`, read through
// scripts/ci/rust-shards.mjs) exists to localise runner loss (#1520): a job
// that dies takes exactly one group with it, and every sibling keeps its log
// and its conclusion. Running one process per shard here reproduces that
// isolation on one machine, and a hang in one group cannot take the rest of
// the suite's evidence with it.
//
// The processes do not serialise their tests (issue #2048). Every DB-backed
// test installs its own database for its own thread, so each test binary is
// internally multi-threaded and the default thread count is the correct
// setting; `--test-threads=1` here would only throw away cores.
//
// Cargo holds the build-directory lock for the whole of `cargo test`, test
// execution included, so concurrent `cargo test` calls on one target directory
// still run one at a time. This compiles the test targets, copies those test
// binaries out of the target directory, then executes the copies. The lock is
// released when the compile exits, and another worktree sharing that directory
// can replace the file before this run executes the path it recorded. Cargo
// also keys that directory by package name and version, not by the worktree
// path, so the other worktree's unchanged files can look fresh and `cargo test
// --no-run` reports that executable (issue #2339). ts-rs writes the shape
// compiled into whichever binary actually runs, into this run's working
// directory. The Buildmesh library binary must carry this worktree's stamp.
// A copy that does not is refused, this repository's packages are deleted from
// the shared target, and the compile is repeated. Registry dependency
// artifacts stay. Doctests are compiled by rustdoc at run time, so they alone
// go through `cargo test --doc`, which holds the lock for that whole
// invocation. Processes run a few at a time (see `slots` below), not all at once.
//
// A passing process prints only its `test result:` lines; a failing one prints
// its full output. Exit code: 0 when every process passed, otherwise the first
// failing process's code (1 when it reported none).
//
// A process counts as finished when it exits. A descendant that inherited its
// stdout/stderr keeps those pipes open, and waiting for them would leave a test
// process that is already gone looking like it were still running (issue #2374),
// so each process's output is collected for a bounded drain after its `exit`.
//
// Usage: node scripts/rust-test-shards.mjs   (from anywhere in the repository)

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { drainBounded } from './ci/run-guarded.mjs';
import { readShards, repoRoot } from './ci/rust-shards.mjs';
import { rustFailures } from './harness-test-failures.mjs';

// FNV-1a 64-bit of the manifest directory. src-tauri/build.rs hashes
// CARGO_MANIFEST_DIR with the same function and bakes it into the library
// test binary. A random stamp would rebuild the package on every run; a
// shared target would otherwise treat another worktree's package as fresh.
export function artifactOwnerToken(manifestDir) {
  const bytes = Buffer.from(path.resolve(manifestDir));
  let hash = 0xcbf29ce484222325n;
  const mask = 0xffffffffffffffffn;
  for (const byte of bytes) {
    hash ^= BigInt(byte);
    hash = (hash * 0x100000001b3n) & mask;
  }
  return `buildmesh-artifact-owner-${hash.toString(16).padStart(16, '0')}`;
}

// The first `name` in the file can belong to a workspace or a dependency.
export function packageName(cargoToml) {
  const match = cargoToml.match(/(?:^|\n)\[package\][^\n]*\n([\s\S]*?)(?:\n\[|$)/);
  return match?.[1].match(/^\s*name\s*=\s*"([^"]+)"/m)?.[1] ?? '';
}

// The root package and in-repo path dependencies. Their artifacts share a
// target directory across worktrees; registry crates do not need to.
export function localPackageNames(cargoToml) {
  const names = new Set();
  const root = packageName(cargoToml);
  if (root) names.add(root);
  for (const match of cargoToml.matchAll(/^\s*([A-Za-z0-9_-]+)\s*=\s*\{[^}\n]*\bpath\s*=/gm)) names.add(match[1]);
  return [...names];
}

export function fileContains(file, text) {
  const needle = Buffer.from(text);
  const fd = fs.openSync(file, 'r');
  try {
    const buf = Buffer.alloc(1024 * 1024);
    let carry = Buffer.alloc(0);
    for (;;) {
      const n = fs.readSync(fd, buf, 0, buf.length, null);
      if (n === 0) return false;
      const chunk = Buffer.concat([carry, buf.subarray(0, n)]);
      if (chunk.includes(needle)) return true;
      carry = chunk.subarray(Math.max(0, chunk.length - (needle.length - 1)));
    }
  } finally {
    fs.closeSync(fd);
  }
}

export function assertOwnedLibraryBinary(executable, token) {
  const owned = Buffer.isBuffer(executable)
    ? executable.includes(Buffer.from(token))
    : fileContains(executable, token);
  if (!owned) {
    throw new Error(`The Buildmesh library test binary does not carry this worktree's Cargo artifact stamp (${token}). A shared target directory was rebuilt by another worktree before this run could take its own test executable. Refusing to run it so that executable cannot export the other worktree's wire types into this tree.`);
  }
}

export function parseTestExecutables(stdout) {
  return stdout.split(/\r?\n/).flatMap((line) => {
    try {
      const message = JSON.parse(line);
      return message.reason === 'compiler-artifact' && message.profile?.test && message.executable
        ? [{ kinds: message.target.kind, name: message.target.name, executable: message.executable }]
        : [];
    } catch {
      return [];
    }
  });
}

export function libraryTestExecutable(executables) {
  return executables.find((target) => !target.kinds.some((kind) => ['bin', 'test', 'bench', 'example'].includes(kind)));
}

// The Windows loader resolves ConPTY from the directory of the test executable.
const CONPTY_RUNTIME = ['conpty.dll', 'x64/OpenConsole.exe', 'arm64/OpenConsole.exe', 'x86/OpenConsole.exe'];

// Copy the executables Cargo just wrote. A later build into the same target
// directory replaces those files in place; the copies keep the bytes this
// compile produced.
export function snapshotExecutables(executables, destDir) {
  fs.rmSync(destDir, { recursive: true, force: true });
  fs.mkdirSync(destDir, { recursive: true });
  const copiedRuntime = new Set();
  return executables.map((target) => {
    const base = path.basename(target.executable);
    const dest = path.join(destDir, base);
    if (fs.existsSync(dest)) throw new Error(`Two test executables share the file name ${base}.`);
    fs.copyFileSync(target.executable, dest);
    const sourceDir = path.dirname(target.executable);
    if (!copiedRuntime.has(sourceDir)) {
      copiedRuntime.add(sourceDir);
      for (const relative of CONPTY_RUNTIME) {
        const from = path.join(sourceDir, relative);
        if (!fs.existsSync(from)) continue;
        const to = path.join(destDir, relative);
        fs.mkdirSync(path.dirname(to), { recursive: true });
        fs.copyFileSync(from, to);
      }
    }
    return { ...target, executable: dest };
  });
}

const crateDir = path.join(repoRoot, 'src-tauri');

// The `[env]` table of the crate's cargo config: simple `KEY = "value"` lines.
function cargoConfigEnv() {
  const configPath = path.join(crateDir, '.cargo', 'config.toml');
  if (!fs.existsSync(configPath)) return {};
  const values = {};
  let inEnv = false;
  for (const line of fs.readFileSync(configPath, 'utf8').split(/\r?\n/)) {
    const section = line.match(/^\s*\[(.+)\]\s*$/);
    if (section) inEnv = section[1].trim() === 'env';
    const entry = inEnv && line.match(/^\s*([A-Za-z_][\w]*)\s*=\s*"([^"]*)"\s*$/);
    if (entry) values[entry[1]] = entry[2];
  }
  return values;
}

// Cargo itself gets the caller's environment unchanged: build scripts re-run
// when their environment changes, so adding variables here would rebuild
// dependencies and invalidate the cache for ordinary cargo runs.
const cargoEnv = { ...process.env, CARGO_TERM_COLOR: 'never' };
delete cargoEnv.BUILDMESH_PREFILL;
// What `cargo test` gives a test binary it runs.
const testEnv = { ...cargoEnv, ...cargoConfigEnv(), CARGO_MANIFEST_DIR: crateDir };

// How long a finished child's output is still collected after it exits. Long
// enough for the last of its output to cross the pipe, short enough that a
// lingering descendant cannot hold up the suite. Same cap the CI guard drains
// its pipes with (issue #1961, reused by `drainBounded`).
const DRAIN_CAP_MS = 3_000;

export function run(label, command, args, { stream = false, env = cargoEnv, cwd = crateDir, drainCapMs = DRAIN_CAP_MS } = {}) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(command, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '';
    let stdout = '';
    let drainTimer = null;
    let settled = false;
    const collect = (chunk, isStdout) => {
      output += chunk;
      if (isStdout) stdout += chunk;
      if (stream && !isStdout) process.stdout.write(chunk);
    };
    const finish = (code, overrides = {}) => {
      if (settled) return;
      settled = true;
      clearTimeout(drainTimer);
      // The child is gone, so these read ends are the only handles left on its
      // pipes. Releasing them lets the runner exit once the child's result is
      // known, whatever a descendant that inherited them still does with them.
      if (!child.stdout.destroyed) child.stdout.destroy();
      if (!child.stderr.destroyed) child.stderr.destroy();
      resolve({ label, code, output, stdout, seconds: Math.round((Date.now() - started) / 1000), ...overrides });
    };
    child.stdout.on('data', (chunk) => collect(chunk, true));
    child.stderr.on('data', (chunk) => collect(chunk, false));
    child.on('error', (error) => finish(127, { output: `${output}\n${error.message}\n`, seconds: 0 }));
    // The child's `exit` is its result; the drain only collects what it left in
    // the pipes. Node's `close` waits for every descendant holding an inherited
    // pipe, so a finished test process would otherwise read as still running
    // until a stray conhost or cargo child let go (issue #2374).
    child.on('exit', (code) => {
      drainTimer = drainBounded([child.stdout, child.stderr], drainCapMs, () => finish(code ?? 1));
    });
  });
}

// Compiles the crate's test binaries and returns private copies. The Buildmesh
// package's library binary is checked for `token` after the copy. Cargo can
// report another worktree's executable as fresh; that copy is refused, the
// local packages are removed from the target directory, and the compile is
// tried again. Registry dependency artifacts are left in place.
export async function compileOwnedTestBinaries(crateDir, { targetDir, env = process.env, token, snapshotDir, stream = false, attempts = 3 } = {}) {
  const manifestDir = path.resolve(crateDir);
  const owner = token ?? artifactOwnerToken(manifestDir);
  const compileEnv = { ...env, CARGO_TERM_COLOR: 'never' };
  if (targetDir) compileEnv.CARGO_TARGET_DIR = targetDir;
  delete compileEnv.BUILDMESH_PREFILL;
  const ownedDir = snapshotDir ?? path.join(manifestDir, '.owned-test-binaries');
  const cargoToml = fs.readFileSync(path.join(manifestDir, 'Cargo.toml'), 'utf8');
  const requireStamp = packageName(cargoToml) === 'buildmesh';
  const localPackages = localPackageNames(cargoToml);
  let lastError;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const build = await run('compile', 'cargo', ['test', '--locked', '--no-run', '--message-format=json-render-diagnostics'], { cwd: manifestDir, env: compileEnv, stream });
    if (build.code !== 0) throw new Error(`cargo test --no-run failed (exit ${build.code}):\n${build.output}`);
    const compiled = [];
    const seen = new Set();
    for (const target of parseTestExecutables(build.stdout)) {
      if (seen.has(target.executable)) continue;
      seen.add(target.executable);
      compiled.push(target);
    }
    const cargoLib = libraryTestExecutable(compiled);
    if (!cargoLib) throw new Error('Cargo reported no unit-test executable for the library target.');
    try {
      const executables = snapshotExecutables(compiled, ownedDir);
      const lib = libraryTestExecutable(executables);
      if (requireStamp) assertOwnedLibraryBinary(lib.executable, owner);
      return { token: owner, lib: lib.executable, executables, profileDir: path.dirname(path.dirname(cargoLib.executable)) };
    } catch (error) {
      lastError = error;
      const foreign = String(error.message).includes('artifact stamp');
      if (attempt === attempts || !foreign) throw error;
      console.error(`Shared Cargo target returned another worktree's test binary. Rebuilding ${localPackages.join(', ')} from this worktree; registry dependency artifacts stay.`);
      const clean = await run('clean', 'cargo', ['clean', ...localPackages.flatMap((name) => ['-p', name])], { cwd: manifestDir, env: compileEnv, stream });
      // eslint-disable-next-line preserve-caught-error -- the caught error is the foreign artifact stamp that triggered the `cargo clean` retry, not what made the clean itself fail. `lastError` (set above) is the error this loop ultimately rethrows if the retries run out.
      if (clean.code !== 0) throw new Error(`cargo clean failed (exit ${clean.code}):\n${clean.output}`);
    }
  }
  throw lastError;
}

async function main() {
  const snapshotDir = path.join(repoRoot, '.harness', 'rust-test-binaries', String(process.pid));
  const cleanup = () => {
    try {
      fs.rmSync(snapshotDir, { recursive: true, force: true });
    } catch {
      // A just-exited Windows image can stay locked for a moment. The directory
      // is under the ignored .harness tree.
    }
  };
  let exitCode = 0;
  try {
    console.log('Compiling every Rust test target once...');
    const compiled = await compileOwnedTestBinaries(crateDir, { snapshotDir, env: cargoEnv, stream: true });
    const { executables } = compiled;
    // A Tauri crate's library reports its crate types (`staticlib`, `cdylib`,
    // `rlib`) rather than `lib`, so recognise it as the non-bin/test/bench target.
    const lib = libraryTestExecutable(executables);

    // The build script stages ConPTY beside the Cargo artifacts. The copies the
    // tests run from must have those files too.
    if (process.platform === 'win32') {
      const runtime = path.dirname(lib.executable);
      const missing = CONPTY_RUNTIME.filter(file => !fs.existsSync(path.join(runtime, file)));
      if (missing.length) {
        console.error(`BLOCKED: Windows ConPTY runtime is not staged beside Rust test binaries: ${missing.join(', ')}. Run node scripts/prepare-conpty.mjs ${process.arch === 'arm64' ? 'aarch64' : process.arch === 'ia32' ? 'x86' : 'x86_64'} "${compiled.profileDir}".`);
        exitCode = 2;
        return;
      }
    }

// Every process runs its tests at libtest's default (multi-threaded) count:
// the per-test database seam makes that safe (issue #2048).
// BUILDMESH_RUST_TEST_THREADS=1 is the diagnostic single-thread override.
const threadArgs = process.env.BUILDMESH_RUST_TEST_THREADS ? ['--test-threads', process.env.BUILDMESH_RUST_TEST_THREADS] : [];
const jobs = [
  ...readShards().map(({ label, args }) => ({ label, command: lib.executable, args: [...args.split(/\s+/).filter(Boolean), ...threadArgs], env: testEnv, target: { kind: 'lib' } })),
  ...executables
    .filter((target) => target !== lib)
    .map((target) => ({ label: `${target.kinds.join('+')}:${target.name}`, command: target.executable, args: threadArgs, env: testEnv, target: { kind: target.kinds.includes('test') ? 'test' : target.kinds[0], name: target.name } })),
  { label: 'doc', command: 'cargo', args: ['test', '--locked', '--doc', ...(threadArgs.length ? ['--', ...threadArgs] : [])], target: { kind: 'doc', name: lib.name } },
];

// The longest jobs (doctests, then the `services` shard) set the suite's
// wall time, so a few slots filled longest-first keep nearly all of the
// speed-up. More processes than that only add CPU contention.
// BUILDMESH_RUST_TEST_JOBS overrides the limit.
const slots = Number(process.env.BUILDMESH_RUST_TEST_JOBS) || Math.max(2, Math.min(4, Math.floor(os.availableParallelism() / 4)));
const queue = [...jobs].sort((a, b) => Number(b.label === 'doc') - Number(a.label === 'doc'));
console.log(`\nRunning ${jobs.length} test processes, ${slots} at a time: ${queue.map((job) => job.label).join(', ')}`);
const results = [];
await Promise.all(
  Array.from({ length: Math.min(slots, queue.length) }, async () => {
    for (let job = queue.shift(); job; job = queue.shift()) {
      results.push({ ...await run(job.label, job.command, job.args, { env: job.env }), target: job.target });
    }
  }),
);

if (process.env.BUILDMESH_TEST_REPORT) {
  const failures = results.flatMap(result => result.code === 0 ? [] : rustFailures(result.output, result.target));
  const count = results.reduce((sum, result) => sum + [...result.output.matchAll(/test result: (?:ok|FAILED)\. (\d+) passed/g)].reduce((total, match) => total + Number(match[1]), 0), 0);
  const unattributed = results.some(result => {
    if (result.code === 0) return false;
    const failedCount = [...result.output.matchAll(/test result: FAILED\. \d+ passed; (\d+) failed/g)].reduce((sum, match) => sum + Number(match[1]), 0);
    return !failedCount || rustFailures(result.output, result.target).length !== failedCount;
  });
  fs.writeFileSync(process.env.BUILDMESH_TEST_REPORT, JSON.stringify({ failures, count, unattributed }));
}

let failed = null;
for (const result of results) {
  const summaries = result.output.split(/\r?\n/).filter((line) => line.startsWith('test result:'));
  if (result.code === 0) {
    console.log(`\n[${result.label}] passed in ${result.seconds}s`);
    for (const line of summaries) console.log(line);
  } else {
    failed ??= result;
    console.log(`\n[${result.label}] FAILED (exit ${result.code}) in ${result.seconds}s - full output:\n${result.output}`);
  }
}

    if (failed) {
      console.error(`\nRust tests failed: ${results.filter((result) => result.code !== 0).map((result) => result.label).join(', ')}`);
      console.error('npm run verify names these failures and reruns each exact test alone once. Inspect its receipt for FAIL or FLAKY outcomes.');
      exitCode = failed.code || 1;
      return;
    }
    console.log(`\nRust tests passed in ${jobs.length} processes, ${slots} at a time.`);
  } catch (error) {
    console.error(`\n${error.message}`);
    exitCode = 1;
  } finally {
    cleanup();
    if (exitCode !== 0) process.exit(exitCode);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
