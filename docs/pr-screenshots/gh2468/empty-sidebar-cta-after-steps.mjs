/**
 * Issue #2468 — AFTER state.
 *
 * With an empty workspace, the Sidebar column renders ONE mesh-creation CTA:
 * the inline "+ New mesh" button under the "No meshes yet" message. The sticky
 * footer stands down there, so the column shows one CTA instead of the pair
 * the baseline capture recorded.
 *
 * The count is scoped to the sidebar column (the sibling of the "Resize
 * sidebar" separator). The centre canvas carries its own "New mesh" button in
 * the empty state, and issue #2468 lists that onboarding surface as out of
 * scope.
 */
export default async ({ page }) => {
  await page.waitForSelector('text=No meshes yet', { timeout: 30000 });

  const column = await page.locator('[aria-label="Resize sidebar"] ~ div').first().elementHandle();
  if (!column) throw new Error('sidebar column not found');

  const ctas = await column.$$eval('button', (buttons) =>
    buttons
      .map((button) => (button.textContent || '').trim())
      .filter((text) => /new mesh/i.test(text)),
  );

  const pageCtas = await page.$$eval('button', (buttons) =>
    buttons
      .map((button) => (button.textContent || '').trim())
      .filter((text) => /new mesh/i.test(text)),
  );

  console.log('[ui-shot] AFTER — sidebar-column mesh CTAs:', JSON.stringify(ctas));
  console.log('[ui-shot] AFTER — whole-page mesh CTAs (sidebar + canvas):', JSON.stringify(pageCtas));

  if (ctas.length !== 1) {
    throw new Error(`expected exactly 1 mesh-creation CTA in the sidebar column, found ${ctas.length}: ${JSON.stringify(ctas)}`);
  }
  if (!/^\+ New mesh$/.test(ctas[0])) {
    throw new Error(`expected the inline "+ New mesh" CTA, found ${JSON.stringify(ctas[0])}`);
  }
};
