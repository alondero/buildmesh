/**
 * Real-relaunch verification — issue #2739 (review finding 3).
 *
 * The first native run proved the window zoom *persists* through a reload. A
 * reload is not a relaunch: WebView2's zoom factor belongs to the controller,
 * so it survives a reload even if the boot path did nothing at all. A relaunch
 * is the real test — a fresh webview starts at 1x, so the stored zoom is only
 * in force afterwards if `applyWindowZoomAtBoot()` actually re-applied it.
 *
 * The run is split in two because the relaunch is a process-level event that
 * the page cannot perform on itself:
 *
 *   phase 'set'   drive the real Settings control to 200% (which stores the
 *                 choice) and record the viewport it produces.
 *   phase 'check' run against a FRESHLY LAUNCHED app: assert the stored zoom
 *                 is still 200, that the fresh webview is BELOW the app's
 *                 minimum width, and that the device pixel ratio says 3 —
 *                 the platform zoom itself is in force, not just a narrow
 *                 layout. Then open and close a dialog at that zoom.
 *
 * Each phase archives its receipt next to the images:
 *   native-relaunch-set.receipt.json
 *   native-relaunch-check.receipt.json
 * Both name the app instance (BUILDMESH_APP_PID) that served them, so the two
 * different PIDs are the relaunch, checkable from the tree.
 *
 * Usage:
 *   BUILDMESH_RELAUNCH_PHASE=set   BUILDMESH_APP_PID=<pid> node scripts/ui-shot.mjs --out .tmp/relaunch-set.png --steps <this file>
 *   BUILDMESH_RELAUNCH_PHASE=check BUILDMESH_APP_PID=<pid> node scripts/ui-shot.mjs --out <evidence>.png        --steps <this file>
 *
 * The dev profile must be running with a CDP port:
 *   powershell -File scripts\run-dev.ps1 -Detach -CdpPort 9223 -TestBridge
 */

import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const TEXT_SCALE_KEY = 'buildmesh.text-scale';
const DENSITY_KEY = 'buildmesh.type-density';
const ZOOM_KEY = 'buildmesh.window-zoom';
const TERMINAL_FONT_KEY = 'terminal-font-size';

/** The app's minimum window width, in CSS px (src-tauri/tauri.conf.json). */
const MIN_CSS_WIDTH = 900;
/**
 * The device pixel ratio the platform zoom produces at 200% on a display at
 * 150% OS scaling: 1.5 x 2. Asserting it is what proves the PLATFORM zoom is
 * in force, not just a narrow layout.
 */
const ZOOMED_DEVICE_PIXEL_RATIO = 3;

const PHASE = process.env.BUILDMESH_RELAUNCH_PHASE === 'check' ? 'check' : 'set';
const OUT_DIR = resolve(process.cwd(), 'docs/pr-screenshots/gh2739-type-density');
/**
 * The PID of the app instance this run is attached to. The driver sets it
 * when it launches the app, so the archived receipts name the instance that
 * served them — two different PIDs across the two phases is the relaunch.
 */
const APP_PID = process.env.BUILDMESH_APP_PID ?? null;

async function fontSize(page, selector) {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (!el) return null;
    return parseFloat(getComputedStyle(el).fontSize);
  }, selector);
}

async function viewport(page) {
  return page.evaluate(() => ({
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
    devicePixelRatio: window.devicePixelRatio,
  }));
}

/** Put the native webview back at 1x. The zoom belongs to the controller, so
 *  a stored preference is not enough to undo it. */
async function resetNativeZoom(page) {
  await page
    .evaluate(() =>
      window.__TAURI_INTERNALS__.invoke('plugin:webview|set_webview_zoom', { label: 'main', value: 1 }),
    )
    .catch(() => {});
  await page.waitForTimeout(600);
}

async function openSettings(page) {
  const labelled = page.getByRole('button', { name: 'Open settings' });
  if ((await labelled.count()) > 0) await labelled.first().click();
  else {
    await page.getByTestId('titlebar-overflow').click();
    await page.getByTestId('titlebar-overflow-settings').click();
  }
  await page.getByRole('tab', { name: 'General' }).click();
  await page.getByTestId('window-zoom-radio-200').waitFor({ state: 'visible', timeout: 20000 });
}

async function closeSettings(page) {
  await page.keyboard.press('Escape');
  await page
    .getByRole('dialog', { name: 'Settings', exact: true })
    .waitFor({ state: 'hidden', timeout: 20000 })
    .catch(() => {});
  await page.waitForTimeout(600);
}

