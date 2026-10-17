/**
 * Browser check: launch/resume navigation authority on a real Back gesture
 * (issue #2327).
 *
 * Runs the mobile SPA against fixture HTTP in a real browser, holds the
 * import-and-resume response open, and drives the exact sequence the issue
 * describes: start a resume, press Back, navigate somewhere else, then let the
 * backend answer. The rendered screen must not move.
 *
 * Mock IPC only — this proves the shell's navigation and history behaviour, not
 * the backend route. Run it with:
 *   node scripts/ui-shot.mjs --mock --serve --mock-url http://127.0.0.1:3999 \
 *     --out .tmp/mobile-launch-authority.png --viewport 390x844 \
 *     --steps tests/integration/ui-shot-mobile-launch-authority.steps.mjs
 */
import { expect } from '@playwright/test';

export default async function ({ page }) {
  const meshOne = { id: 1, name: 'Phone archive', path: '/repo', created_at: '', scratchpad: '', sandbox: false };
  const meshTwo = { id: 2, name: 'Other mesh', path: '/repo2', created_at: '', scratchpad: '', sandbox: false };
  const resumedNode = { id: 99, mesh_id: 1, name: 'resumed-archive', path: '/repo/wt/resumed', branch: 'main', provider: 'anthropic', status: 'running', cli_session_id: 'abc-001', created_at: '2026-08-25T12:00:00Z' };
  const archived = { session_id: 'abc-001', first_message: 'Investigate the flaky spawn', branch: 'main', cwd: '/repo', timestamp: '2026-08-25T12:00:00Z', worktree_name: 'wt-flaky', resumable: true, provider: 'anthropic', node_id: null };

  // The resume the backend acknowledges, plus the node list that grows once the
  // import really has landed.
  let releaseResume;
  const resumePending = new Promise(resolve => { releaseResume = resolve; });
  let resumeSeen = false;
  let imported = false;
  const nodes = () => (imported ? [resumedNode] : []);

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    let body;
    if (path === '/api/session') return route.fulfill({ status: 204 });
    if (path === '/api/meshes') body = [meshOne, meshTwo];
    else if (path === '/api/nodes') body = nodes();
    else if (path === '/api/meshes/1/agent-nodes/discover') body = [archived];
    else if (path === '/api/meshes/2/agent-nodes/discover') body = [];
    else if (path === '/api/meshes/1/agent-nodes/import-and-resume') {
      // Held open: the resume is genuinely in flight while the user navigates.
      resumeSeen = true;
      await resumePending;
      imported = true;
      return route.fulfill({ json: resumedNode });
    } else return route.fulfill({ status: 503, json: { error: 'Fixture endpoint unavailable' } });
    await route.fulfill({ json: body });
  });

  const base = new URL(page.url()).origin;
  await page.goto(`${base}/mobile/index.html`, { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: 'Work', exact: true }).click();

  // Start a resume from Mesh One's Archive.
  await page.getByTestId('mesh-actions-1').click();
  await page.getByTestId('mesh-sheet-discovered-nodes').click();
  await expect(page.getByTestId('discovered-nodes-screen')).toBeVisible();
  await page.getByTestId('node-abc-001').click();
  await page.getByTestId('node-resume-abc-001').click();
  await expect.poll(() => resumeSeen, { timeout: 10_000 }).toBe(true);
  await expect(page.getByTestId('node-resume-abc-001')).toHaveText(/Resuming/);

  // Press Back while the import is still running, then deliberately open a
  // different Mesh's Archive — the user has moved on.
  await page.goBack();
  await expect(page.getByTestId('discovered-nodes-screen')).toHaveCount(0);
  await page.getByTestId('mesh-actions-2').click();
  await page.getByTestId('mesh-sheet-discovered-nodes').click();
  await expect(page.getByTestId('discovered-nodes-screen')).toBeVisible();
  await expect(page.getByTestId('node-abc-001')).toHaveCount(0);
  await page.screenshot({ path: '.tmp/mobile-launch-authority-left.png' });

  // The backend answers now. The user must stay exactly where they went.
  releaseResume();
  await page.waitForTimeout(1000);
  await expect(page.getByTestId('discovered-nodes-screen')).toBeVisible();
  await expect(page.getByText('Other mesh')).toBeVisible();
  await page.screenshot({ path: '.tmp/mobile-launch-authority-completed.png' });

  // The acknowledged work is not lost: it is discoverable from the home list's
  // own refresh, which is where a completed background operation belongs.
  await page.goBack();
  await expect(page.getByTestId('node-list')).toBeVisible();
  await expect(page.getByTestId('node-99')).toBeVisible({ timeout: 15_000 });

  console.log('Mobile launch authority browser assertions passed; the resumed node surfaced in the node list without moving the user off their chosen screen.');
}