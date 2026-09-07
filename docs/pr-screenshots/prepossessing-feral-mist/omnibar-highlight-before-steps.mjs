/**
 * SYNTHETIC before snapshot — not a checkout of the pre-fix commit.
 *
 * Forces the legacy active paint (`#16161d` = bg-bg-card) with inline
 * !important so the before PNG shows the near-invisible keyboard caret
 * against the overlay shell. Prefer a detached worktree at the parent
 * commit when a true pre-change build is available; this path exists for
 * same-session PR evidence when rebuild cost is prohibitive.
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

  // Inline !important so React/Tailwind cannot keep the selection paint.
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('[data-testid="command-omnibar-option"]')) {
      const selected = el.getAttribute('aria-selected') === 'true';
      el.style.setProperty(
        'background-color',
        selected ? '#16161d' : 'transparent',
        'important',
      );
      el.style.setProperty('border-left-color', 'transparent', 'important');
      el.style.setProperty('box-shadow', 'none', 'important');
    }
  });
}
