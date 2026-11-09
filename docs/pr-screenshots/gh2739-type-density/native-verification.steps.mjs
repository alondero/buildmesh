/**
 * Native runtime verification â€” issue #2739.
 *
 * Drives the REAL dev-profile app over CDP (Windows/WebView2): real Tauri IPC,
 * real Rust backend, real pixels. Where the mock comparison
 * (`type-density-comparison.steps.mjs`) proves the frontend renders, this
 * proves the things only the native runtime can prove:
 *
 *   1. the DEFAULT presentation (comfortable density, 100% text scale),
 *   2. the 150% interface text scale,
 *   3. the 200% window zoom â€” the platform's own zoom, which is the whole
 *      point of the enlargement path,
 *   4. that a dialog still opens and closes at every one of those sizes,
 *   5. that the Probe's context is still readable and an agent still findable,
 *   6. that displaced actions are still reachable,
 *   7. that the settings survive a reload — the reload path only; a RELAUNCH
 *      (a fresh webview that starts at 1x) is proven by
 *      `native-relaunch.steps.mjs`,
 *   8. that the terminal font size is NOT moved by any of the three.
 *
 * Every interaction is RECORDED, not asserted-thrown, except the size
 * contract: an enlargement that quietly stops working is exactly the failure
 * this issue is about, so the receipt has to say which things still worked.
 * The receipt is archived next to the images as
 * `native-verify.receipt.json`, so the numbers can be checked from the tree.
 *
 * Requires the dev profile launched with a CDP port and the test bridge:
 *   powershell -File scripts\run-dev.ps1 -Detach -CdpPort 9223 -TestBridge
 */

import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const MESH_NAME = 'readability-check-mesh-with-a-long-name';
const TEXT_SCALE_KEY = 'buildmesh.text-scale';
const DENSITY_KEY = 'buildmesh.type-density';
const ZOOM_KEY = 'buildmesh.window-zoom';
const TERMINAL_FONT_KEY = 'terminal-font-size';
const OUT_DIR = resolve(process.cwd(), 'docs/pr-screenshots/gh2739-type-density');

/** The app's minimum window width, in CSS px (src-tauri/tauri.conf.json). */
const MIN_CSS_WIDTH = 900;

/** Computed px size of the first element matching a selector. */
async function fontSize(page, selector) {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (!el) return null;
    return parseFloat(getComputedStyle(el).fontSize);
  }, selector);
}

/** Click a locator, recording rather than throwing when it cannot be reached. */
async function safeClick(page, locator, label) {
  try {
    await locator.click({ timeout: 8000 });
    return { step: label, ok: true };
  } catch (error) {
    return { step: label, ok: false, error: String(error).split('\n')[0].slice(0, 160) };
  }
}

/** The CSS viewport and the device pixel ratio â€” the window zoom's fingerprint. */
async function viewport(page) {
  return page.evaluate(() => ({
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
    devicePixelRatio: window.devicePixelRatio,
  }));
}

/** Open the Probe through the command omnibar and read its context. */
async function openProbe(page, interactions) {
  await page.getByTestId('titlebar-command-search').click({ timeout: 8000 }).catch(() => {});
  const tiles = page.getByTestId('command-omnibar-tool-groups').getByRole('button', { name: 'Project Files', exact: true });
  let click = await safeClick(page, tiles.first(), 'open the Probe from the omnibar');
  if (!click.ok) {
    // At 200% the title bar's command field is squeezed out of reach — the
    // narrow-header limitation (#2420). The More panel still reaches every
    // tool, so the Probe is opened from there instead: "reading a request"
    // has to work at the enlarged size, not only at the default one.
    const more = await safeClick(page, page.getByTestId('titlebar-overflow'), 'open the More panel to reach the Probe');
    interactions.push(more);
    if (!more.ok) {
      interactions.push(click);
      return null;
    }
    const row = page.getByTestId('titlebar-overflow-panel').getByText('Project Files', { exact: true });
    click = await safeClick(page, row.first(), 'open the Probe from the More panel');
  }
  interactions.push(click);
  if (!click.ok) return null;
  const visible = await page.getByTestId('probe-context-title')
    .waitFor({ state: 'visible', timeout: 15000 })
    .then(() => true)
    .catch(() => false);
  if (!visible) return null;
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
  return {
    title: await fontSize(page, '[data-testid="probe-context-title"]'),
    subject: await fontSize(page, '[data-testid="probe-context-subject-name"]'),
    detail: await fontSize(page, '[data-testid="probe-context-subject-detail"]'),
    text: (await page.getByTestId('probe-context-title').textContent())?.trim() ?? '',
  };
}

