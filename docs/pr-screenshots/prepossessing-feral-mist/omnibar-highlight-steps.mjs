/**
 * /verify-ui steps — command omnibar keyboard-active row contrast.
 *
 * Opens the palette, narrows to a multi-row list, moves the highlight with
 * ArrowDown, then asserts the active option paints with the semantic
 * selection surface (computed style), not a near-invisible bg-card wash.
 */
export default async function ({ page }) {
  // Ensure dark theme for the primary assert + screenshot crop.
  await page.evaluate(() => {
    document.documentElement.removeAttribute('data-theme');
    try {
      localStorage.removeItem('buildmesh.theme');
    } catch {
      /* ignore */
    }
  });

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

  await input.fill('>view');
  const options = page.getByTestId('command-omnibar-option');
  await options.first().waitFor({ state: 'visible', timeout: 5_000 });
  const count = await options.count();
  if (count < 2) {
    throw new Error(`expected ≥2 omnibar options for ">view", got ${count}`);
  }

  await input.press('ArrowDown');
  await page.waitForFunction(() => {
    const inputEl = document.querySelector('[role="combobox"]');
    return inputEl?.getAttribute('aria-activedescendant') === 'command-omnibar-option-1';
  });
  // Wait out animate-scale-in so getComputedStyle is not mid-fade.
  await page.waitForFunction(() => {
    const dialog = document.querySelector('[role="dialog"]');
    return dialog && getComputedStyle(dialog).opacity === '1';
  });

  const paint = await page.evaluate(() => {
    const active = document.querySelector(
      '[data-testid="command-omnibar-option"][aria-selected="true"]',
    );
    const idle = document.querySelector(
      '[data-testid="command-omnibar-option"]:not([aria-selected="true"])',
    );
    if (!active || !idle) return null;
    const activeStyle = getComputedStyle(active);
    const idleStyle = getComputedStyle(idle);
    return {
      activeSelected: active.getAttribute('aria-selected'),
      idleSelected: idle.getAttribute('aria-selected'),
      activeBg: activeStyle.backgroundColor,
      idleBg: idleStyle.backgroundColor,
      activeBorderLeft: activeStyle.borderLeftColor,
      idleBorderLeft: idleStyle.borderLeftColor,
    };
  });

  if (!paint) throw new Error('could not read active/idle option styles');
  console.log('[verify] omnibar row paint:', JSON.stringify(paint));

  if (paint.activeSelected !== 'true' || paint.idleSelected === 'true') {
    throw new Error(`aria-selected mismatch: ${JSON.stringify(paint)}`);
  }
  if (paint.activeBg === paint.idleBg) {
    throw new Error(
      `active and idle backgrounds identical (${paint.activeBg}) — highlight invisible`,
    );
  }
  // Reject the old near-invisible bg-card grey (#16161d → rgb(22, 22, 29)).
  if (/rgba?\(\s*22\s*,\s*22\s*,\s*29/.test(paint.activeBg)) {
    throw new Error(`active still paints bg-card grey: ${paint.activeBg}`);
  }
  // Semantic token on :root (dark #1a2a3a). Composited row RGB can vary;
  // the contract is the CSS variable + active distinct from idle.
  const darkToken = await page.evaluate(() =>
    getComputedStyle(document.documentElement)
      .getPropertyValue('--color-bg-selection')
      .trim()
      .toLowerCase(),
  );
  if (darkToken !== '#1a2a3a' && darkToken !== 'rgb(26, 42, 58)') {
    throw new Error(`dark --color-bg-selection unexpected: ${darkToken}`);
  }
  if (paint.activeBorderLeft === paint.idleBorderLeft) {
    throw new Error(
      `active left border should differ from idle (accent vs transparent): ${paint.activeBorderLeft}`,
    );
  }

  // Light theme: selection token flips to #cce8ff via [data-theme=light].
  await page.evaluate(() => {
    document.documentElement.setAttribute('data-theme', 'light');
  });
  await page.waitForFunction(() => {
    const raw = getComputedStyle(document.documentElement)
      .getPropertyValue('--color-bg-selection')
      .trim()
      .toLowerCase();
    return raw === '#cce8ff' || raw === 'rgb(204, 232, 255)';
  });
  const lightPaint = await page.evaluate(() => {
    const active = document.querySelector(
      '[data-testid="command-omnibar-option"][aria-selected="true"]',
    );
    const idle = document.querySelector(
      '[data-testid="command-omnibar-option"]:not([aria-selected="true"])',
    );
    if (!active || !idle) return null;
    return {
      token: getComputedStyle(document.documentElement)
        .getPropertyValue('--color-bg-selection')
        .trim(),
      activeBg: getComputedStyle(active).backgroundColor,
      idleBg: getComputedStyle(idle).backgroundColor,
    };
  });
  if (!lightPaint) throw new Error('light theme: could not read option styles');
  console.log('[verify] omnibar light paint:', JSON.stringify(lightPaint));
  if (lightPaint.token.toLowerCase() !== '#cce8ff') {
    throw new Error(`light --color-bg-selection expected #cce8ff, got ${lightPaint.token}`);
  }
  if (lightPaint.activeBg === lightPaint.idleBg) {
    throw new Error(`light theme: active/idle backgrounds identical (${lightPaint.activeBg})`);
  }
  // Restore dark for the screenshot crop.
  await page.evaluate(() => {
    document.documentElement.removeAttribute('data-theme');
    try {
      localStorage.removeItem('buildmesh.theme');
    } catch {
      /* ignore */
    }
  });

  console.log('[verify] omnibar keyboard highlight contrast: PASS');
}
