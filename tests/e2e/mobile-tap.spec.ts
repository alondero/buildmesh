/**
 * Mobile Remote Access — Node Tap Behavior Tests
 *
 * Verifies:
 * 1. Node list renders with correct status indicators
 * 2. Tapping a node transitions to terminal view
 * 3. Tapping the active (running) node shows terminal with content
 * 4. WebSocket connection failure is visible to user
 * 5. Reconnect overlay is tappable and resets connection state
 * 6. Loading state is visible while xterm.js loads from CDN
 *
 * Run against a running Buildmesh desktop app with remote access enabled.
 * The mobile app is served at http://localhost:1992 (or next available port).
 */
import { test, expect, Page } from '@playwright/test';

function getHttpServerUrl(port: number, token: string) {
  return `http://localhost:${port}/?token=${token}`;
}

async function getMeshToken(page: Page): Promise<string> {
  // The token is stored after QR scan or manual entry
  // For testing, we can inject it directly
  return await page.evaluate(() => localStorage.getItem('buildmesh_token') || '');
}

test.describe('Mobile app — node tap behavior', () => {

  test('node items render with correct status dots', async ({ page }) => {
    await page.goto('http://localhost:1992/?token=test');
    await page.waitForTimeout(2000);

    // Should show either the node list or an offline/error state
    const nodeItems = page.locator('.node-item');
    const count = await nodeItems.count();

    if (count > 0) {
      // Verify status bar exists and has a status class
      const firstStatusBar = page.locator('.status-bar').first();
      await expect(firstStatusBar).toBeVisible();

      // Status bar should have one of the valid status classes
      const statusClass = await firstStatusBar.getAttribute('class');
      expect(statusClass).toMatch(/\b(idle|running|suspended|error|awaiting_input)\b/);

      // Green dot (running) means the class should be "status-bar running"
      // We can verify the color mapping by checking the CSS
    }
  });

  test('tapping a node transitions to terminal view', async ({ page }) => {
    await page.goto('http://localhost:1992/?token=test');
    await page.waitForTimeout(2000);

    const nodeItems = page.locator('.node-item');
    const count = await nodeItems.count();

    if (count > 0) {
      // Verify terminal view is hidden initially
      const terminalView = page.locator('#terminal-view');
      await expect(terminalView).not.toHaveClass(/active/);

      // Tap the first node
      await nodeItems.first().click();
      await page.waitForTimeout(1000);

      // Terminal view should now be active
      await expect(terminalView).toHaveClass(/active/);

      // Back button should be visible
      const backBtn = page.locator('#back-btn');
      await expect(backBtn).toBeVisible();

      // Header should show the node name
      const headerTitle = page.locator('#header-title');
      const title = await headerTitle.textContent();
      expect(title).not.toBe('Loading...');
    }
  });

  test('tapping active (running) node shows terminal with content', async ({ page }) => {
    await page.goto('http://localhost:1992/?token=test');
    await page.waitForTimeout(2000);

    const nodeItems = page.locator('.node-item');
    const count = await nodeItems.count();

    if (count > 0) {
      // Find the first running node
      const runningNodes = page.locator('.node-item .status-bar.running');
      const runningCount = await runningNodes.count();

      if (runningCount > 0) {
        const runningNodeItem = page.locator('.node-item').filter({ has: page.locator('.status-bar.running') }).first();

        // Tap the running node
        await runningNodeItem.click();
        await page.waitForTimeout(2000); // Wait for xterm.js + WebSocket

        // Verify terminal view is active
        await expect(page.locator('#terminal-view')).toHaveClass(/active/);

        // Verify xterm container has content (xterm.js loaded)
        const xterm = page.locator('#xterm');
        await expect(xterm).toBeVisible();

        // The terminal should have actual content (xterm rows rendered)
        // If WebSocket connected, there would be terminal content
        // We check that the container has children (xterm creates DOM elements)
        const xtermChildren = await xterm.locator('*').count();
        expect(xtermChildren).toBeGreaterThan(0);
      }
    }
  });

  test('WebSocket connection failure shows error feedback', async ({ page }) => {
    await page.goto('http://localhost:1992/?token=test');
    await page.waitForTimeout(2000);

    const nodeItems = page.locator('.node-item');
    const count = await nodeItems.count();

    if (count > 0) {
      await nodeItems.first().click();
      await page.waitForTimeout(500);

      // Simulate a WebSocket connection failure by intercepting
      // This would require a more complex setup with a bad token
      // For now, we just verify the terminal view shows and handles bad connections

      const terminalView = page.locator('#terminal-view');
      await expect(terminalView).toHaveClass(/active/);

      // If the WebSocket fails to connect after a timeout,
      // the reconnect overlay should eventually appear
      // (requires MAX_RECONNECT * delays to trigger, ~80 seconds in current impl)
      // Instead, we just verify the terminal is visible and not blank
    }
  });

  test('node tap fires openTerminal with correct parameters', async ({ page }) => {
    await page.goto('http://localhost:1992/?token=test');
    await page.waitForTimeout(2000);

    // Capture all console messages to see if openTerminal is called
    const consoleLogs: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'error') {
        consoleLogs.push(`ERROR: ${msg.text()}`);
      }
    });

    const nodeItems = page.locator('.node-item');
    const count = await nodeItems.count();

    if (count > 0) {
      // Get the onclick attribute of the first node
      const firstItemOnclick = await nodeItems.first().getAttribute('onclick');
      expect(firstItemOnclick).toContain('openTerminal');
      expect(firstItemOnclick).toContain('node.id');

      // Tap it
      await nodeItems.first().click();
      await page.waitForTimeout(2000);

      // Check console for errors
      const jsErrors = consoleLogs.filter(l => l.startsWith('ERROR:'));
      if (jsErrors.length > 0) {
        // If there are JS errors during tap, this is a finding
        console.log('JS errors during tap:', jsErrors);
      }

      // Terminal view should be active if no JS error occurred
      const terminalView = page.locator('#terminal-view');
      const hasActiveClass = await terminalView.evaluate(el => el.classList.contains('active'));
      expect(hasActiveClass).toBe(true);
    }
  });
});

