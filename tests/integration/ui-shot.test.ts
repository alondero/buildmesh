import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createServer as createTcpServer } from 'node:net';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
  DEV_SERVER_STARTUP_MS,
  DEV_SERVER_STOP_MS,
  BROWSER_LAUNCH_TIMEOUT_MS,
  NAVIGATION_TIMEOUT_MS,
  MOUNT_TIMEOUT_MS,
  ELEMENT_VISIBLE_TIMEOUT_MS,
  STEP_SCRIPT_TIMEOUT_MS,
  SCREENSHOT_TIMEOUT_MS,
  BROWSER_CLOSE_TIMEOUT_MS,
  BROWSER_SETUP_TIMEOUT_MS,
  FIXTURES_LOAD_TIMEOUT_MS,
  STEP_MODULE_LOAD_TIMEOUT_MS,
  UI_SHOT_SLOWEST_PHASE_MS,
  UI_SHOT_WATCHDOG_DEADLINE_MS,
  UI_SHOT_WATCHDOG_MULTIPLE,
} from '../../scripts/ui-shot-budgets.mjs';
import { runSteps } from '../../scripts/ui-shot-steps.mjs';
import { withDeadline } from '../../scripts/ui-shot-deadline.mjs';
import { startDevServer, stopDevServer } from '../../scripts/ui-shot-server.mjs';
import {
  PHASE_FILE_ENV,
  activePhase,
  killDiagnostic,
  readRecordedPhases,
} from '../../scripts/phase-watchdog.mjs';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const uiShot = resolve(repoRoot, 'scripts', 'ui-shot.mjs');
const circuitSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-circuit.steps.mjs');
const circuitFixtures = resolve(repoRoot, 'tests', 'integration', 'ui-shot-circuit.fixtures.mjs');
const nodeHeaderSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-node-header.steps.mjs');
const nodeHeaderFixtures = resolve(repoRoot, 'tests', 'integration', 'ui-shot-node-header.fixtures.mjs');
const hangingSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-hanging.steps.mjs');
const hangingImportSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-hanging-import.steps.mjs');
const recordingSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-recording.steps.mjs');

async function freePort() {
  const server = createTcpServer();
  await new Promise<void>((resolvePromise, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolvePromise);
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Could not determine the free port');
  const port = address.port;
  await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
  return port;
}

// The supervising wrapper needs ONE deadline, and it is derived from the
// per-phase budgets rather than hand-summed (#2168). Each phase is enforced in
// the child and fails with its own named diagnostic, so this number is a
// backstop for a child that somehow exceeds every one of them — not a budget
// the run is expected to consume.
//
// The per-test budget adds slack, so a wrapper timeout — which now names the
// phase it killed the child in, and carries the child's stdout and stderr — is
// what fails the run rather than Vitest cutting the test off with no
// diagnostic.
const WRAPPER_SLACK_MS = 30000;
const WRAPPER_DEADLINE_MS = UI_SHOT_WATCHDOG_DEADLINE_MS + WRAPPER_SLACK_MS;
const TEST_DEADLINE_MS = WRAPPER_DEADLINE_MS + WRAPPER_SLACK_MS;

function runUiShot(args, timeoutMs = WRAPPER_DEADLINE_MS, env: Record<string, string> = {}) {
  return new Promise<{ code: number | null; stdout: string; stderr: string }>((resolvePromise, reject) => {
    const child = spawn(process.execPath, [uiShot, ...args], {
      cwd: repoRoot,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, ...env },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timeout = setTimeout(() => {
      child.kill();
      // The child names each phase it enters into a file as it goes, so a kill
      // reports which phase was actually in flight rather than only how long
      // the wrapper waited. That is the whole diagnostic improvement #2168 asks
      // for: a fixed deadline can say only that time ran out.
      const phase = activePhase(env[PHASE_FILE_ENV]);
      reject(new Error(`${killDiagnostic({ label: 'ui-shot', timeoutMs, phase })}\n${stdout}\n${stderr}`));
    }, timeoutMs);
    child.once('error', (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once('close', (code) => {
      clearTimeout(timeout);
      resolvePromise({ code, stdout, stderr });
    });
  });
}

async function serveHtml(html) {
  const port = await freePort();
  const server = createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'text/html' });
    response.end(html);
  });
  await new Promise<void>((resolvePromise, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolvePromise);
  });
  return { server, url: `http://127.0.0.1:${port}` };
}

