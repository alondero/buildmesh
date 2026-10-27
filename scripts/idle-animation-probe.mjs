/**
 * idle-animation-probe.mjs — measure the renderer's frame production with and
 * without the idle animation gate (issue #2599).
 *
 * WHY
 *
 * The renderer Compositor thread produces a frame every vsync while any
 * infinite CSS animation runs. That is invisible in the UI and expensive: on a
 * long-running hub the compositor sat near one core for hours while the
 * JavaScript main thread was idle, because the page never went idle.
 *
 * The probe renders the REAL desktop frontend in Chromium against the mock IPC
 * bridge (`scripts/ui-mock/tauri-mock.mjs`), then records a Chromium trace
 * while the app is in each of two states:
 *
 *   focused — the window holds focus, the animations run, the compositor
 *             produces frames;
 *   idle    — the window lost focus (`tauri://blur`), the gate pauses the
 *             decorative infinite animations, and the compositor should stop.
 *
 * Measured on the hub-shaped scene (16 nodes, 34 animated elements): the
 * focused state records ~60 compositor frames per second (the trace's
 * BeginFrame events); the idle state records none. The frames are the measurement. The CPU share the probe also
 * reports is the corroboration, and it depends on the machine — a shared,
 * busy host attributes time to the renderer that has nothing to do with it.
 *
 * It prints the frame counts, the animation play states and the gate marker,
 * so the numbers can be compared before and after the change. It REPORTS; it
 * does not assert — the same file runs against a tree that has no gate at all,
 * which is exactly the "before" measurement.
 *
 * USAGE
 *
 *   node scripts/idle-animation-probe.mjs --serve
 *   node scripts/idle-animation-probe.mjs --serve --headed --seconds 8
 *   node scripts/idle-animation-probe.mjs --mock-url http://localhost:1420
 *   node scripts/idle-animation-probe.mjs --serve --fixtures scripts/ui-mock/hub-fixtures.mjs
 *   node scripts/idle-animation-probe.mjs --cdp 9223
 *
 * Options:
 *   --serve         start this worktree's Vite server and stop it afterwards
 *   --mock-url URL  the app URL (default http://localhost:1420)
 *   --fixtures PATH fixture overrides (default: the mock's own defaults)
 *   --cdp PORT      measure the REAL dev-profile window over CDP instead of
 *                   the mock (the window's focus state is then the OS's, not
 *                   the probe's, so the operator sets it between runs)
 *   --seconds N     measurement window per state (default 5)
 *   --viewport WxH  viewport size (default 1920x1080)
 *   --headed        run a visible browser instead of headless
 *   --json PATH     also write the measurement as JSON
 */

import { writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { chromium } from 'playwright';
import { buildInitScript, loadFixtures } from './ui-mock/tauri-mock.mjs';
import { startDevServer, stopDevServer } from './ui-shot-server.mjs';

function arg(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index !== -1 && process.argv[index + 1] ? process.argv[index + 1] : fallback;
}

function flag(name) {
  return process.argv.includes(`--${name}`);
}

const mockUrl = arg('mock-url', 'http://localhost:1420');
const seconds = Number(arg('seconds', '5'));
const [width, height] = arg('viewport', '1920x1080').split('x').map(Number);
const headed = flag('headed');
const jsonOut = arg('json');
const fixturesFile = arg('fixtures');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function launchChromium() {
  const options = { headless: !headed, timeout: 60000 };
  const override = arg('chromium', process.env.BUILDMESH_CHROMIUM);
  if (override) return chromium.launch({ ...options, executablePath: override });
  try {
    return await chromium.launch(options);
  } catch (error) {
    const base = process.env.PLAYWRIGHT_BROWSERS_PATH;
    const candidate = base && join(base, 'chromium');
    if (candidate && existsSync(candidate)) {
      return chromium.launch({ ...options, executablePath: candidate });
    }
    throw error;
  }
}

/**
 * The renderer processes of the browser this run launched.
 *
 * The issue's acceptance is stated in CPU ("the renderer process uses under
 * 5% of a core"), so the probe reports that number rather than leaving the
 * reader to convert frames into an estimate. The browser is a child of this
 * node process, and its renderers are the browser's children of that type.
 *
 * Windows only: the process tree is read through CIM. Elsewhere the CPU
 * sample is reported as null and the frame counts still stand on their own.
 */
function findRendererPids() {
  if (process.platform !== 'win32') return null;
  const query = (filter) => {
    const script = `Get-CimInstance Win32_Process -Filter "${filter}" | Select-Object ProcessId, ParentProcessId, Name, CommandLine | ConvertTo-Json -Compress`;
    const output = execFileSync('powershell', ['-NoProfile', '-Command', script], {
      encoding: 'utf8',
      timeout: 30000,
    }).trim();
    if (!output) return [];
    const parsed = JSON.parse(output);
    return Array.isArray(parsed) ? parsed : [parsed];
  };

  const browsers = query(`ParentProcessId=${process.pid}`).filter((entry) =>
    /chrome|msedge|headless/i.test(entry.Name ?? ''),
  );
  if (browsers.length === 0) return null;
  const rendererPids = [];
  for (const browser of browsers) {
    for (const child of query(`ParentProcessId=${browser.ProcessId}`)) {
      if (child.CommandLine && /--type=renderer/.test(child.CommandLine)) rendererPids.push(child.ProcessId);
    }
  }
  return rendererPids.length ? rendererPids : null;
}

/** The renderer's total processor time, in seconds. */
function sampleRendererCpu(pids) {
  if (!pids) return null;
  const list = pids.join(',');
  // `Measure-Object -Sum` refuses a TimeSpan ("Input object ... is not
  // numeric"), so the times are added as TimeSpans and converted once.
  const script = [
    `$p = @(Get-Process -Id ${list} -ErrorAction SilentlyContinue)`,
    '$total = [TimeSpan]::Zero',
    'foreach ($proc in $p) { $total = $total + $proc.TotalProcessorTime }',
    '$total.TotalSeconds',
  ].join('; ');
  const output = execFileSync('powershell', ['-NoProfile', '-Command', script], {
    encoding: 'utf8',
    timeout: 30000,
  }).trim();
  if (!output) return 0;
  const seconds = Number(output);
  return Number.isFinite(seconds) ? seconds : null;
}

/**
 * Record a Chromium trace for `seconds` and count the events that describe
 * frame production.
 *
 * `DrawFrame` is the compositor drawing a frame; `BeginFrame` is the frame
 * pipeline asking for one. A page with nothing to animate produces neither,
 * which is the whole claim this probe measures.
 */
async function measureFrames(page, seconds, rendererPids) {
  const cpuBefore = sampleRendererCpu(rendererPids);
  const client = await page.context().newCDPSession(page);
  const events = [];
  client.on('Tracing.dataCollected', ({ value }) => {
    for (const event of value) events.push(event);
  });
  const complete = new Promise((resolve) => client.on('Tracing.tracingComplete', resolve));

  await client.send('Tracing.start', {
    categories: 'disabled-by-default-devtools.timeline.frame,devtools.timeline',
    transferMode: 'ReportEvents',
    options: 'sampling-frequency=10000',
  });
  await sleep(seconds * 1000);
  await client.send('Tracing.end');
  await complete;
  await client.detach().catch(() => {});

  const cpuAfter = sampleRendererCpu(rendererPids);
  const counts = {};
  for (const event of events) counts[event.name] = (counts[event.name] || 0) + 1;
  const cpuSeconds = cpuBefore != null && cpuAfter != null ? cpuAfter - cpuBefore : null;
  return {
    totalEvents: events.length,
    drawFrame: counts.DrawFrame || 0,
    beginFrame: counts.BeginFrame || 0,
    commit: counts.Commit || 0,
    // The CPU the renderer burned over the same window, as a share of ONE core
    // — the number the issue's acceptance is written in.
    rendererCpuSeconds: cpuSeconds != null ? Number(cpuSeconds.toFixed(3)) : null,
    rendererPercentOfOneCore:
      cpuSeconds != null ? Number(((100 * cpuSeconds) / seconds).toFixed(2)) : null,
    // The names that actually carry frame production, so an unexpected trace
    // shape is visible instead of silently reading as "no frames".
    eventNames: Object.entries(counts)
      .filter(([, count]) => count > 0)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 12)
      .map(([name, count]) => `${name}=${count}`),
  };
}

