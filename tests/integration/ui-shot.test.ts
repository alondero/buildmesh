import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createServer as createTcpServer } from 'node:net';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
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
  UI_SHOT_STEP_BUDGETS_MS,
} from '../../scripts/ui-shot-budgets.mjs';
import { runSteps } from '../../scripts/ui-shot-steps.mjs';
import { withDeadline } from '../../scripts/ui-shot-deadline.mjs';

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

// Budget ordering matters, and the arithmetic has to actually hold against the
// values the child really uses. The wrapper must exceed the sum of every phase
// the child spends — listed in `PRICED_PHASES` below — or it kills the child
// mid-flight and reports a bare transport error instead of the real diagnostic
// (issue #2049 class). The per-test timeout then exceeds the wrapper, so the
// wrapper's diagnostic — which carries the child's stdout/stderr — is what fails
// the run.
//
// The child's budgets are IMPORTED, not copied: a literal here would drift the
// moment a timeout is raised in the scripts and the check below would keep
// passing while the wrapper silently fell below the real worst case again.
const CHILD_WORST_CASE_MS = UI_SHOT_STEP_BUDGETS_MS;
// Slack above the priced phases, for the child's own startup and teardown
// outside them (Node boot, module loading, process exit).
const WRAPPER_SLACK_MS = 30000;
const WRAPPER_DEADLINE_MS = CHILD_WORST_CASE_MS + WRAPPER_SLACK_MS;
// The per-test budget adds the same slack again, so a wrapper timeout — which
// carries the child's stdout and stderr — is what fails the run rather than
// Vitest cutting the test off with no diagnostic.
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
      reject(new Error(`ui-shot did not finish within ${timeoutMs}ms\n${stdout}\n${stderr}`));
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

// The phases a `--mock` run spends, as `[name, budget, call count]`. The audit
// checks the exported sum against this list, so dropping a phase from either
// side fails: a copied formula would assert its own omission, which is how setup
// went unpriced in the first place (#2063).
const PRICED_PHASES: ReadonlyArray<readonly [string, number, number]> = [
  ['dev-server startup', DEV_SERVER_STARTUP_MS, 1],
  ['browser launch', BROWSER_LAUNCH_TIMEOUT_MS, 2],
  ['page setup', BROWSER_SETUP_TIMEOUT_MS, 2],
  ['fixtures load', FIXTURES_LOAD_TIMEOUT_MS, 1],
  ['navigation', NAVIGATION_TIMEOUT_MS, 1],
  ['mount wait', MOUNT_TIMEOUT_MS, 1],
  ['step script', STEP_SCRIPT_TIMEOUT_MS, 1],
  ['step module load', STEP_MODULE_LOAD_TIMEOUT_MS, 1],
  ['selector wait', ELEMENT_VISIBLE_TIMEOUT_MS, 1],
  ['screenshot', SCREENSHOT_TIMEOUT_MS, 1],
  ['browser close', BROWSER_CLOSE_TIMEOUT_MS, 1],
  ['dev-server stop', DEV_SERVER_STOP_MS, 1],
];
const pricedTotal = () => PRICED_PHASES.reduce((total, [, ms, calls]) => total + ms * calls, 0);

