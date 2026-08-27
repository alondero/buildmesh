/**
 * E2E Test for Project Switching Bug
 *
 * Tests that terminals remain visible when switching between projects via
 * clicking project titles in the sidebar.
 *
 * Bug: When switching back to a project, terminals appear blank/invisible
 * because FitAddon.proposeDimensions() returns null.
 */
import { test, expect, Page } from '@playwright/test';
import { waitForTauriReady, createTestSessionViaHttp, cleanupTestProjects, invokeViaHttp } from './utils/tauri-http';
import { waitForAppBoot } from './utils/state-waits';

async function clickProjectByName(page: Page, projectName: string) {
  await page.locator(`text="${projectName}"`).first().click();
}

async function setActiveSession(nodeId: number) {
  await invokeViaHttp('set_active_node', { nodeId });
}

test.describe('project switching terminal visibility', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
      return;
    }

    await waitForAppBoot(page);
  });

  test.afterEach(async () => {
    await cleanupTestProjects();
  });

  test('terminal remains visible after switching between projects', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await expect(page.locator('[data-session-item]'), 'session 1 row should appear after WS push').toHaveCount(1, { timeout: 10000 });

    await createTestSessionViaHttp(2);
    await expect(page.locator('[data-session-item]'), 'session 2 row should appear after WS push').toHaveCount(2, { timeout: 10000 });

    const sessions = await invokeViaHttp<any[]>('list_agent_nodes', {});

    console.log(`Found ${sessions?.length || 0} sessions in store`);

    if (sessions && sessions.length > 0) {
      await setActiveSession(sessions[0].id);
      await expect(page.locator('.xterm').first(), 'terminal should mount after set_active_node').toBeVisible({ timeout: 10000 });
    }

    const nodeIds = sessions.map((s: any) => s.id);
    await page.evaluate(async (ids: number[]) => {
      const tm = (window as any).__terminalManager;
      if (!tm || !ids || ids.length === 0) return;

      const inst = await (tm as any).getOrCreate(ids[0]);
      if (inst) {
        inst.term.write('$ ');
        inst.term.write('Initial terminal output\r\n');
        inst.term.write('Session active\r\n');
        inst.term.write('\x1b[32m✓ Ready\x1b[0m\r\n');
      }
    }, nodeIds);

    const initialXterm = page.locator('.xterm').first();
    await expect(initialXterm, 'initial terminal should be visible').toBeVisible({ timeout: 5000 });
    const initialBox = await initialXterm.boundingBox();
    console.log(`Initial dimensions: ${initialBox?.width}x${initialBox?.height}`);

    expect(initialBox?.width, 'Terminal should have width').toBeGreaterThan(50);
    expect(initialBox?.height, 'Terminal should have height').toBeGreaterThan(50);

    console.log('Clicking Project 1 title...');
    await clickProjectByName(page, 'Test Project 1');
    await expect(page.locator('.xterm').first(), 'terminal should still be visible after switching to Project 1').toBeVisible({ timeout: 5000 });

    console.log('Clicking Project 2 title...');
    await clickProjectByName(page, 'Test Project 2');
    await expect(page.locator('.xterm').first(), 'terminal should still be visible after switching to Project 2').toBeVisible({ timeout: 5000 });

    console.log('Switching back to Project 1...');
    await clickProjectByName(page, 'Test Project 1');
    await expect(page.locator('.xterm').first(), 'Terminal should remain visible after switching back to original project').toBeVisible({ timeout: 10000 });

    const finalBox = await page.locator('.xterm').first().boundingBox();
    console.log(`Final dimensions: ${finalBox?.width}x${finalBox?.height}`);
    expect(finalBox?.width, 'Terminal width should remain valid').toBeGreaterThan(50);
    expect(finalBox?.height, 'Terminal height should remain valid').toBeGreaterThan(50);
  });

  test('xterm canvas exists and has non-zero dimensions after project switch', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(3);
    await expect(page.locator('[data-session-item]'), 'sidebar should hold 2 session rows').toHaveCount(2, { timeout: 10000 });

    const initialXterm = page.locator('.xterm').first();
    await expect(initialXterm, 'initial xterm should be visible').toBeVisible({ timeout: 10000 });
    const initialBox = await initialXterm.boundingBox();
    expect(initialBox?.width).toBeGreaterThan(50);
    expect(initialBox?.height).toBeGreaterThan(50);
    console.log(`Initial terminal: ${initialBox?.width}x${initialBox?.height}`);

    await clickProjectByName(page, 'Test Project 1');
    await expect(page.locator('.xterm').first(), 'xterm should be visible after switching to Project 1').toBeVisible({ timeout: 5000 });

    await clickProjectByName(page, 'Test Project 3');
    await expect(page.locator('.xterm').first(), 'xterm should be visible after switching to Project 3').toBeVisible({ timeout: 5000 });

    await clickProjectByName(page, 'Test Project 1');
    await expect(page.locator('.xterm').first(), 'xterm should be visible after switching back to Project 1').toBeVisible({ timeout: 5000 });

    const xtermCanvas = page.locator('.xterm canvas').first();
    await expect(xtermCanvas, 'xterm canvas should be visible').toBeVisible({ timeout: 5000 });
    const canvasBox = await xtermCanvas.boundingBox();
    console.log(`Canvas dimensions: ${canvasBox?.width}x${canvasBox?.height}`);

    expect(canvasBox?.width, 'Canvas should have non-zero width').toBeGreaterThan(0);
    expect(canvasBox?.height, 'Canvas should have non-zero height').toBeGreaterThan(0);
  });

  test('terminal proposeDimensions should not be null after project switch', async ({ page }) => {
    const consoleLogs: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'log') consoleLogs.push(msg.text());
    });

    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(3);
    await expect(page.locator('[data-session-item]'), 'sidebar should hold 2 session rows').toHaveCount(2, { timeout: 10000 });

    await clickProjectByName(page, 'Test Project 1');
    await expect(page.locator('.xterm').first(), 'xterm visible after switch to Project 1').toBeVisible({ timeout: 5000 });

    await clickProjectByName(page, 'Test Project 3');
    await expect(page.locator('.xterm').first(), 'xterm visible after switch to Project 3').toBeVisible({ timeout: 5000 });

    await clickProjectByName(page, 'Test Project 1');
    await expect(page.locator('.xterm').first(), 'xterm visible after switch back to Project 1').toBeVisible({ timeout: 5000 });

    const proposeLogs = consoleLogs.filter(l => l.includes('proposeDimensions'));
    console.log('All proposeDimensions logs:');
    proposeLogs.forEach(l => console.log('  ', l));

    const returningExisting = proposeLogs.filter(l => l.includes('returning existing'));
    console.log(`Logs with 'returning existing': ${returningExisting.length}`);

    const nullProposeLogs = proposeLogs.filter(l => l.includes('null'));
    if (nullProposeLogs.length > 0) {
      console.log('NULL proposeDimensions logs found (BUG):');
      nullProposeLogs.forEach(l => console.log('  ', l));
    }

    const xterm = page.locator('.xterm').first();
    await expect(xterm, 'Xterm should be visible after project switch').toBeVisible({ timeout: 5000 });

    const box = await xterm.boundingBox();
    expect(box?.width, 'Xterm should have non-zero width').toBeGreaterThan(10);
    expect(box?.height, 'Xterm should have non-zero height').toBeGreaterThan(10);
  });
});