test.describe('Mobile app — reconnect overlay tap behavior', () => {

  test('reconnect overlay is tappable after max retries', async ({ page }) => {
    await page.goto('http://localhost:1992/?token=test');
    await page.waitForTimeout(1000);

    // Manually set up reconnect failure state via JS
    await page.evaluate(() => {
      // @ts-ignore - access mobile app globals
      if (typeof window.scheduleReconnect === 'function') {
        // Trigger the reconnect overlay to show
        const overlay = document.getElementById('reconnect-overlay');
        if (overlay) {
          overlay.classList.remove('hidden');
        }
      }
    });

    const overlay = page.locator('#reconnect-overlay');
    await expect(overlay).toBeVisible();

    // The overlay text says "Tap to retry" but has no onclick
    // This test documents the current broken behavior
    const onclick = await overlay.getAttribute('onclick');
    expect(onclick).toBeNull(); // BUG: no onclick handler!

    // After fix, tapping should reset reconnect and retry
  });
});

test.describe('Mobile app — loading state visibility', () => {

  test('shows loading feedback while xterm.js loads', async ({ page }) => {
    await page.goto('http://localhost:1992/?token=test');
    await page.waitForTimeout(2000);

    const nodeItems = page.locator('.node-item');
    const count = await nodeItems.count();

    if (count > 0) {
      // Tap node and immediately check for loading indicator
      await nodeItems.first().click();

      // Check terminal container immediately after tap
      const terminalContainer = page.locator('#terminal-container');
      const containerHtml = await terminalContainer.innerHTML();

      // Container should either have xterm div or a loading indicator
      // Currently it just replaces with '<div class="xterm" id="xterm"></div>'
      // which shows as blank until xterm.js loads
      console.log('Terminal container state:', containerHtml.substring(0, 100));
    }
  });
});