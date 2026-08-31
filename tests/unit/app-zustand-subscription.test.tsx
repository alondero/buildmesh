/**
 * Issue #1246 — whole-store Zustand subscriptions caused the entire app to
 * re-render on every `patchAgentNode` (attention flip, status update,
 * spawn event, …). The fix in src/App.tsx replaces `useMeshStore()` /
 * `useAgentNodeStore()` destructuring with per-action selectors whose
 * references are stable across renders.
 *
 * Two layers of coverage here:
 *
 *   1. Behavioural: render-count harnesses that reproduce the OLD and NEW
 *      subscription patterns and assert how each reacts to `patchAgentNode`.
 *      This pins the Zustand contract the fix depends on — if a future
 *      Zustand upgrade changed its default equality semantics, the
 *      behavioural test catches it before the production fix silently
 *      regresses.
 *
 *   2. Source-level: a static read of src/App.tsx asserts no whole-store
 *      `useMeshStore()` / `useAgentNodeStore()` calls remain. If anyone
 *      reintroduces the anti-pattern, this fails.
 *
 * We don't mount the real <App /> — its surface pulls in xterm, Tauri
 * global-shortcut, the sidebar's dnd-kit wiring, the probe panel, etc.,
 * which would require a wall of mocks for a test whose value comes from
 * the subscription contract, not the rendered DOM. The harnesses here use
 * the exact same subscription lines the real App uses.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { render, act, cleanup } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

function makeAgentNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1, mesh_id: 1, name: 'agent', path: '/p', branch: 'main',
    env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
    use_worktree: true, position: 0, is_pinned: false,
    ...overrides,
  };
}

/** Reproduces the post-fix subscription block from src/App.tsx (issue #1246).
 *  Each render increments `onRender` so the parent can count. */
function AppSubscriptionPattern({ onRender }: { onRender: () => void }) {
  onRender();
  const fetchMeshes = useMeshStore((s) => s.fetchMeshes);
  const fetchAgentNodes = useAgentNodeStore((s) => s.fetchAgentNodes);
  const initAttentionListeners = useAgentNodeStore((s) => s.initAttentionListeners);
  const storeError = useAgentNodeStore((state) => state.error);
  return null;
}

/** Reproduces the pre-fix anti-pattern from src/App.tsx (issue #1246). */
function AppWholeStoreAntiPattern({ onRender }: { onRender: () => void }) {
  onRender();
  const { fetchMeshes } = useMeshStore();
  const { fetchAgentNodes, initAttentionListeners } = useAgentNodeStore();
  const storeError = useAgentNodeStore((state) => state.error);
  return null;
}

describe('App.tsx Zustand subscription pattern (issue #1246)', () => {
  beforeEach(() => {
    // Reset state so each test starts with a known baseline (no leftover
    // agentNodes from a sibling test would keep the whole-store re-render
    // count deterministic).
    seedAgentNodes([]);
  });
  afterEach(() => cleanup());

  it('whole-store destructure re-renders on patchAgentNode (anti-pattern)', () => {
    // Pin the failure mode of the anti-pattern. If this test ever stops
    // re-rendering, the harness itself has gone stale and the fix's
    // behaviour proof below needs revisiting.
    let renders = 0;
    render(<AppWholeStoreAntiPattern onRender={() => renders++} />);
    const baseline = renders;
    expect(baseline).toBeGreaterThan(0);

    act(() => {
      // Issue #1384 — `patchAgentNode` writes the entry under id 1 and
      // rebuilds `nodesById`, so the top-level state object still gets a
      // new reference. The whole-store destructure subscribes via Zustand's
      // default `Object.is` and re-renders on any state shape change.
      seedAgentNodes([makeAgentNode({ id: 1 })]);
      useAgentNodeStore.getState().patchAgentNode(1, { status: 'awaiting_input' });
    });

    expect(renders).toBeGreaterThan(baseline);
  });

  it('per-action selectors do NOT re-render on patchAgentNode (the fix)', () => {
    // This is the actual contract the fix relies on. Each selector returns
    // a function reference that is stable across renders (Zustand stores
    // actions at creation time and never replaces them), so the default
    // Object.is comparison short-circuits and no re-render fires — even
    // though the store's underlying `agentNodes` array was replaced.
    let renders = 0;
    render(<AppSubscriptionPattern onRender={() => renders++} />);
    const baseline = renders;
    expect(baseline).toBeGreaterThan(0);

    act(() => {
      // Issue #1384 — even after normalisation, the per-action selectors
      // see a stable function reference and short-circuit. The
      // `nodesById` rebuild does not affect them.
      useAgentNodeStore.getState().patchAgentNode(1, { status: 'awaiting_input' });
    });

    expect(renders).toBe(baseline);
  });

  it('per-action selectors re-render only on the fields they actually subscribe to', () => {
    // Sanity check the inverse: a selector on `error` DOES re-render when
    // `error` changes. Otherwise we could "fix" re-renders by subscribing
    // to nothing — the test guards against that degenerate case.
    let renders = 0;
    function ErrorSubscriber({ onRender }: { onRender: () => void }) {
      onRender();
      const error = useAgentNodeStore((s) => s.error);
      return <span>{error ?? 'no-error'}</span>;
    }
    render(<ErrorSubscriber onRender={() => renders++} />);
    const baseline = renders;
    act(() => {
      useAgentNodeStore.setState({ error: 'boom' });
    });
    expect(renders).toBeGreaterThan(baseline);
  });

  it('src/App.tsx contains no whole-store useMeshStore() calls', () => {
    // Source-level regression guard. Reading the file from disk keeps the
    // test honest even if the App.tsx test mount is skipped in CI for
    // being too heavy — the static check runs in milliseconds.
    const appPath = resolve(__dirname, '../../src/App.tsx');
    const src = readFileSync(appPath, 'utf8');
    // Strip JS comments so an explanatory // line that mentions the
    // pattern in prose doesn't trip the regex. `useStore.getState()`
    // is allowed (imperative reads in callbacks); the discriminator is
    // the bare `useStore()` call.
    const stripped = stripJsComments(src);
    expect(stripped).not.toMatch(/useMeshStore\(\)/);
  });

  it('src/App.tsx contains no whole-store useAgentNodeStore() calls', () => {
    const appPath = resolve(__dirname, '../../src/App.tsx');
    const src = readFileSync(appPath, 'utf8');
    const stripped = stripJsComments(src);
    expect(stripped).not.toMatch(/useAgentNodeStore\(\)/);
  });
});

/** Drop // and /* * / comments from JS/TS source so a regression-guard regex
 *  doesn't false-positive on a prose mention of the anti-pattern. The
 *  goal is to detect the *code* form, not the spelled-out name. */
function stripJsComments(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, '') // /* block */
    .replace(/^\s*\/\/.*$/gm, '') // // line
    .replace(/[ \t]+\/\/.*$/gm, ''); // trailing // comment on a code line
}