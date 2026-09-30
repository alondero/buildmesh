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
    // Issue #1966 — a callback whose shape Buildmesh cannot interpret still
    // needs the user, but it names no request, so the card must not invent one.
    const background = create('Reviewing with background agents');
    const question = create('Waiting for your answer');
    const permission = create('Waiting for approval');
    const unknown = create('Waiting for unclassified input');
    const ready = create('Ready for the next instruction');
    for (const node of [background, question, permission, unknown, ready]) await hook(node, 'UserPromptSubmit');
    await hook(background, 'SubagentStart', { agent_id: 'review-one' });
    await hook(background, 'SubagentStart', { agent_id: 'review-two' });
    await hook(background, 'Stop');
    const prompt = 'Should the deployment target staging or production?';
    await hook(question, 'PreToolUse', {
      tool_name: 'AskUserQuestion', tool_use_id: 'question-one',
      tool_input: { questions: [{ question: prompt, options: [{ label: 'Staging' }, { label: 'Production' }] }] },
    });
    await hook(permission, 'PermissionRequest', { tool_name: 'Edit', tool_input: { file_path: 'src/lib/auth.ts' } });
    // A body `hook()` cannot express: no tool, no question, no approval — only
    // a notification message, which classifies to no request kind at all.
    const raw = await fetch(`http://127.0.0.1:2992/api/attention/${unknown.id}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ hook_event_name: 'Notification', session_id: unknown.session, message: 'still working' }),
      signal: AbortSignal.timeout(10000),
    });
    expect(raw.ok).toBe(true);
    await hook(ready, 'Stop');
    const read = (node) => invoke('get_agent_node', { nodeId: node.id });
    expect((await read(background)).lifecycle.kind).toBe('background_running');
    expect((await read(question)).lifecycle.kind).toBe('question_requested');
    expect((await read(question)).lifecycle.request.choices).toEqual(['Staging', 'Production']);
    expect((await read(permission)).lifecycle.kind).toBe('permission_requested');
    // A permission decision is not a menu: the payload omits `request`
    // entirely, which is the signal a client must not read as "no choices".
    expect((await read(permission)).lifecycle.request).toBeUndefined();
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

    // Issue #1966 — reply controls follow the request, after a live event and
    // after this cold reload alike.
    // A question: the harness's own answers as text, one explicit
    // open-to-answer action, and no yes/no chips to send at a prompt
    // Buildmesh never read.
    const questionCard = mobile.getByTestId(`attn-card-${question.id}`);
    await expect(questionCard.getByTestId('attn-open')).toContainText('Answer in terminal');
    await expect(questionCard.getByTestId('attn-choices')).toContainText('Staging');
    await expect(questionCard.getByTestId('attn-choices')).toContainText('Production');
    await expect(questionCard.getByTestId('attn-approve')).toHaveCount(0);
    await expect(questionCard.getByTestId('attn-reject')).toHaveCount(0);
    await questionCard.screenshot({ path: `${shots}/node-status-question.png` });

    // A permission request keeps its approval controls.
    const permissionCard = mobile.getByTestId(`attn-card-${permission.id}`);
    await expect(permissionCard.getByTestId('attn-approve')).toContainText('Approve (Y)');
    await expect(permissionCard.getByTestId('attn-reject')).toContainText('Reject (N)');
    await expect(permissionCard.getByTestId('attn-choices')).toHaveCount(0);

    // An input request Buildmesh could not classify: open the terminal, and
    // still no yes/no chips.
    const unknownCard = mobile.getByTestId(`attn-card-${unknown.id}`);
    await expect(unknownCard.getByTestId('attn-open')).toContainText('Open terminal to respond');
    await expect(unknownCard.getByTestId('attn-approve')).toHaveCount(0);
    await expect(unknownCard.getByTestId('attn-reject')).toHaveCount(0);

    // The harness answers that question and immediately asks another, without
    // the node ever leaving `awaiting_input` — the replacement must be
    // actionable, not inherit the previous request's state.
    const nextPrompt = 'Which branch should the hotfix target?';
    await hook(question, 'PreToolUse', {
      tool_name: 'AskUserQuestion', tool_use_id: 'question-two',
      tool_input: { questions: [{ question: nextPrompt, options: [{ label: 'Hotfix' }, { label: 'main' }] }] },
    });
    await expect(questionCard.getByText(nextPrompt, { exact: true })).toBeVisible();
    await expect(questionCard.getByTestId('attn-choices')).toContainText('Hotfix');
    await expect(questionCard.getByTestId('attn-open')).toBeEnabled();

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
    console.log('PASS: real backend preserves question and background observations across reload; mobile reply controls follow the request; final child alone makes node ready.');
  } finally {
    await browser?.close();
    db.close();
    await invoke('delete_mesh', { meshId: mesh.id });
  }
}
