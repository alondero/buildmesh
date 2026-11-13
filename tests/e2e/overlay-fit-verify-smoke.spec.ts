/**
 * Overlay fit — the settings help popover stays readable at the edges of
 * the window (issue #2773).
 *
 * The shared help tip used to be an absolutely positioned child of its
 * trigger, inside the settings scroller. That scroller's `overflow` cut
 * the explanation off: at 900x600 the tip's text ran past the bottom of
 * the pane, so most of it was unreadable, and no z-index could have fixed
 * that. The tip also closed itself when the pointer hovered it and then
 * clicked it.
 *
 * This spec drives the real frontend in Chromium with the mock IPC and
 * asserts the geometry a user sees: the tip is inside the viewport, it
 * escapes the clipping ancestor, and hovering then clicking it leaves it
 * open.
 */
import { test, expect, Locator, Page } from '@playwright/test';
import { openSettingsFromTitleBar } from './utils/title-bar';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const VIEWPORTS = [
  { width: 900, height: 600 },
  { width: 1440, height: 900 },
];
const THEMES = ['dark', 'light'];
const TEXT_SCALES = [100, 150];

/** The settings rows whose help the issue called out by name. */
const HELP_LABELS = ['About Theme', 'About Window zoom'];

/** The tip's rect, and how much of it a clipping ancestor would cut off. */
interface TipGeometry {
  rect: { x: number; y: number; width: number; height: number };
  /** The part of the tip that survives every clipping ancestor. */
  visible: { width: number; height: number };
  parentIsBody: boolean;
}

async function readTipGeometry(tip: Page): Promise<TipGeometry> {
  return tip.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    // The visible area is the tip's rect intersected with every ancestor
    // that clips. `body` clips too (the app sets `overflow: hidden` at
    // 100vh), and that is harmless as long as the tip is inside the
    // window — so the assertion is on the surviving area, not on the
    // absence of a clipping ancestor.
    let visible = { top: rect.top, left: rect.left, right: rect.right, bottom: rect.bottom };
    for (let node = element.parentElement; node; node = node.parentElement) {
      const style = getComputedStyle(node);
      const clips = ['hidden', 'auto', 'scroll', 'clip'];
      if (clips.includes(style.overflowY) || clips.includes(style.overflowX)) {
        const box = node.getBoundingClientRect();
        visible = {
          top: Math.max(visible.top, box.top),
          left: Math.max(visible.left, box.left),
          right: Math.min(visible.right, box.right),
          bottom: Math.min(visible.bottom, box.bottom),
        };
      }
    }
    return {
      rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      visible: {
        width: Math.max(0, visible.right - visible.left),
        height: Math.max(0, visible.bottom - visible.top),
      },
      parentIsBody: element.parentElement === document.body,
    };
  });
}

/** The tip must be whole: no ancestor may cut any of it off. */
function expectFullyReadable(geometry: TipGeometry, label: string) {
  expect(geometry.parentIsBody, `${label} should render outside the settings scroller`).toBe(true);
  expect(geometry.visible.width, `${label} should not be clipped horizontally`).toBeCloseTo(geometry.rect.width, 0);
  expect(geometry.visible.height, `${label} should not be clipped vertically`).toBeCloseTo(geometry.rect.height, 0);
}

/**
 * The tip that belongs to a given trigger.
 *
 * Two tips can be open at once — the one the pointer just left stays for
 * a moment — so the locator is scoped to the trigger's own panel rather
 * than to "the tooltip".
 */
async function tipFor(trigger: Locator): Promise<Locator> {
  const id = await trigger.getAttribute('aria-controls');
  return id
    ? trigger.page().locator(`[id="${id}"]`)
    : trigger.page().getByRole('tooltip');
}

/**
 * Wait for the React shell to mount.
 *
 * The wordmark is an SVG with `role="img"`, not an `<img alt=…>`, so the
 * shared `waitForAppBoot` helper (which looks for `img[alt="Buildmesh"]`)
 * does not match it.
 */
async function waitForShell(page: Page): Promise<void> {
  await expect(page.getByRole('img', { name: 'Buildmesh' }), 'the title bar should mount once the React shell boots')
    .toBeVisible({ timeout: 20000 });
}

