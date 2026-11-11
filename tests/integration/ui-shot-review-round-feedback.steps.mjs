import { expect } from '@playwright/test';
import { graph, scenarios } from './ui-shot-circuit-overview.fixtures.mjs';

const history = { entries: [
  { id: 0, node_id: 'verdict', attempt: 1, kind: 'step_transition', source: 'circuit_worker', disposition: 'applied', observed_at: '', detail: 'completed' },
  { id: 1, node_id: 'verdict', attempt: 1, kind: 'classification', source: 'classifier', disposition: 'interpreted', observed_at: '',
    detail: JSON.stringify({ step_id: 'verdict', attempt: 1, interpretation: 'working', lifecycle_verified: true,
      report_text: '## Review — round 1\n\nRetain the terminal instance when switching meshes. Add a test that switches away and back.' }) },
  { id: 2, node_id: 'verdict', attempt: 2, kind: 'operator_review', source: 'operator', disposition: 'changes_requested', observed_at: '',
    detail: 'Human review changes_requested: Cover the remaining race when the terminal is restored.' },
], checkpoints: [], coverage: [] };

async function chooseFeedback(panel) {
  const detail = panel.getByTestId('run-latest-review');
  await panel.getByRole('button', { name: /^Round 1:/ }).click();
  await expect(detail).toContainText('Review · Round 1');
  await expect(detail).toContainText('Retain the terminal instance');
  await expect(detail).not.toContainText('All earlier findings are resolved');
  await expect(panel.getByRole('button', { name: /^Round 1:/ })).toHaveAttribute('aria-pressed', 'true');
  await detail.getByRole('button', { name: 'Read the full review' }).click();
  await expect(detail.locator('pre')).toContainText('## Review — round 1');
  await panel.getByRole('button', { name: /^Round 2:/ }).focus();
  await panel.page().keyboard.press('Enter');
  await expect(detail).toContainText('Cover the remaining race');
  await expect(detail).toContainText('recorded by a person');
  await expect(detail.locator('pre')).toHaveCount(0);
  await panel.getByRole('button', { name: /^Round 3:/ }).click();
  await expect(detail).toContainText('Latest review · Round 3');
  await expect(detail).toContainText('All earlier findings are resolved');
}

export default async function ({ page, mock }) {
  const started = Date.now();
  const overview = scenarios.approved;
  await mock.on('circuit_run_history', history);
  await mock.on('circuit_run_overview', overview);
  await page.locator('#mesh-item-name-1').click();
  const card = page.locator('[data-node-card-id="1"]');
  await mock.on('list_circuit_agent_ownerships', [{ node_id: 1, run_id: 91, circuit_id: 3, circuit_name: 'Review', state: 'completed', parent_node_id: null }]);
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'completed' });
  await card.getByRole('tab', { name: /^Circuit run overview/ }).click();
  const panel = card.getByTestId('run-overview-91');
  await chooseFeedback(panel);
  await panel.getByRole('button', { name: /^Round 1:/ }).click();
  await expect(panel.getByTestId('run-latest-review')).toContainText('Retain the terminal instance');
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'completed' });
  await expect(panel.getByRole('button', { name: /^Round 1:/ })).toHaveAttribute('aria-pressed', 'true');
  await panel.screenshot({ path: '.tmp/review-round-feedback-overview.png' });

  const circuit = { is_preset: true, id: 3, mesh_id: 1, name: 'Review', description: '', enabled: true,
    graph_json: JSON.stringify(graph), created_at: '', updated_at: '' };
  // Activity lists active runs; approval can precede the final merge handoff.
  const active = { ...overview, run: { ...overview.run, state: 'running' } };
  const detail = { run: active.run, steps: active.steps, review_round_limit: 5 };
  await mock.on('circuit_run_overview', active);
  await mock.on('list_circuit_probe', { circuits: [{ circuit, runs: [detail] }], queue: [], queue_waits: {} });
  await mock.on('list_circuits_with_runs', [{ circuit, runs: [detail] }]);
  await page.getByRole('button', { name: 'Search or open' }).click();
  await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
  await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
  await page.getByTestId('run-toggle-91').click();
  const summary = page.getByTestId('run-progress-summary-91');
  await chooseFeedback(summary);
  await summary.getByRole('button', { name: /^Round 1:/ }).click();
  await expect(summary.getByTestId('run-latest-review')).toContainText('Retain the terminal instance');
  await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
  await page.keyboard.press('End');
  for (const theme of ['dark', 'light']) {
    for (const scale of ['1', '1.5']) {
      await page.evaluate(([theme, scale]) => {
        document.documentElement.setAttribute('data-theme', theme);
        document.documentElement.style.setProperty('--ui-text-scale', scale);
      }, [theme, scale]);
      const rounds = summary.getByTestId('run-review-rounds');
      const geometry = await rounds.evaluate((el) => {
        const bounds = el.getBoundingClientRect();
        return { width: bounds.width, scroll: el.scrollWidth, client: el.clientWidth,
          controlsFit: [...el.querySelectorAll('button')].every((button) => {
            const box = button.getBoundingClientRect();
            return box.width >= 24 && box.height >= 24 && box.left >= bounds.left && box.right <= bounds.right;
          }) };
      });
      expect(geometry.width).toBeLessThan(240);
      expect(geometry.scroll).toBeLessThanOrEqual(geometry.client);
      expect(geometry.controlsFit).toBe(true);
      await rounds.screenshot({ path: `.tmp/review-round-feedback-${theme}-${scale}.png` });
    }
  }
  await mock.on('circuit_run_history', { entries: [], checkpoints: [], coverage: [] });
  await summary.getByRole('button', { name: /^Round 2:/ }).click();
  await expect(summary.getByTestId('run-latest-review')).toContainText('No feedback was retained for this round.');
  console.log(`PASS: round feedback in Overview and Probe, keyboard selection, refresh, missing history, both themes at 240px/100–150% text (${Date.now() - started}ms); mock IPC, backend not exercised.`);
}
