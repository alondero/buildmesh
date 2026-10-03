/**
 * Verify-Smoke E2E Test (issue #157)
 *
 * Post-launch Playwright smoke that proves the terminal actually rendered
 * PTY bytes — the gap in `/verify full` that lets regressions like the
 * receiver-binding bug from #149 slip through: the spawn log line says
 * "process spawned successfully" but xterm.js never receives bytes, and
 * the strict log scan (which only fires on ` ERROR ` / `panic`) misses
 * it. This spec asserts on xterm's buffer model instead.
 *
 * Pipeline under test:
 *   backend `app.emit('agent-output', payload)`
 *     -> frontend `listen('agent-output')` (TerminalRegistry.ts:275)
 *     -> writer.append(nodeId, data) -> TerminalWriter schedules via this.scheduler
 *     -> scheduler calls term.write(data)  (NOT a no-op: #149 fix wraps rAF in an arrow)
 *     -> xterm buffer: term.buffer.active.getLine(y).translateToString(true)
 *        (read by the spec via expect.poll below)
 *
 * Why read the buffer model instead of the DOM? xterm.js has two
 * renderers. The DOM renderer creates an accessibility mirror at
 * `.xterm-rows > div` that older specs asserted against. The WebGL
 * renderer (default since issue #1122) draws to <canvas> and never
 * builds `.xterm-rows > div`, so the DOM-only check silently flips red
 * on every host that successfully loads WebGL — which is most of them.
 * The buffer model is the same regardless of renderer, and it's what
 * xterm's own integration tests use.
 *
 * Why a Tauri mock (scripts/ui-mock/tauri-mock.mjs) instead of a real backend:
 *   - #149 lives in the frontend's TerminalWriter scheduler binding. The
 *     same listener chain fires whether the bytes come from a real PTY
 *     reader or a test push — so a mock that emulates the `agent-output`
 *     event faithfully reproduces the regression class without depending
 *     on a configured provider or a stable-hub-free port 1991.
 *   - The spec is self-contained: no Rust backend, no port collisions
 *     with the user's stable hub. Runs anywhere Chromium runs, including
 *     the web-Claude-Code host and CI.
 *   - Vite alone (`npm run dev`) provides the React app at :1420; the
 *     mock installs `window.__TAURI_INTERNALS__` before boot.
 *
 * Acceptance (issue #157):
 *   1. The spec passes against current `main`.
 *   2. Reverting #149 (the `requestAnimationFrame` scheduler wrap at
 *      TerminalWriter.ts) causes `this.scheduler(cb)` to throw "Illegal
 *      invocation" inside Chromium; the Tauri listener swallows it and
 *      bytes never reach xterm.js — the buffer model stays empty and
 *      the assertion fails.
 *   3. /verify's full tier documents and runs this spec standalone.
 *
 * Run standalone: `npx playwright test --project=verify-smoke`
 *   (Requires `npm run dev` to be serving :1420. The `verify-smoke`
 *    project in playwright.config.ts has `reuseExistingServer: true` and
 *    won't try to start the slow `npm run tauri dev` flow, so the
 *    stable hub on :1991 is never disturbed.)
 */
import { test, expect, Page } from '@playwright/test';
import { buildInitScript } from '../../scripts/ui-mock/tauri-mock.mjs';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

// One fixture mesh + one agent node with `status: 'running'` so the
// React app sees a node that's already spawning (mimics the post-spawn
// state). The terminal renders the same way it does for any active
// node — the auto-spawn effect in Terminal.tsx:356 short-circuits on
// `status !== 'idle'`, but the attach effect runs unconditionally.
const SMOKE_MESH_ID = 99001;
const SMOKE_NODE_ID = 99002;
const SMOKE_MESH_NAME = 'verify-smoke';
const SMOKE_NODE_NAME = 'smoke-node';

