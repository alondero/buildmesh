#!/usr/bin/env node
/**
 * ui-shot.mjs — drive the Buildmesh UI and save a screenshot. Three modes:
 *
 * 1. Desktop (default): attach to the RUNNING buildmesh-dev window over the
 *    Chrome DevTools Protocol (CDP). The dev-profile app must have been
 *    launched with a CDP port:
 *        powershell -File scripts\run-dev.ps1 -CdpPort 9223
 *    WebView2 (Tauri's Windows renderer) is Chromium-based, so Playwright
 *    attaches to the real app window — real Tauri IPC, real backend, real
 *    pixels. This is NOT the Vite-dev-server e2e path (ports 1420/1991); it
 *    never touches the stable hub. Windows-only (WKWebView/WebKitGTK have no
 *    CDP attach).
 *
 * 2. Mobile SPA (--url): launch headless Chromium against the dev profile's
 *    HTTP server, e.g. --url "http://127.0.0.1:2992/v2?token=<token>"
 *    (get the token via the invoke bridge: get_root_token).
 *
 * 3. Mock (--mock): launch the pre-installed headless Chromium against a
 *    plain Vite dev server (default http://localhost:1420) with a fake
 *    Tauri IPC bridge injected before boot (scripts/ui-mock/tauri-mock.mjs).
 *    No Windows, no CDP, no Rust backend — the real frontend renders with
 *    FIXTURE data. This is the path for a headless/non-Windows host (Claude
 *    Code on the web, CI) where modes 1–2 can't run. It proves the UI
 *    renders + reacts, NOT that the backend behaves — treat it as a visual
 *    smoke check, and say so in the PR. Start Vite yourself, or pass
 *    `--serve` to have this script start (and stop) it for you.
 *
 * Usage:
 *   node scripts/ui-shot.mjs --out shots/after.png
 *   node scripts/ui-shot.mjs --out shots/after.png --steps my-steps.mjs
 *   node scripts/ui-shot.mjs --out shots/after.png --selector "[data-session-item]"
 *   node scripts/ui-shot.mjs --out shots/mobile.png --url "http://127.0.0.1:2992/v2?token=..." --viewport 390x844
 *   node scripts/ui-shot.mjs --out shots/after.png --mock --serve            # headless, self-hosted dev server
 *   node scripts/ui-shot.mjs --out shots/after.png --mock --fixtures fx.mjs  # against an already-running dev server
 *
 * Options:
 *   --out <file.png>     required; parent dirs are created
 *   --cdp <port>         CDP port (default 9223, matches run-dev.ps1 -CdpPort)
 *   --url <url>          mobile-SPA mode: launch headless Chromium at this URL
 *   --viewport <WxH>     viewport size (--url/--mock modes; default 390x844 / 1440x900)
 *   --mock               mock mode: headless Chromium + fake Tauri IPC (see above)
 *   --mock-url <url>     dev-server URL for --mock (default http://localhost:1420)
 *   --fixtures <file>    --mock only: .mjs/.json overriding the default IPC fixtures
 *   --serve              --mock only: start this worktree's Vite server and wait for it (auto-stops)
 *   --steps <file.mjs>   module whose default export is `async ({ page, invoke, mock }) => {}`
 *                        run before the screenshot (click, fill, wait, assert…)
 *   --selector <css>     screenshot only this element (default: full window/page)
 *   --invoke-port <n>    HTTP test-bridge port for the `invoke` helper
 *                        (default 2991 — the dev profile's test server)
 *
 * The `invoke` helper POSTs to the backend's HTTP test bridge, e.g.
 *   await invoke('create_test_mesh', { name: 'Shot fixture' })
 * Only commands routed in src-tauri/src/commands/test.rs are available there;
 * everything else should be driven through the UI via `page`. In --mock mode
 * there is no bridge — steps get a `mock` helper instead:
 *   await mock.on('list_meshes', [...]);      // override an IPC response
 *   await mock.emit('node-status-changed', { nodeId: 1 });  // push a backend event
 *
 * Exit codes: 0 = screenshot written; 1 = any failure (message on stderr).
 * A throwing steps module fails the run — use that for functional assertions.
 */
import { chromium } from 'playwright';
import { mkdirSync, existsSync } from 'fs';
import { dirname, resolve, join } from 'path';
import { pathToFileURL } from 'url';
import { buildInitScript, loadFixtures } from './ui-mock/tauri-mock.mjs';
import { startDevServer, stopDevServer } from './ui-shot-server.mjs';

/**
 * Launch Chromium, tolerating a host whose pre-installed browser doesn't
 * match the build Playwright pins (headless CI / Claude Code on the web,
 * where `PLAYWRIGHT_BROWSERS_PATH` points at a system Chromium). On a normal
 * dev box the bundled browser is found and this is a plain launch.
 *
 * Resolution order for the executable:
 *   1. --chromium <path> / BUILDMESH_CHROMIUM env (explicit override)
 *   2. plain launch (bundled browser — the Windows/dev path)
 *   3. $PLAYWRIGHT_BROWSERS_PATH/chromium (the symlink the web env provides)
 */
async function launchChromium(opts = {}) {
  const override = arg('chromium', process.env.BUILDMESH_CHROMIUM);
  if (override) return chromium.launch({ ...opts, executablePath: override });
  try {
    return await chromium.launch(opts);
  } catch (e) {
    const base = process.env.PLAYWRIGHT_BROWSERS_PATH;
    const candidate = base && join(base, 'chromium');
    if (candidate && existsSync(candidate)) {
      return chromium.launch({ ...opts, executablePath: candidate });
    }
    throw e;
  }
}

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i !== -1 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
}
function flag(name) {
  return process.argv.includes(`--${name}`);
}

