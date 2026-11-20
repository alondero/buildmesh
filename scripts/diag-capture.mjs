#!/usr/bin/env node
/**
 * diag-capture.mjs — capture a diagnostic bundle from a running Buildmesh hub
 * (issue #2836).
 *
 * WHY
 *
 * When the hub misbehaves, the decisive data lives inside the WebView: the
 * compositor's trace categories, the layer and animation state, the heap. That
 * data is reachable only through the Chrome DevTools Protocol, and only when
 * the window was started with a debugging port. Without a port the only
 * remedy — reloading the window — destroys the evidence, so **the capture
 * runs before any reload**: this script attaches to the *running* window,
 * records a short trace, and writes everything it saw to a folder the user
 * can hand over.
 *
 * WHAT IT WRITES
 *
 * A timestamped folder:
 *
 *   trace.json      the raw CDP trace events (5 s by default)
 *   summary.json    the machine-readable summary: compositor self time per
 *                   event name, the page probe, the build stamp
 *   README.md      the same content, readable
 *   renderer.log   the latest renderer (frontend) lines from buildmesh.log
 *   diag.log       the latest DIAG lines from diagnostics.log
 *
 * USAGE
 *
 *   npm run diag:capture                    # the stable hub
 *   npm run diag:capture -- --dev           # the dev profile (port 9223)
 *   npm run diag:capture -- --port 9222     # a specific port
 *   npm run diag:capture -- --out .tmp/diag # where the bundle lands
 *   npm run diag:capture -- --seconds 10    # a longer trace
 *
 * Options:
 *   --dev        capture the dev profile (`com.alond.buildmesh.dev`, port 9223)
 *   --port N     the DevTools port to attach to
 *   --out DIR    the folder the bundle is written into (default `.tmp/diag-capture`)
 *   --profile D  the app-data profile to read the logs from
 *   --seconds N  the trace window in seconds (default 5)
 *
 * The port is the hub's, not the script's: the hub opens it when its window
 * is created, so the script attaches to whatever is listening and reports a
 * clear error when no port answers.
 */

import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/**
 * The trace categories the issue asks for.
 *
 * `cc` and `viz` carry the compositor's own steps (`Scheduler::BeginImplFrame`,
 * `TileManager::PrepareTiles`), `toplevel` and `blink` the page's work,
 * `animation` the animation state, `gpu` the GPU thread, and the
 * `disabled-by-default-*` families the DevTools Performance panel records —
 * the same set the Performance panel shows, which is why the DevTools panel
 * named only `RunTask` where this trace names the compositor steps it
 * records.
 */
export const TRACE_CATEGORIES = [
  'cc',
  'viz',
  'benchmark',
  'toplevel',
  'blink',
  'animation',
  'gpu',
  'disabled-by-default-cc.debug',
  'disabled-by-default-cc.debug.scheduler',
  'disabled-by-default-cc.debug.scheduler.frames',
  'disabled-by-default-devtools.timeline',
  'disabled-by-default-devtools.timeline.frame',
];

/** The port the dev profile's window is started with (`scripts/run-dev.ps1 -CdpPort`). */
export const DEV_PORT = 9223;

/** The port the stable hub opens when the preference asks for 9222. */
export const DEFAULT_PORT = 9222;

/** The stable bundle identifier. */
export const STABLE_BUNDLE_ID = 'com.alond.buildmesh';

/** The dev profile's bundle identifier. */
export const DEV_BUNDLE_ID = 'com.alond.buildmesh.dev';

/** How many log lines the bundle carries per log. */
const LOG_TAIL_LINES = 80;

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

function argValue(argv, name) {
  const index = argv.indexOf(`--${name}`);
  if (index === -1) return undefined;
  const value = argv[index + 1];
  if (value === undefined || value.startsWith('--')) {
    throw new Error(`--${name} needs a value.`);
  }
  return value;
}

function hasFlag(argv, name) {
  return argv.includes(`--${name}`);
}

/**
 * The options this run uses.
 *
 * The port is resolved in this order: an explicit `--port`, then the port the
 * profile's own preference asks for (so the script follows the setting the
 * user actually made), then the profile's default.
 */
