import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';

// Real frontend geometry and interaction, fixture IPC only. Run with ui-shot --mock --serve.
export default async function ({ page, mock }) {
  await mkdir('.tmp/ui-critical', { recursive: true });
  const shot = async name => {
    await page.waitForTimeout(200);
    await page.screenshot({ path: `.tmp/ui-critical/${name}.png` });
  };
  const state = (method, ...args) => page.evaluate(async ({ method, args }) => {
    const { useUIStore } = await import('../../src/stores/uiStore.ts');
    useUIStore.getState()[method](...args);
  }, { method, args });
  const disjoint = (a, b) => a.x + a.width <= b.x + 1 || b.x + b.width <= a.x + 1
    || a.y + a.height <= b.y + 1 || b.y + b.height <= a.y + 1;
  const widths = () => page.locator('[data-node-card-id]').evaluateAll(elements => elements.map(el => el.getBoundingClientRect().width));
  const dragDivider = async delta => {
    await page.getByTestId('grid-scroll-viewport').evaluate(el => { el.scrollLeft = 0; });
    const divider = await page.getByTestId('grid-scroll-viewport').locator('.cursor-col-resize').first().boundingBox();
    await page.mouse.move(divider.x + divider.width / 2, divider.y + 100);
    await page.mouse.down();
    await page.mouse.move(divider.x + divider.width / 2 + delta, divider.y + 100, { steps: 5 });
    await page.mouse.up();
    await page.waitForTimeout(100);
  };
  const assertDragTransfer = async () => {
    const before = await widths();
    await dragDivider(50);
    const after = await widths();
    assert.ok(Math.abs(after[0] - before[0] - 50) < 1, 'Divider failed to grow its first pane by 50px');
    assert.ok(Math.abs(after[1] - before[1] + 50) < 1, 'Divider failed to shrink its second pane by 50px');
  };
  await page.setViewportSize({ width: 900, height: 600 });
  await mock.on('get_provider_meters', []);
  await state('openProbeTab', 'usage');
  const resize = page.getByRole('separator', { name: 'Resize probe panel' });
  await resize.focus();
  await page.keyboard.press('Home');
  await page.waitForTimeout(200);
  assert.ok((await page.getByTestId('terminal-workspace').boundingBox()).width >= 299);
  const cards = await page.locator('[data-node-card-id]').evaluateAll(elements => elements.map(el => el.getBoundingClientRect().width));
  assert.equal(cards.length, 3);
  assert.ok(cards.every(width => width >= 298), `Narrow terminal cards: ${cards}`);
  assert.ok(await page.getByTestId('grid-scroll-viewport').evaluate(el => el.scrollWidth > el.clientWidth));
  await shot('narrow-grid');
  const dock = page.getByRole('region', { name: 'Probe panel' });
  const beforeKey = (await dock.boundingBox()).width;
  await page.keyboard.press('ArrowRight');
  await page.waitForTimeout(100);
  assert.ok(Math.abs((await dock.boundingBox()).width - (beforeKey - 8)) < 1, 'Constrained keyboard resize jumped');
  await page.keyboard.press('ArrowLeft');
  await page.waitForTimeout(100);
  assert.ok(Math.abs((await dock.boundingBox()).width - beforeKey) < 1);
  const grip = await resize.boundingBox();
  await page.mouse.move(grip.x + grip.width / 2, grip.y + 100);
  await page.mouse.down();
  await page.mouse.move(grip.x + grip.width / 2 + 20, grip.y + 100, { steps: 5 });
  await page.mouse.up();
  await page.waitForTimeout(100);
  assert.ok(Math.abs((await dock.boundingBox()).width - (beforeKey - 20)) < 1, 'Constrained pointer resize jumped');
  await page.evaluate(async () => (await import('../../src/stores/agentNodeStore.ts')).useAgentNodeStore.getState().setActiveNode(3));
  await page.waitForTimeout(100);
  const selectedCard = await page.locator('[data-node-card-id="3"]').boundingBox();
  const viewport = await page.getByTestId('grid-scroll-viewport').boundingBox();
  assert.ok(selectedCard.x >= viewport.x - 1 && selectedCard.x + selectedCard.width <= viewport.x + viewport.width + 1,
    'Keyboard selection did not reveal the offscreen card');
  await state('toggleProbe');

  // Preserve a deliberately unequal split while the window narrows.
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.waitForTimeout(100);
  await dragDivider(-60);
  const wideShares = await widths();
  assert.ok(Math.max(...wideShares) - Math.min(...wideShares) > 50, 'Unequal split fixture was not established');
  await page.setViewportSize({ width: 900, height: 600 });
  await page.waitForTimeout(100);
  assert.ok((await widths()).every(width => width >= 298));
  await assertDragTransfer();

  // The two-pane path has its own resize state and must obey the same floor.
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.locator('#mesh-item-name-1').click();
  await page.waitForTimeout(100);
  await dragDivider(-150);
  await page.setViewportSize({ width: 900, height: 600 });
  await page.waitForTimeout(100);
  assert.ok((await widths()).every(width => width >= 298));
  await assertDragTransfer();

  const expand = page.getByRole('button', { name: 'Show agents for playground', exact: true });
  if (await expand.count()) await expand.click();
  const select = page.getByRole('button', { name: 'Select scratch', exact: true });
  await select.focus();
  await page.keyboard.press('Enter');
  assert.equal(await page.evaluate(async () => (await import('../../src/stores/agentNodeStore.ts')).useAgentNodeStore.getState().activeNodeId), 3);
  await page.keyboard.press('F2');
  await page.getByRole('textbox', { name: 'Rename scratch' }).waitFor();
  await page.keyboard.press('Escape');

  await page.evaluate(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__uiCriticalInvoke = original;
    window.__TAURI_INTERNALS__.invoke = (cmd, args) => cmd === 'update_mesh_color'
      ? Promise.reject(new Error('Fixture: colour save failed')) : original(cmd, args);
  });
  await page.getByRole('button', { name: /Change mesh colour for buildmesh/ }).click();
  const swatch = page.getByRole('button', { name: 'Colour #38bdf8', exact: true });
  await swatch.click();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByRole('alert').filter({ hasText: 'Fixture: colour save failed' }).waitFor();
  assert.equal(await swatch.getAttribute('aria-pressed'), 'true');
  await shot('colour-error');
  await page.evaluate(() => { window.__TAURI_INTERNALS__.invoke = window.__uiCriticalInvoke; });
  await mock.on('update_mesh_color', null);
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });

  const graph = { version: 1, nodes: [{ id: 'start', type: { type: 'manual' } }, { id: 'end', type: { type: 'notify', message: 'Done' } }],
    edges: [{ from: 'start', to: 'end', condition: 'always' }] };
  await mock.on('get_circuit', { id: 101, mesh_id: 1, name: 'UI verification', is_preset: false, enabled: true,
    description: '', created_at: '', updated_at: '', graph_json: JSON.stringify(graph) });
  await mock.on('list_circuit_runs', []);
  await state('openCircuitEditor', 101);
  await page.getByTestId('editor-save').waitFor();
  assert.equal(await page.getByTestId('circuit-palette').count(), 0);
  await page.getByTestId('toggle-node-palette').click();
  const palette = await page.getByTestId('circuit-palette').boundingBox();
  assert.ok(disjoint(palette, await page.getByTestId('canvas-wrapper').boundingBox()));
  await page.getByTestId('toggle-run-history').click();
  assert.equal(await page.getByTestId('circuit-palette').count(), 0);
  assert.ok(disjoint(await page.getByTestId('run-history-drawer').boundingBox(), await page.getByTestId('canvas-wrapper').boundingBox()));
  await page.setViewportSize({ width: 1700, height: 900 });
  await page.waitForTimeout(100);
  await page.getByTestId('toggle-node-palette').click();
  assert.equal(await page.getByTestId('circuit-palette').count(), 1);
  assert.equal(await page.getByTestId('run-history-drawer').count(), 1);
  await page.setViewportSize({ width: 900, height: 600 });
  await page.waitForTimeout(100);
  assert.equal(await page.getByTestId('circuit-palette').count(), 0, 'Wide palette survived compact history transition');
  await shot('circuit-history');
  await state('openProbeTab', 'usage');
  await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
  await page.keyboard.press('Home');
  await page.waitForTimeout(200);
  const canvas = await page.getByTestId('canvas-wrapper').boundingBox();
  assert.ok(canvas.width >= 298 && canvas.height >= 180);
  assert.ok(disjoint(await page.getByTestId('run-history-drawer').boundingBox(), canvas));
  await shot('circuit-stacked');
  await page.evaluate(async () => {
    (await import('../../src/lib/theme.ts')).setTheme('light');
    (await import('../../src/lib/textScale.ts')).setTextScale(1.5);
  });
  await page.waitForTimeout(100);
  assert.ok(disjoint(await page.getByTestId('run-history-drawer').boundingBox(), await page.getByTestId('canvas-wrapper').boundingBox()));
  await shot('circuit-light-150');

  // A non-authenticated 200 response yields Connect without a deliberate 401 console error.
  await page.route('**/api/**', route => route.fulfill({ status: 200, contentType: 'application/json', body: '{}' }));
  await page.goto(new URL('/mobile/index.html', page.url()).href);
  await page.setViewportSize({ width: 390, height: 300 });
  await page.getByTestId('connect-screen').waitFor();
  await page.getByTestId('token-input').fill('expired-fixture');
  await page.getByTestId('connect-submit').click();
  await page.getByTestId('connect-error').waitFor();
  await page.getByTestId('use-saved').scrollIntoViewIfNeeded();
  const reconnect = await page.getByTestId('use-saved').boundingBox();
  assert.ok(reconnect.y >= 0 && reconnect.y + reconnect.height <= 301);
  assert.ok(await page.getByTestId('connect-screen').evaluate(el => el.scrollHeight > el.clientHeight && el.scrollTop > 0));
  await shot('mobile-pairing-short');
}
