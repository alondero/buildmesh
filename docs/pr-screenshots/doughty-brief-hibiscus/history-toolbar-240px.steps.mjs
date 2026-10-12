// Drives the Circuits Probe History toolbar at the dock's narrow bound (240px)
// against the REAL backend (dev-profile WebView2 over CDP): seeds a failed
// review lineage, opens Circuits → History, and asserts the bulk-acknowledgement
// control is present, readable, unclipped, and that arming it names the same
// count it will acknowledge.
//
// Throwing fails the run, so these are assertions rather than observations.

const PANEL_WIDTH = 240;

export default async function ({ page, invoke }) {
  const fixtureName = `Attention backlog ${Date.now()}`;
  const fixture = await invoke('create_attention_backlog_fixture', {
    name: fixtureName,
    count: 3,
  });
  const meshId = fixture?.mesh_id;

  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1500);

  // Select the mesh this run just created, by name. Clicking an already-selected
  // row would toggle the selection back off, so only click when the panel has no
  // mesh selected.
  const selectedScope = await page.evaluate(() => {
    const el = Array.from(document.querySelectorAll('[title^="Mesh scope:"]'))[0];
    return el ? el.getAttribute('title') : null;
  });
  if (!selectedScope || !selectedScope.includes(fixtureName)) {
    await page.getByText(fixtureName, { exact: true }).first().click();
    await page.waitForTimeout(2500);
  }

  // More tools → Circuits opens the Probe dock on that destination.
  await page.getByTitle('Open more tools').first().click();
  await page.waitForTimeout(700);
  await page.getByText('Circuits', { exact: true }).first().click();
  await page.waitForTimeout(2500);

  const panel = page.locator('#probe-tab-panel');
  await panel.waitFor({ state: 'visible', timeout: 20000 });

  // Narrow the dock to its minimum bound. ProbePanel owns the width as an inline
  // style on the body wrapper, so drive that property rather than assuming a
  // test id it does not carry.
  await page.evaluate((width) => {
    const wrapper = document.querySelector('#probe-tab-panel')?.closest('div.relative');
    if (!wrapper) throw new Error('probe panel wrapper not found');
    wrapper.style.width = `${width}px`;
  }, PANEL_WIDTH);
  await page.waitForTimeout(400);

  await page.getByTestId('circuits-view-history').first().click();
  await page.waitForTimeout(1500);

  const toolbar = page.getByTestId('circuit-history-filters').first();
  await toolbar.waitFor({ state: 'visible', timeout: 20000 });

  const dismissAll = page.getByTestId('history-dismiss-all').first();
  await dismissAll.waitFor({ state: 'visible', timeout: 20000 });
  const label = (await dismissAll.textContent())?.trim();
  if (!/^Dismiss all \(\d+\)$/.test(label ?? '')) {
    throw new Error(`unreadable "Dismiss all" label at ${PANEL_WIDTH}px: ${JSON.stringify(label)}`);
  }

  // Nothing in the control row may be clipped at the narrow bound, and neither
  // the toolbar nor the panel may scroll sideways.
  const clipped = await page.evaluate(() => {
    const row = document.querySelector('[data-testid="circuit-history-filters"]');
    if (!row) return ['filters row missing'];
    const rowBox = row.getBoundingClientRect();
    const bad = [];
    for (const el of row.querySelectorAll('button, input, label, textarea')) {
      const box = el.getBoundingClientRect();
      if (box.width === 0 && box.height === 0) continue;
      if (box.right > rowBox.right + 0.5 || box.left < rowBox.left - 0.5) {
        bad.push(`${el.tagName}[${(el.textContent ?? '').trim().slice(0, 24)}] overflows the toolbar`);
      }
    }
    if (row.scrollWidth > row.clientWidth + 1) bad.push('toolbar scrolls horizontally');
    const panelEl = document.querySelector('#probe-tab-panel')?.closest('div.relative');
    if (panelEl && panelEl.scrollWidth - panelEl.clientWidth > 1) {
      bad.push(`panel scrolls sideways by ${panelEl.scrollWidth - panelEl.clientWidth}px`);
    }
    return bad;
  });
  if (clipped.length) throw new Error(`clipped at ${PANEL_WIDTH}px: ${clipped.join('; ')}`);

  // Arm the confirmation: it must name the same count the button did.
  await dismissAll.click();
  const confirm = page.getByTestId('history-dismiss-all-confirm').first();
  await confirm.waitFor({ state: 'visible', timeout: 5000 });
  const confirmLabel = (await confirm.textContent())?.trim();
  if (!/^Dismiss \d+$/.test(confirmLabel ?? '')) {
    throw new Error(`unreadable confirmation label: ${JSON.stringify(confirmLabel)}`);
  }
  const armed = Number(/(\d+)/.exec(confirmLabel ?? '')?.[1]);
  const offered = Number(/(\d+)/.exec(label ?? '')?.[1]);
  if (armed !== offered) {
    throw new Error(`confirmation says "${confirmLabel}" but the button said "${label}"`);
  }

  // Leave no fixture rows behind, so a repeat run starts from the same state.
  if (meshId != null) await invoke('delete_mesh', { meshId }).catch(() => {});

  return { label, confirmLabel, meshId };
}