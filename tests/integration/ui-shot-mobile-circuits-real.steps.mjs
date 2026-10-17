import { chromium, expect } from '@playwright/test';
import { readFileSync, writeFileSync } from 'node:fs';

// Requires this worktree's freshly built dev profile and test bridge. Setup
// parks a persisted built-in review at its source gate; the real worker, HTTP
// routes, device pairing and desktop IPC handle the decision without mocks.
// This proves gate advancement, not AI review or publication (no live PTY).
export default async function ({ page: desktop, invoke }) {
  const instance = JSON.parse(readFileSync('.tmp/dev-instance.json', 'utf8'));
  const origin = `http://127.0.0.1:${1992 + instance.portOffset}`;
  const fixture = await invoke('create_mobile_review_approval_fixture', { name: 'Issue 2314 phone approval' });
  const browser = await chromium.launch();
  const phone = await browser.newPage({ viewport: { width: 390, height: 844 } });
  const desktopInvoke = (command, args) => desktop.evaluate(([cmd, values]) => window.__TAURI_INTERNALS__.invoke(cmd, values), [command, args]);
  const readGate = async () => {
    const runs = await desktopInvoke('list_circuit_runs', { circuitId: fixture.circuit, limit: 10 });
    const run = runs.find((detail) => detail.run.id === fixture.run);
    return run?.steps.find((step) => step.node_id === 'confirm_source')?.status;
  };
  try {
    expect(await readGate()).toBe('blocked');
    const { ticket } = await invoke('create_pairing_ticket');
    await phone.goto(`${origin}/v2/#pair=${ticket}`, { waitUntil: 'domcontentloaded' });
    await phone.getByTestId(`node-${fixture.source}`).click();
    await expect(phone.getByTestId('circuit-run-line')).toContainText('Needs approval');
    await phone.screenshot({ path: '.tmp/mobile-real-approval-before.png' });
    await phone.getByRole('button', { name: 'Approve…', exact: true }).click();
    await phone.getByRole('button', { name: 'Confirm', exact: true }).click();
    await expect(phone.getByText('Approved. The run will carry on.')).toBeVisible();
    await expect.poll(readGate, { timeout: 30000 }).toBe('completed');
    await phone.screenshot({ path: '.tmp/mobile-real-approval-after.png' });
    writeFileSync('.tmp/mobile-circuit-runtime.json', JSON.stringify({ origin, run: fixture.run, source: fixture.source,
      initialGate: 'blocked', desktopGateAfterPhoneApproval: await readGate(),
      boundary: 'paired browser -> real HTTP approve -> worker -> same desktop run ledger',
      limitation: 'Persisted approval fixture; no live PTY, AI reviewer or publication exercised.' }, null, 2));
    console.log(`PASS: paired phone approved real Review Run ${fixture.run}; desktop gate is completed`);
  } finally {
    await browser.close();
    await invoke('delete_test_review_fixture', { meshId: fixture.mesh.id });
  }
}
