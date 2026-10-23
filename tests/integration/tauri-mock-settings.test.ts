import { createServer } from 'node:net';
import { mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import { chromium } from 'playwright';
import { expect as browserExpect } from '@playwright/test';
import { expect, it } from 'vitest';
import { buildInitScript } from '../../scripts/ui-mock/tauri-mock.mjs';
import { collectPageErrors } from '../../scripts/ui-shot-errors.mjs';
import { startDevServer, stopDevServer } from '../../scripts/ui-shot-server.mjs';
import { closeBrowser } from '../../scripts/ui-shot-close.mjs';

it('renders Settings and prompt defaults in Chromium with the default mock IPC', async () => {
  const reservation = createServer();
  await new Promise<void>(done => reservation.listen(0, '127.0.0.1', done));
  const address = reservation.address();
  if (!address || typeof address === 'string') throw new Error('No port reserved');
  const url = `http://127.0.0.1:${address.port}`;
  await new Promise<void>((done, reject) => reservation.close(error => error ? reject(error) : done()));
  const server = await startDevServer(url);
  let browser;
  try {
    browser = await chromium.launch();
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    page.setDefaultTimeout(15000);
    await page.addInitScript(buildInitScript());
    const diagnostics = collectPageErrors(page);
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    // At 1440px Settings is folded into the More panel (#2421); open it there.
    await page.getByRole('button', { name: 'More tools', exact: true }).click();
    await page.getByTestId('titlebar-overflow-settings').click();
    const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
    await browserExpect(dialog.getByRole('checkbox', { name: 'Confirm before quitting' })).toBeChecked();
    await dialog.getByRole('tab', { name: 'Launch Configurations', exact: true }).click();
    await dialog.getByRole('textbox', { name: 'Claude default model', exact: true }).waitFor({ state: 'visible' });
    await dialog.getByRole('tab', { name: 'Prompts', exact: true }).click();
    const issue = dialog.getByTestId('probe-prompt-input-issue');
    await browserExpect(issue).toBeEnabled();
    await browserExpect(issue).toHaveValue(/\{\{url\}\}/);
    const pr = dialog.getByTestId('probe-prompt-input-pr');
    await browserExpect(pr).toBeEnabled();
    await browserExpect(pr).toHaveValue(/\{\{policy\}\}/);
    await diagnostics.assertHealthy();
    const artifacts = resolve(import.meta.dirname, '../../.tmp/ui-shot-errors');
    await mkdir(artifacts, { recursive: true });
    await page.screenshot({ path: resolve(artifacts, 'default-settings.png') });
    // Prove the same collector was armed during the successful render.
    await page.evaluate(() => console.error('settings-collector-control'));
    await expect(diagnostics.assertHealthy()).rejects.toThrow('settings-collector-control');
  } finally {
    if (browser) await closeBrowser(browser, { timeoutMs: 15000, graceMs: 15000 });
    if (server) await stopDevServer(server);
  }
}, 120000);