export function resolveOptions(argv, { profileDir, preferencePort } = {}) {
  const dev = hasFlag(argv, 'dev');
  const explicitPort = argValue(argv, 'port');
  const port = explicitPort
    ? Number(explicitPort)
    : preferencePort ?? (dev ? DEV_PORT : DEFAULT_PORT);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`--port must be a whole number between 1 and 65535 (got ${explicitPort}).`);
  }
  return {
    dev,
    port,
    seconds: Number(argValue(argv, 'seconds') ?? 5),
    outDir: argValue(argv, 'out') ?? join('.tmp', 'diag-capture'),
    profileDir: argValue(argv, 'profile') ?? profileDir,
  };
}

// ---------------------------------------------------------------------------
// The profile (where the logs live)
// ---------------------------------------------------------------------------

/**
 * The app-data profile directory for a bundle identifier.
 *
 * Mirrors what Tauri resolves: the roaming app-data folder on Windows, the
 * Application Support folder on macOS, the XDG data folder on Linux. The
 * dev profile has its own identifier, so the two do not share a log.
 */
export function profileDirFor(bundleId, env = process.env) {
  if (process.platform === 'win32') {
    const appData = env.APPDATA || join(homedir(), 'AppData', 'Roaming');
    return join(appData, bundleId);
  }
  if (process.platform === 'darwin') {
    return join(homedir(), 'Library', 'Application Support', bundleId);
  }
  const xdg = env.XDG_DATA_HOME || join(homedir(), '.local', 'share');
  return join(xdg, bundleId);
}

/**
 * The DevTools port the profile's own preference asks for, if any.
 *
 * A read of one field out of `preferences.json`; a missing or unreadable
 * file simply means "no preference", which is the default anyway.
 */