/** What the page itself says about its animations. */
async function readAnimationState(page) {
  return page.evaluate(() => {
    const root = document.documentElement;
    const animated = [...document.querySelectorAll('.animate-pulse-fast, .animate-pulse, .animate-spin, .node-glyph-comet, .animate-border-pulse')];
    const sample = animated[0] || null;
    // The animation may sit on a pseudo-element (`.animate-border-pulse::after`),
    // and the element's own play state would then read "running" while the
    // pseudo-element is paused — so the state is read from whichever of the
    // two actually carries an animation.
    const own = sample ? getComputedStyle(sample) : null;
    const after = sample ? getComputedStyle(sample, '::after') : null;
    const carriesOwn = Boolean(own && own.animationName && own.animationName !== 'none');
    const carriesAfter = Boolean(after && after.animationName && after.animationName !== 'none');
    const samplePlayState = !sample
      ? null
      : carriesOwn
        ? own.animationPlayState
        : carriesAfter
          ? after.animationPlayState
          : own.animationPlayState;
    return {
      idleMarker: root.getAttribute('data-window-idle'),
      animatedElementCount: animated.length,
      sampleSelector: sample ? sample.className.toString() : null,
      samplePlayState,
      // Only reported when the pseudo-element carries an animation of its
      // own: otherwise its computed play state is the initial "running",
      // which would read as "not gated" next to a paused sample.
      sampleAfterPlayState: carriesAfter ? after.animationPlayState : null,
      animations: document.getAnimations().map((animation) => ({
        name: animation.animationName ?? '(none)',
        playState: animation.playState,
      })),
    };
  });
}

/**
 * Attach to the real dev-profile window over CDP.
 *
 * The real window's focus is the OS's business, not the probe's: the marker
 * on the root element says which state the app is in, and the operator sets
 * that state (alt-tab, another window) between runs. Nothing here fakes a
 * focus change — a mocked `tauri://blur` would prove only that the probe can
 * make the page do what it was told.
 */
async function attachRealWindow(port) {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  const pages = browser.contexts().flatMap((context) => context.pages());
  const page = pages.find((candidate) => /^https?:\/\/tauri\.localhost/.test(candidate.url()))
    ?? pages[0];
  if (!page) throw new Error(`No page found over CDP on port ${port}.`);
  return { browser, page };
}

const cdpPort = arg('cdp');
const devServer = !cdpPort && flag('serve') ? await startDevServer(mockUrl) : null;
let browser;
let attached = false;
try {
  let page;
  if (cdpPort) {
    const attachedWindow = await attachRealWindow(cdpPort);
    browser = attachedWindow.browser;
    page = attachedWindow.page;
    attached = true;
    await sleep(1500);
  } else {
    browser = await launchChromium();
    page = await browser.newPage({ viewport: { width: width || 1920, height: height || 1080 } });
    await page.addInitScript(buildInitScript(await loadFixtures(fixturesFile)));
    await page.goto(mockUrl, { waitUntil: 'domcontentloaded', timeout: 60000 });
    await page.locator('#root > *').first().waitFor({ state: 'attached', timeout: 60000 });
    // The shell renders its fixtures asynchronously; give the workspace a moment
    // so the animated elements exist before the first measurement.
    await sleep(1500);
  }

  const results = {};
  // The renderer of the browser this run launched. In CDP mode the browser is
  // the real app's, whose renderer belongs to the app, not to this process.
  const rendererPids = cdpPort ? null : findRendererPids();

  if (cdpPort) {
    // One state: whichever the OS left the window in.
    const state = await readAnimationState(page);
    results.current = {
      idleMarker: state.idleMarker,
      frames: await measureFrames(page, seconds, rendererPids),
      animations: state,
    };
  } else {
    results.focused = {
      frames: await measureFrames(page, seconds, rendererPids),
      animations: await readAnimationState(page),
    };

    // The same OS event the real app delivers when the user alt-tabs away.
    const delivered = await page.evaluate(() => window.__BUILDMESH_MOCK__.emit('tauri://blur'));
    await sleep(500);
    results.idle = {
      blurEventListeners: delivered,
      frames: await measureFrames(page, seconds, rendererPids),
      animations: await readAnimationState(page),
    };

    await page.evaluate(() => window.__BUILDMESH_MOCK__.emit('tauri://focus'));
    await sleep(500);
    results.refocused = {
      frames: await measureFrames(page, seconds, rendererPids),
      animations: await readAnimationState(page),
    };
  }

  const summary = {
    url: mockUrl,
    viewport: `${width}x${height}`,
    secondsPerState: seconds,
    rendererPids,
    ...results,
  };

  console.log(JSON.stringify(summary, null, 2));
  if (jsonOut) await writeFile(jsonOut, `${JSON.stringify(summary, null, 2)}\n`, 'utf8');
} finally {
  if (browser) await browser.close().catch(() => {});
  if (devServer) await stopDevServer(devServer);
}
