#!/usr/bin/env node
// agent-tests.mjs — the owner of the agent-infrastructure suite's scheduling.
//
// The suite's contract tests are not cheap: they compile Rust, build a Tauri
// app and run real Vitest fixtures in temporary directories. Node's test
// runner starts one process per test file, and its default concurrency is the
// machine's core count minus one, so a 24-core workstation ran 23 of the
// suite's 30 files at once while a 2-core CI runner ran them one at a time.
// The suite's demand therefore scaled with the machine, and the fixtures that
// carry a wall-clock budget expired when the machine was busy rather than when
// they were broken (issue #2254).
//
// Two bounds fix that.
//
// 1. The file concurrency is capped. A fixed cap means a workstation asks for
//    about what CI asks for: 23 concurrent files became 4, where a 2-core CI
//    runner ran 1.
//
// 2. The suite takes a machine-wide slot. `scripts/check.ps1 all-ts` and
//    `npm run verify` each start a copy, and the issue's failing run had both
//    of them beside a full frontend suite and a Rust compile. The second copy
//    now waits and says so, instead of doubling the demand.
//
// The slot budget is deliberately separate from the heavy-gate slots: the
// product lanes need both of those to overlap (#2126), and a suite holding one
// for minutes would serialise them.
//
// Usage: node scripts/agent-tests.mjs <test-file> [<test-file> ...]

import { spawn } from 'node:child_process';
import { availableParallelism } from 'node:os';
import { pathToFileURL } from 'node:url';
import { acquireSlot, agentTestLimit, agentTestSlotDirectory } from './harness-lanes.mjs';

// How many contract-test files may run at once. The files are not equally
// heavy - a few compile a crate or build an app - so the count is a property
// of the suite, not of the machine it runs on.
export const FILE_CONCURRENCY_CAP = 4;

// The cap never raises the runner's own default (which is the core count minus
// one), so a small CI runner keeps running as few files at once as it did.
export function fileConcurrency(available = availableParallelism()) {
  return Math.max(1, Math.min(FILE_CONCURRENCY_CAP, available - 1));
}

export function suiteCommand(files, available = availableParallelism()) {
  return ['--test', `--test-concurrency=${fileConcurrency(available)}`, ...files];
}

async function main() {
  const files = process.argv.slice(2);
  if (!files.length) {
    process.stderr.write('Usage: node scripts/agent-tests.mjs <test-file> [<test-file> ...]\n');
    process.exitCode = 2;
    return;
  }
  const limit = agentTestLimit();
  // The harness takes the slot before it starts this gate, so its deadline
  // starts once it holds the slot (scripts/harness.mjs). Only a direct call -
  // `scripts/check.ps1`, CI, a developer - takes it here.
  const held = process.env.BUILDMESH_AGENT_TEST_SLOT === 'held';
  const slot = held ? { release: () => {} } : await acquireSlot({
    dir: agentTestSlotDirectory(),
    limit,
    gate: 'agent-tests',
    root: process.cwd(),
    onQueued: holders => console.log(`QUEUED agent-tests: waiting for an infrastructure-suite slot (limit ${limit}; held by ${holders.map(held => `${held.gate} in ${held.root}`).join(', ') || 'a process that just exited'})`),
  });
  try {
    console.log(`agent-tests: running ${files.length} file(s) with a concurrency of ${fileConcurrency()}`);
    // Everything below inherits the slot, so a nested run - the harness's own
    // agent-tests gate, or a fixture that verifies a plan - never waits for a
    // slot this tree already holds.
    const child = spawn(process.execPath, suiteCommand(files), { stdio: 'inherit', env: { ...process.env, BUILDMESH_AGENT_TEST_SLOT: 'held' } });
    const code = await new Promise(done => child.on('exit', (code, signal) => done(signal ? 1 : code)));
    process.exitCode = code ?? 1;
  } finally {
    slot.release();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