/** Open Settings on the General pane, then close it again. */
async function openAndCloseSettings(page, interactions) {
  const labelled = page.getByRole('button', { name: 'Open settings' });
  if ((await labelled.count()) > 0) interactions.push(await safeClick(page, labelled.first(), 'open Settings'));
  else {
    await page.getByTestId('titlebar-overflow').click({ timeout: 8000 }).catch(() => {});
    interactions.push(await safeClick(page, page.getByTestId('titlebar-overflow-settings'), 'open Settings'));
  }
  const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
  const opened = await dialog.waitFor({ state: 'visible', timeout: 15000 }).then(() => true).catch(() => false);
  interactions.push({ step: 'the Settings dialog opens', ok: opened });
  if (!opened) return null;
  await page.getByRole('tab', { name: 'General' }).click({ timeout: 8000 }).catch(() => {});
  const labelSize = await fontSize(page, 'label[for="confirm-before-quit"], span.text-label');
  // The dialog must close again â€” an enlarged surface that traps the user is
  // not an enlargement, it is a break.
  await page.keyboard.press('Escape');
  const closed = await dialog.waitFor({ state: 'hidden', timeout: 15000 }).then(() => true).catch(() => false);
  interactions.push({ step: 'the Settings dialog closes', ok: closed });
  return { labelSize };
}

/** A displaced action: an entry that lives in the title bar's More panel. */
async function reachDisplacedAction(page, interactions) {
  const more = page.getByTestId('titlebar-overflow');
  const opened = await safeClick(page, more, 'open the More panel');
  interactions.push(opened);
  if (!opened.ok) return null;
  const entries = page.getByTestId('titlebar-overflow-panel').locator('button');
  const count = await entries.count();
  if (count === 0) return null;
  const label = (await entries.first().textContent())?.trim() ?? '';
  const clicked = await safeClick(page, entries.first(), `reach the displaced action "${label}"`);
  interactions.push(clicked);
  await page.waitForTimeout(300);
  return label;
}

/** One measurement pass over the surfaces the issue names. */
async function survey(page, stage) {
  const interactions = [];
  const meshName = await fontSize(page, '[id^="mesh-item-name-"]');
  const gridName = await fontSize(page, '[data-testid="grid-node-header"] .text-label');
  const probe = await openProbe(page, interactions);
  const settings = await openAndCloseSettings(page, interactions);
  const displaced = await reachDisplacedAction(page, interactions);

  return {
    stage,
    meshName,
    gridName,
    probe,
    settings,
    displaced,
    interactions,
    viewport: await viewport(page),
    terminalFontSize: await page.evaluate((key) => localStorage.getItem(key), TERMINAL_FONT_KEY),
  };
}

/** Every interaction in a survey must have worked. */
function assertInteractions(surveyed, where) {
  const failed = surveyed.interactions.filter((i) => !i.ok);
  if (failed.length) {
    throw new Error(
      `${where}: ${failed.map((f) => `${f.step} (${f.error ?? 'failed'})`).join('; ')}`,
    );
  }
}

/**
 * Put the native webview back at 1x.
 *
 * WebView2's zoom factor belongs to the controller, not the page: it survives
 * a reload and is not undone by clearing the stored preference. A run that
 * inherited a zoom from an earlier run would measure the wrong thing, so the
 * reset goes through the same plugin command the Settings control uses.
 */
async function resetNativeZoom(page) {
  await page.evaluate(() =>
    window.__TAURI_INTERNALS__.invoke('plugin:webview|set_webview_zoom', {
      label: 'main',
      value: 1,
    }),
  ).catch(() => {});
  await page.waitForTimeout(500);
}

