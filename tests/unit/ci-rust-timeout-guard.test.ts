/**
 * The Rust test steps bound themselves with a deadline (issue #1961). The
 * original in-shell `timeout ... cargo test ... 2>&1 | tee log` guard only
 * ended the *step* if nothing sat between the test and the shell holding the
 * step open: a test's descendant can leave the process group while still
 * holding the pipe's write end, so `tee` never sees EOF and the step outlives
 * the guard -- run 36531715263 lost the `services` shard that way, about 45
 * minutes with no step conclusion and no log, because a cancelled job flushes
 * no log either.
 *
 * The guard now lives in scripts/ci/run-guarded.mjs (unit-tested in
 * tests/agent-infra/run-guarded.test.mjs): it streams output into the step log
 * while appending to the file the upload preserves, kills the command's whole
 * process tree at the deadline, and never waits on the output pipes after the
 * kill. These assertions read the workflow text and pin the contract that
 * keeps the guard real at the workflow level: the step invokes that tested
 * guard exactly once with the deadline the release notes document, nothing
 * pipes or swallows the command, and the log the guard writes is exactly what
 * the `if: always()` upload collects, so a killed shard leaves evidence behind
 * rather than a warning.
 */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const workflow = readFileSync(join(__dirname, '..', '..', '.github', 'workflows', 'verify.yml'), 'utf8');

const STEP_MARKER = '\n      - ';

/** The region from `start` to the next step, or to the next job's key. */
function blockFrom(start: number): string {
  const rest = workflow.slice(start);
  const nextStep = rest.indexOf(STEP_MARKER);
  const nextJob = rest.search(/\n {2}[a-z][\w-]*:\n/);
  // A step that is last in its job runs to the *next job's* key, and that
  // region carries the next job's own steps and `if: always()`. Bounding only
  // on the next step would let those satisfy this job's assertions.
  const boundaries = [nextStep, nextJob].filter((offset) => offset >= 0);
  const end = boundaries.length > 0 ? Math.min(...boundaries) : undefined;
  return end === undefined ? rest : rest.slice(0, end);
}

/** Every step in the workflow, as its declared name and its own block. */
function steps(): { name: string; body: string }[] {
  const found: { name: string; body: string }[] = [];
  for (let at = workflow.indexOf(STEP_MARKER); at !== -1; ) {
    const body = blockFrom(at + STEP_MARKER.length);
    found.push({ name: (body.match(/^name: (.*)$/m)?.[1] ?? '').trim(), body });
    at = workflow.indexOf(STEP_MARKER, at + STEP_MARKER.length);
  }
  return found;
}

function stepNamed(stepName: string): string {
  const step = steps().find((candidate) => candidate.name === stepName);
  expect(step, `verify.yml has no step named "${stepName}"`).toBeDefined();
  return step?.body ?? '';
}

