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
import { DiagnosticCategory, ModuleKind, ScriptTarget, flattenDiagnosticMessageText, transpileModule } from 'typescript';
import { fileURLToPath, pathToFileURL } from 'url';
import { dirname, resolve } from 'path';

// Keep the browser script JavaScript while sourcing its JSON from the same
// TypeScript fixture builder checked by tsc. Transpile in memory for Node 22+.
const fixturePath = resolve(dirname(fileURLToPath(import.meta.url)), 'default-fixtures.ts');
const fixtureSource = readFileSync(fixturePath, 'utf8');
const { outputText, diagnostics } = transpileModule(fixtureSource, {
  fileName: fixturePath,
  reportDiagnostics: true,
  compilerOptions: { module: ModuleKind.ESNext, target: ScriptTarget.ES2020 },
});
const errors = (diagnostics ?? []).filter(diagnostic => diagnostic.category === DiagnosticCategory.Error);
if (errors.length) {
  throw new Error(errors.map(diagnostic =>
    `${fixturePath}: TS${diagnostic.code}: ${flattenDiagnosticMessageText(diagnostic.messageText, '\n')}`
  ).join('\n'));
}
const { buildDefaultFixtures } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
export const defaultFixtures = buildDefaultFixtures();

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
  list_circuit_probe: (args, responses) => {
    const snapshot = responses.list_circuit_probe;
    const meshId = args?.meshId;
    if (!snapshot || meshId === undefined || meshId === null) return snapshot;
    return {
      ...snapshot,
      circuits: (snapshot.circuits ?? []).filter((row) => row?.circuit?.mesh_id === Number(meshId)),
      queue: (snapshot.queue ?? []).filter((entry) => entry?.run?.mesh_id === Number(meshId)),
    };
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
 * overrides go through `window.__BUILDMESH_MOCK__` at runtime. Unmodelled
 * commands reject; intentionally ignored side effects must be named in
 * the third argument's `ignoredCommands` allowlist (or fixtured explicitly).
 */
export function buildInitScript(fixtures, handlers = defaultHandlers, { ignoredCommands = [] } = {}) {
  const json = JSON.stringify(fixtures ?? defaultFixtures);
  const handlerSource = serializeHandlers(handlers);
  // Note: `fixtures` values that are functions can't cross the JSON
  // boundary. Dynamic responses belong in explicit handlers or runtime
  // overrides via mock.on(...) (page.evaluate).
  return `(() => {
  const responses = ${json};
  const handlers = { ${handlerSource} };
  const ignoredCommands = new Set(${JSON.stringify(ignoredCommands)});
  const overrides = {};
  let nextId = 1;
  const callbacks = {};            // callback id -> fn (from transformCallback)
  const listeners = {};            // event name -> Set<callback id>

  const win = { label: 'main' };
  const benignPlugins = {
    'plugin:event|unlisten': null,
    'plugin:event|emit': null,
    'plugin:event|emit_to': null,
    'plugin:window|is_focused': true,
    'plugin:window|is_maximized': false,
    'plugin:window|scale_factor': 1,
    'plugin:window|theme': 'dark',
    'plugin:window|inner_size': { width: 1440, height: 900 },
    'plugin:window|outer_size': { width: 1440, height: 900 },
    'plugin:global-shortcut|is_registered': false,
    'plugin:global-shortcut|register': null,
    'plugin:global-shortcut|unregister': null,
    'plugin:global-shortcut|unregister_all': null,
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
    if (Object.prototype.hasOwnProperty.call(benignPlugins, cmd)) return benignPlugins[cmd];
    if (ignoredCommands.has(cmd)) return null;
    // Debug bypasses the frontend log bridge, avoiding recursive IPC logging.
    if (!window.__BUILDMESH_MOCK__.quiet) console.debug('[tauri-mock] unmocked invoke:', cmd, args ?? '');
    throw new Error('[tauri-mock] Unmocked IPC command: ' + cmd);
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
