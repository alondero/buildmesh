/**
 * Real-relaunch verification — issue #2739 (review findings 3 and 5).
 *
 * The first native run proved the window zoom *persists* through a reload. A
 * reload is not a relaunch: WebView2's zoom factor belongs to the controller,
 * so it survives a reload even when the boot path re-applies nothing. A relaunch
 * is the real test — a fresh webview starts at 1x, so the stored zoom is only
 * in force afterwards if `applyWindowZoomAtBoot()` actually re-applied it.
 *
 * The run is split in two because the relaunch is a process-level event that
 * the page has no way to perform on itself:
 *
 *   phase 'set'   drive the real Settings control to 200% (which stores the
 *                 choice) and record the viewport it produces.
 *   phase 'check' run against a FRESHLY LAUNCHED app: assert the stored zoom
 *                 is still 200, that the fresh webview is BELOW the app's
 *                 minimum width, and that the device pixel ratio says 3 —
 *                 the platform zoom itself is in force, not just a narrow
 *                 layout. Then open and close a dialog at that zoom, and
 *                 measure the label of a REAL agent node in the grid.
 *
 * The fixture is a mesh with a long name AND an agent node in it. A mesh with
 * no node renders no grid header at all, so "the agent name is still 13px"
 * could not be measured without one — the same gap the native verification
 * run had before it grew an agent node.
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

const MESH_NAME = 'readability-relaunch-mesh-with-a-long-name';
const AGENT_NAME = 'readability-relaunch-agent-with-a-long-name';
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
/** The functional-label size at the default text scale (src/App.css). */
const LABEL_SIZE = 13;

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

/**
 * The fixture mesh, found through its agent node.
 *
 * The mesh is NOT looked up by name: `create_test_mesh` upserts by PATH
 * (src-tauri/src/db/mesh.rs), so every fixture created against the default
 * temporary path lands in the SAME mesh row and keeps the name it was first
 * given. The agent node is the unambiguous handle: it carries the mesh id it
 * belongs to, and it has to have survived the relaunch for the check phase to
 * have anything to measure.
 */
async function findFixtureMesh(invoke) {
  const nodes = await invoke('list_agent_nodes').catch(() => null);
  if (!Array.isArray(nodes)) return null;
  const matches = nodes.filter((node) => node?.name === AGENT_NAME);
  if (matches.length === 0) {
    throw new Error(`no agent node named "${AGENT_NAME}" survived the relaunch`);
  }
  const meshId = matches[0].mesh_id;
  const meshes = await invoke('list_meshes').catch(() => null);
  const mesh = Array.isArray(meshes)
    ? meshes.find((candidate) => candidate?.id === meshId) ?? null
    : null;
  return { id: meshId, name: mesh?.name ?? null };
}

/**
 * Select the fixture mesh in the sidebar and wait for its grid header.
 *
 * The element id is `mesh-item-name-<id>`
 * (src/components/Sidebar/MeshItem.tsx): a `mesh-name-` prefix matches
 * nothing and would silently record `null` — the defect this phase exists to
 * rule out.
 */
async function selectFixtureMesh(page, meshId) {
  const item = page.locator(`[id="mesh-item-name-${meshId}"]`);
  await item.click({ timeout: 15000 });
  const header = page.locator('[data-testid="grid-node-header"]').first();
  const visible = await header
    .waitFor({ state: 'visible', timeout: 30000 })
    .then(() => true)
    .catch(() => false);
  return visible;
}

export default async function ({ page, invoke }) {
  const receipt = { phase: PHASE, appPid: APP_PID, at: new Date().toISOString() };

  if (PHASE === 'set') {
    // A mesh with a long name AND an agent node in it: without the node the
    // grid renders no header, and the check phase would have nothing to
    // measure after the relaunch. `create_test_mesh` upserts by path, so the
    // name may be the one an earlier fixture gave this mesh row; the id is
    // what the run relies on.
    const mesh = await invoke('create_test_mesh', { name: MESH_NAME }).catch(() => null);
    if (!mesh?.id) throw new Error('the relaunch fixture mesh was not created');
    const node = await invoke('create_agent_node', {
      meshId: mesh.id,
      name: AGENT_NAME,
      path: mesh.path ?? '',
      branch: 'main',
      useWorktree: false,
      provider: 'anthropic',
    }).catch(() => null);
    if (!node?.id) throw new Error('the relaunch fixture agent node was not created');
    receipt.fixture = { meshId: mesh.id, meshName: mesh.name ?? null, agentNodeId: node.id };

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

  // The agent must be FINDABLE after the relaunch, and its label must still
  // be measured: a `null` here would mean the selector matched nothing, which
  // is the defect this phase is here to rule out.
  const mesh = await findFixtureMesh(invoke);
  receipt.fixture = { meshId: mesh.id, name: mesh.name, agentName: AGENT_NAME };
  receipt.agentFound = await selectFixtureMesh(page, mesh.id);
  if (!receipt.agentFound) {
    throw new Error(`no agent node was found in the grid of mesh ${mesh.id} after the relaunch`);
  }
  receipt.meshName = await fontSize(page, '[id^="mesh-item-name-"]');
  receipt.gridName = await fontSize(page, '[data-testid="grid-node-header"] .text-label');
  if (receipt.meshName === null || receipt.gridName === null) {
    throw new Error(
      `the label sizes were not measured after the relaunch: ${JSON.stringify({ meshName: receipt.meshName, gridName: receipt.gridName })}`,
    );
  }
  if (Math.abs(receipt.gridName - LABEL_SIZE) > 0.3) {
    throw new Error(
      `the grid header agent name should be ${LABEL_SIZE}px, measured ${receipt.gridName}`,
    );
  }

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
