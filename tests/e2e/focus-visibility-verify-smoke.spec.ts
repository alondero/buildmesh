/**
 * Focus visibility, verified in a real browser (issue #2776).
 *
 * Three inputs hid their keyboard focus: the mesh colour picker's custom
 * swatch (a fully transparent native colour input with no `focus-within`
 * styling on the visible wrapper), the Probe scratch pad's textarea and the
 * Circuit quick-connect search field (both `focus:outline-none` with no
 * replacement). The mobile mesh-actions sheet had no accessible name at all.
 *
 * This spec drives the REAL app with fixture IPC and checks the painted
 * result in Chromium, in both the dark and the light theme: the focus
 * indicator must be there when the keyboard arrives, it must be the app's
 * accent token, and it must go away when focus leaves. A class-name check
 * would not show any of that — only a browser can.
 */

import { test, expect, type Locator, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const fixtures = {
  ...defaultFixtures,
  'plugin:app|version': '1.7.0',
  get_mesh_health: {
    base_ref: 'origin/main', local_base_branch: 'main', current_branch: 'main', current_short_sha: '1234567',
    is_detached: false, is_dirty: false, unpushed_ahead: 0, has_upstream: true,
    is_drifted: false, base_branch_holder: null, pool_fill_failure: null,
  },
};

/** The focus token, per theme (`--color-accent-cyan` in App.css). */
const ACCENT = {
  dark: 'rgb(0, 212, 255)',
  light: 'rgb(8, 145, 178)',
} as const;

/**
 * Walk the real focus order with Tab until the target holds focus. The app's
 * dialogs and rails put several controls in between, so the count is a bound
 * rather than a fixed number of presses.
 */
async function tabTo(page: Page, target: Locator, presses = 30) {
  for (let index = 0; index < presses; index++) {
    if (await target.evaluate(element => element === document.activeElement)) return;
    await page.keyboard.press('Tab');
  }
  throw new Error(`Tab never reached ${await target.getAttribute('aria-label')}`);
}

for (const theme of ['dark', 'light'] as const) {
  test(`the custom colour swatch shows a keyboard focus indicator in the ${theme} theme`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.addInitScript(buildInitScript(fixtures));
    await page.addInitScript(value => localStorage.setItem('buildmesh.theme', value), theme);
    await page.goto('/');

    // The "New mesh" dialog carries the shared colour picker.
    await page.getByRole('button', { name: 'New mesh', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'New mesh' });
    await expect(dialog).toBeVisible();

    const custom = dialog.getByLabel('Custom colour', { exact: true });
    // The visible swatch is the input's wrapper; the indicator is painted
    // there, because the input itself is fully transparent.
    const swatch = custom.locator('xpath=..');
    await tabTo(page, custom);
    await expect(custom).toBeFocused();

    // The indicator is the app's focus token on the visible wrapper.
    await expect(swatch).toHaveCSS('outline-style', 'solid');
    await expect(swatch).toHaveCSS('outline-width', '2px');
    await expect(swatch).toHaveCSS('outline-color', ACCENT[theme]);
    await expect(swatch).toHaveCSS('outline-offset', '2px');

    // The transparent native input adds no second decoration of its own.
    await expect(custom).toHaveCSS('opacity', '0');

    // The indicator is not clipped by the swatch wrapper: the wrapper clips
    // its descendants, and the decoration is on the wrapper itself — the
    // parent of the transparent input, not the input the wrapper clips. The
    // input keeps the browser's own focus ring, but that ring is painted
    // transparent by `opacity: 0`, so the accent indicator appears once.
    const structure = await custom.evaluate(el => {
      const wrapper = el.parentElement as HTMLElement;
      return {
        wrapperTag: wrapper.tagName,
        wrapperOverflow: getComputedStyle(wrapper).overflow,
        inputOutlineColor: getComputedStyle(el).outlineColor,
      };
    });
    expect(structure.wrapperTag).toBe('LABEL');
    expect(structure.wrapperOverflow).toBe('hidden');
    expect(structure.inputOutlineColor).not.toBe(ACCENT[theme]);

    // Shift+Tab leaves the field and the indicator goes with it.
    await page.keyboard.press('Shift+Tab');
    await expect(custom).not.toBeFocused();
    await expect(swatch).toHaveCSS('outline-style', 'none');

    // A mouse hover is not a keyboard focus: hovering the swatch paints no
    // indicator, so a pointer user is not shown a "keyboard" ring.
    await swatch.hover();
    await expect(swatch).toHaveCSS('outline-style', 'none');

    // Tabbing back restores it — the indicator is bound to focus, not to a
    // one-shot event.
    await tabTo(page, custom);
    await expect(swatch).toHaveCSS('outline-color', ACCENT[theme]);

    // A selected colour and a focused colour are two different signals: the
    // chosen swatch wears the white selection ring while the focused swatch
    // wears the accent outline.
    const palette = dialog.getByRole('button', { name: /^Colour #/ }).first();
    await palette.click();
    await tabTo(page, custom);
    await expect(swatch).toHaveCSS('outline-color', ACCENT[theme]);
    const selectionRing = await palette.evaluate(element => getComputedStyle(element).boxShadow);
    expect(selectionRing).toContain('255, 255, 255');
  });

  test(`the scratch pad shows a focused border in the ${theme} theme`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.addInitScript(buildInitScript(fixtures));
    await page.addInitScript(value => localStorage.setItem('buildmesh.theme', value), theme);
    await page.goto('/');

    // The Probe panel follows the selection, so the note field only exists
    // once a mesh is in scope.
    await page.locator('[data-mesh-card="1"]').getByText('buildmesh', { exact: true }).click();

    // Open the Notes destination through the command search.
    await page.getByTestId('titlebar-command-search').click();
    await page.getByTestId('command-omnibar-tool-groups')
      .getByRole('button', { name: 'Notes', exact: true })
      .click();

    const notes = page.getByLabel('Scratch pad', { exact: true });
    await expect(notes).toBeVisible();
    await expect(notes).toBeEnabled();

    // At rest the border is transparent, so the field keeps the look it had
    // before the repair.
    await expect(notes).toHaveCSS('border-top-width', '1px');
    await expect(notes).toHaveCSS('border-top-color', 'rgba(0, 0, 0, 0)');

    // Enter the dock's own tab order. The dock sits after the canvas in the
    // document, so tabbing from the top of the page would first have to walk
    // the whole terminal tree; the panel's resize separator is the last
    // control before the panel body.
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await tabTo(page, notes);
    await expect(notes).toBeFocused();

    // Focused, the border takes the accent token — a visible focus ring that
    // the dock's scroll container has no chance to clip, because it is drawn
    // inside the field's own border box.
    await expect(notes).toHaveCSS('border-top-color', ACCENT[theme]);
    await expect(notes).toHaveCSS('outline-style', 'none');

    // Leaving the field returns it to the resting border.
    await page.keyboard.press('Tab');
    await expect(notes).not.toBeFocused();
    await expect(notes).toHaveCSS('border-top-color', 'rgba(0, 0, 0, 0)');
  });
}
