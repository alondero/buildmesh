/**
 * Tests for `scripts/ui-mock/tauri-mock.mjs` — the browser-side Tauri IPC
 * stand-in that lets `/verify-ui`'s `--mock` mode render the frontend in
 * headless Chromium (Claude Code on the web / CI) without WebView2 or a Rust
 * backend. See `scripts/ui-shot.mjs` mode 3.
 *
 * The init script is a string injected via `page.addInitScript`; here we eval
 * it against jsdom's `window` and assert the shim behaves: fixtures resolve,
 * plugin calls get benign defaults, unknown commands resolve null (not throw),
 * and the listen/emit event roundtrip works.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { buildInitScript, defaultFixtures, loadFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

declare global {
  interface Window {
    __TAURI_INTERNALS__: any;
    __TAURI_EVENT_PLUGIN_INTERNALS__: any;
    __BUILDMESH_MOCK__: any;
  }
}

function install(fixtures?: Record<string, unknown>) {
  eval(buildInitScript(fixtures));
  window.__BUILDMESH_MOCK__.quiet = true; // silence the unmocked-command log
}

describe('tauri-mock harness', () => {
  beforeEach(() => {
    delete (window as any).__TAURI_INTERNALS__;
    delete (window as any).__TAURI_EVENT_PLUGIN_INTERNALS__;
    delete (window as any).__BUILDMESH_MOCK__;
  });

  it('ships boot fixtures so the shell renders populated', () => {
    expect(defaultFixtures.list_meshes).toHaveLength(2);
    expect(defaultFixtures.list_agent_nodes.length).toBeGreaterThan(0);
    expect(defaultFixtures.list_providers[0].id).toBe('anthropic');
  });

  it('installs a single-window/webview metadata surface', () => {
    install();
    const meta = window.__TAURI_INTERNALS__.metadata;
    expect(meta.currentWindow.label).toBe('main');
    expect(meta.currentWebview.label).toBe('main'); // getCurrentWebview() needs this
  });

  it('resolves fixtured commands and null for unknown ones (never throws)', async () => {
    install();
    const invoke = window.__TAURI_INTERNALS__.invoke;
    await expect(invoke('list_meshes')).resolves.toHaveLength(2);
    await expect(invoke('this_command_has_no_fixture')).resolves.toBeNull();
  });

  it('gives plugin calls benign defaults', async () => {
    install();
    const invoke = window.__TAURI_INTERNALS__.invoke;
    await expect(invoke('plugin:window|is_focused')).resolves.toBe(true);
    await expect(invoke('plugin:global-shortcut|is_registered')).resolves.toBe(false);
    await expect(invoke('plugin:global-shortcut|register')).resolves.toBeNull();
  });

  it('roundtrips listen → emit → unlisten through the callback registry', async () => {
    install();
    const internals = window.__TAURI_INTERNALS__;
    const received: unknown[] = [];
    const handlerId = internals.transformCallback((e: { payload: unknown }) => received.push(e.payload), false);

    // What @tauri-apps/api/event.listen() does under the hood:
    const eventId = await internals.invoke('plugin:event|listen', { event: 'node-status-changed', handler: handlerId });
    expect(eventId).toBe(handlerId); // return value must equal stored id so unlisten matches

    expect(window.__BUILDMESH_MOCK__.emit('node-status-changed', { nodeId: 7 })).toBe(1);
    expect(received).toEqual([{ nodeId: 7 }]);

    // event.js unlisten() drops the listener via this separate global.
    window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener('node-status-changed', eventId);
    expect(window.__BUILDMESH_MOCK__.emit('node-status-changed', { nodeId: 8 })).toBe(0);
    expect(received).toEqual([{ nodeId: 7 }]);
  });

  it('lets steps override a response at runtime via mock.on', async () => {
    install();
    window.__BUILDMESH_MOCK__.on('list_meshes', []);
    await expect(window.__TAURI_INTERNALS__.invoke('list_meshes')).resolves.toEqual([]);
  });

  it('advances idle nodes when the mock receives a spawn request', async () => {
    install({
      list_agent_nodes: [{ id: 42, status: 'idle' }],
    });

    await expect(window.__TAURI_INTERNALS__.invoke('spawn_agent', {
      request: { sessionId: 42 },
    })).resolves.toBeNull();
    await expect(window.__TAURI_INTERNALS__.invoke('list_agent_nodes')).resolves.toEqual([
      { id: 42, status: 'spawning' },
    ]);
  });

  it('keeps runtime overrides ahead of request-specific handlers', async () => {
    install();
    const override = [{ sentinel: true }];
    window.__BUILDMESH_MOCK__.on('list_circuits_with_runs', override);

    await expect(window.__TAURI_INTERNALS__.invoke('list_circuits_with_runs', { meshId: 1 }))
      .resolves.toEqual(override);
  });

  it('ships a circuit ledger fixture for Probe screenshots', () => {
    expect(defaultFixtures.list_circuits_with_runs).toHaveLength(1);
    expect(defaultFixtures.list_circuits_with_runs[0].runs[0].run.state).toBe('failed');
    expect(defaultFixtures.list_circuit_queue.map((entry) => entry.queue_rank)).toEqual([1, 2]);
  });

  it('keeps circuit fixtures scoped to the requested mesh', async () => {
    install();
    await expect(window.__TAURI_INTERNALS__.invoke('list_circuits_with_runs', { meshId: 2 })).resolves.toEqual([]);
  });

  it('does not send unknown-command diagnostics through console.info', async () => {
    const info = vi.spyOn(console, 'info').mockImplementation(() => {});
    const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});
    try {
      install();
      window.__BUILDMESH_MOCK__.quiet = false;
      await expect(window.__TAURI_INTERNALS__.invoke('unknown_command')).resolves.toBeNull();
      expect(info).not.toHaveBeenCalled();
      expect(debug).toHaveBeenCalled();
    } finally {
      info.mockRestore();
      debug.mockRestore();
    }
  });

  it('loadFixtures() with no override returns the defaults', async () => {
    await expect(loadFixtures(undefined)).resolves.toBe(defaultFixtures);
  });
});
