/**
 * tauri-mock.mjs — a browser-side stand-in for Tauri's IPC bridge.
 *
 * WHY: `/verify-ui` drives the real WebView2 window over CDP, which only
 * exists on Windows. On a headless Linux host (Claude Code on the web,
 * CI) there is no WebView2 and no Rust backend, so the visual UI check
 * can't run — the previous agent hit exactly this wall. This module lets
 * the *real* frontend render in the pre-installed headless Chromium by
 * installing a fake `window.__TAURI_INTERNALS__` before the app boots.
 *
 * Everything the frontend does — `invoke('list_meshes')`, `listen(...)`,
 * `getCurrentWindow()`, the global-shortcut plugin — funnels through
 * `window.__TAURI_INTERNALS__` in Tauri v2 (see
 * node_modules/@tauri-apps/api/core.js). So one shim at that chokepoint
 * covers the whole surface: IPC commands resolve from a fixtures map,
 * plugin calls get benign defaults, and events can be pushed to listeners
 * from a steps file via `window.__BUILDMESH_MOCK__.emit(...)`.
 *
 * This is NOT a substitute for the real Windows CDP path (that exercises
 * the actual Rust backend). It's a visual-render + interaction harness so
 * a non-Windows agent can still capture before/after screenshots and drive
 * the changed component with fixture data.
 */

import { readFileSync } from 'fs';
import { fileURLToPath, pathToFileURL } from 'url';
import { dirname, resolve } from 'path';