// The mount-failure run supervises its child with the same wrapper deadline as
// every other mock render, and does so by name rather than by a second copy of
// the arithmetic — a hand-written subset had to be kept in step with the sum
// above, and that is the drift this table exists to remove. There is deliberately
// no separate constant to check here: an alias of `WRAPPER_DEADLINE_MS` could
// only be compared against itself.

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
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-header-'));
    try {
      const port = await freePort();
      const output = join(folder, 'header.png');
      const result = await runUiShot([
        '--out', output,
        '--mock',
        '--serve',
        '--mock-url', `http://127.0.0.1:${port}`,
        '--fixtures', nodeHeaderFixtures,
        '--steps', nodeHeaderSteps,
        '--selector', '[data-testid=grid-node-header]',
      ]);

      expect(result.code, result.stderr).toBe(0);
      expect(result.stdout).toContain('Saved');
      expect((await stat(output)).size).toBeGreaterThan(0);
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

  it('bounds the listed mock-run phases in the child with a priced budget', () => {
    // The sum only holds if the child actually applies a budget at each phase.
    // Nothing else in this file proves that: the render tests assert an exit code
    // and a PNG, which are produced whether or not any timeout was passed, so
    // removing one of these leaves the suite green while the phase silently
    // reverts to Playwright's own default — the exact way the sum came to
    // underprice the child (#2063).
    //
    // Scope: the phases below, which are the ones a `--mock` run spends and the
    // sum prices. Elsewhere the budgets differ: `--url` launches Chromium under
    // the same budget but leaves page creation and the step phases unbounded,
    // and its navigation falls back to Playwright's own default rather than a
    // priced one. The CDP-attach branch is unbounded throughout. None of that is
    // audited here. A phase absent from both the list and the sum is not covered
    // by this test.
    //
    // This is a source audit rather than a runtime observation, because the
    // budgets are large by design (minutes) and waiting them out would test
    // patience, not wiring. It fails when a listed call loses its budget.
    const source = readFileSync(uiShot, 'utf8');

    // Each entry: the call under audit, and the priced constant that must appear
    // in its argument object. `withDeadline` entries are the phases Playwright
    // accepts no timeout for, bounded by racing instead.
    const bounded = [
      { call: 'page.goto', phase: 'NAVIGATION_TIMEOUT_MS', label: 'navigation' },
      { call: '#root > *', phase: 'MOUNT_TIMEOUT_MS', label: 'mount wait' },
      { call: "el.waitFor({ state: 'visible'", phase: 'ELEMENT_VISIBLE_TIMEOUT_MS', label: 'selector wait' },
      { call: 'page.screenshot', phase: 'SCREENSHOT_TIMEOUT_MS', label: 'page screenshot' },
      { call: 'el.screenshot', phase: 'SCREENSHOT_TIMEOUT_MS', label: 'element screenshot' },
    ];
    for (const { call, phase, label } of bounded) {
      const at = source.indexOf(call);
      expect(at, `${call} is no longer in scripts/ui-shot.mjs, so the ${label} phase is unbudgeted`).toBeGreaterThan(-1);
      // The budget must belong to this call, not merely appear later in the file.
      // Scan the statement rather than to the first `)`: some of these selectors
      // contain one, which would truncate the search before the timeout.
      const statement = source.slice(at, source.indexOf('\n', at));
      expect(statement, `${call} must pass timeout: ${phase}`).toContain(`timeout: ${phase}`);
    }

    // Each launch call must spread the budgeted options object. Checking the exact
    // argument — rather than searching the matched text for the word "launch",
    // which every match contains by construction — is what makes this able to
    // fail. A primary or fallback branch that drops the spread (M1, M4) fails.
    expect(source, 'the launch options must carry the launch budget').toContain(
      'const launch = { timeout: BROWSER_LAUNCH_TIMEOUT_MS }',
    );
    const launchCalls = source.match(/chromium\.launch\([^;\n]*/g) ?? [];
    expect(launchCalls.length, 'both the override and the fallback launch must exist').toBe(3);
    for (const call of launchCalls) {
      // Each branch either passes the budgeted object or spreads it into a larger
      // literal. Anything else (`opts`, `{ executablePath }`) has dropped the
      // budget and falls back to Playwright's default.
      expect(call, `${call} must pass or spread the budgeted launch options`).toMatch(
        /^chromium\.launch\((launch|\{\s*\.\.\.launch\b)/,
      );
      // Presenting a budget object is not enough: a later `timeout` key would
      // override the priced one, and `timeout: 0` disables it outright. The
      // budget belongs solely to the `launch` object, so no launch call may
      // carry a `timeout` of its own.
      expect(call, `${call} must not set a timeout of its own; only the priced object may`).not.toMatch(/\btimeout\s*:/);
    }

    // The close phases Playwright gives no timeout argument for, so they are
    // bounded by racing. Both sites are checked on their own statement: a
    // file-wide substring check would let one site drop its budget while the
    // other's text still satisfied it.
    const closeRaces = source.match(/withDeadline\(\s*browser\.close\(\)[^;]*;/g) ?? [];
    expect(closeRaces.length, 'both the normal and the setup-failure close must be raced').toBe(2);
    for (const race of closeRaces) {
      expect(race, `the browser close race must pass ${BROWSER_CLOSE_TIMEOUT_MS}`).toContain('BROWSER_CLOSE_TIMEOUT_MS');
    }

    // Page setup and the fixtures load take no `timeout` argument either, so each
    // is raced against its own priced budget rather than left unbounded. Reading
    // the fixtures is awaited before `addInitScript` is even reached, so its race
    // is the only budget that phase has.
    const racedPhases = [
      {
        pattern: /withDeadline\(\s*(?:browser\.newPage|page\.addInitScript)\([^;]*;/g,
        count: 2,
        constant: 'BROWSER_SETUP_TIMEOUT_MS',
        label: 'page setup',
      },
      {
        pattern: /withDeadline\(\s*loadFixtures\([^;]*;/g,
        count: 1,
        constant: 'FIXTURES_LOAD_TIMEOUT_MS',
        label: 'fixtures load',
      },
    ];
    for (const { pattern, count, constant, label } of racedPhases) {
      const races = source.match(pattern) ?? [];
      expect(races.length, `every ${label} call must be raced`).toBe(count);
      for (const race of races) {
        expect(race, `the ${label} race must pass ${constant}`).toContain(constant);
      }
    }

    // The normal teardown runs in every mode, so its close must stay gated on
    // `--mock`; only the setup-failure site, already inside the mock branch, may
    // pass the bare constant. Counting the ungated ones is what catches a
    // teardown that lost its gate (M2): a file-wide search would be satisfied by
    // whichever site still had the gate.
    expect(
      closeRaces.filter((race) => /browser\.close\(\),\s*BROWSER_CLOSE_TIMEOUT_MS\b/.test(race)).length,
      'only the setup-failure close may be ungated',
    ).toBe(1);
    expect(
      closeRaces.filter((race) => /mock \? BROWSER_CLOSE_TIMEOUT_MS : null/.test(race)).length,
      'the teardown close must keep its --mock gate',
    ).toBe(1);

    // The step run and the module load are gated on `--mock` on purpose: real-app
    // modes have no supervising wrapper and no priced sum, so a cap there would
    // limit them by a number nothing accounts for. A runtime test cannot detect
    // this — the current step scripts finish in seconds, far inside either cap —
    // so the gate is audited where it is written.
    expect(source).toContain(
      'mock\n        ? { timeoutMs: STEP_SCRIPT_TIMEOUT_MS, moduleLoadTimeoutMs: STEP_MODULE_LOAD_TIMEOUT_MS }\n        : { timeoutMs: null, moduleLoadTimeoutMs: null }',
    );

    // The sum must equal every phase the table names, so an omission on either
    // side fails here.
    expect(CHILD_WORST_CASE_MS, 'the exported sum must equal every priced phase').toBe(pricedTotal());
    // The server module is the other place a phase can overrun: its readiness probe
    // and the dev-server stop were a bare fetch and a bare 2s literal. The probe
    // must also be given only the remaining startup budget, or a probe starting
    // near the deadline runs past it.
    const server = readFileSync(resolve(repoRoot, 'scripts', 'ui-shot-server.mjs'), 'utf8');
    expect(server).toContain('AbortSignal.timeout(timeoutMs)');
    // Every probe is bounded by what remains of the startup budget, and there is
    // no separate per-probe cap: one would stop a live dev server whose first
    // response is slow from ever answering, which is the reuse regression the
    // TCP-connect check exists to prevent (#2063).
    expect(
      (server.match(/isReady\(mockUrl, remaining\(\)\)/g) ?? []).length,
      'the reuse and spawn probes must both be bounded by the remaining budget',
    ).toBe(2);
    expect(server, 'reuse must be decided by a TCP connect, not by an HTTP probe').toContain('isListening(host, port)');
    expect(server, 'the startup clock must start before the reuse check').toMatch(/const startedAt = Date\.now\(\);[\s\S]*?isListening\(host, port\)/);
    expect(server).toContain('setTimeout(finish, DEV_SERVER_STOP_MS)');
    expect(server).toContain('Math.min(DEV_READY_POLL_MS, remaining())');
  });

  it('runs real-app steps through the CLI, proving that path still executes', async () => {
    // `--url` drives a real app with no supervising wrapper and no priced sum, so
    // its step phases stay unbounded. This drives the real CLI in that mode and
    // asserts the steps ran. It cannot observe whether a cap was applied — the
    // current step scripts finish in seconds, far inside any cap — so the
    // `--mock` gate that leaves them uncapped is audited in the budget test
    // above. Between them the gate is covered; this test alone would not be.
    const { server, url } = await serveHtml('<div id="root"><p>mounted</p></div>');
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-url-steps-'));
    try {
      const markerPath = join(folder, 'marker.json');
      const result = await runUiShot(
        ['--out', join(folder, 'shot.png'), '--url', url, '--steps', recordingSteps],
        WRAPPER_DEADLINE_MS,
        { UI_SHOT_MARKER_PATH: markerPath },
      );

      expect(result.code, result.stderr).toBe(0);
      // The marker is written only if the CLI imported and ran the steps file in
      // `--url` mode, which has no mock IPC helper but still passes a page and
      // the invoke bridge.
      expect(JSON.parse(await readFile(markerPath, 'utf8'))).toMatchObject({
        hasPage: true, hasInvoke: true, fromCli: true,
      });
    } finally {
      await rm(folder, { recursive: true, force: true });
      await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
    }
  }, TEST_DEADLINE_MS);
});