const SMOKE_FIXTURES = {
  list_meshes: [
    {
      id: SMOKE_MESH_ID,
      name: SMOKE_MESH_NAME,
      path: 'C:/temp/verify-smoke',
      layout: 'grid',
      position: 0,
      created_at: '2026-07-17T00:00:00Z',
      build_command: null,
      run_command: null,
      model: null,
      effort: null,
      use_worktree: true,
      worktree_mode: 'perNode',
      default_provider: null,
      base_ref: 'origin/main',
      scratchpad: '',
      sandbox: false,
      pre_spawn_pool_size: 1,
      color: '#6366f1',
    },
  ],
  list_agent_nodes: [
    {
      id: SMOKE_NODE_ID,
      mesh_id: SMOKE_MESH_ID,
      name: SMOKE_NODE_NAME,
      path: 'C:/temp/verify-smoke',
      branch: 'origin/main',
      env: 'Windows',
      provider: 'anthropic',
      cli_session_id: null,
      worktree_name: null,
      use_worktree: true,
      source_issue: null,
      source_pr: null,
      head_repo_owner: null,
      head_repo_clone_url: null,
      source_pr_pinned_sha: null,
      created_at: '2026-07-17T00:00:00Z',
      status: 'running',
      position: 0,
    },
  ],
  get_default_provider: 'anthropic',
  list_providers: [
    { id: 'anthropic', name: 'Claude', description: 'Anthropic Claude Code', icon: null, available: true, kind: 'cwrap' },
  ],
  get_app_preferences: { default_provider: 'anthropic', minimax_api_key_set: false, google_cloud_project: null },
  get_provider_accounts: [],
  get_network_status: { lan_exposure_enabled: false, bound_port: 1992, realized_binds: [] },
  auto_resume_agent_nodes: [],
  is_agent_running: false,
  is_attention_pending: false,
  get_git_status: [],
  get_open_pr_for_node: null,
  get_mesh_pool_count: 0,
};

