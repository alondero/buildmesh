/**
 * Forces the pre-fix active paint (`bg-bg-card` only) so the before shot
 * shows the near-invisible keyboard caret against the overlay shell.
 */
export default async function ({ page }) {
  const existing = page.getByRole('combobox');
  if (await existing.count()) {
    await page.keyboard.press('Escape');
    await existing.waitFor({ state: 'hidden', timeout: 3_000 }).catch(() => {});
  }

  await page.getByTestId('titlebar-command-search').click();
  const input = page.getByRole('combobox');
  await input.waitFor({ state: 'visible', timeout: 5_000 });
  await input.fill('>view');
  const options = page.getByTestId('command-omnibar-option');
  await options.first().waitFor({ state: 'visible', timeout: 5_000 });
  await input.press('ArrowDown');
  await page.waitForFunction(() => {
    const inputEl = document.querySelector('[role="combobox"]');
    return inputEl?.getAttribute('aria-activedescendant') === 'command-omnibar-option-1';
  });

  // Inline !important so React/Tailwind cannot keep the cyan fill/bar.
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('[data-testid="command-omnibar-option"]')) {
      const selected = el.getAttribute('aria-selected') === 'true';
      el.style.setProperty(
        'background-color',
        selected ? '#16161d' : 'transparent',
        'important',
      );
      el.style.setProperty('box-shadow', 'none', 'important');
    }
  });
}
