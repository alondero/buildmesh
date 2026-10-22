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
 *
 * The job-level backstop is the other half of that contract, and it is where
 * the Rust shards actually failed. A job cap is a budget for everything the
 * job does before its guard starts, so the cap has to cover the *real* setup
 * cost -- the composite action's own `apt-get update` and `apt-get install`
 * deadlines, plus the checkout, cache restore and toolchain setup around them
 * -- and then the deadline and kill grace of each step that invokes the
 * guard. The first version of this test assumed a three-minute setup, a
 * number no configuration ever promised: the composite action allowed ten
 * minutes for `apt-get update` and twenty for `apt-get install`, so a
 * shard's setup could legitimately claim thirty of the fifteen minutes its
 * whole job had. The test passed and the shards were still cancelled -- run
 * 39777829786 attempt 2 cancelled `agent` and `git-env-preferences` at "The
 * job has exceeded the maximum execution time of 15m0s", both of them still
 * inside `apt-get install`, with the other five shards green -- and the `db`
 * shard, which survived, had spent 12m20s of its fifteen minutes downloading
 * packages.
 *
 * So the setup budget below is *derived* from the composite action and the
 * workflow instead of being assumed, and the deadline the workflow declares
 * for `apt-get install` has to be the deadline the action's shell applies.
 */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const workflow = readFileSync(join(__dirname, '..', '..', '.github', 'workflows', 'verify.yml'), 'utf8');
const buildDepsAction = readFileSync(
  join(__dirname, '..', '..', '.github', 'actions', 'install-linux-build-deps', 'action.yml'),
  'utf8',
);

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

/** The steps of one job, in order, as their declared name and their own block. */
function jobSteps(jobId: string): { name: string; body: string }[] {
  const block = jobBlock(jobId);
  const found: { name: string; body: string }[] = [];
  for (let at = block.indexOf(STEP_MARKER); at !== -1; ) {
    const rest = block.slice(at + STEP_MARKER.length);
    const nextStep = rest.indexOf(STEP_MARKER);
    const body = nextStep === -1 ? rest : rest.slice(0, nextStep);
    found.push({ name: (body.match(/^name: (.*)$/m)?.[1] ?? '').trim(), body });
    at = block.indexOf(STEP_MARKER, at + STEP_MARKER.length);
  }
  return found;
}

/**
 * The steps of one job that invoke the guard, as the deadline each runs under
 * and the kill grace each allows afterwards. A job can hold more than one:
 * `rust-nonshard` runs the fuzz smoke and then the export/doc/integration
 * pass, and both have their own deadline, so the job's budget is the sum of
 * them.
 */
function guardedSteps(jobId: string): { minutes: number; graceSeconds: number }[] {
  const guarded = jobSteps(jobId)
    .filter((step) => step.body.includes('run-guarded.mjs'))
    .map((step) => ({
      minutes: Number(step.body.match(/--minutes (\d+)/)?.[1]),
      graceSeconds: Number(step.body.match(/--kill-grace-seconds (\d+)/)?.[1]),
    }));
  expect(guarded.length, `the \`${jobId}\` job has no guarded step`).toBeGreaterThan(0);
  for (const step of guarded) {
    expect(Number.isFinite(step.minutes), `a guarded step in \`${jobId}\` has no --minutes`).toBe(true);
    expect(Number.isFinite(step.graceSeconds), `a guarded step in \`${jobId}\` has no --kill-grace-seconds`).toBe(true);
  }
  return guarded;
}

/** The default the composite action declares for one of its inputs, in minutes. */
function actionInputDefault(name: string): number {
  const key = `\n  ${name}:\n`;
  const start = buildDepsAction.indexOf(key);
  expect(start, `the composite action declares no \`${name}\` input`).toBeGreaterThan(-1);
  // Start after the key line itself: searching from the key would match the
  // key as the next sibling and yield an empty body.
  const rest = buildDepsAction.slice(start + key.length);
  const end = rest.search(/\n {2}[a-z][\w-]*:\n/);
  const block = end === -1 ? rest : rest.slice(0, end);
  const value = block.match(/default:\s*'?(\d+)'?/)?.[1];
  expect(value, `the composite action's \`${name}\` input has no numeric default`).toBeDefined();
  return Number(value);
}

