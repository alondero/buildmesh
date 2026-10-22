/**
 * Issue #2468 — BEFORE state (baseline commit 9b3ce9d7).
 *
 * With an empty workspace, the Sidebar column renders TWO mesh-creation CTAs:
 * the inline "+ New mesh" button under the "No meshes yet" message and the
 * sticky footer "New mesh" button. This asserts the duplicate so the
 * baseline capture records the bug it replaces.
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

  console.log('[ui-shot] BEFORE — sidebar-column mesh CTAs:', JSON.stringify(ctas));

  if (ctas.length !== 2) {
    throw new Error(`expected the duplicate pair of mesh CTAs, found ${ctas.length}: ${JSON.stringify(ctas)}`);
  }
  if (ctas[0] !== '+ New mesh' || ctas[1] !== 'New mesh') {
    throw new Error(`expected the inline "+ New mesh" plus the sticky "New mesh" footer, found ${JSON.stringify(ctas)}`);
  }
};
