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
  pre_spawn_pool_size: 1, color: null,
};

const nodeDefaults = {
  env: 'Windows', provider: 'anthropic', cli_session_id: null,
  worktree_name: null, use_worktree: true, source_issue: null,
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

export const defaultFixtures = {
  list_meshes: meshes,
  list_agent_nodes: agentNodes,
  get_default_provider: 'anthropic',
  list_providers: [
    {
      id: 'anthropic', label: 'Claude', name: 'Claude', description: 'Anthropic Claude Code',
      icon: null, available: true, kind: 'cwrap', harness_id: 'claude',
      provider_id: 'anthropic', is_proxied: false, group_key: 'anthropic',
    },
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
 * Build the init-script source (a string) that installs the mock. Runs in
 * the browser via `page.addInitScript`, BEFORE any app module — so the
 * frontend's very first `invoke` already sees the shim. Fixtures are JSON
 * inlined so no Node<->browser bridge is needed at boot; interactive
 * overrides go through `window.__BUILDMESH_MOCK__` at runtime.
 */
export function buildInitScript(fixtures) {
  const json = JSON.stringify(fixtures ?? defaultFixtures);
  // Note: `fixtures` values that are functions can't cross the JSON
  // boundary — only serializable defaults live in the init script. Dynamic
  // per-call responses are set at runtime via mock.on(...) (page.evaluate).
  return `(() => {
  const responses = ${json};
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
    if (Object.prototype.hasOwnProperty.call(responses, cmd)) {
      const r = responses[cmd];
      return typeof r === 'function' ? r(args) : r;
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
    // throwing. Logged so a steps author can see what to add.
    if (!window.__BUILDMESH_MOCK__.quiet) console.info('[tauri-mock] unmocked invoke:', cmd, args ?? '');
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
    on(cmd, value) { responses[cmd] = value; },
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
