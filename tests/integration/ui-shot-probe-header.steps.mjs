import { expect } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import { MESH_NAME, NODE_NAMES } from './ui-shot-probe-header.fixtures.mjs';

// Run with ui-shot --mock --serve --fixtures tests/integration/ui-shot-probe-header.fixtures.mjs
// --steps tests/integration/ui-shot-probe-header.steps.mjs --out .tmp/probe-header.png
export default async function ({ page }) {
  await page.locator('#mesh-item-name-1').click();
  await page.getByRole('button', { name: 'Open Notifications' }).click();
  const panel = page.getByRole('region', { name: 'Probe panel' });
  const separator = page.getByRole('separator', { name: 'Resize probe panel' });
  const title = page.getByTestId('probe-context-title');
  const scope = page.getByTestId('probe-context-subject');
  const details = page.getByTestId('probe-context-details');
  const open = tab => page.evaluate(async tab => {
    const { useUIStore } = await import('../../src/stores/uiStore.ts');
    useUIStore.getState().openProbeTab(tab);
  }, tab);
  await separator.focus();
  await page.keyboard.press('End');
  await expect(separator).toHaveAttribute('aria-valuenow', '240');

  const destinations = await page.evaluate(async () => {
    const { PROBE_TAB_ORDER, PROBE_TAB_DEFINITIONS } = await import('../../src/lib/probeContext.ts');
    return PROBE_TAB_ORDER.map(tab => ({ tab, label: PROBE_TAB_DEFINITIONS[tab].label }));
  });
  for (const { tab, label } of destinations) {
    await open(tab);
    await expect(title).toHaveText(label);
    await expect(details).toBeHidden();
    const geometry = await scope.evaluate(summary => {
      const header = summary.parentElement.parentElement;
      const panel = summary.closest('section');
      const bounds = panel.getBoundingClientRect();
      const scopeBox = summary.getBoundingClientRect();
      const titleBox = header.firstElementChild.getBoundingClientRect();
      const closeBox = panel.querySelector('[aria-label="Close panel"]').getBoundingClientRect();
      return { rows: header.children.length, width: bounds.width,
        scopeBelowTitle: scopeBox.top >= titleBox.bottom, scopeHitHeight: scopeBox.height,
        closeInside: closeBox.left >= bounds.left && closeBox.right <= bounds.right,
        overflow: panel.scrollWidth > panel.clientWidth };
    });
    expect(geometry, tab).toMatchObject({ rows: 2, width: 240, scopeBelowTitle: true, closeInside: true, overflow: false });
    expect(geometry.scopeHitHeight, tab).toBeGreaterThanOrEqual(24);
  }

  await open('files');
  await expect(title).toHaveText('Project Files');
  await page.locator('[data-node-card-id="1"]').getByTestId('grid-node-header').click({ position: { x: 80, y: 12 } });
  await expect(scope).toHaveAccessibleName(`Mesh: ${MESH_NAME} · Working tree: ${NODE_NAMES[0]} · Following selection`);
  await scope.focus();
  await page.keyboard.press('Enter');
  await expect(details).toBeVisible();
  await expect(details).toContainText(NODE_NAMES[0]);
  await expect(details).toContainText('Following selection');
  const popup = await details.boundingBox();
  const bounds = await panel.boundingBox();
  expect(popup.x).toBeGreaterThanOrEqual(bounds.x);
  expect(popup.x + popup.width).toBeLessThanOrEqual(bounds.x + bounds.width);
  expect(await details.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await expect(panel.getByText('File Tree', { exact: true })).toBeVisible();
  await mkdir('.tmp', { recursive: true });
  await panel.screenshot({ path: '.tmp/probe-mesh-long-240.png' });
  await details.click();
  await expect(details).toBeVisible();
  await expect(details).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(details).toBeHidden();
  await expect(scope).toBeFocused();
  await page.keyboard.press('Space');
  await expect(details).toBeVisible();

  // Open with focus outside so the title press cannot blur the disclosure.
  await separator.focus();
  await expect(details).toBeHidden();
  await scope.evaluate(summary => { summary.parentElement.open = true; });
  await expect(details).toBeVisible();
  await expect(separator).toBeFocused();
  await title.click();
  await expect(details, 'Outside pointer-down must dismiss with focus starting on the separator').toBeHidden();
  await scope.press('Enter');
  await expect(details).toBeVisible();
  await separator.focus();
  await expect(details).toBeHidden();
  await expect(separator).toBeFocused();

  // A scope update must close the panel while retaining the focused summary.
  await scope.press('Enter');
  await page.evaluate(async () => {
    const { useAgentNodeStore } = await import('../../src/stores/agentNodeStore.ts');
    useAgentNodeStore.getState().setActiveNode(2);
  });
  await expect(details).toBeHidden();
  await expect(scope).toBeFocused();
  await expect(scope).toHaveAccessibleName(`Mesh: ${MESH_NAME} · Working tree: ${NODE_NAMES[1]} · Following selection`);

  await page.locator('[data-node-card-id="1"]').getByTestId('grid-node-header').click({ position: { x: 80, y: 12 } });
  await expect(scope).toHaveAccessibleName(`Mesh: ${MESH_NAME} · Working tree: ${NODE_NAMES[0]} · Following selection`);

  await page.locator('[data-node-card-id="2"]').getByTestId('grid-node-header').click({ position: { x: 80, y: 12 } });
  await expect(details).toBeHidden();
  await expect(scope).toHaveAccessibleName(`Mesh: ${MESH_NAME} · Working tree: ${NODE_NAMES[1]} · Following selection`);

  await open('review');
  await expect(title).toHaveText('Agent Changes');
  await expect(scope).toHaveAccessibleName(`Agent: ${NODE_NAMES[1]} · Mesh: ${MESH_NAME} · Following selection`);
  // Dispatch a real touch tap, with no hover to summon the native title tooltip.
  const scopeBox = await scope.boundingBox();
  const cdp = await page.context().newCDPSession(page);
  const point = { x: scopeBox.x + scopeBox.width / 2, y: scopeBox.y + scopeBox.height / 2 };
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] });
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await expect(details).toBeVisible();
  await expect(details).toContainText(NODE_NAMES[1]);
  await expect(details).toContainText(MESH_NAME);
  await expect(panel.getByText('No changes vs Base Ref', { exact: true })).toBeVisible();
  await panel.screenshot({ path: '.tmp/probe-agent-long-240.png' });
  const titleBox = await title.boundingBox();
  await cdp.send('Input.dispatchTouchEvent', {
    type: 'touchStart', touchPoints: [{ x: titleBox.x + titleBox.width / 2, y: titleBox.y + titleBox.height / 2 }],
  });
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await expect(details).toBeHidden();
  await cdp.detach();
  await scope.press('Enter');
  await expect(details).toBeVisible();
  await scope.press('Escape');
  await expect(details).toBeHidden();

  await separator.focus();
  for (let i = 0; i < 15; i++) await page.keyboard.press('ArrowLeft');
  await expect(separator).toHaveAttribute('aria-valuenow', '360');
  await panel.getByRole('button', { name: 'Close panel' }).click();
  await expect(panel).toHaveCount(0);
  await page.getByRole('button', { name: 'Open Notifications' }).click();
  await expect(scope).toHaveText('Host-wide');
  console.log(`PASS: ${destinations.length} two-row Probe headers at 240px; long names, keyboard/touch disclosure, selection changes, resize and close/reopen.`);
}
