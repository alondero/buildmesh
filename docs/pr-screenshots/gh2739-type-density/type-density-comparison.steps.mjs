/**
 * Type-density comparison — issue #2739.
 *
 * Renders the SAME fixture data at a chosen viewport and type density, then
 * measures the four surfaces the issue names — sidebar, grid header, Probe
 * and Settings — and captures a screenshot of each. The point is a
 * like-for-like comparison: the only thing that changes between a run and
 * the next is the density (and the viewport width), so the difference in the
 * pictures is the difference the policy makes.
 *
 * Driven by two environment variables:
 *
 *   BUILDMESH_SHOT_WIDTH    viewport width in px (default 1440)
 *   BUILDMESH_TYPE_DENSITY  'comfortable' (default) or 'compact'
 *
 * Outputs (relative to the repo root):
 *   docs/pr-screenshots/gh2739-type-density/<width>-<density>-shell.png
 *   docs/pr-screenshots/gh2739-type-density/<width>-<density>-settings.png
 *   docs/pr-screenshots/gh2739-type-density/<width>-<density>-narrow-probe.png
 *
 * Mock-IPC mode: this proves the frontend renders and reacts. It does NOT
 * prove backend behaviour — nothing here touches the Rust side (the density
 * and the zoom are localStorage plus a CSS attribute / a native call).
 */

import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';

const WIDTH = Number(process.env.BUILDMESH_SHOT_WIDTH ?? 1440);
const DENSITY = process.env.BUILDMESH_TYPE_DENSITY === 'compact' ? 'compact' : 'comfortable';
const OUT_DIR = resolve(process.cwd(), 'docs/pr-screenshots/gh2739-type-density');
const DENSITY_KEY = 'buildmesh.type-density';
/** The Probe dock's own minimum width (src/components/Probe/useProbeResize.ts). */
const PROBE_WIDTH_KEY = 'buildmesh.probe-panel-width';
const PROBE_MIN_WIDTH = 240;

/** The ladder the policy promises, in px. Comfortable is the default. */
const EXPECTED = {
  comfortable: { label: 13, meta: 11.375 },
  compact: { label: 11.375, meta: 9.75 },
};

/**
 * Measure an element's computed font size and whether it is clipped by an
 * ancestor that hides its overflow.
 *
 * A truncated label is not clipping — that is what `truncate` is for — so the
 * check is whether the element's own box is cut off by a container, not
 * whether its text overflows its own box.
 */
async function measure(page, selector) {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (!el) return null;
    const style = getComputedStyle(el);
    const rect = el.getBoundingClientRect();
    const clippedBy = [];
    for (let parent = el.parentElement; parent; parent = parent.parentElement) {
      const parentStyle = getComputedStyle(parent);
      if (parentStyle.overflow === 'hidden' || parentStyle.overflowX === 'hidden') {
        const box = parent.getBoundingClientRect();
        if (rect.right > box.right + 0.5 || rect.left < box.left - 0.5) {
          clippedBy.push(parent.tagName.toLowerCase());
        }
      }
    }
    return {
      fontSize: parseFloat(style.fontSize),
      lineHeight: style.lineHeight,
      width: Math.round(rect.width),
      height: Math.round(rect.height),
      clippedBy,
    };
  }, selector);
}

/** Open the Probe on Project Files through the command omnibar. */
async function openProbe(page) {
  await page.getByTestId('titlebar-command-search').click();
  const tiles = page.getByTestId('command-omnibar-tool-groups').getByRole('button', { name: 'Project Files', exact: true });
  await tiles.first().click();
  await page.getByTestId('probe-context-title').waitFor({ state: 'visible', timeout: 15000 });
  await page.waitForTimeout(400);
}

/** Open Settings on the General pane. */
async function openSettings(page) {
  // The labelled Settings pill only exists in the wide title-bar tier; at
  // narrower widths the same control sits in the More panel.
  const labelled = page.getByRole('button', { name: 'Open settings' });
  if ((await labelled.count()) > 0) {
    await labelled.first().click();
  } else {
    await page.getByTestId('titlebar-overflow').click();
    await page.getByTestId('titlebar-overflow-settings').click();
  }
  await page.getByRole('tab', { name: 'General' }).click();
  await page.getByTestId('interface-text-scale-slider').waitFor({ state: 'visible', timeout: 15000 });
  await page.waitForTimeout(300);
}