export async function readPreferencePort(profileDir) {
  if (!profileDir) return null;
  try {
    const raw = await readFile(join(profileDir, 'preferences.json'), 'utf8');
    const parsed = JSON.parse(raw);
    const port = parsed?.diag_cdp_port;
    return typeof port === 'number' && Number.isInteger(port) ? port : null;
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// The trace summary
// ---------------------------------------------------------------------------

/**
 * Turn raw trace events into complete (`X`) events with a duration.
 *
 * Chromium emits most events as complete events, but the begin/end pair is
 * still in the format, and a summary that silently dropped those would
 * under-report the very events it is looking for. Each begin/end pair on a
 * thread becomes one synthetic complete event.
 */
export function toCompleteEvents(events) {
  const complete = [];
  const open = new Map();
  for (const event of events ?? []) {
    if (!event || typeof event.name !== 'string') continue;
    if (event.ph === 'X' && typeof event.dur === 'number') {
      complete.push({
        name: event.name,
        cat: event.cat ?? '',
        ts: event.ts ?? 0,
        dur: event.dur,
        pid: event.pid ?? 0,
        tid: event.tid ?? 0,
      });
    } else if (event.ph === 'B' || event.ph === 'b') {
      const key = `${event.pid ?? 0}:${event.tid ?? 0}:${event.name}`;
      const stack = open.get(key) ?? [];
      stack.push(event.ts ?? 0);
      open.set(key, stack);
    } else if (event.ph === 'E' || event.ph === 'e') {
      const key = `${event.pid ?? 0}:${event.tid ?? 0}:${event.name}`;
      const stack = open.get(key);
      const start = stack?.pop();
      if (start !== undefined) {
        complete.push({
          name: event.name,
          cat: event.cat ?? '',
          ts: start,
          dur: Math.max(0, (event.ts ?? start) - start),
          pid: event.pid ?? 0,
          tid: event.tid ?? 0,
        });
      }
    }
  }
  return complete;
}

/**
 * The self time of each event: its own duration minus the durations of the
 * events nested inside it.
 *
 * "Self time" is the compositor's own work rather than the work it waited
 * for. A summary of total durations would rank `Scheduler::BeginImplFrame`
 * above everything simply because it contains the frame's other steps — the
 * self-time summary is what names the step that actually burned the time.
 */
export function computeSelfTimes(completeEvents) {
  const byThread = new Map();
  for (const event of completeEvents) {
    const key = `${event.pid}:${event.tid}`;
    const list = byThread.get(key) ?? [];
    list.push(event);
    byThread.set(key, list);
  }

  const selfTimes = [];
  for (const list of byThread.values()) {
    const sorted = [...list].sort((a, b) => (a.ts - b.ts) || (b.dur - a.dur));
    const stack = [];
    for (const event of sorted) {
      while (
        stack.length > 0 &&
        event.ts >= stack[stack.length - 1].ts + stack[stack.length - 1].dur
      ) {
        stack.pop();
      }
      if (stack.length > 0) {
        const parent = stack[stack.length - 1];
        parent.childDur = (parent.childDur ?? 0) + event.dur;
      }
      stack.push(event);
    }
    for (const event of list) {
      selfTimes.push({
        ...event,
        selfDur: Math.max(0, event.dur - (event.childDur ?? 0)),
      });
    }
  }
  return selfTimes;
}

/**
 * The compositor self-time summary: one row per event name.
 *
 * Rows are sorted by self time, so the step that burned the most time is
 * first. `totalUs` is the inclusive duration and `selfUs` the exclusive
 * one; both are microseconds, the unit the trace itself uses.
 */
export function summariseTraceEvents(events) {
  const complete = toCompleteEvents(events);
  const rows = new Map();
  for (const event of computeSelfTimes(complete)) {
    const row = rows.get(event.name) ?? {
      name: event.name,
      count: 0,
      totalUs: 0,
      selfUs: 0,
      categories: new Set(),
    };
    row.count += 1;
    row.totalUs += event.dur;
    row.selfUs += event.selfDur;
    for (const category of String(event.cat).split(',')) {
      const trimmed = category.trim();
      if (trimmed) row.categories.add(trimmed);
    }
    rows.set(event.name, row);
  }
  return [...rows.values()]
    .map(row => ({
      name: row.name,
      count: row.count,
      totalUs: row.totalUs,
      selfUs: row.selfUs,
      categories: [...row.categories].sort(),
    }))
    .sort((a, b) => b.selfUs - a.selfUs || b.totalUs - a.totalUs || a.name.localeCompare(b.name));
}

/**
 * The names the issue's acceptance looks for, and whether the trace has them.
 *
 * The two compositor steps the acceptance names are the ones the DevTools
 * Performance panel hides behind a single `RunTask`; a capture that does not
 * contain them is a capture that missed the compositor.
 */
export const EXPECTED_COMPOSITOR_EVENTS = [
  'Scheduler::BeginImplFrame',
  'TileManager::PrepareTiles',
];

/**
 * The compositor events a summary is missing, if any.
 */
export function missingCompositorEvents(summary) {
  const names = new Set(summary.map(row => row.name));
  return EXPECTED_COMPOSITOR_EVENTS.filter(name => !names.has(name));
}

// ---------------------------------------------------------------------------
// The page probe
// ---------------------------------------------------------------------------

/**
 * Read what the page itself says about its own health.
 *
 * Run after the trace, so the measurement does not add a `requestAnimationFrame`
 * loop to the window the trace is recording. The frame gaps are measured
 * over a short window: a gap is the time between two animation frames, and a
 * long one is the visible symptom of a busy main thread.
 */
export async function readPageProbe(page, { frameWindowMs = 1000 } = {}) {
  return page.evaluate(async (windowMs) => {
    const root = document.documentElement;

    // --- frames -------------------------------------------------------
    const frameTimestamps = [];
    await new Promise((resolve) => {
      const started = performance.now();
      const tick = (now) => {
        frameTimestamps.push(now);
        if (now - started >= windowMs) {
          resolve();
          return;
        }
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
      // A window that is not foreground produces no frames at all; resolve on
      // a timer so the probe does not hang on a hidden window.
      setTimeout(resolve, windowMs + 500);
    });
    const gaps = [];
    for (let i = 1; i < frameTimestamps.length; i += 1) {
      gaps.push(frameTimestamps[i] - frameTimestamps[i - 1]);
    }
    const sorted = [...gaps].sort((a, b) => a - b);
    const percentile = (p) =>
      sorted.length === 0 ? null : sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))];

    // --- heap ---------------------------------------------------------
    let heap = null;
    try {
      const memory = performance.memory;
      if (memory) {
        heap = {
          usedJsHeapBytes: memory.usedJSHeapSize ?? null,
          totalJsHeapBytes: memory.totalJSHeapSize ?? null,
          jsHeapSizeLimitBytes: memory.jsHeapSizeLimit ?? null,
        };
      }
    } catch {
      heap = null;
    }

    // --- animations ---------------------------------------------------
    const animations = document.getAnimations();
    const animationStates = {};
    for (const animation of animations) {
      const state = animation.playState ?? 'unknown';
      animationStates[state] = (animationStates[state] ?? 0) + 1;
    }

    // --- canvases -----------------------------------------------------
    const canvases = [...document.querySelectorAll('canvas')].map((canvas) => ({
      width: canvas.width,
      height: canvas.height,
      clientWidth: canvas.clientWidth,
      clientHeight: canvas.clientHeight,
      context: canvas.getContext ? 'readable' : 'unreadable',
    }));

    return {
      url: location.href,
      title: document.title,
      dom: {
        elements: document.getElementsByTagName('*').length,
        rootChildren: root ? root.children.length : 0,
      },
      animations: {
        count: animations.length,
        byPlayState: animationStates,
      },
      canvases: {
        count: canvases.length,
        items: canvases.slice(0, 20),
      },
      heap,
      frames: {
        sampled: frameTimestamps.length,
        windowMs,
        maxGapMs: gaps.length ? Number(Math.max(...gaps).toFixed(1)) : null,
        p95GapMs: percentile(0.95) != null ? Number(percentile(0.95).toFixed(1)) : null,
        medianGapMs: percentile(0.5) != null ? Number(percentile(0.5).toFixed(1)) : null,
        gapsOver50Ms: gaps.filter((gap) => gap > 50).length,
        gapsOver100Ms: gaps.filter((gap) => gap > 100).length,
      },
      idleGate: {
        attribute: root ? root.getAttribute('data-window-idle') : null,
        idle: Boolean(root && root.getAttribute('data-window-idle') === 'true'),
      },
      visibility: document.visibilityState,
      hasFocus: document.hasFocus(),
    };
  }, frameWindowMs);
}

