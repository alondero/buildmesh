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

async function clickProjectByName(page: Page, projectName: string) {
  const projectTitle = page.locator(`text="${projectName}"`).first();
  if (await projectTitle.isVisible()) {
    await projectTitle.click();
    await page.waitForTimeout(300);
  }
}

// Use HTTP test server to set active session
async function setActiveSession(page: Page, nodeId: number) {
  // Use invokeViaHttp which works via fetch to 127.0.0.1:1991
  // Pass as 'nodeId' not 'id' to match the test server handler
  await invokeViaHttp('set_active_node', { nodeId });
}

test.describe('project switching terminal visibility', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }
  });

  test.afterEach(async () => {
    await cleanupTestProjects();
  });

  test('terminal remains visible after switching between projects', async ({ page }) => {
    // Create sessions in Project A
    const proj1 = await createTestSessionViaHttp(1); // Creates "Test Project 1" with Session 1
    await page.waitForTimeout(1000); // Wait for session-created event to be processed

    // Create sessions in Project B
    const proj2 = await createTestSessionViaHttp(2); // Creates "Test Project 2" with Session 2
    await page.waitForTimeout(1000);

    // Get session IDs via the HTTP test server (same as createTestSessionViaHttp uses)
    const sessions = await invokeViaHttp<any[]>('list_agent_nodes', {});

    console.log(`Found ${sessions?.length || 0} sessions in store`);

    if (sessions && sessions.length > 0) {
      // Set the first session as active (triggers terminal visibility)
      // sessions is an array of session objects with .id field
      await setActiveSession(page, sessions[0].id);
      await page.waitForTimeout(1000);
    }

    // Now inject fake terminal content via window.__terminalManager
    // Extract just the IDs for the terminal manager
    const nodeIds = sessions.map((s: any) => s.id);
    await page.evaluate(async (ids: number[]) => {
      const tm = (window as any).__terminalManager;
      if (!tm || !ids || ids.length === 0) return;

      // Write fake terminal content to the first session
      const inst = await (tm as any).getOrCreate(ids[0]);
      if (inst) {
        inst.term.write('$ ');
        inst.term.write('Initial terminal output\r\n');
        inst.term.write('Session active\r\n');
        inst.term.write('\x1b[32m✓ Ready\x1b[0m\r\n');
      }
    }, nodeIds);

    // Get initial terminal visibility and dimensions
    const initialXterm = page.locator('.xterm').first();
    const initialVisible = await initialXterm.isVisible();
    console.log(`Initial terminal visible: ${initialVisible}`);

    const initialBox = initialVisible ? await initialXterm.boundingBox() : null;
    console.log(`Initial dimensions: ${initialBox?.width}x${initialBox?.height}`);

    expect(initialVisible, 'Initial terminal should be visible').toBe(true);
    if (initialBox) {
      expect(initialBox.width, 'Terminal should have width').toBeGreaterThan(50);
      expect(initialBox.height, 'Terminal should have height').toBeGreaterThan(50);
    }

    // Click Project 1 title to filter to Project 1
    console.log('Clicking Project 1 title...');
    await clickProjectByName(page, 'Test Project 1');
    await page.waitForTimeout(500);

    const project1Visible = await page.locator('.xterm').first().isVisible();
    console.log(`After switch to Project 1, terminal visible: ${project1Visible}`);

    // Click Project 2 title to filter to Project 2
    console.log('Clicking Project 2 title...');
    await clickProjectByName(page, 'Test Project 2');
    await page.waitForTimeout(500);

    const project2Visible = await page.locator('.xterm').first().isVisible();
    console.log(`After switch to Project 2, terminal visible: ${project2Visible}`);

    // Click Project 1 title again to switch back
    console.log('Switching back to Project 1...');
    await clickProjectByName(page, 'Test Project 1');
    await page.waitForTimeout(1000);

    // This is the bug - terminal should be visible but often isn't
    const backToProject1Visible = await page.locator('.xterm').first().isVisible();
    console.log(`After switch back to Project 1, terminal visible: ${backToProject1Visible}`);

    expect(backToProject1Visible, 'Terminal should remain visible after switching back to original project').toBe(true);

    // Also check dimensions are still valid
    const finalBox = await page.locator('.xterm').first().boundingBox();
    console.log(`Final dimensions: ${finalBox?.width}x${finalBox?.height}`);
    if (finalBox) {
      expect(finalBox.width, 'Terminal width should remain valid').toBeGreaterThan(50);
      expect(finalBox.height, 'Terminal height should remain valid').toBeGreaterThan(50);
    }
  });

  test('xterm canvas exists and has non-zero dimensions after project switch', async ({ page }) => {
    const consoleLogs: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'log') consoleLogs.push(msg.text());
    });

    // Create sessions in two different projects
    await createTestSessionViaHttp(1); // Project 1
    await createTestSessionViaHttp(3); // Project 3
    await page.waitForTimeout(800);

    // Get initial terminal state
    const initialXterm = page.locator('.xterm').first();
    expect(await initialXterm.isVisible()).toBe(true);
    const initialBox = await initialXterm.boundingBox();
    expect(initialBox?.width).toBeGreaterThan(50);
    expect(initialBox?.height).toBeGreaterThan(50);
    console.log(`Initial terminal: ${initialBox?.width}x${initialBox?.height}`);

    // Switch to Project 1
    await clickProjectByName(page, 'Test Project 1');
    await page.waitForTimeout(500);

    // Switch to Project 3
    await clickProjectByName(page, 'Test Project 3');
    await page.waitForTimeout(500);

    // Switch back to Project 1
    await clickProjectByName(page, 'Test Project 1');
    await page.waitForTimeout(1000);

    // Verify xterm canvas element exists and has valid dimensions
    const xtermCanvas = page.locator('.xterm canvas').first();
    const canvasVisible = await xtermCanvas.isVisible();
    console.log(`Xterm canvas visible after switch: ${canvasVisible}`);

    const canvasBox = canvasVisible ? await xtermCanvas.boundingBox() : null;
    console.log(`Canvas dimensions: ${canvasBox?.width}x${canvasBox?.height}`);

    // The terminal div should be visible
    const terminalDiv = page.locator('.xterm').first();
    expect(await terminalDiv.isVisible(), 'Xterm div should be visible').toBe(true);

    // The canvas should have non-zero dimensions
    expect(canvasBox?.width, 'Canvas should have non-zero width').toBeGreaterThan(0);
    expect(canvasBox?.height, 'Canvas should have non-zero height').toBeGreaterThan(0);
  });

  test('terminal proposeDimensions should not be null after project switch', async ({ page }) => {
    const consoleLogs: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'log') consoleLogs.push(msg.text());
    });

    // Create sessions in two projects
    await createTestSessionViaHttp(1); // Project 1
    await createTestSessionViaHttp(3); // Project 3
    await page.waitForTimeout(800);

    // Perform project switches
    await clickProjectByName(page, 'Test Project 1');
    await page.waitForTimeout(500);
    await clickProjectByName(page, 'Test Project 3');
    await page.waitForTimeout(500);
    await clickProjectByName(page, 'Test Project 1');
    await page.waitForTimeout(1000);

    // Check proposeDimensions from console logs
    const proposeLogs = consoleLogs.filter(l => l.includes('proposeDimensions'));
    console.log('All proposeDimensions logs:');
    proposeLogs.forEach(l => console.log('  ', l));

    // Find logs after "returning existing" (post-switch-back)
    const returningExisting = proposeLogs.filter(l => l.includes('returning existing'));
    console.log(`Logs with 'returning existing': ${returningExisting.length}`);

    // The critical check: after switch-back, proposeDimensions should NOT be null
    // The bug manifests as: proposeDimensions={"cols":null,"rows":null} after switch-back
    const nullProposeLogs = proposeLogs.filter(l => l.includes('null'));

    if (nullProposeLogs.length > 0) {
      console.log('NULL proposeDimensions logs found (BUG):');
      nullProposeLogs.forEach(l => console.log('  ', l));
    }

    // Terminal must be visible after the switch
    const xterm = page.locator('.xterm').first();
    const isVisible = await xterm.isVisible();
    expect(isVisible, 'Xterm should be visible after project switch').toBe(true);

    // Terminal must have valid bounding box
    const box = await xterm.boundingBox();
    expect(box?.width, 'Xterm should have non-zero width').toBeGreaterThan(10);
    expect(box?.height, 'Xterm should have non-zero height').toBeGreaterThan(10);
  });
});