/**
 * Tab forward until `testId` holds the focus.
 *
 * A mouse-only click proves nothing about a keyboard. This walks the real tab
 * order in a real browser, so "the new controls are reachable" is measured
 * rather than assumed.
 */
async function tabTo(page, testId, maxTabs = 120) {
  for (let i = 0; i < maxTabs; i += 1) {
    const focused = await page.evaluate(() => document.activeElement?.getAttribute('data-testid'));
    if (focused === testId) return true;
    await page.keyboard.press('Tab');
  }
  const focused = await page.evaluate(() => document.activeElement?.getAttribute('data-testid'));
  return focused === testId;
}

export default async function ({ page }) {
  mkdirSync(OUT_DIR, { recursive: true });

  // Seed the density before the app boots, then reload so the very first
  // paint already carries it — the same path a relaunch takes.
  await page.addInitScript(
    ({ key, density }) => {
      if (density === 'compact') localStorage.setItem(key, density);
      else localStorage.removeItem(key);
    },
    { key: DENSITY_KEY, density: DENSITY },
  );
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.getByTestId('titlebar-command-search').waitFor({ state: 'visible', timeout: 20000 });
  await page.waitForTimeout(800);

  const expected = EXPECTED[DENSITY];
  const measured = {};

  // ---- Sidebar ----------------------------------------------------------
  measured.sidebar = await measure(page, '[id^="node-item-name-"]');
  if (!measured.sidebar) throw new Error('no sidebar node name found in the fixture');
  if (Math.abs(measured.sidebar.fontSize - expected.label) > 0.3) {
    throw new Error(
      `sidebar task name should be ${expected.label}px at ${DENSITY}, measured ${measured.sidebar.fontSize}`,
    );
  }
  if (measured.sidebar.clippedBy.length) {
    throw new Error(`sidebar task name is clipped by ${measured.sidebar.clippedBy.join(', ')}`);
  }

  // ---- Grid header ------------------------------------------------------
  measured.gridHeader = await measure(page, '[data-testid="grid-node-header"] .text-label');
  if (!measured.gridHeader) throw new Error('no grid node header name found');
  if (Math.abs(measured.gridHeader.fontSize - expected.label) > 0.3) {
    throw new Error(
      `grid header task name should be ${expected.label}px at ${DENSITY}, measured ${measured.gridHeader.fontSize}`,
    );
  }
  if (measured.gridHeader.clippedBy.length) {
    throw new Error(`grid header task name is clipped by ${measured.gridHeader.clippedBy.join(', ')}`);
  }

  // ---- Probe ------------------------------------------------------------
  await openProbe(page);
  measured.probeTitle = await measure(page, '[data-testid="probe-context-title"]');
  measured.probeSubject = await measure(page, '[data-testid="probe-context-subject-name"]');
  measured.probeDetail = await measure(page, '[data-testid="probe-context-subject-detail"]');
  if (!measured.probeTitle) throw new Error('the Probe did not open');
  if (Math.abs(measured.probeTitle.fontSize - expected.label) > 0.3) {
    throw new Error(
      `probe destination title should be ${expected.label}px at ${DENSITY}, measured ${measured.probeTitle.fontSize}`,
    );
  }
  if (measured.probeDetail && Math.abs(measured.probeDetail.fontSize - expected.meta) > 0.3) {
    throw new Error(
      `probe detail should be ${expected.meta}px at ${DENSITY}, measured ${measured.probeDetail.fontSize}`,
    );
  }
  for (const key of ['probeTitle', 'probeSubject', 'probeDetail']) {
    if (measured[key]?.clippedBy.length) {
      throw new Error(`${key} is clipped by ${measured[key].clippedBy.join(', ')}`);
    }
  }

  await page.screenshot({ path: `${OUT_DIR}/${WIDTH}-${DENSITY}-shell.png` });

  // ---- Probe at its narrowest -------------------------------------------
  // The Probe dock has a 240px floor. That is where the enlarged labels are
  // most likely to be cut off, so the comparison has to include it: the
  // width is the app's own setting, so it is set the way a user sets it and
  // the app is reloaded onto it.
  await page.evaluate(
    ({ key, width }) => localStorage.setItem(key, String(width)),
    { key: PROBE_WIDTH_KEY, width: PROBE_MIN_WIDTH },
  );
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.getByTestId('titlebar-command-search').waitFor({ state: 'visible', timeout: 20000 });
  await openProbe(page);

  measured.narrowProbeTitle = await measure(page, '[data-testid="probe-context-title"]');
  measured.narrowProbeSubject = await measure(page, '[data-testid="probe-context-subject-name"]');
  measured.narrowProbeDetail = await measure(page, '[data-testid="probe-context-subject-detail"]');
  if (!measured.narrowProbeTitle) throw new Error('the Probe did not open at its minimum width');
  if (Math.abs(measured.narrowProbeTitle.fontSize - expected.label) > 0.3) {
    throw new Error(
      `narrow probe destination title should be ${expected.label}px at ${DENSITY}, measured ${measured.narrowProbeTitle.fontSize}`,
    );
  }
  for (const key of ['narrowProbeTitle', 'narrowProbeSubject', 'narrowProbeDetail']) {
    if (measured[key]?.clippedBy.length) {
      throw new Error(`${key} is clipped by ${measured[key].clippedBy.join(', ')} at ${PROBE_MIN_WIDTH}px`);
    }
  }
  await page.screenshot({ path: `${OUT_DIR}/${WIDTH}-${DENSITY}-narrow-probe.png` });

  // ---- Settings ---------------------------------------------------------
  await openSettings(page);
  // Scoped to the Settings dialog: an unscoped `span.text-label` matches the
  // sidebar node name first in document order, which would measure the wrong
  // element and make the assertion redundant.
  measured.settingsLabel = await measure(page, '[role="dialog"] span.text-label, [role="dialog"] label.text-label');
  if (!measured.settingsLabel) throw new Error('no settings row label found');
  if (Math.abs(measured.settingsLabel.fontSize - expected.label) > 0.3) {
    throw new Error(
      `settings row label should be ${expected.label}px at ${DENSITY}, measured ${measured.settingsLabel.fontSize}`,
    );
  }
  if (measured.settingsLabel.clippedBy.length) {
    throw new Error(`settings row label is clipped by ${measured.settingsLabel.clippedBy.join(', ')}`);
  }
  // The new controls have to be present and reachable, not just rendered.
  await page.getByTestId('type-density-radio-comfortable').waitFor({ state: 'visible', timeout: 5000 });
  await page.getByTestId('window-zoom-radio-200').waitFor({ state: 'visible', timeout: 5000 });

  // ...and reachable AND operable with KEYBOARD. A click proves the control
  // works; the tab order proves a user without a mouse can get to it. Only
  // the CHECKED radio of a group is in the tab order — that is how a native
  // radio group behaves — so the check lands on the checked one, whichever
  // density is active.
  const checkedDensity = await page.evaluate(() => {
    const inputs = Array.from(document.querySelectorAll('input[name="type-density"]'));
    return inputs.find((input) => input.checked)?.getAttribute('data-testid') ?? null;
  });
  if (!checkedDensity) throw new Error('no density radio is checked');
  if (!(await tabTo(page, checkedDensity))) {
    throw new Error(`the text density radio (${checkedDensity}) is not reachable with the keyboard`);
  }

  // ArrowRight moves the selection to the next radio of the group, the way a
  // native radio group does — and the app must act on it. Pressing Space on
  // the ALREADY CHECKED radio would prove nothing, so the selection is moved
  // first and the change is checked.
  const densityBefore = await page.evaluate(() => document.documentElement.getAttribute('data-type-density'));
  await page.keyboard.press('ArrowRight');
  await page.waitForTimeout(200);
  const densityAfter = await page.evaluate(() => document.documentElement.getAttribute('data-type-density'));
  if (densityAfter === densityBefore) {
    throw new Error(`arrow-right on the density radio did not change the density (still ${densityAfter})`);
  }
  // Put it back, so the capture is of the density under test.
  await page.keyboard.press('ArrowRight');
  await page.waitForTimeout(200);
  const densityRestored = await page.evaluate(() => document.documentElement.getAttribute('data-type-density'));
  if (densityRestored !== densityBefore) {
    throw new Error(`the density was not restored (${densityBefore} -> ${densityRestored})`);
  }

  if (!(await tabTo(page, 'window-zoom-radio-100'))) {
    throw new Error('the window zoom radio is not reachable with the keyboard');
  }

  await page.screenshot({ path: `${OUT_DIR}/${WIDTH}-${DENSITY}-settings.png` });

  const result = { width: WIDTH, density: DENSITY, expected, measured };
  // Printed so the run's receipt carries the numbers, not just the pixels.
  console.log(`[type-density] ${JSON.stringify(result)}`);
  return result;
}
