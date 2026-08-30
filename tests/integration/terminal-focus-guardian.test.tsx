/**
 * Regression test for the "focus stolen mid-typing" bug.
 *
 * Symptom (reported in a 3+ pane grid with a stable node list): while typing
 * into the active node's terminal, keyboard focus is silently dropped — with no
 * click or other obvious action — and the next characters go nowhere. There is
 * no static code path that *moves* focus in that scenario, which means focus is
 * falling out to <body> entirely (a stray DOM reconciliation around xterm's
 * imperatively-appended element, or a WebView2 focus hiccup), rather than being
 * handed to another control.
 *
 * Fix: a focus guardian on each AgentTerminal. When *this* node is the active
 * one and the app window still holds OS focus, but focus has fallen to
 * <body>/null, pull it straight back to the terminal. It must NOT fire when
 * focus legitimately moved to a real control (a button, the search box, a
 * rename input, another pane) — those land on an element, not <body>.
 *
 * Run with: npm test -- --run tests/integration/terminal-focus-guardian.test.tsx
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, act } from '@testing-library/react';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

if (!('ResizeObserver' in globalThis)) {
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

// vi.hoisted runs before the SUT import, which triggers the TerminalRegistry
// constructor's eager listen() (issue #332). Same pattern as the auto-spawn
// test — without it the mock factory's closure over mockListeners hits TDZ.
const mockListeners = vi.hoisted(() => new Map<string, Set<(...args: unknown[]) => void>>());

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation((event: string, callback: (event: { payload: unknown }) => void) => {
    if (!mockListeners.has(event)) mockListeners.set(event, new Set());
    mockListeners.get(event)!.add(callback as (...args: unknown[]) => void);
    return Promise.resolve(() => mockListeners.get(event)?.delete(callback as (...args: unknown[]) => void));
  }),
  emit: vi.fn().mockImplementation((event: string, payload?: unknown) => {
    mockListeners.get(event)?.forEach(cb => cb({ payload }));
    return Promise.resolve();
  }),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockImplementation((cmd: string) => {
    if (cmd === 'list_agent_nodes') return Promise.resolve([]);
    // `list_providers` / `get_default_provider` are the new wrapper-memoised
    // lookups (issue #405); Terminal.tsx's handover-label effect reads them
    // on every node mount, so the mock must satisfy it with deterministic data.
    if (cmd === 'list_providers') return Promise.resolve([
      { id: 'anthropic', label: 'Claude' },
    ]);
    if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
    return Promise.resolve({});
  }),
  Channel: class Channel {
    onmessage = (_message: unknown) => {};
    constructor(handler?: (message: unknown) => void) {
      if (handler) this.onmessage = handler;
    }
  },
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('@xterm/xterm', () => {
  class MockTerminal {
    write = vi.fn();
    onData = vi.fn();
    onTitleChange = vi.fn();
    onResize = vi.fn();
    onScroll = vi.fn().mockReturnValue({ dispose: vi.fn() });
    open(container: HTMLElement) {
      const el = document.createElement('div');
      el.className = 'xterm';
      container.appendChild(el);
      this.element = el;
      // xterm's real `.open()` creates a hidden helper `<textarea>` that
      // receives keyboard focus when `term.focus()` is called. The focus
      // guardian in Terminal.tsx relies on this — focusout bubbles from
      // the helper textarea through `.xterm` and up to the container
      // div, so we mirror that structure here. Without a real focusable
      // child inside the container, the tests can't reproduce a click
      // that moves focus away from the terminal (and the guardian's
      // "don't fight a real control" check never fires).
      const helper = document.createElement('textarea');
      helper.className = 'xterm-helper-textarea';
      helper.setAttribute('aria-hidden', 'true');
      helper.tabIndex = 0;
      el.appendChild(helper);
      this.helperTextarea = helper;
    }
    dispose = vi.fn();
    // Mirror real xterm: focus the helper textarea (it owns keyboard
    // input). When the helper is in the DOM, .focus() moves DOM focus
    // and triggers a real focusout on whatever element previously held
    // focus, so the focus guardian's bubbling listener fires correctly.
    focus = vi.fn(() => { this.helperTextarea?.focus(); });
    loadAddon = vi.fn();
    attachCustomKeyEventHandler = vi.fn();
    scrollToBottom = vi.fn();
    refresh = vi.fn();
    clear = vi.fn();
    selectAll = vi.fn();
    hasSelection = vi.fn().mockReturnValue(false);
    getSelection = vi.fn().mockReturnValue('');
    paste = vi.fn();
    buffer = { active: { getWindow: vi.fn() } };
    unicode = {
      register: vi.fn(),
      get activeVersion() { return '11'; },
      set activeVersion(_v: string) { /* noop */ },
    };
    _core = { unicodeService: { _providers: { '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 } }, register: vi.fn() } };
    rows = 24;
    cols = 80;
    options = { fontSize: 10 };
    element: HTMLElement | null = null;
    helperTextarea: HTMLTextAreaElement | null = null;
    constructor(_options?: unknown) {}
  }
  return { Terminal: MockTerminal };
});

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit = vi.fn();
    dispose = vi.fn();
    proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 });
  },
}));
vi.mock('@xterm/addon-serialize', () => ({
  SerializeAddon: class { serialize = vi.fn().mockReturnValue(''); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-search', () => ({
  SearchAddon: class { findNext = vi.fn(); findPrevious = vi.fn(); clearDecorations = vi.fn(); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-web-links', () => ({
  WebLinksAddon: class { dispose = vi.fn(); constructor(_h?: unknown) {} },
}));
vi.mock('@xterm/addon-unicode11', () => ({
  Unicode11Addon: class { dispose = vi.fn(); },
}));

// The handover label effect in AgentTerminal hits `api.getDefaultProvider`
// + `api.listProviders` (issue #405) — both are now stubbed at the IPC
// level in the `vi.mock('@tauri-apps/api/core', …)` block above, so no
// separate cache mock is needed.

import { AgentTerminal, terminalManager } from '../../src/components/Terminal/Terminal';

// 'running' (not 'idle') so the auto-spawn effect stays out of the way — we
// only care about the focus lifecycle here.
const RUNNING_NODE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  use_worktree: false,
  position: 0,
  created_at: new Date(0).toISOString(),
};

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
  scratchpad: '',
  sandbox: false,
};