for (const size of [{ width: 900, height: 600 }, { width: 1280, height: 800 }, { width: 1920, height: 1080 }]) {
  for (const theme of ['dark', 'light']) {
    for (const reducedMotion of ['no-preference', 'reduce'] as const) {
      test(`desktop polish ${size.width}x${size.height} ${theme} motion=${reducedMotion}`, async ({ page }) => {
        await page.setViewportSize(size);
        await page.emulateMedia({ reducedMotion });
        await page.addInitScript({ content: buildInitScript({
          ...SMOKE_FIXTURES,
          list_providers: [{
            id: 'anthropic', label: 'Claude Code', harness_id: 'anthropic',
            group_key: 'anthropic', is_proxied: false, provider_id: null,
            color: '#00d4ff', icon: 'anthropic', resumable: true, runtime: 'windows',
            capabilities: {
              harness_id: 'anthropic', background_inference: null,
              supports_resume: true, auto_resume_on_startup: false,
              requires_attention_hook: false, attention_capability: { kind: 'none' },
              supports_passive_turn_watcher: false, produces_readable_transcript: false,
              supports_model_override: false, supports_effort_override: false,
              supports_extra_args: false, supports_prefill: false,
              is_plain_terminal: false, effort_control: { kind: 'none' }, available_on: ['windows'],
            },
          } satisfies ProviderInfo],
          get_keyed_first_class_catalog: [], get_provider_pairings: [],
          get_pairing_verifications: [], list_device_sessions: [],
          get_coordinator_status: { enabled: false, has_token: false },
          list_spawn_configurations: [], get_launch_targets: [],
          compatible_providers_for_harness: [],
          list_agent_nodes: [
            { ...SMOKE_FIXTURES.list_agent_nodes[0], status: 'suspended', cli_session_id: 'captured-session' },
            { ...SMOKE_FIXTURES.list_agent_nodes[0], id: SMOKE_NODE_ID + 1, name: 'failed-node', status: 'error' },
          ],
        }) });
        await page.addInitScript(theme => localStorage.setItem('buildmesh.theme', theme), theme);
        await page.goto('/');
        await page.mouse.move(size.width - 1, size.height - 1);
        const colour = page.getByRole('button', { name: 'Change mesh colour', exact: true });
        const resume = page.getByRole('button', { name: `Resume ${SMOKE_NODE_NAME}`, exact: true });
        const restart = page.getByRole('button', { name: 'Restart failed-node', exact: true });
        const close = page.getByRole('button', { name: `Delete ${SMOKE_NODE_NAME}`, exact: true });
        const disclosure = page.getByRole('button', { name: 'Choose provider', exact: true });
        for (const control of [colour, resume, restart, close, disclosure]) {
          await expect(control).toBeVisible();
          const bounds = await control.boundingBox();
          expect(bounds?.width).toBeGreaterThanOrEqual(24);
          expect(bounds?.height).toBeGreaterThanOrEqual(24);
          await expect(control).toHaveAttribute('title', /.+/);
          expect(await control.evaluate(element => getComputedStyle(element).opacity)).toBe('1');
        }
        await disclosure.click();
        await expect(disclosure).toHaveAttribute('aria-expanded', 'true');
        await page.keyboard.press('Escape');
        await expect(disclosure).toHaveAttribute('aria-expanded', 'false');
        await expect(disclosure).toBeFocused();

        await page.getByRole('button', { name: 'Open settings', exact: true }).click();
        const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
        const tabs = dialog.getByRole('tab');
        await expect(tabs.nth(0)).toBeFocused();
        for (const [key, index] of [['ArrowUp', 3], ['ArrowDown', 0], ['End', 3], ['Home', 0], ['ArrowDown', 1]] as const) {
          await page.keyboard.press(key);
          await expect(tabs.nth(index)).toBeFocused();
          await expect(tabs.nth(index)).toHaveAttribute('aria-selected', 'true');
          await expect(dialog.locator('[role="tab"][tabindex="0"]')).toHaveCount(1);
          await expect(dialog.getByRole('tabpanel')).toHaveAttribute('id', await tabs.nth(index).getAttribute('aria-controls') ?? 'missing');
          await expect(dialog.getByRole('tabpanel')).toHaveAttribute('aria-labelledby', await tabs.nth(index).getAttribute('id') ?? 'missing');
        }
        for (const index of [0, 1, 2, 3]) {
          await tabs.nth(index).click();
          await page.keyboard.press('Tab');
          await expect(dialog.getByRole('tabpanel')).toBeFocused();
          for (let step = 0; step < 150; step++) {
            await page.keyboard.press('Tab');
            const focus = await page.evaluate(() => {
              const element = document.activeElement;
              const pane = element?.closest('[role="tabpanel"]');
              return { role: element?.getAttribute('role'), inside: !!element?.closest('[role="dialog"]'), hidden: !!element?.closest('[hidden], [inert]'), pane: pane?.getAttribute('aria-labelledby') };
            });
            expect(focus.inside).toBe(true);
            expect(focus.hidden).toBe(false);
            if (focus.pane) expect(focus.pane).toBe(await tabs.nth(index).getAttribute('id'));
            if (focus.role === 'tab') break;
            expect(step).toBeLessThan(149);
          }
          const dimensions = await dialog.getByRole('tabpanel').evaluate(element => ({ client: element.clientWidth, scroll: element.scrollWidth }));
          expect(dimensions.scroll).toBeLessThanOrEqual(dimensions.client + 1);
        }
        const bounds = await dialog.boundingBox();
        expect(bounds!.x).toBeGreaterThanOrEqual(0);
        expect(bounds!.y).toBeGreaterThanOrEqual(0);
        expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(size.width);
        expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(size.height);

        await tabs.nth(1).click();
        await dialog.getByRole('button', { name: /^\+ add provider$/i }).click();
        await dialog.getByRole('button', { name: /other \/ custom/i }).click();
        const field = dialog.getByLabel(/custom provider name/i);
        await field.fill('Unsaved draft');
        await expect(page.getByTestId('settings-tab-dirty-providers')).toBeVisible();
        await field.focus();
        await page.keyboard.press('Escape');
        const keep = page.getByTestId('modal-discard-cancel');
        await expect(keep).toBeFocused();
        await page.keyboard.press('Tab');
        await expect(page.getByTestId('modal-discard-confirm')).toBeFocused();
        await page.keyboard.press('Tab');
        await expect(keep).toBeFocused();
        await page.keyboard.press('Enter');
        await expect(field).toBeFocused();
        await expect(field).toHaveValue('Unsaved draft');
      });
    }
  }
}

