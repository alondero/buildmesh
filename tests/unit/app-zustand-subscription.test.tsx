/**
 * Issue #1246 — whole-store Zustand subscriptions caused the entire app to
 * re-render on every `patchAgentNode` (attention flip, status update, spawn
 * event, …). The fix in src/App.tsx replaces `useMeshStore()` /
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
 *   2. Lint (moved here from a source read in issue #2240): `no-restricted-syntax`
 *      in eslint.config.js forbids a zero-argument `useXStore()` call anywhere
 *      in `src/` and `tests/`. The old layer-2 read src/App.tsx off disk at
 *      test time and regexed it for the anti-pattern spelling, which proved
 *      one file's text rather than the codebase's behaviour — a lint rule is
 *      the stronger gate and applies to every file. The two intentional
 *      harnesses below that reproduce the anti-pattern carry a scoped disable.
 *
 * We don't mount the real <App /> — its surface pulls in xterm, Tauri
 * global-shortcut, the sidebar's dnd-kit wiring, the probe panel, etc.,
 * which would require a wall of mocks for a test whose value comes from
 * the subscription contract, not the rendered DOM. The harnesses here use
 * the exact same subscription lines the real App uses.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { render, act, cleanup } from '@testing-library/react';
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

/** Reproduces the pre-fix anti-pattern from src/App.tsx (issue #1246) — this
 *  component exists ONLY to prove the anti-pattern re-renders, so it opts out
 *  of the `no-restricted-syntax` rule that keeps it out of `src/`. */
function AppWholeStoreAntiPattern({ onRender }: { onRender: () => void }) {
  onRender();
  /* eslint-disable no-restricted-syntax -- the deliberate whole-store subscription this test pins. */
  const { fetchMeshes } = useMeshStore();
  const { fetchAgentNodes, initAttentionListeners } = useAgentNodeStore();
  /* eslint-enable no-restricted-syntax */
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
});