/**
 * The deadline one job gives the composite action for one of its apt
 * phases, in minutes: the value it passes, or the action's own default when
 * the job says nothing. Read per phase so a job cannot give one phase a
 * deadline the budget does not know about.
 */
function jobDeadline(
  jobId: string,
  name: 'update-timeout-minutes' | 'resolve-timeout-minutes' | 'install-timeout-minutes',
): number {
  const step = jobSteps(jobId).find((candidate) => candidate.body.includes('install-linux-build-deps'));
  expect(step, `the \`${jobId}\` job does not use the composite action`).toBeDefined();
  const passed = step?.body.match(new RegExp(`^\\s*${name}:\\s*'?(\\d+)'?\\s*$`, 'm'))?.[1];
  return passed === undefined ? actionInputDefault(name) : Number(passed);
}

/**
 * The SIGKILL escalation the apt deadlines can each add, in seconds.
 *
 * `timeout --kill-after=30s 5m apt-get …` allows the command five minutes
 * and then *another* 30 seconds to die after SIGTERM, so the worst case of an
 * apt phase is its deadline plus that grace. Every phase has to carry its
 * own: `timeout` without `--kill-after` waits for the child indefinitely, so
 * a phase that lost its escalation would not only cost the budget 30 seconds
 * less, it would have no bound at all.
 */
function aptKillAfterSeconds(): number {
  const phases = ['UPDATE_TIMEOUT_MINUTES', 'RESOLVE_TIMEOUT_MINUTES', 'INSTALL_TIMEOUT_MINUTES'];
  let total = 0;
  for (const variable of phases) {
    const invocation = buildDepsAction.match(new RegExp(`timeout[^\\n]*\\$\\{${variable}\\}m[^\\n]*`));
    expect(invocation, `no apt phase runs under ${variable}`).not.toBeNull();
    const grace = invocation?.[0].match(/--kill-after=(\d+)s/);
    expect(grace, `the apt phase under ${variable} has no --kill-after escalation`).not.toBeNull();
    total += Number(grace?.[1]);
  }
  return total;
}

/**
 * The runner overhead around the apt phase: checkout, the cache restores, the
 * Rust toolchain and the artifact download. Measured on run 39777829786
 * attempt 2, the `remaining` shard spent 34 seconds on all of it
 * (12:59:29-12:59:35 and 13:00:58-13:01:26), so two minutes is roughly
 * three and a half times the observed cost.
 */
const SETUP_OVERHEAD_SECONDS = 2 * 60;

const shard = runScript('Run the ${{ matrix.shard.label }} tests');
const nonShard = runScript('Run Rust export, documentation, and integration tests');