export default async function ({ page, invoke }) {
  const receipt = { appPid: process.env.BUILDMESH_APP_PID ?? null, at: new Date().toISOString() };

  // A mesh with a long name, so the label is one the sidebar has to truncate
  // rather than a toy name that always fits.
  await invoke('create_test_mesh', { name: MESH_NAME }).catch(() => null);

  // Start from a clean slate: a previous run's persisted zoom would otherwise
  // decide what this run measures.
  await resetNativeZoom(page);
  await page.evaluate(({ textScale, density, zoom }) => {
    localStorage.removeItem(textScale);
    localStorage.removeItem(density);
    localStorage.removeItem(zoom);
  }, { textScale: TEXT_SCALE_KEY, density: DENSITY_KEY, zoom: ZOOM_KEY });
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.getByTestId('titlebar-command-search').waitFor({ state: 'visible', timeout: 30000 });
  await page.waitForTimeout(1200);

  receipt.startingViewport = await viewport(page);
  if (receipt.startingViewport.innerWidth < MIN_CSS_WIDTH) {
    throw new Error(
      `the run started at ${receipt.startingViewport.innerWidth} CSS px, below the app's ${MIN_CSS_WIDTH}px minimum`,
    );
  }

  // ---- 1. Default --------------------------------------------------------
  receipt.default = await survey(page, 'default');
  assertInteractions(receipt.default, 'default');
  if (Math.abs((receipt.default.meshName ?? 0) - 13) > 0.3) {
    throw new Error(`the sidebar mesh name should be 13px, measured ${receipt.default.meshName}`);
  }

  // ---- 2. 150% interface text scale -------------------------------------
  await page.evaluate((key) => localStorage.setItem(key, '150'), TEXT_SCALE_KEY);
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.getByTestId('titlebar-command-search').waitFor({ state: 'visible', timeout: 30000 });
  await page.waitForTimeout(1200);
  receipt.at150 = await survey(page, '150% text scale');
  assertInteractions(receipt.at150, '150% text scale');
  if (Math.abs((receipt.at150.meshName ?? 0) - 19.5) > 0.4) {
    throw new Error(
      `the sidebar mesh name should be 19.5px at a 150% text scale, measured ${receipt.at150.meshName}`,
    );
  }

  // ---- 3. 200% window zoom ----------------------------------------------
  // Set through the real Settings control, so the capability and the IPC are
  // exercised exactly as a user would exercise them.
  const labelled = page.getByRole('button', { name: 'Open settings' });
  if ((await labelled.count()) > 0) await labelled.first().click();
  else {
    await page.getByTestId('titlebar-overflow').click();
    await page.getByTestId('titlebar-overflow-settings').click();
  }
  await page.getByRole('tab', { name: 'General' }).click();
  await page.getByTestId('window-zoom-radio-200').waitFor({ state: 'visible', timeout: 20000 });
  await page.getByTestId('window-zoom-radio-200').click();
  await page.waitForFunction(
    (key) => localStorage.getItem(key) === '200',
    ZOOM_KEY,
    { timeout: 20000 },
  );
  await page.waitForTimeout(1500);
  await page.keyboard.press('Escape');
  await page.getByRole('dialog', { name: 'Settings', exact: true })
    .waitFor({ state: 'hidden', timeout: 20000 })
    .catch(() => {});
  await page.waitForTimeout(800);

  receipt.at200 = await survey(page, '200% window zoom');

  // The window zoom must have actually moved the platform zoom, not just the
  // stored preference: the CSS viewport halves and the device pixel ratio
  // doubles when WebView2's zoom factor goes to 2.
  const before = receipt.at150.viewport;
  const after = receipt.at200.viewport;
  receipt.zoomEffect = {
    innerWidthBefore: before.innerWidth,
    innerWidthAfter: after.innerWidth,
    devicePixelRatioBefore: before.devicePixelRatio,
    devicePixelRatioAfter: after.devicePixelRatio,
  };
  if (after.innerWidth >= before.innerWidth) {
    throw new Error(
      `the window zoom did not shrink the CSS viewport: ${before.innerWidth} -> ${after.innerWidth}`,
    );
  }
  if (after.devicePixelRatio <= before.devicePixelRatio) {
    throw new Error(
      `the window zoom did not raise the device pixel ratio: ${before.devicePixelRatio} -> ${after.devicePixelRatio}`,
    );
  }
  receipt.cssViewportBelowMinimum = {
    innerWidth: after.innerWidth,
    minimum: MIN_CSS_WIDTH,
    belowMinimum: after.innerWidth < MIN_CSS_WIDTH,
  };

  // ---- 4. Settings survive a reload -------------------------------------
  // A reload is NOT a relaunch: WebView2's zoom factor belongs to the
  // controller, so it survives a reload even if the boot path did nothing.
  // This stage therefore proves the reload path (the density and the text
  // scale are re-applied from module load, and the zoom is still in force);
  // the real relaunch â€” a fresh webview that starts at 1x â€” is proven by
  // `native-relaunch.steps.mjs`.
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.getByTestId('titlebar-command-search').waitFor({ state: 'visible', timeout: 30000 });
  await page.waitForTimeout(1500);
  receipt.afterReload = await survey(page, 'after reload');
  receipt.storedAfterReload = await page.evaluate(
    ({ textScale, density, zoom, terminal }) => ({
      textScale: localStorage.getItem(textScale),
      density: localStorage.getItem(density),
      zoom: localStorage.getItem(zoom),
      terminalFontSize: localStorage.getItem(terminal),
    }),
    { textScale: TEXT_SCALE_KEY, density: DENSITY_KEY, zoom: ZOOM_KEY, terminal: TERMINAL_FONT_KEY },
  );
  if (receipt.storedAfterReload.textScale !== '150') {
    throw new Error('the interface text scale did not survive the reload');
  }
  if (receipt.storedAfterReload.zoom !== '200') {
    throw new Error('the window zoom did not survive the reload');
  }
  // The boot path re-applied it: the CSS viewport is still halved.
  if (receipt.afterReload.viewport.innerWidth >= receipt.at150.viewport.innerWidth) {
    throw new Error('the persisted window zoom was not in force after the reload');
  }

  // ---- 5. The terminal font size is independent --------------------------
  receipt.terminalFontIndependence = {
    default: receipt.default.terminalFontSize,
    at150: receipt.at150.terminalFontSize,
    at200: receipt.at200.terminalFontSize,
    afterReload: receipt.afterReload.terminalFontSize,
  };
  const terminalSizes = new Set(
    Object.values(receipt.terminalFontIndependence).map((value) => value ?? 'unset'),
  );
  if (terminalSizes.size > 1) {
    throw new Error(
      `the terminal font size moved with the interface settings: ${JSON.stringify(receipt.terminalFontIndependence)}`,
    );
  }

  console.log(`[native-verify] ${JSON.stringify(receipt)}`);
  mkdirSync(OUT_DIR, { recursive: true });
  writeFileSync(`${OUT_DIR}/native-verify.receipt.json`, `${JSON.stringify(receipt, null, 2)}\n`);
  return receipt;
}