test('Windows Grok clipboard gestures send one native paste command through real xterm', async ({ page }) => {
  const writes: Array<{ sessionId: number; data: string }> = [];
  await page.exposeFunction('recordGrokPaste', (args: { sessionId: number; data: string }) => writes.push(args));
  await page.addInitScript({ content: buildInitScript({
    ...SMOKE_FIXTURES,
    list_agent_nodes: [{ ...SMOKE_FIXTURES.list_agent_nodes[0], provider: 'grok', env: 'windows' }],
  }) });
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'platform', { value: 'Win32' });
    const host = window as unknown as {
      recordGrokPaste(args: unknown): Promise<void>;
      __BUILDMESH_MOCK__: { on(command: string, handler: (args: unknown) => unknown): void };
    };
    host.__BUILDMESH_MOCK__.on('write_to_agent', (args) => host.recordGrokPaste(args));
    host.__BUILDMESH_MOCK__.on('read_clipboard', () => { throw new Error('Grok must read its own clipboard'); });
  });
  await page.goto('/');
  await page.locator(`[data-session-id="${SMOKE_NODE_ID}"]`).click();
  const terminal = page.locator(`[data-node-id="${SMOKE_NODE_ID}"] .xterm`);
  const textarea = terminal.locator('textarea');
  await expect(terminal).toBeVisible();
  await textarea.focus();
  await page.keyboard.press('Control+v');
  await expect.poll(() => writes).toEqual([{ sessionId: SMOKE_NODE_ID, data: '\x16' }]);
  writes.length = 0;
  await page.keyboard.press('Control+Shift+v');
  await expect.poll(() => writes).toEqual([{ sessionId: SMOKE_NODE_ID, data: '\x16' }]);
  writes.length = 0;
  await page.keyboard.press('Shift+Insert');
  await expect.poll(() => writes).toEqual([{ sessionId: SMOKE_NODE_ID, data: '\x16' }]);
  writes.length = 0;
  await terminal.click({ button: 'right' });
  await page.getByRole('button', { name: /^Paste/ }).click();
  await expect.poll(() => writes).toEqual([{ sessionId: SMOKE_NODE_ID, data: '\x16' }]);

  // Browser paste must not also run xterm's handler.
  writes.length = 0;
  await textarea.evaluate((element) => {
    const clipboardData = new DataTransfer();
    clipboardData.setData('text/plain', Array.from({ length: 200 }, (_, i) => `line ${i}`).join('\r\n'));
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true }));
  });
  await expect.poll(() => writes).toEqual([{ sessionId: SMOKE_NODE_ID, data: '\x16' }]);

  // A normal Enter remains a separate user action, never appended to paste.
  writes.length = 0;
  await textarea.focus();
  await page.keyboard.press('Enter');
  await expect.poll(() => writes).toEqual([{ sessionId: SMOKE_NODE_ID, data: '\r' }]);

  // File drops and other supplied text must not paste the desktop clipboard.
  writes.length = 0;
  await page.evaluate(async (id) => {
    const term = window.__terminalManager!.getTerminal(id)!;
    await new Promise<void>((resolve) => term.write('\x1b[?2004h', resolve));
    term.paste('supplied\ntext');
  }, SMOKE_NODE_ID);
  await expect.poll(() => writes).toEqual([{
    sessionId: SMOKE_NODE_ID, data: '\x1b[200~supplied\rtext\x1b[201~',
  }]);
});

