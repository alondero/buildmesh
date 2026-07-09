#!/usr/bin/env node
/**
 * ui-shot.mjs — drive the Buildmesh UI and save a screenshot. Two modes:
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
 * Usage:
 *   node scripts/ui-shot.mjs --out shots/after.png
 *   node scripts/ui-shot.mjs --out shots/after.png --steps my-steps.mjs
 *   node scripts/ui-shot.mjs --out shots/after.png --selector "[data-session-item]"
 *   node scripts/ui-shot.mjs --out shots/mobile.png --url "http://127.0.0.1:2992/v2?token=..." --viewport 390x844
 *
 * Options:
 *   --out <file.png>     required; parent dirs are created
 *   --cdp <port>         CDP port (default 9223, matches run-dev.ps1 -CdpPort)
 *   --url <url>          mobile-SPA mode: launch headless Chromium at this URL
 *   --viewport <WxH>     viewport size, --url mode only (default 390x844)
 *   --steps <file.mjs>   module whose default export is `async ({ page, invoke }) => {}`
 *                        run before the screenshot (click, fill, wait, assert…)
 *   --selector <css>     screenshot only this element (default: full window/page)
 *   --invoke-port <n>    HTTP test-bridge port for the `invoke` helper
 *                        (default 2991 — the dev profile's test server)
 *
 * The `invoke` helper POSTs to the backend's HTTP test bridge, e.g.
 *   await invoke('create_test_mesh', { name: 'Shot fixture' })
 * Only commands routed in src-tauri/src/commands/test.rs are available there;
 * everything else should be driven through the UI via `page`.
 *
 * Exit codes: 0 = screenshot written; 1 = any failure (message on stderr).
 * A throwing steps module fails the run — use that for functional assertions.
 */
import { chromium } from 'playwright';
import { mkdirSync } from 'fs';
import { dirname, resolve } from 'path';
import { pathToFileURL } from 'url';

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i !== -1 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
}

const out = arg('out');
const cdpPort = Number(arg('cdp', '9223'));
const url = arg('url');
const viewport = arg('viewport', '390x844');
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
  if (url) {
    const [w, h] = viewport.split('x').map(Number);
    const browser = await chromium.launch();
    const page = await browser.newPage({ viewport: { width: w || 390, height: h || 844 } });
    await page.goto(url, { waitUntil: 'networkidle' });
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

const { browser, page } = await getPage();
try {
  if (stepsFile) {
    const mod = await import(pathToFileURL(resolve(stepsFile)).href);
    if (typeof mod.default !== 'function') throw new Error(`${stepsFile} must default-export an async function`);
    await mod.default({ page, invoke });
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
  // in --url mode it closes the headless browser.
  await browser.close();
}