// ---------------------------------------------------------------------------
// The logs
// ---------------------------------------------------------------------------

/**
 * The last lines of a log file that match a predicate.
 *
 * A missing log is not an error: a fresh profile has no `buildmesh.log` yet,
 * and the bundle says so rather than failing the capture.
 */
export async function readLogTail(path, predicate, limit = LOG_TAIL_LINES) {
  let raw;
  try {
    raw = await readFile(path, 'utf8');
  } catch {
    return [];
  }
  const lines = raw.split(/\r?\n/).filter(line => line.trim().length > 0);
  const matched = lines.filter(predicate);
  return matched.slice(-limit);
}

/**
 * The renderer (frontend) lines from `buildmesh.log`.
 *
 * The frontend log bridge forwards the WebView's `console.*` into the Rust
 * log under the `frontend` target, so a renderer line is one carrying that
 * target.
 */
export function readRendererLines(profileDir, limit = LOG_TAIL_LINES) {
  return readLogTail(
    join(profileDir, 'logs', 'buildmesh.log'),
    line => /\bfrontend:/.test(line),
    limit,
  );
}

/**
 * The DIAG lines from `diagnostics.log`.
 *
 * These are the resource sampler's own lines: memory, handles, threads, the
 * PTY queue, the DB reader pool. They are the other half of a slow-UI story
 * — the page probe says what the renderer is doing, the DIAG lines say what
 * the process is holding.
 */
export function readDiagLines(profileDir, limit = LOG_TAIL_LINES) {
  return readLogTail(
    join(profileDir, 'logs', 'diagnostics.log'),
    line => line.startsWith('DIAG '),
    limit,
  );
}

