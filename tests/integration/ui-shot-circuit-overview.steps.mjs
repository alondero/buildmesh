// The Circuit Run overview, as a person reads it (mock IPC).
//
//   node scripts/ui-shot.mjs --out .tmp/circuit-overview.png --mock --serve \
//     --mock-url http://127.0.0.1:3999 --steps tests/integration/ui-shot-circuit-overview.steps.mjs
//
// On the agent node card's Overview tab, for the built-in Review circuit in
// four states, the panel leads with what is happening, the stage and the
// review rounds; at a 260px card nothing scrolls sideways and every control
// keeps a 24px hit area. In the Probe, an opened review run leads with the
// same summary, and the card fits the dock's 240px minimum in both themes at
// 1x and 1.5x text. Screenshots land in `.tmp/circuit-overview-*.png`.
import { expect } from '@playwright/test';
import { graph, scenarios } from './ui-shot-circuit-overview.fixtures.mjs';

const EXPECTED = {
  reviewing: { headline: 'Code reviewer is reviewing round 2', caption: 'Round 2 of 5 · Reviewing now', stage: 'review' },
  fixing: { headline: 'fix-terminal-blank is fixing round 1 feedback', caption: 'Round 1 of 5 · Changes requested, fixes in progress', stage: 'review' },
  approval: { headline: 'Waiting for your approval', caption: 'Round 1 of 5 · Not started', stage: 'prepare' },
  approved: { headline: 'Review approved', caption: 'Round 3 of 5 · Approved', stage: null },
};

async function assertFits(panel, label) {
  const overflow = await panel.evaluate((el) => el.scrollWidth - el.clientWidth);
  expect(overflow, `${label}: scrolled sideways`).toBeLessThanOrEqual(0);
  const small = await panel.evaluate((el) => [...el.querySelectorAll('button, select, a')]
    .map((control) => ({ text: control.textContent.trim().slice(0, 30), rect: control.getBoundingClientRect() }))
    .filter(({ rect }) => rect.width > 0 && (rect.height < 23.5 || rect.width < 23.5))
    .map(({ text, rect }) => `${text} ${rect.width.toFixed(0)}x${rect.height.toFixed(0)}`));
  expect(small, `${label}: controls under 24px`).toEqual([]);
}

async function nodeCardOverview(page, mock) {
  await page.locator('#mesh-item-name-1').click();
  const card = page.locator('[data-node-card-id="1"]');
  await expect(card).toBeVisible();
  const ownership = { node_id: 1, run_id: 91, circuit_id: 3, circuit_name: 'Review', state: 'running', parent_node_id: null };
  await mock.on('circuit_run_history', { entries: [], checkpoints: [], coverage: [] });
  for (const [name, overview] of Object.entries(scenarios)) {
    await mock.on('list_circuit_agent_ownerships', [{ ...ownership, state: overview.run.state }]);
    await mock.on('circuit_run_overview', overview);
    await mock.emit('circuit-run-updated', { run_id: 91, state: overview.run.state });
    await card.getByRole('tab', { name: /^Circuit run overview/ }).click();
    const panel = card.getByTestId('run-overview-91');
    await expect(panel).toHaveAttribute('data-run-state', overview.run.state);
    await expect(panel.getByTestId('run-overview-headline')).toHaveText(EXPECTED[name].headline);
    await expect(panel.getByTestId('run-review-rounds-caption')).toHaveText(EXPECTED[name].caption);
    const current = panel.locator('[data-testid^="run-stage-"][aria-current="step"]');
    if (EXPECTED[name].stage === null) await expect(current).toHaveCount(0);
    else await expect(current).toHaveAttribute('data-testid', `run-stage-${EXPECTED[name].stage}`);
    // Detail stays one click away, collapsed until asked for.
    await expect(panel.getByRole('list', { name: 'Circuit steps' })).toHaveCount(0);
    await assertFits(panel, `${name} default width`);
    await card.screenshot({ path: `.tmp/circuit-overview-${name}.png`, animations: 'disabled' });

    await card.evaluate((el) => { el.style.width = '260px'; el.style.flex = 'none'; });
    await assertFits(panel, `${name} 260px`);
    await card.screenshot({ path: `.tmp/circuit-overview-${name}-260.png`, animations: 'disabled' });
    await card.evaluate((el) => { el.style.width = ''; el.style.flex = ''; });
  }
  // The full step list opens on request, with the current step's detail open.
  await mock.on('circuit_run_overview', scenarios.reviewing);
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'running' });
  const panel = card.getByTestId('run-overview-91');
  await expect(panel.getByTestId('run-overview-headline')).toHaveText(EXPECTED.reviewing.headline);
  await panel.getByRole('button', { name: /All steps/ }).click();
  await expect(panel.getByTestId('rail-row-verdict')).toHaveAttribute('data-rail-state', 'current');
}

async function probeRunCard(page, mock) {
  const overview = scenarios.reviewing;
  const circuit = {
    is_preset: true, id: 3, mesh_id: 1, name: 'Review', description: '', enabled: true,
    graph_json: JSON.stringify(graph), created_at: '2026-11-10T09:00:00Z', updated_at: '2026-11-10T09:00:00Z',
  };
  const detail = { run: overview.run, steps: overview.steps, review_round_limit: 5 };
  await mock.on('list_circuit_probe', { circuits: [{ circuit, runs: [detail] }], queue: [], queue_waits: {} });
  await mock.on('list_circuits_with_runs', [{ circuit, runs: [detail] }]);
  await mock.on('circuit_run_overview', overview);

  await page.getByRole('button', { name: 'Search or open' }).click();
  await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
  await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
  await page.getByTestId('circuits-probe-tab').waitFor({ state: 'visible' });
  await page.getByTestId('run-toggle-91').click();
  const summary = page.getByTestId('run-progress-summary-91');
  await expect(summary.getByTestId('run-review-rounds-caption')).toHaveText(EXPECTED.reviewing.caption);
  await expect(summary.locator('[aria-current="step"]')).toHaveAttribute('data-testid', 'run-stage-review');

  await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
  await page.keyboard.press('End');
  const body = page.getByTestId('circuits-probe-body');
  for (const theme of ['dark', 'light']) {
    for (const scale of ['1', '1.5']) {
      await page.evaluate(([name, size]) => {
        if (name === 'light') document.documentElement.setAttribute('data-theme', 'light');
        else document.documentElement.removeAttribute('data-theme');
        document.documentElement.style.setProperty('--ui-text-scale', size);
      }, [theme, scale]);
      const { scroll, client, width } = await body.evaluate((el) => ({ scroll: el.scrollWidth, client: el.clientWidth, width: el.getBoundingClientRect().width }));
      expect(width, 'the Probe did not reach its minimum width').toBeLessThanOrEqual(241);
      expect(scroll, `Probe ${theme} ${scale}x: an opened run scrolled sideways`).toBeLessThanOrEqual(client);
      await page.getByTestId('run-card-91').screenshot({ path: `.tmp/circuit-overview-probe-${theme}-${scale}x.png`, animations: 'disabled' });
    }
  }
  await page.evaluate(() => {
    document.documentElement.removeAttribute('data-theme');
    document.documentElement.style.removeProperty('--ui-text-scale');
  });
}

export default async function ({ page, mock }) {
  await nodeCardOverview(page, mock);
  await probeRunCard(page, mock);
  console.log('PASS: the Overview leads with the headline, stage and review rounds in four states at default and 260px; '
    + 'the Probe opens a review run on the same summary and fits 240px in both themes at 1x and 1.5x text.');
}