const out = arg('out');
const cdpPort = Number(arg('cdp', '9223'));
const url = arg('url');
const mock = flag('mock');
const mockUrl = arg('mock-url', 'http://localhost:1420');
const fixturesFile = arg('fixtures');
const serve = flag('serve');
const viewport = arg('viewport', mock ? '1440x900' : '390x844');
const stepsFile = arg('steps');
const selector = arg('selector');
const invokePort = Number(arg('invoke-port', '2991'));

if (!out) {
  console.error('Missing --out <file.png>');
  process.exit(1);
}

async function invoke(cmd, args = {}) {
  const res = await fetch(`http://127.0.0.1:${invokePort}/invoke`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ cmd, args }),
  });
  if (!res.ok) throw new Error(`invoke ${cmd}: HTTP ${res.status}: ${await res.text()}`);
  const json = await res.json();
  if (!json.ok) throw new Error(`invoke ${cmd}: ${json.error || 'unknown error'}`);
  return json.data;
}

async function getPage() {
  if (mock) {
    const [w, h] = viewport.split('x').map(Number);
    let devServer = null;
    let browser;
    try {
      devServer = serve ? await startDevServer(mockUrl) : null;
      browser = await launchChromium();
      const page = await browser.newPage({ viewport: { width: w || 1440, height: h || 900 } });
      // Install the fake Tauri IPC before ANY app module runs.
      await page.addInitScript(buildInitScript(await loadFixtures(fixturesFile)));
      // Surface app-side crashes (a React error, an unhandled rejection) on
      // stderr so a mock render that "renders blank" is diagnosable.
      const pageErrors = [];
      page.on('pageerror', (e) => {
        pageErrors.push(e.message);
        console.error('[page error]', e.message);
      });
      page.on('console', (message) => {
        if (message.type() === 'error') {
          pageErrors.push(message.text());
          console.error('[page console error]', message.text());
        }
      });
      // NOT networkidle: index.html preconnects to Google Fonts, which never
      // settles on an offline/proxied host. Wait for the DOM, then for the app
      // to actually mount something under #root.
      await page.goto(mockUrl, { waitUntil: 'domcontentloaded' }).catch((e) => {
        throw new Error(
          `Could not load ${mockUrl}. Start the dev server (\`npm run dev\`), ` +
          `or pass --serve to have this script start it.\n${e.message}`
        );
      });
      await page.locator('#root > *').first().waitFor({ state: 'attached', timeout: 15000 }).catch(() => {
        const details = pageErrors.length > 0 ? ` Page errors: ${pageErrors.join(' | ')}` : '';
        throw new Error(`[tauri-mock] #root never populated within 15s.${details}`);
      });
      return { browser, page, devServer };
    } catch (e) {
      // Don't leak the browser or a dev server we spawned if setup failed.
      if (browser) await browser.close().catch(() => {});
      if (devServer) await stopDevServer(devServer);
      throw e;
    }
  }
  if (url) {
    const [w, h] = viewport.split('x').map(Number);
    const browser = await launchChromium();
    const page = await browser.newPage({ viewport: { width: w || 390, height: h || 844 } });
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    return { browser, page };
  }
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`).catch((e) => {
    console.error(
      `Could not attach to CDP on port ${cdpPort}. Is buildmesh-dev running with a CDP port?\n` +
      `Launch it with: powershell -File scripts\\run-dev.ps1 -CdpPort ${cdpPort}\n${e.message}`
    );
    process.exit(1);
  });
  const pages = browser.contexts().flatMap((c) => c.pages());
  // The Tauri window's origin on Windows is http://tauri.localhost (tauri://localhost elsewhere).
  const page = pages.find((p) => /^https?:\/\/tauri\.localhost/.test(p.url()) || p.url().startsWith('tauri://'))
    ?? pages[0];
  if (!page) {
    await browser.close();
    throw new Error('No pages found over CDP — is the app window open?');
  }
  return { browser, page };
}

const { browser, page, devServer } = await getPage();
// In --mock mode steps drive fixtures/events through the page instead of the
// HTTP bridge (which has no backend here).
const mockHelper = {
  on: (cmd, value) => page.evaluate(([c, v]) => window.__BUILDMESH_MOCK__.on(c, v), [cmd, value]),
  emit: (event, payload) => page.evaluate(([e, p]) => window.__BUILDMESH_MOCK__.emit(e, p), [event, payload]),
};
try {
  if (stepsFile) {
    const mod = await import(pathToFileURL(resolve(stepsFile)).href);
    if (typeof mod.default !== 'function') throw new Error(`${stepsFile} must default-export an async function`);
    await mod.default({ page, invoke, mock: mockHelper });
  }

  mkdirSync(dirname(resolve(out)), { recursive: true });
  if (selector) {
    const el = page.locator(selector).first();
    await el.waitFor({ state: 'visible', timeout: 10000 });
    await el.screenshot({ path: out });
  } else {
    await page.screenshot({ path: out });
  }
  console.log(`Saved ${out} (page: ${page.url()})`);
} finally {
  // In CDP mode this detaches from the app without closing it;
  // in --url/--mock mode it closes the headless browser.
  try {
    await browser.close();
  } finally {
    // Only stop a dev server WE started (--serve); a reused one is left up.
    if (devServer) await stopDevServer(devServer);
  }
}
