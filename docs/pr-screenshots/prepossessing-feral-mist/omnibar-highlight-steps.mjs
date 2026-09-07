/**
 * /verify-ui steps — command omnibar keyboard-active row contrast.
 *
 * Opens the palette in commands mode, narrows to a multi-row list, moves
 * the highlight with ArrowDown, then asserts the active option paints with
 * the selection surface (not the near-invisible bg-card on overlay).
 */
export default async function ({ page }) {
  // Clear any leftover palette from a prior shot (backdrop intercepts clicks).
  const existing = page.getByRole('combobox');
  if (await existing.count()) {
    await page.keyboard.press('Escape');
    await existing.waitFor({ state: 'hidden', timeout: 3_000 }).catch(() => {});
  }

  // Title-bar field opens the palette in the webview (global Ctrl+Shift+P
  // is a Tauri shortcut and does not fire through CDP key events).
  await page.getByTestId('titlebar-command-search').click();
  const input = page.getByRole('combobox');
  await input.waitFor({ state: 'visible', timeout: 5_000 });

  // Commands mode + narrow to a multi-row list.
  await input.fill('>view');
  const options = page.getByTestId('command-omnibar-option');
  await options.first().waitFor({ state: 'visible', timeout: 5_000 });
  const count = await options.count();
  if (count < 2) {
    throw new Error(`expected ≥2 omnibar options for ">view", got ${count}`);
  }

  // Move highlight off the first row so the shot shows a mid-list caret.
  await input.press('ArrowDown');
  await options.nth(1).waitFor({ state: 'visible' });
  // Wait until aria-activedescendant tracks the second row (no arbitrary sleep).
  await page.waitForFunction(() => {
    const inputEl = document.querySelector('[role="combobox"]');
    return inputEl?.getAttribute('aria-activedescendant') === 'command-omnibar-option-1';
  });

  const paint = await page.evaluate(() => {
    const active = document.querySelector(
      '[data-testid="command-omnibar-option"][aria-selected="true"]',
    );
    const idle = document.querySelector(
      '[data-testid="command-omnibar-option"]:not([aria-selected="true"])',
    );
    if (!active || !idle) return null;
    const activeBg = getComputedStyle(active).backgroundColor;
    const idleBg = getComputedStyle(idle).backgroundColor;
    return {
      activeClass: active.className,
      idleClass: idle.className,
      activeBg,
      idleBg,
    };
  });

  if (!paint) throw new Error('could not read active/idle option styles');
  console.log('[verify] omnibar row paint:', JSON.stringify(paint));

  if (!/\bbg-accent-cyan\/20\b/.test(paint.activeClass)) {
    throw new Error(`active row missing bg-accent-cyan/20: ${paint.activeClass}`);
  }
  if (/\bbg-bg-card\b/.test(paint.activeClass)) {
    throw new Error(`active row still uses bg-bg-card: ${paint.activeClass}`);
  }
  // accent-cyan/20 must paint a non-transparent fill. Chromium may report
  // rgb()/rgba()/oklab(); reject the old opaque bg-card grey (#16161d).
  if (paint.activeBg === paint.idleBg) {
    throw new Error(
      `active and idle backgrounds identical (${paint.activeBg}) — highlight invisible`,
    );
  }
  if (/rgba?\(\s*22\s*,\s*22\s*,\s*29/.test(paint.activeBg)) {
    throw new Error(`active still paints bg-card grey: ${paint.activeBg}`);
  }
  // Opacity must be material (~0.20). Chromium may emit 0.199999 for /20.
  const alphaMatch = paint.activeBg.match(/\/\s*([0-9.]+)\s*\)|,\s*([0-9.]+)\s*\)/);
  const alpha = alphaMatch ? Number(alphaMatch[1] ?? alphaMatch[2]) : NaN;
  // Composited alpha can land below the declared /20 when the row sits over
  // another surface; require a material tint distinct from idle.
  const hasFill = Number.isFinite(alpha)
    ? alpha >= 0.1 && alpha > (Number.isFinite(Number(paint.idleBg.match(/\/\s*([0-9.]+)/)?.[1])) ? Number(paint.idleBg.match(/\/\s*([0-9.]+)/)?.[1]) : 0)
    : /^rgb\(/i.test(paint.activeBg);
  if (!hasFill) {
    throw new Error(
      `active background expected cyan tint stronger than idle, got active=${paint.activeBg} idle=${paint.idleBg}`,
    );
  }

  console.log('[verify] omnibar keyboard highlight contrast: PASS');
}