async function openSettings(page: Page): Promise<void> {
  await openSettingsFromTitleBar(page);
  await expect(page.getByRole('dialog', { name: 'Settings', exact: true })).toBeVisible();
}

for (const viewport of VIEWPORTS) {
  for (const theme of THEMES) {
    for (const textScale of TEXT_SCALES) {
      test(`help popover fits at ${viewport.width}x${viewport.height} ${theme} text=${textScale}%`, async ({ page }) => {
        await page.setViewportSize(viewport);
        await page.addInitScript({ content: buildInitScript({ ...defaultFixtures }) });
        await page.addInitScript(t => localStorage.setItem('buildmesh.theme', t), theme);
        await page.addInitScript(scale => localStorage.setItem('buildmesh.text-scale', scale), String(textScale));
        await page.goto('/');
        await waitForShell(page);
        await openSettings(page);

        for (const label of HELP_LABELS) {
          const trigger = page.getByRole('button', { name: label });
          await expect(trigger, `${label} should be in the General pane`).toBeVisible();
          await trigger.scrollIntoViewIfNeeded();
          await trigger.hover();

          const tip = await tipFor(trigger);
          await expect(tip, `${label} should reveal its explanation`).toBeVisible();
          const geometry = await readTipGeometry(tip);

          // The tip is portalled out of the settings scroller, and no
          // ancestor cuts any of it off.
          expectFullyReadable(geometry, label);

          // And the whole of it is inside the window.
          expect(geometry.rect.x).toBeGreaterThanOrEqual(0);
          expect(geometry.rect.y).toBeGreaterThanOrEqual(0);
          expect(geometry.rect.x + geometry.rect.width).toBeLessThanOrEqual(viewport.width);
          expect(geometry.rect.y + geometry.rect.height).toBeLessThanOrEqual(viewport.height);
        }
      });
    }
  }
}

test('hovering then clicking the help icon leaves it open', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 600 });
  await page.addInitScript({ content: buildInitScript({ ...defaultFixtures }) });
  await page.goto('/');
  await waitForShell(page);
  await openSettings(page);

  const trigger = page.getByRole('button', { name: 'About Theme' });
  await trigger.scrollIntoViewIfNeeded();
  await trigger.hover();
  const tip = await tipFor(trigger);
  await expect(tip).toBeVisible();

  // The regression: the click used to close the tip the pointer had just
  // hovered open.
  await trigger.click();
  await expect(tip).toBeVisible();

  // Moving the pointer into the tip keeps it open.
  const box = await tip.boundingBox();
  await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await expect(tip).toBeVisible();

  // Escape dismisses it and puts focus back on the trigger.
  await page.keyboard.press('Escape');
  await expect(tip).toHaveCount(0);
  await expect(trigger).toBeFocused();
});

test('the lowest visible help icon in the settings pane is fully readable', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 600 });
  await page.addInitScript({ content: buildInitScript({ ...defaultFixtures }) });
  await page.goto('/');
  await waitForShell(page);
  await openSettings(page);

  // Scroll the settings body to its end, so the lowest help icon in the
  // pane is the one under test.
  const body = page.getByRole('dialog').locator('div.overflow-y-auto').last();
  await body.evaluate(element => { element.scrollTop = element.scrollHeight; });
  await page.waitForTimeout(200);

  const helpButtons = page.getByRole('button', { name: /^About / });
  const count = await helpButtons.count();
  expect(count).toBeGreaterThan(0);
  const lowest = helpButtons.nth(count - 1);
  await lowest.scrollIntoViewIfNeeded();
  await lowest.hover();

  const tip = await tipFor(lowest);
  await expect(tip).toBeVisible();
  const geometry = await readTipGeometry(tip);
  expectFullyReadable(geometry, 'the lowest visible help icon');
  expect(geometry.rect.x).toBeGreaterThanOrEqual(0);
  expect(geometry.rect.y).toBeGreaterThanOrEqual(0);
  expect(geometry.rect.x + geometry.rect.width).toBeLessThanOrEqual(900);
  expect(geometry.rect.y + geometry.rect.height).toBeLessThanOrEqual(600);
});
