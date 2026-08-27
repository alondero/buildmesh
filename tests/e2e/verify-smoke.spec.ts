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
  const container = page.locator(`[data-node-id="${nodeId}"]`);
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
    await expect(page.locator('img[alt="Buildmesh"]')).toBeVisible({ timeout: 15000 });

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
});