/**
 * The build stamp of the running process.
 *
 * The diagnostics sampler writes one `DIAG session_start git_sha=…` line per
 * session, so the last one names the build that produced this capture.
 */
export function buildStampFromDiagLines(diagLines) {
  for (let i = diagLines.length - 1; i >= 0; i -= 1) {
    const match = /git_sha=(\S+)/.exec(diagLines[i]);
    if (match) {
      return {
        gitSha: match[1],
        sessionLine: diagLines[i],
      };
    }
  }
  return { gitSha: null, sessionLine: null };
}

// ---------------------------------------------------------------------------
// The capture
// ---------------------------------------------------------------------------

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Attach to the running window over CDP and record a trace.
 *
 * The trace is recorded with `ReportEvents` so the events arrive as they
 * happen rather than in one dump at the end — the capture has to survive a
 * window that is already struggling.
 */
export async function captureTrace(page, seconds) {
  const client = await page.context().newCDPSession(page);
  const events = [];
  client.on('Tracing.dataCollected', ({ value }) => {
    for (const event of value) events.push(event);
  });
  const complete = new Promise((resolve) => client.on('Tracing.tracingComplete', resolve));

  await client.send('Tracing.start', {
    categories: TRACE_CATEGORIES.join(','),
    transferMode: 'ReportEvents',
    options: 'sampling-frequency=10000',
  });
  await sleep(seconds * 1000);
  await client.send('Tracing.end');
  await complete;
  await client.detach().catch(() => {});

  return events;
}

/**
 * The page to capture.
 *
 * The dev profile serves the app from Vite (`http://localhost:1420`) and a
 * shipped build serves it from `http://tauri.localhost`, so both are
 * recognised; anything else is only used when it is the only page there is.
 */
export function pickPage(pages) {
  const known = pages.find(page => /tauri\.localhost/.test(page.url())) ??
    pages.find(page => /localhost:1420/.test(page.url()));
  return known ?? pages[0] ?? null;
}

/**
 * The bundle's folder name: a timestamp that sorts chronologically and says
 * what it is.
 */
export function bundleName(date = new Date()) {
  const pad = (value) => String(value).padStart(2, '0');
  const stamp =
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}` +
    `-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`;
  return `diag-capture-${stamp}`;
}

/**
 * The human-readable bundle report.
 */
export function renderBundleMarkdown({ summary, probe, stamp, options, missing }) {
  const rows = summary
    .slice(0, 25)
    .map(
      row =>
        `| \`${row.name}\` | ${row.count} | ${(row.totalUs / 1000).toFixed(1)} | ${(row.selfUs / 1000).toFixed(1)} |`,
    )
    .join('\n');

  const probeRows = [
    ['DOM elements', probe?.dom?.elements ?? 'n/a'],
    ['Animations', probe?.animations?.count ?? 'n/a'],
    ['Canvases', probe?.canvases?.count ?? 'n/a'],
    [
      'JS heap (used)',
      probe?.heap?.usedJsHeapBytes != null
        ? `${(probe.heap.usedJsHeapBytes / (1024 * 1024)).toFixed(1)} MiB`
        : 'n/a',
    ],
    ['Max frame gap', probe?.frames?.maxGapMs != null ? `${probe.frames.maxGapMs} ms` : 'n/a'],
    ['Frames over 50 ms', probe?.frames?.gapsOver50Ms ?? 'n/a'],
    [
      'Idle gate',
      probe?.idleGate?.idle
        ? 'idle (animations paused)'
        : probe?.idleGate?.attribute ?? 'not idle',
    ],
    ['Visibility', probe?.visibility ?? 'n/a'],
  ]
    .map(([name, value]) => `| ${name} | ${value} |`)
    .join('\n');

  return `# Buildmesh diagnostic capture

Captured at ${new Date().toISOString()}.

| | |
|---|---|
| DevTools port | ${options.port} (127.0.0.1) |
| Profile | \`${options.profileDir}\` |
| Trace window | ${options.seconds} s |
| Build stamp | \`${stamp.gitSha ?? 'unknown'}\` |
| Missing compositor events | ${missing.length === 0 ? 'none' : missing.join(', ')} |

## Compositor self time (top 25 by self time)

| Event | Count | Total (ms) | Self (ms) |
|---|---:|---:|---:|
${rows}

## Page probe

| | |
|---|---|
${probeRows}

## What to do with this

Hand the whole folder over. The trace (\`trace.json\`) is the raw evidence;
\`renderer.log\` and \`diag.log\` are the tails of the app's own logs, so the
page-level story and the process-level story sit next to each other.
`;
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

