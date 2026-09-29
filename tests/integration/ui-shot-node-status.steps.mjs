import { expect } from '@playwright/test';
import { chromium } from 'playwright';
import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { mkdirSync } from 'node:fs';
import path from 'node:path';

// Synthetic native callbacks through the real dev HTTP/lifecycle/SQLite path.
// This verifies the clients and delivery boundary, not a live harness runner.
export default async function ({ page, invoke }) {
  const mesh = await invoke('create_test_mesh', { name: 'Node status verification' });
  const db = new DatabaseSync(path.join(process.env.APPDATA, 'com.alond.buildmesh.dev', 'buildmesh.db'));
  const shots = 'docs/pr-screenshots/maiden-sprawling-bison';
  mkdirSync(shots, { recursive: true });
  let browser;
  try {
    db.prepare('UPDATE meshes SET path=?, pre_spawn_pool_size=0 WHERE id=?').run(process.cwd(), mesh.id);
    const create = (name) => {
      const session = randomUUID();
      const id = Number(db.prepare("INSERT INTO agent_nodes (mesh_id,name,path,provider,status,use_worktree,cli_session_id,session_started_at) VALUES (?,?,?,'anthropic','running',0,?,?)")
        .run(mesh.id, name, process.cwd(), session, Math.floor(Date.now() / 1000)).lastInsertRowid);
      return { id, session, name };
    };
    const hook = async (node, event, extra = {}) => {
      const response = await fetch(`http://127.0.0.1:2992/api/attention/${node.id}`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ hook_event_name: event, session_id: node.session, prompt_id: 'ui-turn', ...extra }),
        signal: AbortSignal.timeout(10000),
      });
      expect(response.ok).toBe(true);
    };
    const background = create('Reviewing with background agents');
    const question = create('Waiting for your answer');
    const ready = create('Ready for the next instruction');
    for (const node of [background, question, ready]) await hook(node, 'UserPromptSubmit');
    await hook(background, 'SubagentStart', { agent_id: 'review-one' });
    await hook(background, 'SubagentStart', { agent_id: 'review-two' });
    await hook(background, 'Stop');
    const prompt = 'Should the deployment target staging or production?';
    await hook(question, 'PreToolUse', { tool_name: 'AskUserQuestion', tool_use_id: 'question-one', tool_input: { questions: [{ question: prompt }] } });
    await hook(ready, 'Stop');
    const read = (node) => invoke('get_agent_node', { nodeId: node.id });
    expect((await read(background)).lifecycle.kind).toBe('background_running');
    expect((await read(question)).lifecycle.kind).toBe('question_requested');
    expect((await read(ready)).status).toBe('ready');

    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.locator(`#node-item-name-${background.id}`)).toBeVisible();
    await expect(page.locator('[title^="Waiting for background work."]').first()).toBeVisible();
    await expect(page.locator('[title^="Needs an answer."]').first()).toBeVisible();
    await page.locator(`#mesh-item-name-${mesh.id}`)
      .locator('xpath=ancestor::div[contains(@class,"group/mesh")][1]')
      .screenshot({ path: `${shots}/node-status-desktop.png` });

    const { ticket } = await invoke('create_pairing_ticket');
    browser = await chromium.launch();
    const mobile = await browser.newPage({ viewport: { width: 390, height: 844 } });
    await mobile.goto(`http://127.0.0.1:2992/v2#pair=${encodeURIComponent(ticket)}`, { waitUntil: 'domcontentloaded' });
    await expect(mobile.getByText(prompt, { exact: true }).first()).toBeVisible();
    await mobile.reload({ waitUntil: 'domcontentloaded' });
    await expect(mobile.getByText(prompt, { exact: true }).first()).toBeVisible();
    await mobile.getByTestId(`attn-card-${question.id}`).screenshot({ path: `${shots}/node-status-question.png` });
    await mobile.locator('nav button').filter({ hasText: 'Work' }).click();
    await mobile.getByRole('textbox', { name: 'Search work' }).fill('Node status verification');
    await expect(mobile.getByText('Waiting for background work', { exact: true }).first()).toBeVisible();
    const bounds = await mobile.evaluate(() => ({ scroll: document.documentElement.scrollWidth, client: document.documentElement.clientWidth }));
    expect(bounds.scroll).toBeLessThanOrEqual(bounds.client);
    await mobile.screenshot({ path: `${shots}/node-status-mobile.png` });

    await hook(background, 'SubagentStop', { agent_id: 'review-one' });
    expect((await read(background)).lifecycle.kind).toBe('background_running');
    await hook(background, 'SubagentStop', { agent_id: 'review-two' });
    expect((await read(background)).status).toBe('ready');
    await expect(mobile.getByTestId(`node-${background.id}`)).toContainText('Ready');
    console.log('PASS: real backend preserves question and background observations across reload; final child alone makes node ready.');
  } finally {
    await browser?.close();
    db.close();
    await invoke('delete_mesh', { meshId: mesh.id });
  }
}
