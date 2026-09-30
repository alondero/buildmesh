/**
 * The Rust test steps bound themselves with an in-shell `timeout`
 * (issue #1961). That bound only ends the *step* if nothing sits between the
 * test and the shell holding the step open. It used to run
 * `cargo test ... 2>&1 | tee log`, and `timeout` signals the child's process
 * group -- which a test's descendant can leave, while still holding the pipe's
 * write end. `tee` then never sees EOF, so the step outlives the guard meant to
 * end it: run 36531715263 lost the `services` shard that way, about 45 minutes
 * with no step conclusion and no log, because a cancelled job flushes no log
 * either.
 *
 * These assertions read the workflow text and pin the shape that makes the
 * guard real: the guarded command writes its log by redirection, nothing pipes
 * it, the exit status is still propagated, and the log path still matches what
 * an `if: always()` upload step expects, so a killed shard leaves evidence
 * behind rather than a warning.
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
 * The file the guarded command writes into, resolving a `log=` variable so the
 * assertion is about the path rather than the spelling of the redirect.
 */
function logFile(script: string): string {
  const literal = script.match(/>\s*"?([^\s"'>]+\.log)"?\s*2>&1/);
  if (literal) return literal[1];
  const viaVariable = script.match(/>\s*"\$(\w+)"\s*2>&1/);
  expect(viaVariable, 'the guarded command does not redirect stdout+stderr into a log file').not.toBeNull();
  const name = viaVariable?.[1] ?? '';
  const assignment = script.match(new RegExp(`^\\s*${name}=("[^"]+"|\\S+)$`, 'm'));
  expect(assignment, `the redirect target \`${name}\` is never assigned a path`).not.toBeNull();
  return (assignment?.[1] ?? '').replace(/^"|"$/g, '');
}

const shard = runScript('Run the ${{ matrix.shard.label }} tests');
const nonShard = runScript('Run Rust export, documentation, and integration tests');

const guards = [
  { job: 'rust-tests', script: shard, stepMinutes: 30 },
  { job: 'rust-bindings', script: nonShard, stepMinutes: 45 },
];

describe.each(guards)('$job timeout guard', ({ script }) => {
  it('does not pipe the guarded test command', () => {
    // The pipe is the bug: a descendant holding the write end keeps `tee`
    // waiting after `timeout` has already killed the test, so the guard ends
    // nothing.
    const shell = shellLines(script);
    expect(shell).not.toMatch(/\|\s*tee\b/);
    expect(shell).not.toContain('PIPESTATUS');
    // The step shell's own `set -o pipefail` was plumbing for that pipeline
    // and goes with it. The `pipefail` inside `bash -euo pipefail -c` is the
    // inner script's strict mode and stays.
    expect(shell).not.toMatch(/^\s*set -o pipefail$/m);
  });

  it('writes the log by redirection so the guard can end the step', () => {
    expect(script).toMatch(/timeout --kill-after=/);
    expect(logFile(script)).toMatch(/\.log$/);
  });

  it('captures the status without letting errexit skip the failure path', () => {
    // GitHub runs a `run:` block under `bash -e`, so a bare `timeout` that
    // fails ends the step right there. The status capture, the `cat`, and the
    // 124 annotation after it would never run, and a hung shard would fail
    // with an empty step log and no explanation. Consuming the guarded
    // command's own failure with `||` keeps those lines reachable.
    const guard = script.indexOf('timeout --kill-after=');
    expect(guard, 'the guarded command is not wrapped in `timeout`').toBeGreaterThan(-1);
    const initialised = script.indexOf('status=0');
    expect(
      initialised,
      '`status` is never initialised, so a passing run reaches `exit` with an empty argument',
    ).toBeGreaterThan(-1);
    expect(initialised, '`status` is initialised after the guarded command runs').toBeLessThan(guard);
    expect(
      script.indexOf('|| status=$?'),
      'the guarded command does not consume its own failure with `|| status=$?`, so `bash -e` ends the step before the status is captured',
    ).toBeGreaterThan(guard);
    expect(script).toContain('exit "$status"');
  });

  it('uploads the log it wrote, including after a kill', () => {
    // `working-directory: src-tauri` is where the log lands; an upload step
    // addresses it from the repository root. If the two names drift, or no
    // upload step exists at all, a hung step leaves no retrievable evidence.
    const path = `path: src-tauri/${logFile(script)}`;
    const upload = stepContaining(path);
    expect(upload, `no upload step reads the log at \`${path}\``).not.toBe('');
    expect(upload).toMatch(/^\s*if: always\(\)$/m);
  });
});

describe.each(guards)('$job bounds', ({ job, script, stepMinutes }) => {
  it('keeps the in-step guard the release notes document', () => {
    expect(script).toMatch(new RegExp(`timeout --kill-after=\\S+ ${stepMinutes}m\\b`));
  });

  it('keeps a job-level backstop above the in-step guard', () => {
    // The job cap is the layer that does not depend on the runner's shell
    // reporting back, so it has to outlive the in-step guard.
    const cap = jobBlock(job).match(/^\s*timeout-minutes: (\d+)$/m);
    expect(cap, `the ${job} job has no timeout-minutes backstop`).not.toBeNull();
    expect(Number(cap?.[1])).toBeGreaterThan(stepMinutes);
  });
});
