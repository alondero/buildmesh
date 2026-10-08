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
// still run one at a time. This compiles every target once, then executes the
// compiled test binaries directly, the way `cargo test` would: from the crate
// directory, with the crate's `.cargo/config.toml` [env] applied (ts-rs reads
// TS_RS_EXPORT_DIR at runtime). Doctests are compiled by rustdoc at run time,
// so they alone go through `cargo test --doc`, beside the binaries. Processes
// run a few at a time (see `slots` below), not all at once.
//
// A passing process prints only its `test result:` lines; a failing one prints
// its full output. Exit code: 0 when every process passed, otherwise the first
// failing process's code (1 when it reported none).
//
// Usage: node scripts/rust-test-shards.mjs   (from anywhere in the repository)

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { readShards, repoRoot } from './ci/rust-shards.mjs';
import { rustFailures } from './harness-test-failures.mjs';

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

function run(label, command, args, { stream = false, env = cargoEnv } = {}) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(command, args, { cwd: crateDir, env, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '';
    let stdout = '';
    const collect = (chunk, isStdout) => {
      output += chunk;
      if (isStdout) stdout += chunk;
      if (stream && !isStdout) process.stdout.write(chunk);
    };
    child.stdout.on('data', (chunk) => collect(chunk, true));
    child.stderr.on('data', (chunk) => collect(chunk, false));
    child.on('error', (error) => resolve({ label, code: 127, output: `${output}\n${error.message}\n`, stdout, seconds: 0 }));
    child.on('close', (code) => resolve({ label, code: code ?? 1, output, stdout, seconds: Math.round((Date.now() - started) / 1000) }));
  });
}

console.log('Compiling every Rust test target once...');
const build = await run('compile', 'cargo', ['test', '--locked', '--no-run', '--message-format=json-render-diagnostics'], { stream: true });
if (build.code !== 0) {
  console.error(`\nRust test compile failed (exit ${build.code}).`);
  process.exit(build.code);
}

// Cargo reports each compiled test executable with its target kind.
const executables = build.stdout
  .split(/\r?\n/)
  .flatMap((line) => {
    try {
      const message = JSON.parse(line);
      return message.reason === 'compiler-artifact' && message.profile?.test && message.executable
        ? [{ kinds: message.target.kind, name: message.target.name, executable: message.executable }]
        : [];
    } catch {
      return [];
    }
  });
// A Tauri crate's library reports its crate types (`staticlib`, `cdylib`,
// `rlib`) rather than `lib`, so recognise it as the non-bin/test/bench target.
const lib = executables.find((target) => !target.kinds.some((kind) => ['bin', 'test', 'bench', 'example'].includes(kind)));
if (!lib) {
  console.error('Cargo reported no unit-test executable for the library target.');
  process.exit(1);
}

// The build script stages ConPTY; report a missing runtime before tests run.
if (process.platform === 'win32') {
  const runtime = path.dirname(lib.executable);
  const missing = ['conpty.dll', 'x64/OpenConsole.exe', 'arm64/OpenConsole.exe', 'x86/OpenConsole.exe'].filter(file => !fs.existsSync(path.join(runtime, file)));
  if (missing.length) {
    console.error(`BLOCKED: Windows ConPTY runtime is not staged beside Rust test binaries: ${missing.join(', ')}. Run node scripts/prepare-conpty.mjs ${process.arch === 'arm64' ? 'aarch64' : process.arch === 'ia32' ? 'x86' : 'x86_64'} "${path.dirname(runtime)}".`);
    process.exit(2);
  }
}

// Every process runs its tests at libtest's default (multi-threaded) count:
// the per-test database seam makes that safe (issue #2048).
const jobs = [
  ...readShards().map(({ label, args }) => ({ label, command: lib.executable, args: args.split(/\s+/).filter(Boolean), env: testEnv, target: { kind: 'lib' } })),
  ...executables
    .filter((target) => target !== lib)
    .map((target) => ({ label: `${target.kinds.join('+')}:${target.name}`, command: target.executable, args: [], env: testEnv, target: { kind: target.kinds.includes('test') ? 'test' : target.kinds[0], name: target.name } })),
  { label: 'doc', command: 'cargo', args: ['test', '--locked', '--doc'], target: { kind: 'doc', name: lib.name } },
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
  process.exit(failed.code || 1);
}
console.log(`\nRust tests passed in ${jobs.length} processes, ${slots} at a time.`);