/** The `run: |` body of the named step. */
function runScript(stepName: string): string {
  const body = stepNamed(stepName);
  const runStart = body.indexOf('\n        run: |');
  expect(runStart, `the "${stepName}" step has no \`run: |\` block`).toBeGreaterThan(-1);
  const lines = body.slice(runStart).split('\n');
  // The next step's leading comment block is indented like a step, so trim
  // trailing blank and comment lines to keep this step's own body.
  while (lines.length > 0 && /^\s*(#.*)?$/.test(lines[lines.length - 1])) lines.pop();
  return lines.join('\n');
}

/** The executable shell of a step: comments are prose, including prose about this very fix. */
function shellLines(script: string): string {
  return script
    .split('\n')
    .filter((line) => !/^\s*#/.test(line))
    .join('\n');
}

/** The step that reads `needle`, or an empty string. */
function stepContaining(needle: string): string {
  return steps().find((step) => step.body.includes(needle))?.body ?? '';
}

/** The body of one top-level `job:` block, so one job's cap is not read as another's. */
function jobBlock(jobId: string): string {
  const marker = `\n  ${jobId}:\n`;
  const start = workflow.indexOf(marker);
  expect(start, `verify.yml has no \`${jobId}\` job`).toBeGreaterThan(-1);
  const rest = workflow.slice(start + marker.length);
  const end = rest.search(/\n {2}[a-z][\w-]*:\n/);
  return end === -1 ? workflow.slice(start) : workflow.slice(start, start + marker.length + end);
}

/**
 * The file the guard appends to, read from the `--log` flag it passes to
 * run-guarded.mjs rather than from shell redirection (the guard does the
 * writing now).
 */
function logFile(script: string): string {
  // Quoted when the name embeds a `${{ ... }}` expression (spaces inside),
  // bare otherwise; either way it must end in `.log`.
  const flag = script.match(/--log\s+(?:"([^"]+\.log)"|(\S+\.log))/);
  expect(flag, 'the guarded step passes no `--log <file>.log` to run-guarded.mjs').not.toBeNull();
  const name = flag?.[1] ?? flag?.[2] ?? '';
  expect(name, 'the guard log does not end in `.log`').toMatch(/\.log$/);
  return name;
}

const shard = runScript('Run the ${{ matrix.shard.label }} tests');
const nonShard = runScript('Run Rust export, documentation, and integration tests');

// `maxJobMinutes` is what a lost runner costs: the guard cannot fire on a
// runner that is gone, so the run waits out the job cap before the CI retry
// can act. Five lost `services` runners in October 2026 each held their run
// 45-60 minutes under the old 40-minute cap; a healthy shard takes ~2.
const guards = [
  { job: 'rust-tests', script: shard, stepMinutes: 10, maxJobMinutes: 15 },
  { job: 'rust-nonshard', script: nonShard, stepMinutes: 45, maxJobMinutes: 60 },
];

// Checkout, apt, toolchain and cache restore run before the guard starts.
const SETUP_ALLOWANCE_SECONDS = 3 * 60;

describe.each(guards)('$job timeout guard', ({ script, stepMinutes }) => {
  it('does not pipe the guarded test command', () => {
    // The pipe is the bug: a descendant holding the write end keeps `tee`
    // waiting after the guard has already killed the test, so the guard ends
    // nothing. run-guarded.mjs streams the output itself; the step shell must
    // not reintroduce a pipeline around it.
    const shell = shellLines(script);
    expect(shell).not.toMatch(/\|\s*tee\b/);
    expect(shell).not.toContain('PIPESTATUS');
    // The step shell's own `set -o pipefail` was plumbing for that pipeline
    // and goes with it. The `pipefail` inside `bash -euo pipefail -c` is the
    // inner script's strict mode and stays.
    expect(shell).not.toMatch(/^\s*set -o pipefail$/m);
  });

  it('runs the command as one invocation of the tested guard at the documented deadline', () => {
    // GitHub runs a `run:` block under `bash -e`, so a single invocation's
    // exit status is the step's exit status: whatever run-guarded.mjs decides
    // (child code, 124 on the deadline, 127 on ENOENT) reaches the job result
    // with nothing in between to swallow it. A second command, a pipe, or a
    // `|| true` in the step shell would break that chain the same way the old
    // `set -e` interaction broke the inline `timeout` annotation path.
    const shell = shellLines(script);
    expect(
      shell.match(/run-guarded\.mjs/g),
      'the step must invoke scripts/ci/run-guarded.mjs exactly once',
    ).toHaveLength(1);
    expect(shell).toMatch(new RegExp(`--minutes ${stepMinutes}\\b`));
    expect(shell).toMatch(/--kill-grace-seconds \d+/);
    // The guarded command itself, after the `--` separator.
    expect(shell).toMatch(/-- (cargo|bash)\b/);
    expect(shell).not.toMatch(/\|\|\s*true/);
  });

  it('writes the log that an always() upload collects, including after a kill', () => {
    // `working-directory: src-tauri` is where the log lands; an upload step
    // addresses it from the repository root. If the two names drift, or no
    // upload step exists at all, a hung step leaves no retrievable evidence.
    const path = `path: src-tauri/${logFile(script)}`;
    const upload = stepContaining(path);
    expect(upload, `no upload step reads the log at \`${path}\``).not.toBe('');
    expect(upload).toMatch(/^\s*if: always\(\)$/m);
  });
});

describe.each(guards)('$job bounds', ({ job, script, stepMinutes, maxJobMinutes }) => {
  it('keeps the in-step guard the release notes document', () => {
    expect(script).toContain('scripts/ci/run-guarded.mjs');
    expect(script).toMatch(new RegExp(`--minutes ${stepMinutes}\\b`));
  });

  it('keeps a job-level backstop above the in-step guard and its kill grace', () => {
    // The job cap is the layer that does not depend on the runner's shell
    // reporting back, so it has to outlive the in-step guard including the
    // SIGKILL escalation; otherwise the cap cuts a hung test off first and
    // the guard's annotation and log never happen.
    const cap = jobBlock(job).match(/^\s*timeout-minutes: (\d+)$/m);
    expect(cap, `the ${job} job has no timeout-minutes backstop`).not.toBeNull();
    const grace = Number(shellLines(script).match(/--kill-grace-seconds (\d+)/)?.[1]);
    expect(Number(cap?.[1]) * 60).toBeGreaterThanOrEqual(
      SETUP_ALLOWANCE_SECONDS + stepMinutes * 60 + grace,
    );
  });

  it('caps what a lost runner costs', () => {
    const cap = jobBlock(job).match(/^\s*timeout-minutes: (\d+)$/m);
    expect(Number(cap?.[1])).toBeLessThanOrEqual(maxJobMinutes);
  });
});
