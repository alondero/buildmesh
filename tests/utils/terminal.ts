import type { Page } from '@playwright/test';

/**
 * Get the bounding box of a terminal for a specific session.
 * Requires the terminal to have data-session-id attribute.
 */
export async function getTerminalBoundingBox(
  page: Page,
  nodeId: number
): Promise<DOMRect | null> {
  const selector = `[data-session-id="${nodeId}"] .xterm`;
  const terminal = page.locator(selector);
  const count = await terminal.count();
  if (count === 0) return null;
  return terminal.boundingBox();
}

/**
 * Get terminal element count for a specific session.
 */
export async function getTerminalCount(
  page: Page,
  nodeId?: number
): Promise<number> {
  if (nodeId !== undefined) {
    return page.locator(`[data-session-id="${nodeId}"] .xterm`).count();
  }
  return page.locator('.xterm').count();
}

/**
 * Click a session tab in the session view tab bar.
 */
export async function clickSessionTab(
  page: Page,
  nodeId: number
): Promise<void> {
  await page.click(`button[data-session-tab="${nodeId}"]`);
  // Wait for potential fit operations
  await page.waitForTimeout(50);
}

/**
 * Wait for terminal to be fitted (after potential fit-terminals event).
 */
export async function waitForTerminalFit(
  page: Page,
  nodeId: number,
  timeout: number = 500
): Promise<void> {
  await page.waitForTimeout(timeout);
  // The fit-terminals event uses 100ms + 300ms delays
  await page.waitForTimeout(400);
}

/**
 * Check if a terminal has non-minimal dimensions.
 * A "minimal" terminal is considered < 50px in either dimension.
 */
export async function hasNonMinimalDimensions(
  page: Page,
  nodeId: number
): Promise<boolean> {
  const box = await getTerminalBoundingBox(page, nodeId);
  if (!box) return false;
  return box.width > 50 && box.height > 50;
}

/**
 * Compare terminal dimensions between two points in time.
 * Returns true if dimensions are within tolerance (default 90%).
 */
export async function compareTerminalDimensions(
  page: Page,
  nodeId: number,
  baseline: DOMRect,
  tolerance: number = 0.9
): Promise<{ width: boolean; height: boolean; overall: boolean }> {
  const current = await getTerminalBoundingBox(page, nodeId);
  if (!current) return { width: false, height: false, overall: false };

  const widthOk = current.width >= baseline.width * tolerance;
  const heightOk = current.height >= baseline.height * tolerance;

  return {
    width: widthOk,
    height: heightOk,
    overall: widthOk && heightOk,
  };
}

/**
 * Get all session tab buttons from the tab bar.
 */
export async function getSessionTabs(page: Page): Promise<number[]> {
  const tabs = page.locator('[data-session-tab]');
  const count = await tabs.count();
  const ids: number[] = [];
  for (let i = 0; i < count; i++) {
    const id = await tabs.nth(i).getAttribute('data-session-tab');
    if (id) ids.push(parseInt(id, 10));
  }
  return ids;
}
