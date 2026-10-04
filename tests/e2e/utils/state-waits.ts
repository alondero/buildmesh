import { expect, Locator, Page } from '@playwright/test';

/**
 * Wait for the React shell to mount by polling `img[alt="Buildmesh"]`.
 * Replaces `await page.goto('/'); await page.waitForTimeout(1000);` in
 * the `beforeEach` of every desktop spec.
 */
export async function waitForAppBoot(page: Page, timeoutMs = 15000): Promise<void> {
  await expect(page.locator('img[alt="Buildmesh"]'), 'Buildmesh logo should be visible once the React shell mounts').toBeVisible({ timeout: timeoutMs });
}

/**
 * Expand every collapsed mesh card in the sidebar.
 *
 * The mesh row is a self-contained card whose agent-node list starts
 * COLLAPSED: only a mesh with a node needing attention (or in error) starts
 * expanded, and the status-dots subtitle line toggles the rest. A spec that
 * drives a node row must therefore make that click first — the same gesture
 * a user makes. Already-expanded cards carry the `Hide agents for …` label,
 * so they never match and are left alone; safe to call again after new
 * nodes arrive.
 */
export async function expandMeshCards(page: Page): Promise<void> {
  const collapsed = page.getByRole('button', { name: /^Show agents for / });
  // Each click flips one card's label Show -> Hide, so the match list
  // shrinks. Bounded to keep a detached/stale locator from spinning.
  for (let guard = 0; guard < 50 && (await collapsed.count()) > 0; guard++) {
    await collapsed.first().click();
  }
}

/**
 * Wait until the supplied xterm `Locator` finishes its post-click layout
 * pass. Polls the bounding box until two consecutive identical snapshots
 * land, which is the real "fit done" signal that the previous
 * `waitForTerminalFit` approximated with a hard `waitForTimeout`.
 *
 * Takes a Locator (not an index) so callers can target by entity ID via
 * `[data-node-id="${id}"] .xterm` instead of mixing primary-key values
 * with 0-based DOM indices — nth() expects the latter, session IDs are
 * the former.
 */
export async function waitForTerminalFit(xterm: Locator, timeoutMs = 5000): Promise<void> {
  await expect(xterm, 'xterm should be visible before fitting').toBeVisible({ timeout: timeoutMs });
  let lastBox: { width: number; height: number } | null = null;
  await expect
    .poll(
      async () => {
        const box = await xterm.boundingBox();
        if (!box || box.width <= 0 || box.height <= 0) return null;
        const snapshot = { width: Math.round(box.width), height: Math.round(box.height) };
        if (lastBox && lastBox.width === snapshot.width && lastBox.height === snapshot.height) {
          return snapshot;
        }
        lastBox = snapshot;
        return null;
      },
      { timeout: timeoutMs, intervals: [50, 100, 200], message: 'xterm bounding box should stabilise after a layout pass' },
    )
    .not.toBeNull();
}