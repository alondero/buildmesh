import { expect } from '@playwright/test';
export default async function ({ page, invoke }) {
  const mesh = await invoke('create_test_mesh', { name: 'Issue tags verification', path: process.cwd() });
  try {
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator(`#mesh-item-name-${mesh.id}`).click();
    const live = await page.evaluate(async meshId => {
      const ipc = window.__TAURI_INTERNALS__.invoke;
      const labels = await ipc('get_repo_labels', { meshId });
      let validation;
      try { await ipc('set_issue_label', { meshId, issueNumber: 0, label: 'bug', present: true }); }
      catch (error) { validation = String(error); }
      return { labels, validation };
    }, mesh.id);
    expect(live.labels.length).toBeGreaterThan(0);
    expect(live.validation).toContain('positive issue number');
    // Only GitHub/circuit fixture data is intercepted; the real application
    // shell, WebView and other IPC remain connected to the dev backend.
    const issue = { number: 101, title: '[Circuit] Validate agent lifecycle and ownership', body: 'Prepare the implementation, then verify agent lifecycle ownership.', url: 'https://github.com/alondero/buildmesh/issues/101', state: 'open', labels: ['needs-triage', 'ready-for-agent'], blocked_by: [], author: 'alondero' };
    const labels = ['needs-triage', 'ready-for-agent', 'team/ui', 'a-very-long-repository-label-that-must-wrap-at-the-narrow-probe-width'];
    const circuit = { id: 1, mesh_id: mesh.id, name: 'Ready for agent implementation', description: '', enabled: true, is_preset: false, created_at: '', updated_at: '', graph_json: JSON.stringify({ version: 3, nodes: [{ id: 'trigger', type: { type: 'github_issue_label', label: 'ready-for-agent' } }], edges: [] }) };
    let labelReads = 0;
    await page.route('http://ipc.localhost/**', async route => {
      const command = new URL(route.request().url()).pathname.slice(1);
      let response;
      let error = false;
      if (command === 'get_repo_issues') {
        // Issue #2024 rank 6 - the feed commands return an items + completeness
        // wrapper rather than a bare array, so a truncated read can state its
        // truncation instead of looking complete. complete: true - this fixture
        // is not testing pagination.
        const items = [{ ...issue, labels: [...issue.labels] }];
        response = {
          items,
          completeness: { returned: items.length, pages_fetched: 1, complete: true, incomplete_reason: null, reported_total: null },
        };
      }
      else if (command === 'get_repo_labels') { labelReads += 1; response = labels; }
      else if (command === 'list_circuits') response = [circuit];
      else if (command === 'set_issue_label') {
        const args = route.request().postDataJSON();
        if (args.label === 'team/ui') {
          response = 'GitHub denied permission to change this issue label. Retry after restoring access.';
          error = true;
        } else {
          issue.labels = args.present ? [...issue.labels, args.label] : issue.labels.filter(label => label !== args.label);
          response = null;
        }
      } else return route.continue();
      await route.fulfill({ status: 200, headers: { 'Tauri-Response': error ? 'error' : 'ok', 'Access-Control-Allow-Origin': '*', 'Access-Control-Expose-Headers': 'Tauri-Response', 'Content-Type': 'application/json' }, body: JSON.stringify(response) });
    });
    await page.getByRole('button', { name: 'Search or open' }).click();
    await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open GitHub Issues');
    await page.getByRole('option', { name: /^Open GitHub Issues Find work/ }).click();
    await expect(page.locator('[data-circuit-trigger-label="ready-for-agent"]')).toBeVisible();
    await page.getByRole('button', { name: 'Edit tags for issue #101' }).click();
    await expect(page.getByRole('checkbox', { name: /^ready-for-agent/ })).toBeChecked();
    expect(labelReads).toBe(1);
    await page.getByRole('checkbox', { name: 'needs-triage' }).click();
    await expect(page.locator('[data-issue-label="needs-triage"]')).toHaveCount(0);
    await page.getByRole('checkbox', { name: 'needs-triage' }).click();
    await expect(page.locator('[data-issue-label="needs-triage"]')).toBeVisible();
    await page.getByRole('checkbox', { name: 'team/ui' }).click();
    await expect(page.locator('[data-issue-row="101"]').getByRole('alert')).toContainText('GitHub denied permission');
    await page.keyboard.press('Escape');
    await expect(page.locator('[data-issue-row="101"]').getByRole('alert')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Edit tags for issue #101' })).toBeFocused();
    await page.getByRole('button', { name: 'Edit tags for issue #101' }).click();
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await page.keyboard.press('End');
    const bounds = await page.locator('[data-issue-row="101"]').evaluate(row => {
      const rect = row.getBoundingClientRect();
      const escaped = [...row.querySelectorAll('button, input, label')].filter(control => {
        const bounds = control.getBoundingClientRect();
        return bounds.left < rect.left || bounds.right > rect.right;
      }).map(control => control.outerHTML);
      return { width: rect.width, scroll: row.scrollWidth, client: row.clientWidth, escaped };
    });
    expect(bounds.width).toBeLessThanOrEqual(240);
    expect(bounds.scroll).toBeLessThanOrEqual(bounds.client);
    expect(bounds.escaped).toEqual([]);
    await page.locator('[data-issue-row="101"]').screenshot({ path: '.tmp/issue-tags-240-after.png' });
    await page.getByRole('checkbox', { name: 'needs-triage' }).click();
    await expect(page.locator('[data-issue-label="needs-triage"]')).toHaveCount(0);
    await page.getByRole('checkbox', { name: 'needs-triage' }).click();
    await expect(page.locator('[data-issue-label="needs-triage"]')).toBeVisible();
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await page.keyboard.press('End');
    await page.keyboard.press('PageUp');
    for (let step = 0; step < 5; step++) await page.keyboard.press('ArrowLeft');
    await page.keyboard.press('Escape');
    await page.locator('[data-issue-row="101"]').screenshot({ path: '.tmp/issue-tags-after.png' });
  } finally {
    await page.unroute('http://ipc.localhost/**');
    await invoke('delete_mesh', { meshId: mesh.id });
    await page.reload({ waitUntil: 'domcontentloaded' });
  }
}
