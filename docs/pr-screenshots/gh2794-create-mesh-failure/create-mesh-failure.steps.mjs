/**
 * The New mesh dialog after a failed repository creation — issue #2794.
 *
 * The dialog used to answer a failed creation with "Failed to create mesh.
 * See logs for details." This run drives the real frontend in mock-IPC mode
 * with a distinctive rejection, and checks that:
 *
 *   1. the backend's own message reaches the dialog (not the generic copy),
 *   2. the recovery the cause supports is offered beside the message,
 *   3. the picked folder and the colour survive the failure,
 *   4. the keyboard path works end to end (Tab + Enter, no mouse),
 *   5. focus stays INSIDE the dialog after the failure, and
 *   6. a long message wraps instead of being clipped.
 *
 * The same run captures the dialog in four presentations — dark and light,
 * at the default interface text size and at the 150% size the Settings →
 * General → Appearance slider reaches — because the failure panel carries a
 * long, unbreakable-ish message and a control row.
 *
 * Mock-IPC mode: this proves the frontend renders and reacts. It does NOT
 * prove backend behaviour — the rejection here is a fixture, not a real
 * filesystem or database failure.
 */

import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';

const OUT_DIR = resolve(process.cwd(), 'docs/pr-screenshots/gh2794-create-mesh-failure');

/**
 * A long, distinctive rejection. It carries the phrases the classifier reads
 * ("access denied") and is long enough to expose a wrapping or clipping
 * problem that a short message would hide.
 */
const ACCESS_DENIED =
  'Access denied creating repository workspace: the folder D:\\repos\\buildmesh is on a share ' +
  'that this account cannot write to, so Buildmesh could not register the repository as a Mesh ' +
  '(os error 5)';

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

/** Apply the theme and interface text size the way the app's own setters do. */
async function applyPresentation(page, theme, textScale) {
  await page.evaluate(([t, s]) => {
    document.documentElement.setAttribute('data-theme', t);
    document.documentElement.style.setProperty('--ui-text-scale', String(s));
  }, [theme, textScale]);
  await page.waitForTimeout(150);
}

/**
 * Tab forward until the focused element satisfies `test`, and report how many
 * presses it took. The whole run stays on the keyboard: no click is used to
 * reach the submit control.
 */
async function tabUntil(page, test, max = 40) {
  for (let i = 0; i < max; i += 1) {
    const label = await page.evaluate(() => {
      const el = document.activeElement;
      return el ? (el.getAttribute('aria-label') || el.textContent || '').trim() : '';
    });
    if (test(label)) return i;
    await page.keyboard.press('Tab');
  }
  throw new Error('the keyboard never reached the expected control');
}

export default async function ({ page, mock }) {
  mkdirSync(OUT_DIR, { recursive: true });

  // The mock's own unmocked-command notice is a debug line, not a defect.
  await mock.on('log_frontend', null);
  await page.evaluate(() => { window.__BUILDMESH_MOCK__.quiet = true; });
  await page.evaluate((message) => {
    window.__BUILDMESH_MOCK__.on('pick_mesh_folder', () => ({
      path: 'D:\\repos\\buildmesh',
      name: 'buildmesh',
    }));
    window.__BUILDMESH_MOCK__.on('create_mesh', () => {
      throw new Error(message);
    });
  }, ACCESS_DENIED);

  // Open the dialog from the sidebar.
  await page.getByRole('button', { name: 'New mesh' }).click();
  await page.getByRole('dialog').waitFor();

  // Keyboard only from here: the dialog opens with the folder picker focused
  // (issue #2463), so Enter opens the picker; then Tab reaches the submit
  // control and Enter submits it.
  const openingFocus = await page.evaluate(() => (document.activeElement?.textContent || '').trim());
  assert(/Choose folder/i.test(openingFocus), `the dialog must open on the folder picker, got: ${openingFocus}`);
  await page.keyboard.press('Enter');
  await page.getByText('D:\\repos\\buildmesh', { exact: true }).waitFor();

  const tabs = await tabUntil(page, (label) => label === 'Create mesh');
  assert(tabs > 0, 'the submit control must be reachable by Tab');
  await page.keyboard.press('Enter');

  // The failure surface.
  const panel = page.getByTestId('mesh-create-failure');
  await panel.waitFor();

  const message = await page.getByTestId('mesh-create-failure-message').innerText();
  assert(message.includes('Access denied creating repository workspace'), `the backend message must reach the dialog, got: ${message}`);
  const body = await page.locator('body').innerText();
  assert(!/see logs for details/i.test(body), 'the generic logs message must not replace the real cause');
  assert(!/failed to create mesh/i.test(body), 'the generic failure copy must not replace the real cause');

  // The recovery the cause supports, and only that one.
  assert(await page.getByTestId('mesh-create-change-folder').isVisible(), 'an access failure must offer another folder');
  assert(!(await page.getByTestId('mesh-create-retry').count()), 'an access failure must not offer a retry');

  // The draft survives: the folder and the colour picker are still there.
  assert(await page.getByText('D:\\repos\\buildmesh', { exact: true }).isVisible(), 'the picked folder must survive the failure');
  assert(await page.getByText('Colour').isVisible(), 'the colour control must survive the failure');

  // Focus stayed inside the dialog.
  const focusInside = await page.evaluate(() => {
    const dialog = document.querySelector('[role="dialog"]');
    return !!dialog && dialog.contains(document.activeElement);
  });
  assert(focusInside, 'focus must stay inside the dialog after the failure');

  // The long message wraps rather than being clipped.
  const clipped = await page.evaluate(() => {
    const el = document.querySelector('[data-testid="mesh-create-failure-message"]');
    if (!el) return true;
    return el.scrollWidth > el.clientWidth + 1;
  });
  assert(!clipped, 'a long failure message must wrap, not be clipped horizontally');

  // Four presentations of the same failure.
  await applyPresentation(page, 'dark', 1);
  await page.screenshot({ path: resolve(OUT_DIR, 'dark-100.png') });
  await applyPresentation(page, 'light', 1);
  await page.screenshot({ path: resolve(OUT_DIR, 'light-100.png') });
  await applyPresentation(page, 'dark', 1.5);
  await page.screenshot({ path: resolve(OUT_DIR, 'dark-150.png') });
  await applyPresentation(page, 'light', 1.5);
  await page.screenshot({ path: resolve(OUT_DIR, 'light-150.png') });

  // The expanded diagnostics carry the operation, the folder and the cause.
  await page.getByRole('group').getByText('Details', { exact: true }).click();
  const detail = await page.getByTestId('mesh-create-failure-detail').innerText();
  assert(detail.includes('Folder: D:\\repos\\buildmesh'), 'the diagnostic block must name the folder');
  assert(detail.includes('Cause: Access denied'), 'the diagnostic block must carry the cause');
  await applyPresentation(page, 'dark', 1);
  await page.screenshot({ path: resolve(OUT_DIR, 'dark-100-details.png') });
}