/**
 * Push a few lines of PTY bytes into the Tauri mock's `agent-output`
 * event. The mock fans out to every registered listener — including
 * the one TerminalRegistry attaches when the AgentTerminal mounts.
 * Line 277 of TerminalRegistry.ts filters by `event.payload.session_id
 * === nodeId`, so we send `session_id` (the mock's wire shape).
 */
async function pushAgentOutput(page: Page, nodeId: number, lines: string[]) {
  await page.evaluate(({ nodeId, lines }) => {
    const mock = (window as unknown as {
      __BUILDMESH_MOCK__?: {
        emit(event: string, payload: unknown): number;
      };
    }).__BUILDMESH_MOCK__;
    if (!mock) throw new Error('Tauri mock not installed — did the init script run?');
    // AgentOutputPayload shape from src/types/generated/AgentOutputPayload.ts:
    // { session_id: number; line?: string; data?: string; chunk_kind?: 'data'|'snapshot' }
    // `line` is the single-string form; the listener passes it through
    // to term.write() verbatim (TerminalRegistry.ts:277-280).
    for (const line of lines) {
      mock.emit('agent-output', { session_id: nodeId, line, chunk_kind: 'data' });
    }
  }, { nodeId, lines });
}

/**
 * Asserts that at least one row in the node's xterm buffer has non-empty
 * text content. Reads xterm's renderer-agnostic internal buffer model
 * (`term.buffer.active.getLine(y).translateToString(true)`) via
 * `window.__terminalManager`, which is what xterm's own integration
 * tests use.
 *
 * Earlier versions read the DOM accessibility mirror at
 * `.xterm-rows > div`. That selector only works under xterm's DOM
 * renderer — the WebGL renderer (default since issue #1122) draws to
 * <canvas> and never builds the mirror, so the assertion silently
 * flipped red on every host that successfully loaded WebGL (which is
 * most of them). Reading the buffer model is the same regardless of
 * renderer, and that's the invariant the issue #149 regression
 * breaks (bytes never reach `term.write`).
 *
 * Polling is delegated to `expect.poll`, which retries on thrown
 * errors and exposes the timeout message in the failure diff. The
 * inner `evaluate` returns a primitive number (non-empty row count)
 * rather than a string[] snapshot — moving large arrays across CDP
 * every tick was wasted bandwidth and the second `translateToString`
 * call inside Playwright added an unnecessary IPC roundtrip per row.
 */
async function assertXtermHasRenderedBytes(page: Page, nodeId: number, timeoutMs = 10000) {
  // `data-node-id` is set on both the grid node header and the terminal host,
  // so the attribute alone is not a unique handle (Playwright strict mode
  // rejects it). Select the host that actually contains a terminal — that
  // keeps the "the AgentTerminal mounted and xterm attached inside it"
  // assertion while tolerating the header sharing the attribute.
  const container = page.locator(`[data-node-id="${nodeId}"]`).filter({ has: page.locator('.xterm') });
  await expect(container, `AgentTerminal container for node ${nodeId} should mount`).toBeVisible({ timeout: 10000 });

  const xterm = container.locator('.xterm');
  await expect(xterm, `xterm should attach inside the AgentTerminal container`).toBeVisible({ timeout: 10000 });

  await expect.poll(
    async () => {
      // Returns 0 when the terminal hasn't mounted yet so expect.poll
      // keeps retrying instead of throwing an uncaught "Terminal not
      // mounted" rejection that would crash the polling loop on
      // legitimate async-mount races.
      return await page.evaluate((id) => {
        const term = (window as unknown as {
          __terminalManager?: {
            getTerminal(nodeId: number): {
              buffer: {
                active: {
                  length: number;
                  getLine(y: number): { translateToString(trim?: boolean): string } | undefined;
                };
              };
            } | undefined;
          };
        }).__terminalManager?.getTerminal(id);
        if (!term) return 0;
        let nonEmpty = 0;
        for (let y = 0; y < term.buffer.active.length; y++) {
          const line = term.buffer.active.getLine(y);
          if (line && line.translateToString(true).trim().length > 0) nonEmpty++;
        }
        return nonEmpty;
      }, nodeId);
    },
    {
      timeout: timeoutMs,
      intervals: [100, 200, 500],
      message:
        `PTY->xterm pipeline did not deliver bytes to xterm buffer (node ${nodeId}). ` +
        `Likely the agent-output listener wrapper is throwing (issue #149 regression: ` +
        `a bare requestAnimationFrame stored on TerminalWriter loses its window receiver).`,
    },
  ).toBeGreaterThan(0);
}

