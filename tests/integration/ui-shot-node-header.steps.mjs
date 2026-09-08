/**
 * Chromium geometry for the agent-node header at a 240px pane.
 * jsdom cannot prove flex overflow; this is the layout contract.
 */
export default async function ({ page }) {
  await page.locator('#mesh-item-name-1').click();
  const header = page.getByTestId('grid-node-header').first();
  await header.waitFor({ state: 'visible' });
  await header.evaluate((el) => {
    const card = el.parentElement;
    if (!card) throw new Error('header has no parent card');
    card.style.width = '240px';
    card.style.maxWidth = '240px';
    card.style.minWidth = '240px';
  });
  await page.waitForFunction(() => {
    const root = document.querySelector('[data-testid="grid-node-header"]');
    const pr = root?.querySelector('[data-testid="pr-pill-trigger"]');
    return Boolean(pr) && !pr.textContent.includes('PR #');
  });

  const metrics = await header.evaluate((el) => {
    const card = el.parentElement;
    const rect = el.getBoundingClientRect();
    const cardRect = card.getBoundingClientRect();
    const title = el.querySelector('.truncate');
    const buttons = [...el.querySelectorAll('button')];
    const last = buttons[buttons.length - 1];
    const overflowing = [...el.querySelectorAll('button')].some((control) => {
      const box = control.getBoundingClientRect();
      return box.width > 0 && (box.left < cardRect.left - 1 || box.right > cardRect.right + 1);
    });
    return {
      cardWidth: Math.round(cardRect.width),
      headerWidth: Math.round(rect.width),
      titleWidth: title?.getBoundingClientRect().width ?? 0,
      lastLabel: last?.getAttribute('aria-label'),
      overflowing,
      prText: el.querySelector('[data-testid="pr-pill-trigger"]')?.textContent?.trim() ?? '',
      closeIsSvg: Boolean(el.querySelector('[aria-label="Close agent node"] svg')),
    };
  });

  if (metrics.cardWidth !== 240) {
    throw new Error(`card width is ${metrics.cardWidth}, expected 240`);
  }
  if (metrics.headerWidth > metrics.cardWidth) {
    throw new Error(`header ${metrics.headerWidth}px is wider than the 240px card`);
  }
  if (metrics.titleWidth <= 8) {
    throw new Error(`node title crushed to ${metrics.titleWidth}px at 240px`);
  }
  if (metrics.overflowing) {
    throw new Error('header controls overflow a 240px pane');
  }
  if (metrics.lastLabel !== 'Close agent node') {
    throw new Error(`trailing control is ${metrics.lastLabel}, expected Close agent node`);
  }
  if (metrics.prText.includes('PR #')) {
    throw new Error(`full PR number label still rendered at 240px: ${metrics.prText}`);
  }
  if (!metrics.closeIsSvg) {
    throw new Error('close control is not an SVG icon');
  }
}