async function main() {
  const argv = process.argv.slice(2);
  const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const dev = hasFlag(argv, 'dev');
  const bundleId = dev ? DEV_BUNDLE_ID : STABLE_BUNDLE_ID;
  const profileDir = profileDirFor(bundleId);
  const preferencePort = await readPreferencePort(profileDir);
  const options = resolveOptions(argv, { profileDir, preferencePort });
  options.profileDir = options.profileDir ?? profileDir;

  console.log(`diag:capture — attaching to 127.0.0.1:${options.port} (${bundleId})`);

  const { chromium } = await import('playwright');
  let browser;
  try {
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${options.port}`);
  } catch (error) {
    console.error(
      `\nNo DevTools port answered on 127.0.0.1:${options.port}.\n` +
        'The port is opened when the hub window is created, so it has to be on\n' +
        'before the launch: set it in Settings → General → Diagnostics (or set\n' +
        `BUILDMESH_DIAG_CDP_PORT=${options.port} for a debug/dev-tools build) and\n` +
        'restart Buildmesh.\n\n' +
        `Underlying error: ${error instanceof Error ? error.message : String(error)}`,
    );
    process.exitCode = 1;
    return;
  }

  try {
    const pages = browser.contexts().flatMap(context => context.pages());
    const page = pickPage(pages);
    if (!page) {
      throw new Error(`No page found over CDP on port ${options.port}.`);
    }
    console.log(`diag:capture — recording ${options.seconds}s trace on ${page.url()}`);

    const events = await captureTrace(page, options.seconds);
    const summary = summariseTraceEvents(events);
    const missing = missingCompositorEvents(summary);

    const probe = await readPageProbe(page);
    const diagLines = await readDiagLines(options.profileDir);
    const rendererLines = await readRendererLines(options.profileDir);
    const stamp = buildStampFromDiagLines(diagLines);

    const outRoot = resolve(repoRoot, options.outDir);
    const bundleDir = join(outRoot, bundleName());
    await mkdir(bundleDir, { recursive: true });

    const payload = {
      capturedAt: new Date().toISOString(),
      port: options.port,
      profileDir: options.profileDir,
      seconds: options.seconds,
      buildStamp: stamp,
      missingCompositorEvents: missing,
      compositorSummary: summary,
      pageProbe: probe,
    };

    await writeFile(join(bundleDir, 'trace.json'), JSON.stringify(events, null, 2), 'utf8');
    await writeFile(join(bundleDir, 'summary.json'), `${JSON.stringify(payload, null, 2)}\n`, 'utf8');
    await writeFile(
      join(bundleDir, 'README.md'),
      renderBundleMarkdown({ summary, probe, stamp, options, missing }),
      'utf8',
    );
    await writeFile(join(bundleDir, 'renderer.log'), `${rendererLines.join('\n')}\n`, 'utf8');
    await writeFile(join(bundleDir, 'diag.log'), `${diagLines.join('\n')}\n`, 'utf8');

    console.log(`diag:capture — bundle written to ${bundleDir}`);
    console.log(
      `diag:capture — compositor events: ${summary.length} distinct; ` +
        (missing.length === 0
          ? 'the expected compositor steps are present.'
          : `MISSING: ${missing.join(', ')}`),
    );
    console.log(`diag:capture — build stamp: ${stamp.gitSha ?? 'unknown'}`);
  } finally {
    await browser.close().catch(() => {});
  }
}

const invokedDirectly =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (invokedDirectly) {
  main().catch(error => {
    console.error(error);
    process.exitCode = 1;
  });
}