// `maxJobMinutes` is what a lost runner costs: the guard cannot fire on a
// runner that is gone, so the run waits out the job cap before the CI retry
// can act. Five lost `services` runners in October 2026 each held their run
// 45-60 minutes under the old 40-minute cap.
const guards = [
  { job: 'rust-tests', script: shard, stepMinutes: 10, maxJobMinutes: 30 },
  { job: 'rust-nonshard', script: nonShard, stepMinutes: 45, maxJobMinutes: 75 },
];

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

  it('keeps a job-level backstop above the real setup cost and every guarded step', () => {
    // The job cap is the layer that does not depend on the runner's shell
    // reporting back, so it has to outlive the setup *and* every guarded
    // step, including the SIGKILL escalation; otherwise the cap cuts a step
    // off first and the guard's annotation and log never happen.
    //
    // The setup cost is the composite action's own deadlines plus the runner
    // overhead around them -- not an assumed three minutes. Run 39777829786
    // attempt 2 cancelled two shards at "The job has exceeded the maximum
    // execution time of 15m0s" while they were still inside `apt-get
    // install`, because the action allowed thirty minutes of apt work inside
    // a fifteen-minute job.
    const cap = jobBlock(job).match(/^\s*timeout-minutes: (\d+)$/m);
    expect(cap, `the ${job} job has no timeout-minutes backstop`).not.toBeNull();
    // The setup cost is the composite action's own deadlines plus the runner
    // overhead around them -- not an assumed three minutes. Run 39777829786
    // attempt 2 cancelled two shards at "The job has exceeded the maximum
    // execution time of 15m0s" while they were still inside `apt-get
    // install`, because the action allowed thirty minutes of apt work inside
    // a fifteen-minute job.
    //
    // Each apt phase can also burn the SIGKILL escalation its `timeout`
    // allows after the deadline, so those count too: without them the model
    // said the shard's worst case was exactly the cap, and a job cancelled at
    // the cap leaves neither a log nor a conclusion (issue #1961).
    const deadlineMinutes =
      jobDeadline(job, 'update-timeout-minutes') +
      jobDeadline(job, 'resolve-timeout-minutes') +
      jobDeadline(job, 'install-timeout-minutes');
    const setupSeconds = deadlineMinutes * 60 + SETUP_OVERHEAD_SECONDS + aptKillAfterSeconds();
    const guardedSeconds = guardedSteps(job).reduce(
      (total, step) => total + step.minutes * 60 + step.graceSeconds,
      0,
    );
    expect(Number(cap?.[1]) * 60).toBeGreaterThanOrEqual(setupSeconds + guardedSeconds);
  });

  it('caps what a lost runner costs', () => {
    const cap = jobBlock(job).match(/^\s*timeout-minutes: (\d+)$/m);
    expect(Number(cap?.[1])).toBeLessThanOrEqual(maxJobMinutes);
  });
});

describe('the composite action applies the deadlines the workflow declares', () => {
  it('takes every apt deadline as an input rather than baking it into the shell', () => {
    // A deadline that is only written in the shell cannot be budgeted for by
    // the workflow, and the two would drift the moment one of them changed.
    expect(buildDepsAction).toMatch(/^\s*update-timeout-minutes:\s*$/m);
    expect(buildDepsAction).toMatch(/^\s*resolve-timeout-minutes:\s*$/m);
    expect(buildDepsAction).toMatch(/^\s*install-timeout-minutes:\s*$/m);
  });

  it('applies the declared deadlines instead of a literal minute count', () => {
    const shell = buildDepsAction;
    expect(shell).toMatch(/UPDATE_TIMEOUT_MINUTES/);
    expect(shell).toMatch(/RESOLVE_TIMEOUT_MINUTES/);
    expect(shell).toMatch(/INSTALL_TIMEOUT_MINUTES/);
    // No hardcoded `timeout ... Nm apt-get` left behind: the number that the
    // budget is computed from has to be the number the shell enforces.
    expect(shell).not.toMatch(/timeout --kill-after=\d+s \d+m apt-get/);
  });

  it('keeps every apt deadline finite', () => {
    expect(actionInputDefault('update-timeout-minutes')).toBeGreaterThan(0);
    expect(actionInputDefault('resolve-timeout-minutes')).toBeGreaterThan(0);
    expect(actionInputDefault('install-timeout-minutes')).toBeGreaterThan(0);
  });

  it('bounds the resolve step too, so it cannot be cancelled by the job cap', () => {
    // The resolution reads the index apt has just fetched and works out the
    // plan. It is local work, so it cannot stall on the mirror, but it has to
    // have a deadline for the same reason the other apt phases do: a phase
    // without one is a phase the job cap cancels without a diagnosis.
    // The anchor is the shell's own invocation, not the prose above it.
    const at = buildDepsAction.indexOf('install --print-uris -y');
    expect(at, 'the action no longer resolves the package set').toBeGreaterThan(-1);
    const before = buildDepsAction.slice(Math.max(0, at - 400), at);
    expect(before, 'the resolve step has no deadline of its own').toMatch(
      /timeout --kill-after=\d+s "\$\{RESOLVE_TIMEOUT_MINUTES\}m"/,
    );
  });
});