test.describe('verify-smoke (issue #157)', () => {

  test.beforeEach(async ({ page }) => {
    // Install the Tauri mock BEFORE any app module evaluates. Vite's
    // dev server compiles modules on demand, but the very first
    // `import { listen } from '@tauri-apps/api/event'` (App.tsx:2)
    // reaches for `window.__TAURI_INTERNALS__` synchronously, so the
    // shim must exist before the navigation commit.
    await page.addInitScript({ content: buildInitScript(SMOKE_FIXTURES) });
    // Surface uncaught page errors as test failures — a #149 regression
    // throws "Illegal invocation" synchronously inside Chromium when
    // the listener wrapper calls writer.append(); without this handler
    // the throw becomes a generic Playwright pageerror that loses the
    // stack-trace anchor the summary quotes.
    page.on('pageerror', (err) => {
      throw new Error(`[verify-smoke] page error: ${err.message}`);
    });
  });

  test('spawned agent renders PTY bytes into xterm.js', async ({ page }) => {
    // Vite serves the React UI at baseURL (http://localhost:1420).
    // The page has no window.__TAURI__ natively — our addInitScript
    // installs the shim with fixture data so the sidebar renders the
    // smoke mesh + node without any backend round-trip.
    await page.goto('/');
    // The lockup is an inline SVG behind `role="img"` + `aria-label`
    // (TitleBar/Wordmark.tsx), not an `<img alt>` raster — it replaced the
    // baked wordmark so it can follow the theme tokens. Assert the
    // accessible name, which is stable across that change.
    await expect(page.getByRole('img', { name: 'Buildmesh' })).toBeVisible({ timeout: 15000 });

    // The fixture node should appear in the sidebar (data-session-id
    // is set on the row by NodeItem.tsx:294).
    const sidebarNode = page.locator(`[data-session-id="${SMOKE_NODE_ID}"]`);
    await expect(sidebarNode, 'smoke node should appear in the sidebar from fixture').toBeVisible({ timeout: 10000 });
    await sidebarNode.click();

    // After the click, AgentTerminal mounts (Terminal.tsx:31). The
    // attach effect (line 307) registers the `agent-output` listener,
    // and the auto-spawn effect (line 356) short-circuits because
    // status is already 'running' — no spurious spawn IPC. Now the
    // listener is live and waiting for bytes.

    // Push deterministic PTY bytes via the mock. These flow through
    // the EXACT same listener that a real PTY reader would trigger
    // (TerminalRegistry.ts:275-282), so a healthy frontend sees them
    // appear in xterm; a frontend with the #149 receiver-binding
    // regression never receives them — `this.scheduler(cb)` throws
    // "Illegal invocation" inside Chromium, the Tauri listener
    // wrapper swallows it, and no bytes reach term.write().
    await pushAgentOutput(page, SMOKE_NODE_ID, [
      'verify-smoke: receiver-binding contract check\r\n',
      'Hello from the mock IPC — if you see this, PTY->xterm works.\r\n',
    ]);

    // The actual assertion (issue spec step 4): xterm mounted AND at
    // least one row in the active buffer has non-empty text — read via
    // the renderer-agnostic buffer model so this works under both xterm
    // renderers (DOM and WebGL).
    await assertXtermHasRenderedBytes(page, SMOKE_NODE_ID, 10000);
  });

  test('agent input preserves separate recovery keys and a single Alt+Enter event', async ({ page }) => {
    const writes: string[] = [];
    await page.exposeFunction('recordTerminalInput', (data: string) => { writes.push(data); });
    await page.goto('/');
    await page.locator(`[data-session-id="${SMOKE_NODE_ID}"]`).click();
    const input = page.locator(`[data-node-id="${SMOKE_NODE_ID}"] .xterm-helper-textarea`);
    await input.focus();
    await page.evaluate(() => {
      const host = window as unknown as {
        __BUILDMESH_MOCK__: { on(command: string, handler: (args: { data: string }) => void): void };
        recordTerminalInput(data: string): void;
      };
      host.__BUILDMESH_MOCK__.on('write_to_agent', ({ data }) => host.recordTerminalInput(data));
    });
    await page.keyboard.press('Escape');
    await page.keyboard.press('Enter');
    await page.keyboard.press('Alt+Enter');
    await page.keyboard.press('Control+c');
    await expect.poll(() => writes).toEqual(['\x1b', '\r', '\x1b\r', '\x03']);
  });

  test('utility tabs fill the body and preserve terminal and keyboard state across switches', async ({ page }) => {
    await page.goto('/');
    await page.locator(`[data-session-id="${SMOKE_NODE_ID}"]`).click();
    const header = page.getByTestId('grid-node-header');
    await header.getByRole('button', { name: 'Open build menu' }).click();
    await page.getByRole('menuitem', { name: /^Terminal/ }).click();
    const panel = page.getByRole('tabpanel');
    await expect(panel.locator('.xterm')).toHaveCount(1);
    await expect(panel.locator('.xterm')).toBeVisible();
    expect(await panel.locator(`[data-node-id="${SMOKE_NODE_ID}"]`).count()).toBe(0);
    const panelBounds = await panel.boundingBox();
    const terminalBounds = await panel.locator('.xterm').boundingBox();
    expect(terminalBounds!.height).toBeGreaterThan(panelBounds!.height * 0.9);

    const utilityTab = page.getByRole('tab', { name: /^Terminal/ });
    const agentTab = page.getByRole('tab', { name: /^Agent/ });
    await utilityTab.focus();
    await page.keyboard.press('ArrowLeft');
    await expect(panel.locator(`[data-node-id="${SMOKE_NODE_ID}"] .xterm`)).toBeVisible();
    await expect(agentTab).toBeFocused();
    await page.keyboard.press('ArrowRight');
    await expect(panel.locator('.xterm')).toHaveCount(1);
    await expect(panel.locator('.xterm')).toBeVisible();
    await expect(utilityTab).toBeFocused();

    await page.getByRole('button', { name: /^Close Terminal/ }).click();
    await expect(page.getByRole('tablist', { name: 'Node activities' })).toHaveCount(0);
    await expect(page.locator(`[data-node-id="${SMOKE_NODE_ID}"] .xterm`)).toBeVisible();
  });

  // Issue #1947: switching away from a terminal tab detaches its view. When
  // the tab is selected again, the existing terminal element and its buffer
  // should be reused.
  test('utility terminal reattaches the same .xterm element across a tab round trip', async ({ page }) => {
    await page.goto('/');
    await page.locator(`[data-session-id="${SMOKE_NODE_ID}"]`).click();
    await page.getByTestId('grid-node-header').getByRole('button', { name: 'Open build menu' }).click();
    await page.getByRole('menuitem', { name: /^Terminal/ }).click();
    const panel = page.getByRole('tabpanel');
    await expect(panel.locator('.xterm')).toHaveCount(1);
    const utilityElement = await panel.locator('.xterm').elementHandle();

    const utilityTab = page.getByRole('tab', { name: /^Terminal/ });
    await utilityTab.focus();
    await page.keyboard.press('ArrowLeft');
    await expect(panel.locator(`[data-node-id="${SMOKE_NODE_ID}"] .xterm`)).toBeVisible();
    await page.keyboard.press('ArrowRight');
    await expect(panel.locator('.xterm')).toHaveCount(1);
    expect(await panel.locator('.xterm').evaluate((element, previous) => element === previous, utilityElement)).toBe(true);

    await utilityElement?.dispose();
  });
});