// The mount-failure run supervises its child with the same wrapper deadline as
// every other mock render, and does so by name rather than by a second copy of
// the arithmetic — a hand-written subset had to be kept in step with the sum
// that used to live above, and that is the drift the watchdog removes. There is
// deliberately no separate constant to check here: an alias of
// `WRAPPER_DEADLINE_MS` could only be compared against itself.

const throwingSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-throwing.steps.mjs');

describe('ui-shot mock mode', () => {
  it('groups, reloads, swaps and ungroups nodes through pointer and keyboard interactions', async () => {
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-groups-'));
    try {
      const port = await freePort();
      const result = await runUiShot([
        '--out', join(folder, 'groups.png'), '--mock', '--serve',
        '--mock-url', `http://127.0.0.1:${port}`,
        '--fixtures', resolve(repoRoot, 'tests/integration/ui-shot-node-groups.fixtures.mjs'),
        '--steps', resolve(repoRoot, 'tests/integration/ui-shot-node-groups.steps.mjs'),
      ]);
      expect(result.code, result.stderr).toBe(0);
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  }, TEST_DEADLINE_MS);

  it('serves the fixture UI, drives a circuit, and writes a screenshot', async () => {
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-'));
    try {
      const port = await freePort();
      const output = join(folder, 'circuit.png');
      const result = await runUiShot([
        '--out', output,
        '--mock',
        '--serve',
        '--mock-url', `http://127.0.0.1:${port}`,
        '--fixtures', circuitFixtures,
        '--steps', circuitSteps,
      ]);

      expect(result.code, result.stderr).toBe(0);
      expect(result.stdout).toContain('Saved');
      expect((await stat(output)).size).toBeGreaterThan(0);
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  }, TEST_DEADLINE_MS);

  it('keeps the node title and trailing close visible in a 240px pane', async () => {
    // This run also carries the phase-recording assertion: it is the one test
    // that exercises the full set of phases a `--mock` run spends (dev server
    // started and stopped, steps imported and run, a selector waited for), so
    // it is where the emitted phase list is checked. Folding it in here rather
    // than adding a dedicated test keeps the browser launches this file makes
    // unchanged — each one costs a real Chromium and a real Vite server.
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-header-'));
    try {
      const port = await freePort();
      const output = join(folder, 'header.png');
      const phaseFile = join(folder, 'phases.log');
      const result = await runUiShot([
        '--out', output,
        '--mock',
        '--serve',
        '--mock-url', `http://127.0.0.1:${port}`,
        '--fixtures', nodeHeaderFixtures,
        '--steps', nodeHeaderSteps,
        '--selector', '[data-testid=grid-node-header]',
      ], WRAPPER_DEADLINE_MS, { [PHASE_FILE_ENV]: phaseFile });

      expect(result.code, result.stderr).toBe(0);
      expect(result.stdout).toContain('Saved');
      expect((await stat(output)).size).toBeGreaterThan(0);

      // The behavioural replacement for the old source audit of `ui-shot.mjs`,
      // which read the script's text and required a budget constant at each
      // call site. That could not see a phase in neither copy, and matched the
      // first occurrence of a call string, so a comment could satisfy it.
      // Asserting on what a real run emits is an observation of behaviour: drop
      // a `phase(...)` call, or a budget that stops being enforced, and this
      // list changes.
      const phases = readRecordedPhases(phaseFile);
      for (const expected of [
        'dev-server startup',
        'browser launch',
        'page setup',
        'fixtures load',
        'navigation',
        'mount wait',
        'step module load',
        'step script',
        'selector wait',
        'screenshot',
        'browser close',
        'dev-server stop',
      ]) {
        expect(phases, `a mock render must record the "${expected}" phase`).toContain(expected);
      }
      // Recorded on entry, so the last entry is where the run ended — the same
      // read the wrapper's kill diagnostic uses.
      expect(activePhase(phaseFile)).toBe('dev-server stop');
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  }, TEST_DEADLINE_MS);

  it('reports root mount failure and browser console errors', async () => {
    const { server, url } = await serveHtml(
      '<div id="root"></div><script>console.error("mock mount exploded")</script>'
    );
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-'));
    try {
      const output = join(folder, 'should-not-exist.png');
      const result = await runUiShot(['--out', output, '--mock', '--mock-url', url], WRAPPER_DEADLINE_MS);

      expect(result.code).toBe(1);
      // Built from the constant the script formats its message with, so
      // raising MOUNT_TIMEOUT_MS cannot leave this asserting a stale number.
      expect(result.stderr).toContain(`#root never populated within ${MOUNT_TIMEOUT_MS / 1000}s`);
      expect(result.stderr).toContain('Page errors: mock mount exploded');
      await expect(readFile(output)).rejects.toThrow();
    } finally {
      await rm(folder, { recursive: true, force: true });
      await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
    }
  }, TEST_DEADLINE_MS);

  it('runs the step script through the CLI, so dropping the step phase cannot stay green', async () => {
    // The CLI's call to the step phase is what the rest of this file budgets,
    // and nothing else here would notice if it were removed: the mock render
    // tests only assert an exit code and a PNG, which are produced whether or
    // not steps ran. So drive the real CLI with a steps file that leaves a
    // marker, and assert on the marker.
    const { server, url } = await serveHtml('<div id="root"><p>mounted</p></div>');
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-cli-steps-'));
    try {
      const markerPath = join(folder, 'marker.json');
      const result = await runUiShot(
        ['--out', join(folder, 'shot.png'), '--mock', '--mock-url', url, '--steps', recordingSteps],
        WRAPPER_DEADLINE_MS,
        { UI_SHOT_MARKER_PATH: markerPath },
      );

      expect(result.code, result.stderr).toBe(0);
      // The marker is written only if the CLI imported and ran the steps file
      // with the real page, invoke and mock deps.
      expect(JSON.parse(await readFile(markerPath, 'utf8'))).toEqual({
        hasPage: true, hasInvoke: true, hasMock: true, fromCli: true,
      });
    } finally {
      await rm(folder, { recursive: true, force: true });
      await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
    }
  }, TEST_DEADLINE_MS);

  it('runSteps rejects a hanging step script with the step-phase diagnostic', async () => {
    // The mechanism under test: the step phase is bounded, and the bound is
    // reported as a named phase. STEP_SCRIPT_TIMEOUT_MS is minutes long by
    // design (ordinary CPU load must not trip it), so the bound is exercised
    // with an injected short budget instead of waiting out the real one —
    // testing the deadline, not the patience.
    //
    // The fixture steps files live in the repo rather than a temp dir: Vitest's
    // module runner resolves dynamic imports through its own transform, which
    // cannot load a caller-supplied path from outside the project root.
    await expect(runSteps(hangingSteps, {}, { timeoutMs: 250 })).rejects.toThrow(
      /step script phase\) did not finish within 250ms/,
    );
    // A step script that settles in time must still run to completion with its
    // deps intact: the deadline bounds the phase, it does not replace it.
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-steps-'));
    try {
      const markerPath = join(folder, 'marker.json');
      await runSteps(recordingSteps, { page: {}, invoke: () => {}, mock: {}, markerPath }, { timeoutMs: 5000 });
      expect(JSON.parse(await readFile(markerPath, 'utf8'))).toEqual({
        hasPage: true, hasInvoke: true, hasMock: true, fromCli: false,
      });
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  });

  it('runSteps bounds the module load separately from the step run', async () => {
    // Loading a steps module and running it are separate priced phases, so each
    // takes its own budget. Deriving the load budget from the step budget would
    // hand the load phase the step phase's 120s, which is how the sum stopped
    // being an upper bound on a mock run (#2063).
    await expect(runSteps(hangingImportSteps, {}, { timeoutMs: 5000, moduleLoadTimeoutMs: 250 })).rejects.toThrow(
      /Loading the step script .*ui-shot-hanging-import\.steps\.mjs did not finish within 250ms/,
    );
    // The two budgets are independent: a generous step budget must not rescue a
    // load that never settles, which is the defect this guards.
    await expect(runSteps(hangingImportSteps, {}, { timeoutMs: 60000, moduleLoadTimeoutMs: 250 })).rejects.toThrow(
      /did not finish within 250ms/,
    );
    // And with no load budget — what the real-app modes pass — the import is
    // unbounded, matching those modes' deliberate lack of supervision.
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-unbounded-'));
    try {
      await expect(
        runSteps(recordingSteps, { markerPath: join(folder, 'marker.json') }, { timeoutMs: 5000, moduleLoadTimeoutMs: null }),
      ).resolves.toBeUndefined();
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  });

  it('withDeadline treats a null budget as unbounded and still bounds a real one', async () => {
    // The null path is what real-app `--url` and CDP modes rely on; the CLI
    // wiring that selects it is covered by the `--url` test below.
    await expect(withDeadline(Promise.resolve('done'), null, 'unbudgeted phase')).resolves.toBe('done');
    // A budgeted phase still bounds: the null path must not disable the
    // mechanism the wrapper depends on.
    await expect(withDeadline(new Promise(() => {}), 200, 'budgeted phase')).rejects.toThrow(
      /budgeted phase did not finish within 200ms/,
    );
  });

  it('fails the run when a step script throws, and reports the step error', async () => {
    // A failing step must produce a non-zero exit and its own message. Without
    // this, the CLI can capture the failure and drop it: every other test here
    // only checks that a screenshot exists, which a run that swallowed a
    // throwing step would still produce.
    const { server, url } = await serveHtml('<div id="root"><p>mounted</p></div>');
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-throwing-'));
    try {
      const output = join(folder, 'should-not-exist.png');
      const result = await runUiShot(
        ['--out', output, '--mock', '--mock-url', url, '--steps', throwingSteps],
        WRAPPER_DEADLINE_MS,
      );

      expect(result.code, result.stderr).toBe(1);
      expect(result.stderr).toContain('step assertion exploded');
      // The run failed, so it must not leave a screenshot claiming success.
      await expect(readFile(output)).rejects.toThrow();
    } finally {
      await rm(folder, { recursive: true, force: true });
      await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
    }
  }, TEST_DEADLINE_MS);

it('derives the wrapper deadline from the phase budgets instead of hand-summing them', () => {
    // The invariant this replaced was a 12-term sum that a second copy of the
    // phase list in this file had to mirror — and the copy, not the code, was
    // what the gate protected (#2168). The deadline is now a multiple of the
    // slowest budget, so raising any phase widens it with no second list.
    expect(UI_SHOT_SLOWEST_PHASE_MS).toBe(STEP_SCRIPT_TIMEOUT_MS);
    expect(UI_SHOT_WATCHDOG_DEADLINE_MS).toBe(UI_SHOT_SLOWEST_PHASE_MS * UI_SHOT_WATCHDOG_MULTIPLE);

    // The backstop must still outlast a run that is slow in several phases at
    // once, which is the only case the multiple is there for. This is the sum
    // the deadline replaces, computed from the imported constants so it cannot
    // drift from them — the failure mode that motivated the change.
    const wholeRunMs = DEV_SERVER_STARTUP_MS
      + BROWSER_LAUNCH_TIMEOUT_MS * 2
      + BROWSER_SETUP_TIMEOUT_MS * 2
      + FIXTURES_LOAD_TIMEOUT_MS
      + NAVIGATION_TIMEOUT_MS
      + MOUNT_TIMEOUT_MS
      + STEP_SCRIPT_TIMEOUT_MS
      + STEP_MODULE_LOAD_TIMEOUT_MS
      + ELEMENT_VISIBLE_TIMEOUT_MS
      + SCREENSHOT_TIMEOUT_MS
      + BROWSER_CLOSE_TIMEOUT_MS
      + DEV_SERVER_STOP_MS;
    expect(UI_SHOT_WATCHDOG_DEADLINE_MS).toBeGreaterThan(wholeRunMs);
  });

  it('names the phase a killed run was in, instead of reporting only the timeout', async () => {
    // The diagnostic improvement #2168 asks for. Today a wrapper kill reports
    // `ui-shot did not finish within <n>ms`, which says nothing about where the
    // time went; the child-side watchdog appends each phase name as it enters
    // one, so the wrapper can name the phase that was in flight.
    //
    // Driven against a real child with a hanging steps file and a deliberately
    // short wrapper deadline, so the kill happens during a phase rather than
    // being simulated. The deadline is the injected one, not the derived
    // backstop: the point is what the kill reports, not that a normal run
    // finishes.
    const { server, url } = await serveHtml('<div id="root"><p>mounted</p></div>');
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-kill-'));
    try {
      const phaseFile = join(folder, 'phases.log');
      // Long enough to get past launch, navigation and mount on a loaded
      // machine, short enough that the hanging step script is still running.
      const killAfterMs = 30000;
      const started = Date.now();
      await expect(
        runUiShot(
          ['--out', join(folder, 'shot.png'), '--mock', '--mock-url', url, '--steps', hangingSteps],
          killAfterMs,
          { [PHASE_FILE_ENV]: phaseFile },
        ),
      ).rejects.toThrow(/ui-shot did not finish within 30000ms \(killed while in phase: step script\)/);
      // It was the step phase that was in flight, not merely the last phase
      // recorded before the child died.
      expect(activePhase(phaseFile)).toBe('step script');
      expect(Date.now() - started).toBeLessThan(killAfterMs + 15000);
    } finally {
      await rm(folder, { recursive: true, force: true });
      await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
    }
  }, 120000);

  it('keeps a phase list readable when nothing recorded one', () => {
    // A child that dies before entering a phase (a boot crash, an import
    // error) has recorded nothing, and the wrapper must say that rather than
    // invent a phase or crash reading a file that was never written.
    expect(readRecordedPhases(join(tmpdir(), 'buildmesh-ui-shot-absent.log'))).toEqual([]);
    expect(activePhase(join(tmpdir(), 'buildmesh-ui-shot-absent.log'))).toBeNull();
    expect(killDiagnostic({ label: 'ui-shot', timeoutMs: 1000, phase: null }))
      .toBe('ui-shot did not finish within 1000ms (killed before it recorded a phase)');
  });

  it('bounds the dev-server phases in the server module rather than the CLI', async () => {
    // The server module is the other place a phase can overrun: its readiness
    // probe and the dev-server stop were a bare fetch and a bare 2s literal. The
    // probe must be bounded by what remains of the startup budget, or a probe
    // starting near the deadline runs past it.
    //
    // Exercised through the real module rather than by reading its source: a
    // listener that accepts connections but never answers must be given up on
    // within the startup budget, so a probe that started near the deadline
    // cannot run past it. The reuse half of that contract (a server that is
    // merely slow to answer is still reused) is covered in
    // `ui-shot-server.test.ts` against the same module.
    const timeoutMs = 5000;
    const hung = createTcpServer((socket) => socket.on('data', () => {}));
    await new Promise<void>((resolvePromise, reject) => {
      hung.once('error', reject);
      hung.listen(0, '127.0.0.1', resolvePromise);
    });
    const address = hung.address();
    if (!address || typeof address === 'string') throw new Error('Could not determine the free port');
    try {
      const startedAt = Date.now();
      await expect(
        startDevServer(`http://127.0.0.1:${address.port}`, { timeoutMs }),
      ).rejects.toThrow('is listening but did not answer');
      // Within the budget plus a margin far smaller than an unbounded probe
      // loop would take, so a regressed probe cannot pass by being slow.
      expect(Date.now() - startedAt).toBeLessThan(timeoutMs + 2000);
    } finally {
      await new Promise<void>((resolvePromise) => hung.close(() => resolvePromise()));
    }
  }, 60000);

  it('bounds the dev-server stop even when the child never reports it exited', async () => {
    // `stopDevServer` is the one teardown step with no Playwright timeout to
    // lean on: it kills the child and then waits for a `close` event, which a
    // wedged child may never send. DEV_SERVER_STOP_MS is the only thing bounding
    // that wait, so it is what this asserts — on a child that accepts the kill
    // and stays silent, which is the case the budget exists for.
    //
    // A real spawned child is not usable here: `kill()` succeeds on it, `close`
    // arrives immediately, and the timer never becomes the thing under test.
    const neverCloses = {
      pid: 4242,
      exitCode: null,
      signalCode: null,
      killed: false,
      kill() { this.killed = true; },
      once() { return this; },
    };
    const startedAt = Date.now();
    await stopDevServer(neverCloses as never);
    const elapsed = Date.now() - startedAt;

    expect(neverCloses.killed).toBe(true);
    // It did wait for the budget rather than returning instantly, and it did
    // not wait for the child. Slack on both sides absorbs timer granularity and
    // process scheduling without making the bound itself vague.
    expect(elapsed).toBeGreaterThanOrEqual(DEV_SERVER_STOP_MS / 2);
    expect(elapsed).toBeLessThan(DEV_SERVER_STOP_MS * 5);
  }, 60000);

  it('runs real-app steps through the CLI, proving that path still executes', async () => {
    // `--url` drives a real app with no supervising wrapper and no priced
    // budget, so its step phases stay unbounded. This drives the real CLI in
    // that mode and asserts the steps ran. It cannot observe whether a cap was
    // applied — the current step scripts finish in seconds, far inside any cap
    // — so the `--mock` gate that leaves them uncapped is covered by the phase
    // assertions below and the budget derivation test.
    const { server, url } = await serveHtml('<div id="root"><p>mounted</p></div>');
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-url-steps-'));
    try {
      const markerPath = join(folder, 'marker.json');
      const phaseFile = join(folder, 'phases.log');
      const result = await runUiShot(
        ['--out', join(folder, 'shot.png'), '--url', url, '--steps', recordingSteps],
        WRAPPER_DEADLINE_MS,
        { UI_SHOT_MARKER_PATH: markerPath, [PHASE_FILE_ENV]: phaseFile },
      );

      expect(result.code, result.stderr).toBe(0);
      // The marker is written only if the CLI imported and ran the steps file in
      // `--url` mode, which has no mock IPC helper but still passes a page and
      // the invoke bridge.
      expect(JSON.parse(await readFile(markerPath, 'utf8'))).toMatchObject({
        hasPage: true, hasInvoke: true, fromCli: true,
      });

      // This run never starts a dev server — `--url` points at one that is
      // already serving — so the two dev-server phases must not appear. Without
      // this a "dev-server stop" could be recorded against a server the run
      // does not own, and a kill would then name the wrong phase.
      const phases = readRecordedPhases(phaseFile);
      expect(phases).toContain('browser launch');
      expect(phases).not.toContain('dev-server startup');
      expect(phases).not.toContain('dev-server stop');
    } finally {
      await rm(folder, { recursive: true, force: true });
      await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
    }
  }, TEST_DEADLINE_MS);
});