async function mountAndSettle() {
  const result = render(<AgentTerminal nodeId={RUNNING_NODE.id} />);
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });
  const inst = terminalManager.getInstance(RUNNING_NODE.id);
  if (!inst) throw new Error('terminal instance was not created');
  // Drop the attach-time focus() call so assertions see only post-mount focus.
  vi.mocked(inst.term.focus).mockClear();
  const host = result.container.querySelector<HTMLElement>(`[data-node-id="${RUNNING_NODE.id}"]`);
  if (!host) throw new Error('terminal host element not found');
  return { ...result, inst, host };
}

async function dispatchFocusOut(host: HTMLElement) {
  await act(async () => {
    host.dispatchEvent(new FocusEvent('focusout', { bubbles: true }));
    // Flush the guardian's deferred (microtask) check.
    await new Promise((r) => setTimeout(r, 0));
  });
}

/** Pretend focus has fallen to <body>: explicitly blur whatever the helper
 *  textarea owns so the guardian's activeElement check sees body. Mirrors
 *  the runtime scenario ("xterm's helper textarea blurred mid-keystroke,
 *  no other control picked up focus"). */
function focusFallsToBody() {
  if (document.activeElement instanceof HTMLElement) {
    document.activeElement.blur();
  }
}

describe('AgentTerminal focus guardian', () => {
  beforeEach(() => {
    mockListeners.clear();
    vi.clearAllMocks();
    useAgentNodeStore.setState({ agentNodes: [RUNNING_NODE], activeNodeId: RUNNING_NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ dragTargetNodeId: null });
    terminalManager.dispose(RUNNING_NODE.id);
    // jsdom's hasFocus() can be unreliable across versions; pin it true so the
    // guardian's "window still focused" guard doesn't gate the test.
    vi.spyOn(document, 'hasFocus').mockReturnValue(true);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  });

  afterEach(() => {
    terminalManager.dispose(RUNNING_NODE.id);
    vi.restoreAllMocks();
  });

  it('restores focus to the active terminal when focus falls out to <body>', async () => {
    const { inst, host } = await mountAndSettle();
    // Blur the helper textarea (which the attach effect focuses) so the
    // guardian sees activeElement === <body> when its microtask runs —
    // the exact "focus silently dropped" signature this guard fixes.
    focusFallsToBody();
    await dispatchFocusOut(host);
    expect(inst.term.focus).toHaveBeenCalled();
  });

  it('does NOT reclaim focus when it moved to a real control (e.g. a button)', async () => {
    const { inst, host } = await mountAndSettle();
    const button = document.createElement('button');
    document.body.appendChild(button);
    button.focus(); // document.activeElement is now a real element, not <body>
    await dispatchFocusOut(host);
    expect(inst.term.focus).not.toHaveBeenCalled();
    button.remove();
  });

  it('does NOT reclaim focus for a node that is not the active one', async () => {
    const { inst, host } = await mountAndSettle();
    // Another pane is active; this terminal must not yank focus to itself.
    act(() => { useAgentNodeStore.setState({ activeNodeId: 999 }); });
    await dispatchFocusOut(host);
    expect(inst.term.focus).not.toHaveBeenCalled();
  });

  it('does NOT reclaim focus when the app window has lost OS focus', async () => {
    const { inst, host } = await mountAndSettle();
    vi.mocked(document.hasFocus).mockReturnValue(false);
    await dispatchFocusOut(host);
    expect(inst.term.focus).not.toHaveBeenCalled();
  });

  // Regression for "I open the Scratch Pad and the active terminal keeps
  // stealing focus away from the textarea". This mirrors the actual click
  // sequence in Chromium: when the user clicks a focusable element, the
  // browser synchronously moves focus as part of the mousedown handling,
  // so by the time the focusout listener fires the new element is already
  // the active element. We use the real `focus()` call (which dispatches
  // blur/focus in JSDOM the same way Chromium dispatches them mid-click) so
  // the microtask sees the textarea as the active element — and must
  // therefore not reclaim.
  it('does NOT reclaim focus when focus moves to a focusable control via real click flow', async () => {
    const { inst } = await mountAndSettle();

    const textarea = document.createElement('textarea');
    textarea.setAttribute('aria-label', 'Scratch pad');
    document.body.appendChild(textarea);

    try {
      // Real click flow: textarea.focus() moves focus synchronously, which
      // blurs the terminal helper textarea (which fires focusout on the
      // container) and focuses the textarea in the same task. By the time
      // the queued microtask runs, activeElement IS the textarea.
      textarea.focus();

      await act(async () => {
        await new Promise((r) => setTimeout(r, 0));
      });

      expect(inst.term.focus).not.toHaveBeenCalled();
      expect(document.activeElement).toBe(textarea);
    } finally {
      textarea.remove();
    }
  });

  // Regression for the user's exact complaint: the agent terminal steals
  // focus from text inputs elsewhere in the dock. To exercise the real
  // "container re-renders around me" sequence (the dock panel mounting
  // alongside the terminal), mount the terminal, focus its helper
  // textarea (the steady-state when the user opens the dock), then
  // focus a sibling textarea. The guardian must not steal it back.
  it('does NOT reclaim focus from a sibling-mounted text input when probe opens', async () => {
    // Mount the terminal first, with the helper textarea actually focused
    // — that's the steady-state when the user is mid-typing and decides to
    // click 📝 to open the dock.
    const { inst } = await mountAndSettle();
    inst.term.helperTextarea?.focus();
    expect(document.activeElement).toBe(inst.term.helperTextarea);

    // Now mount a sibling (the probe panel — simplified to just the
    // ScratchpadTab). This mirrors the layout where the dock and the
    // terminal live side-by-side.
    const probe = document.createElement('textarea');
    probe.setAttribute('aria-label', 'Scratch pad');
    document.body.appendChild(probe);

    try {
      probe.focus();
      await act(async () => {
        await new Promise((r) => setTimeout(r, 0));
      });

      expect(inst.term.focus).not.toHaveBeenCalled();
      expect(document.activeElement).toBe(probe);
    } finally {
      probe.remove();
    }
  });

  // Pins the user's bug at the event-source level. The original guard
  // only checked `document.activeElement` at microtask time, which has a
  // window where Chromium (and WebView2) hasn't yet committed the
  // focus event on the new control — so `activeElement` is still
  // <body>, and the guard reclaims focus away from the user's click.
  // The fix uses `focusout.relatedTarget` (where focus is *going*) as
  // the primary check; this test spies on queueMicrotask to verify the
  // relatedTarget branch short-circuits BEFORE the microtask is queued
  // at all. With the old logic, the guard would queue a microtask on
  // every focusout (and only the activeElement check inside would
  // decide to skip) — the spy would record the call.
  it('does not queue a reclaim microtask when focusout.relatedTarget is a real control', async () => {
    const { host } = await mountAndSettle();
    const textarea = document.createElement('textarea');
    textarea.setAttribute('aria-label', 'Scratch pad');
    document.body.appendChild(textarea);

    const queueSpy = vi.spyOn(globalThis, 'queueMicrotask');
    try {
      host.dispatchEvent(new FocusEvent('focusout', {
        bubbles: true,
        relatedTarget: textarea,
      }));

      // The relatedTarget check must short-circuit before any microtask
      // is queued — that's the whole point of using relatedTarget as the
      // primary signal instead of relying on the microtask-time
      // activeElement read.
      expect(queueSpy).not.toHaveBeenCalled();
    } finally {
      textarea.remove();
      queueSpy.mockRestore();
    }
  });

  // relatedTarget === <body> (e.g. clicking a non-focusable area of the
  // dock header): focus has genuinely fallen to nothing, so the guard
  // must still reclaim. Pins the body-relatedTarget boundary so a future
  // tweak can't accidentally start skipping the real-loss case. The
  // `relatedTarget: null` case (programmatic focus change / focus
  // leaving the document) hits the exact same code path — both
  // expressions `rt && rt !== document.body` evaluate to false — so
  // exercising one pins both.
  it('still reclaims when focusout.relatedTarget is body (non-focusable click)', async () => {
    const { inst, host } = await mountAndSettle();
    focusFallsToBody();
    host.dispatchEvent(new FocusEvent('focusout', {
      bubbles: true,
      relatedTarget: document.body,
    }));

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(inst.term.focus).toHaveBeenCalled();
  });
});