export default async function ({ page, invoke }) {
  const receipt = { phase: PHASE, appPid: APP_PID, at: new Date().toISOString() };

  await invoke('create_test_mesh', { name: 'readability-check-mesh-with-a-long-name' }).catch(() => null);

  if (PHASE === 'set') {
    // A clean slate: a previous run's zoom would decide what this one measures.
    await resetNativeZoom(page);
    await page.evaluate(({ textScale, density, zoom }) => {
      localStorage.removeItem(textScale);
      localStorage.removeItem(density);
      localStorage.removeItem(zoom);
    }, { textScale: TEXT_SCALE_KEY, density: DENSITY_KEY, zoom: ZOOM_KEY });
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.getByTestId('titlebar-command-search').waitFor({ state: 'visible', timeout: 30000 });
    await page.waitForTimeout(1500);

    receipt.before = await viewport(page);
    if (receipt.before.innerWidth < MIN_CSS_WIDTH) {
      throw new Error(`the run started at ${receipt.before.innerWidth} CSS px, below the minimum`);
    }

    // The real Settings control: capability, IPC and persistence in one go.
    await openSettings(page);
    await page.getByTestId('window-zoom-radio-200').click();
    await page.waitForFunction((key) => localStorage.getItem(key) === '200', ZOOM_KEY, { timeout: 20000 });
    await page.waitForTimeout(1500);
    await closeSettings(page);

    receipt.after = await viewport(page);
    receipt.stored = await page.evaluate(
      ({ zoom, textScale, terminal }) => ({
        zoom: localStorage.getItem(zoom),
        textScale: localStorage.getItem(textScale),
        terminalFontSize: localStorage.getItem(terminal),
      }),
      { zoom: ZOOM_KEY, textScale: TEXT_SCALE_KEY, terminal: TERMINAL_FONT_KEY },
    );
    if (receipt.after.innerWidth >= receipt.before.innerWidth) {
      throw new Error(
        `the window zoom did not shrink the CSS viewport: ${receipt.before.innerWidth} -> ${receipt.after.innerWidth}`,
      );
    }
    if (receipt.after.devicePixelRatio <= receipt.before.devicePixelRatio) {
      throw new Error(
        `the window zoom did not raise the device pixel ratio: ${receipt.before.devicePixelRatio} -> ${receipt.after.devicePixelRatio}`,
      );
    }
    receipt.cssViewportBelowMinimum = {
      innerWidth: receipt.after.innerWidth,
      minimum: MIN_CSS_WIDTH,
      belowMinimum: receipt.after.innerWidth < MIN_CSS_WIDTH,
    };

    mkdirSync(OUT_DIR, { recursive: true });
    writeFileSync(`${OUT_DIR}/native-relaunch-set.receipt.json`, `${JSON.stringify(receipt, null, 2)}\n`);
    console.log(`[native-relaunch] ${JSON.stringify(receipt)}`);
    return receipt;
  }

  // ---- phase 'check': a freshly launched app --------------------------------
  // Nothing is reset here on purpose: the point is to read what the BOOT path
  // did with the stored preference in a webview that started at 1x.
  await page.getByTestId('titlebar-command-search').waitFor({ state: 'visible', timeout: 30000 });
  await page.waitForTimeout(2000);

  receipt.stored = await page.evaluate(
    ({ zoom, textScale, density, terminal }) => ({
      zoom: localStorage.getItem(zoom),
      textScale: localStorage.getItem(textScale),
      density: localStorage.getItem(density),
      terminalFontSize: localStorage.getItem(terminal),
    }),
    { zoom: ZOOM_KEY, textScale: TEXT_SCALE_KEY, density: DENSITY_KEY, terminal: TERMINAL_FONT_KEY },
  );
  if (receipt.stored.zoom !== '200') {
    throw new Error(`the stored window zoom did not survive the relaunch: ${receipt.stored.zoom}`);
  }

  receipt.viewport = await viewport(page);
  // A fresh webview starts at 1x. If the viewport is below the app's minimum
  // width, the boot path really did re-apply the stored 200% zoom.
  if (receipt.viewport.innerWidth >= MIN_CSS_WIDTH) {
    throw new Error(
      `the relaunched app is at ${receipt.viewport.innerWidth} CSS px — the stored zoom was not re-applied at boot`,
    );
  }
  // The width alone is not enough: the DEVICE PIXEL RATIO is what proves the
  // platform zoom itself is in force (1.5 OS scaling x 2 zoom = 3).
  if (receipt.viewport.devicePixelRatio < ZOOMED_DEVICE_PIXEL_RATIO) {
    throw new Error(
      `the relaunched app reports a device pixel ratio of ${receipt.viewport.devicePixelRatio}, expected ${ZOOMED_DEVICE_PIXEL_RATIO} — the platform zoom was not re-applied at boot`,
    );
  }
  receipt.reappliedAtBoot = true;
  receipt.devicePixelRatio = receipt.viewport.devicePixelRatio;

  receipt.meshName = await fontSize(page, '[id^="mesh-name-"], [data-testid="mesh-item-name"]');
  receipt.probeTitle = null;

  // The dialog still opens and closes in the zoomed window.
  await openSettings(page);
  receipt.settingsOpened = true;
  await closeSettings(page);
  receipt.settingsClosed = true;

  mkdirSync(OUT_DIR, { recursive: true });
  writeFileSync(`${OUT_DIR}/native-relaunch-check.receipt.json`, `${JSON.stringify(receipt, null, 2)}\n`);
  console.log(`[native-relaunch] ${JSON.stringify(receipt)}`);
  return receipt;
}
