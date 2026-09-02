#!/usr/bin/env node
/**
 * Interactive launcher for the throwaway Probe #1375 prototypes.
 *
 * Starts the frontend dev server, injects the repo's mock Tauri bridge, and
 * opens a headed Chromium window at `?variant=A`. Use left/right arrow keys or the floating
 * switcher to compare the five layouts. Close the browser to stop the
 * temporary server when this script started it.
 */

import { chromium } from 'playwright';
import { spawn } from 'child_process';
import { buildInitScript, loadFixtures } from './ui-mock/tauri-mock.mjs';

const url = 'http://127.0.0.1:1420/?variant=A';
let devServer = null;
let browser = null;

async function serverIsReady() {
  return fetch('http://127.0.0.1:1420/').then((response) => response.ok).catch(() => false);
}

async function startDevServer() {
  if (await serverIsReady()) return;
  const command = process.platform === 'win32' ? (process.env.ComSpec ?? 'cmd.exe') : 'npm';
  const args = process.platform === 'win32'
    ? ['/d', '/s', '/c', 'npm run dev -- --host 127.0.0.1']
    : ['run', 'dev', '--', '--host', '127.0.0.1'];
  devServer = spawn(command, args, {
    stdio: 'inherit',
    windowsHide: true,
  });
  const startedAt = Date.now();
  while (Date.now() - startedAt < 60000) {
    if (await serverIsReady()) return;
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error('Vite did not start on http://127.0.0.1:1420 within 60 seconds');
}

try {
  await startDevServer();
  browser = await chromium.launch({ headless: false });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const fixtures = await loadFixtures();
  // Keep the repository context while avoiding auto-resume/terminal noise in
  // a shell-only prototype session.
  fixtures.list_agent_nodes = [];
  await page.addInitScript(buildInitScript(fixtures));
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await page.getByTestId('probe-prototype-switcher').waitFor({ state: 'visible', timeout: 15000 });
  console.log(`Probe prototypes ready at ${url}`);
  console.log('Compare five variants: A, B, C, D, and E.');
  await new Promise((resolve) => browser.on('disconnected', resolve));
} finally {
  if (browser) await browser.close().catch(() => {});
  if (devServer) devServer.kill();
}