const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * Default fixtures — enough for the desktop shell to boot populated: two
 * meshes, a couple of agent nodes, the Anthropic provider, and empty/benign
 * reads for the rest. Keyed by Tauri command name (the string passed to
 * `invoke`). A value may be a literal or `(args) => value`. Steps files and
 * `--fixtures` merge over this.
 *
 * Field shapes mirror the generated wire types (src/types/generated/*). The
 * `..default` spreads below keep new backend columns from breaking the mock:
 * unknown-but-required fields fall back to zero-values, matching the Rust
 * `#[derive(Default)]` fixtures.
 */
const meshDefaults = {
  layout: 'grid', position: 0, created_at: '1970-01-01T00:00:00Z',
  build_command: null, run_command: null, model: null, effort: null,
  use_worktree: true, worktree_mode: 'perNode', default_provider: null,
  base_ref: 'origin/main', scratchpad: '', sandbox: false,
  pre_spawn_pool_size: 1, color: null, worktree_directory: null,
};

const nodeDefaults = {
  env: 'Windows', provider: 'anthropic', cli_session_id: null,
  worktree_name: null, use_worktree: true, source_issue: null,
  worktree_path: null,
  source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
  source_pr_pinned_sha: null, created_at: '1970-01-01T00:00:00Z',
};

const meshes = [
  { ...meshDefaults, id: 1, name: 'buildmesh', path: '/home/user/buildmesh', position: 0, color: '#6366f1' },
  { ...meshDefaults, id: 2, name: 'playground', path: '/home/user/playground', position: 1, color: '#10b981' },
];

const agentNodes = [
  { ...nodeDefaults, id: 1, mesh_id: 1, name: 'fix-terminal-blank', path: '/home/user/buildmesh', branch: 'origin/main', status: 'running', position: 0 },
  { ...nodeDefaults, id: 2, mesh_id: 1, name: 'diff-panel-refactor', path: '/home/user/buildmesh', branch: 'origin/main', status: 'awaiting_input', position: 1 },
  { ...nodeDefaults, id: 3, mesh_id: 2, name: 'scratch', path: '/home/user/playground', branch: 'origin/main', status: 'idle', position: 0 },
];

const circuitsWithRuns = [
  {
    circuit: {
      id: 101,
      mesh_id: 1,
      name: 'Issue review loop',
      description: 'Review labelled issues and report the result.',
      enabled: true,
      concurrency_limit: 1,
      graph_json: '{"nodes":[],"edges":[]}',
      created_at: '2026-01-01T00:00:00Z',
      updated_at: '2026-01-01T00:00:00Z',
    },
    runs: [
      {
        run: {
          id: 1001,
          circuit_id: 101,
          mesh_id: 1,
          trigger_identity: 'issue:1468:buildmesh:run',
          state: 'failed',
          context_json: '{}',
          created_at: '2026-01-01T00:00:00Z',
          updated_at: '2026-01-01T00:02:00Z',
        },
        steps: [
          {
            id: 1002,
            run_id: 1001,
            node_id: 'reviewer',
            agent_node_id: 2,
            status: 'failed',
            attempt: 1,
            outcome: 'failed',
            error_message: 'The review command exited before producing a result.',
            started_at: '2026-01-01T00:01:00Z',
            completed_at: '2026-01-01T00:02:00Z',
          },
        ],
      },
    ],
  },
];

export const defaultFixtures = {
  list_meshes: meshes,
  list_agent_nodes: agentNodes,
  get_default_provider: 'anthropic',
  list_providers: [
    {
      id: 'anthropic', label: 'Claude Code', color: '#d97706', icon: 'claude',
      resumable: true, harness_id: 'anthropic', provider_id: null,
      is_proxied: false, group_key: 'anthropic',
      capabilities: {
        harness_id: 'anthropic', supports_resume: true, auto_resume_on_startup: true,
        requires_attention_hook: true, attention_capability: { kind: 'none' },
        supports_passive_turn_watcher: true, produces_readable_transcript: true,
        supports_model_override: true, supports_effort_override: true,
        supports_extra_args: true, supports_prefill: true, is_plain_terminal: false,
        effort_control: { kind: 'closed', allowed: ['low', 'medium', 'high'] },
        available_on: ['windows', 'macos', 'linux'],
      },
    },
  ],
  list_autopilot_runs: [],
  list_circuit_agent_ownerships: [],
  list_semantic_turns: [],
  list_circuits_with_runs: circuitsWithRuns,
  get_github_url_for_mesh: null,
  get_git_branch_status: null,
  get_git_summary: null,
  get_app_identifier: 'com.alond.buildmesh.dev',
  log_frontend: null,
  subscribe_agent_output: null,
  get_app_preferences: {
    default_provider: 'anthropic', minimax_api_key_set: false,
    google_cloud_project: null, worktree_directory: null,
  },
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
 * Stateful responses that need request-specific behaviour live here instead
 * of in the generic invoke dispatcher. Handler arguments follow the actual
 * Tauri wire contracts; each handler receives the browser-local fixture
 * state and may replace a response without mutating a fixture object.
 */
const defaultHandlers = {
  spawn_agent: (args, responses) => {
    const sessionId = args?.request?.sessionId;
    if (sessionId === undefined || sessionId === null || !Array.isArray(responses.list_agent_nodes)) return null;
    responses.list_agent_nodes = responses.list_agent_nodes.map((node) => (
      node?.id === Number(sessionId) && node.status === 'idle'
        ? { ...node, status: 'spawning' }
        : node
    ));
    return null;
  },
  list_circuits_with_runs: (args, responses) => {
    const rows = responses.list_circuits_with_runs;
    const meshId = args?.meshId;
    if (!Array.isArray(rows) || meshId === undefined || meshId === null) return rows;
    return rows.filter((row) => row?.circuit?.mesh_id === Number(meshId));
  },
};

function serializeHandlers(handlers) {
  return Object.entries(handlers)
    .map(([cmd, handler]) => {
      if (typeof handler !== 'function') throw new TypeError(`Mock handler for ${cmd} must be a function`);
      return `${JSON.stringify(cmd)}: ${handler.toString()}`;
    })
    .join(', ');
}

/**
 * Build the init-script source (a string) that installs the mock. Runs in
 * the browser via `page.addInitScript`, BEFORE any app module — so the
 * frontend's very first `invoke` already sees the shim. Fixtures are JSON
 * inlined so no Node<->browser bridge is needed at boot; interactive
 * overrides go through `window.__BUILDMESH_MOCK__` at runtime.
 */
export function buildInitScript(fixtures, handlers = defaultHandlers) {
  const json = JSON.stringify(fixtures ?? defaultFixtures);
  const handlerSource = serializeHandlers(handlers);
  // Note: `fixtures` values that are functions can't cross the JSON
  // boundary. Dynamic responses belong in explicit handlers or runtime
  // overrides via mock.on(...) (page.evaluate).
  return `(() => {
  const responses = ${json};
  const handlers = { ${handlerSource} };
  const overrides = {};
  let nextId = 1;
  const callbacks = {};            // callback id -> fn (from transformCallback)
  const listeners = {};            // event name -> Set<callback id>

  const win = { label: 'main' };
  const benignPlugin = (cmd) => {
    if (cmd.startsWith('plugin:event|')) return null;
    if (cmd.startsWith('plugin:window|is_focused')) return true;
    if (cmd.startsWith('plugin:window|scale_factor')) return 1;
    if (cmd.startsWith('plugin:window|theme')) return 'dark';
    if (cmd.startsWith('plugin:window|inner_size') || cmd.startsWith('plugin:window|outer_size')) return { width: 1440, height: 900 };
    if (cmd.startsWith('plugin:global-shortcut|is_registered')) return false;
    if (cmd.startsWith('plugin:')) return null;   // register/unregister/emit/etc.
    return undefined;
  };

  const invoke = async (cmd, args) => {
    if (Object.prototype.hasOwnProperty.call(overrides, cmd)) {
      const response = overrides[cmd];
      return typeof response === 'function' ? response(args) : response;
    }
    if (Object.prototype.hasOwnProperty.call(handlers, cmd)) {
      return handlers[cmd](args, responses);
    }
    if (Object.prototype.hasOwnProperty.call(responses, cmd)) {
      return responses[cmd];
    }
    // Track event listeners so steps can push events to the app. Return the
    // handler's callback id AS the event id, so unlisten (which is called with
    // this return value) removes the same entry we stored here.
    if (cmd === 'plugin:event|listen' && args && typeof args.handler === 'number') {
      (listeners[args.event] ||= new Set()).add(args.handler);
      return args.handler;
    }
    const p = benignPlugin(cmd);
    if (p !== undefined) return p;
    // Unknown app command: resolve null rather than reject, so a screen
    // that reads a command we didn't fixture renders empty instead of
    // throwing. console.debug deliberately bypasses the frontend log bridge,
    // which forwards console.info/warn/error back through invoke.
    if (!window.__BUILDMESH_MOCK__.quiet) console.debug('[tauri-mock] unmocked invoke:', cmd, args ?? '');
    return null;
  };

  window.__TAURI_INTERNALS__ = {
    // getCurrentWindow() / getCurrentWebview() read these labels; both point
    // at the same 'main' surface, matching the real single-window app.
    metadata: { currentWindow: win, currentWebview: win, windows: [win] },
    invoke,
    transformCallback(cb, once) {
      const id = nextId++;
      callbacks[id] = (payload) => { if (once) delete callbacks[id]; cb(payload); };
      return id;
    },
    unregisterCallback(id) { delete callbacks[id]; },
    convertFileSrc(path) { return path; },
  };

  // @tauri-apps/api/event's unlisten() calls into this separate global (not
  // __TAURI_INTERNALS__) to drop the listener before the IPC round-trip.
  // Without it, every effect cleanup that unlistens throws.
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener(event, eventId) {
      const ids = listeners[event];
      if (ids) ids.delete(eventId);
      delete callbacks[eventId];
    },
  };

  // Runtime control surface for steps files (via page.evaluate).
  window.__BUILDMESH_MOCK__ = {
    quiet: false,
    // Override / add a command response at runtime.
    on(cmd, value) { overrides[cmd] = value; },
    // Push a backend event to every registered listener, e.g.
    //   __BUILDMESH_MOCK__.emit('node-status-changed', { nodeId: 1 })
    emit(event, payload) {
      const ids = listeners[event];
      if (!ids) return 0;
      let n = 0;
      for (const id of ids) { const cb = callbacks[id]; if (cb) { cb({ event, id, payload }); n++; } }
      return n;
    },
  };
})();`;
}

/**
 * Load a fixtures override from a `.mjs` (default-exports an object or a
 * function returning one) or `.json` file, merged over `defaultFixtures`.
 */
export async function loadFixtures(file) {
  if (!file) return defaultFixtures;
  const abs = resolve(file);
  if (abs.endsWith('.json')) {
    return { ...defaultFixtures, ...JSON.parse(readFileSync(abs, 'utf8')) };
  }
  const mod = await import(pathToFileURL(abs).href);
  const extra = typeof mod.default === 'function' ? await mod.default() : mod.default;
  return { ...defaultFixtures, ...extra };
}
