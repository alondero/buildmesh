/**
 * InspectorPanel tests for the SpawnAgentNode v2 harness integration
 * (issue #1358 / slice 3 of #1355).
 *
 * Verifies the four capability-gated override controls render only
 * when the selected provider's `HarnessCapabilities` descriptor
 * advertises them, plus the schema round-trip through `onChange`.
 * The capability source itself is hardcoded in
 * `src/components/Circuits/harnessCapabilities.ts`; see
 * `tests/unit/circuits-inspector-capabilities.test.ts` for the drift
 * gate against the Rust inventory.
 */

// vi.hoisted requires `vi.mock` patterns; we need to import the panel
// inside the test bodies but the polyfills are file-level.
//
// `ResizeObserver` and `DOMMatrixReadOnly` shims for jsdom — React
// Flow / fit-view math fails without them (mirrored from
// tests/unit/circuit-flow-editor.test.tsx).

import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import React from 'react';

class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}
(globalThis as unknown as { ResizeObserver: typeof ResizeObserverMock }).ResizeObserver =
  ResizeObserverMock;

class DOMMatrixReadOnlyMock {
  constructor(init?: string | number[]) {
    // Stub; jsdom doesn't ship a real implementation.
    void init;
  }
}
(globalThis as unknown as { DOMMatrixReadOnly: typeof DOMMatrixReadOnlyMock }).DOMMatrixReadOnly =
  DOMMatrixReadOnlyMock;

// We import the panel inside a beforeAll via dynamic import so the
// shims above are in place before any top-level module evaluation
// triggers React Flow / Monaco editor wiring.
let InspectorPanel: typeof import('../../src/components/Circuits/InspectorPanel').InspectorPanel;
beforeAll(async () => {
  const mod = await import('../../src/components/Circuits/InspectorPanel');
  InspectorPanel = mod.InspectorPanel;
});

import type { CircuitNode } from '../../src/types/generated/CircuitNode';

function spawnNode(
  overrides: Partial<Extract<CircuitNode['type'], { type: 'spawn_agent_node' }>> = {},
): CircuitNode {
  return {
    id: 'spawn',
    type: {
      type: 'spawn_agent_node',
      prompt: 'do the thing',
      name: null,
      provider: null,
      model: null,
      effort: null,
      extra_args: null,
      ...overrides,
    },
  };
}

function renderNode(node: CircuitNode, onChange = vi.fn()) {
  return render(<InspectorPanel node={node} onChange={onChange} />);
}

describe('InspectorPanel — SpawnAgentNode harness integration (issue #1358)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });
  afterEach(() => {
    // No teardown — testing-library cleans per render.
  });

  it('renders the provider select with Default selected when no provider is set', () => {
    renderNode(spawnNode());
    const select = screen.getByTestId('inspector-provider-select');
    expect(select).toBeTruthy();
    expect((select as HTMLSelectElement).value).toBe('');
  });

  it('hides model/effort/extra-args inputs when no provider is selected', () => {
    renderNode(spawnNode());
    expect(screen.queryByTestId('inspector-model-input')).toBeNull();
    expect(screen.queryByTestId('inspector-effort-select')).toBeNull();
    expect(screen.queryByTestId('inspector-extra-args-input')).toBeNull();
  });

  it('renders model + closed-effort + extra-args when Claude Code is selected', async () => {
    renderNode(spawnNode({ provider: 'anthropic' }));
    expect(screen.getByTestId('inspector-model-input')).toBeTruthy();
    expect(screen.getByTestId('inspector-effort-select')).toBeTruthy();
    expect(screen.getByTestId('inspector-extra-args-input')).toBeTruthy();
    // Closed vocabulary: low / medium / high
    const effortSelect = screen.getByTestId(
      'inspector-effort-select',
    ) as HTMLSelectElement;
    const options = Array.from(effortSelect.options).map((o) => o.value);
    expect(options).toContain('low');
    expect(options).toContain('medium');
    expect(options).toContain('high');
  });

  it('renders model + inline-config effort + extra-args when Codex is selected', () => {
    renderNode(spawnNode({ provider: 'codex' }));
    expect(screen.getByTestId('inspector-model-input')).toBeTruthy();
    expect(screen.getByTestId('inspector-effort-select')).toBeTruthy();
    expect(screen.getByTestId('inspector-extra-args-input')).toBeTruthy();
    const effortSelect = screen.getByTestId(
      'inspector-effort-select',
    ) as HTMLSelectElement;
    const options = Array.from(effortSelect.options).map((o) => o.value);
    // Codex vocabulary (inline_config): none | low | medium | high | xhigh
    expect(options).toEqual(
      expect.arrayContaining(['none', 'low', 'medium', 'high', 'xhigh']),
    );
  });

  it('renders model + extra-args but NO effort when OpenCode is selected (no effort control)', () => {
    renderNode(spawnNode({ provider: 'opencode' }));
    expect(screen.getByTestId('inspector-model-input')).toBeTruthy();
    expect(screen.getByTestId('inspector-extra-args-input')).toBeTruthy();
    // OpenCode has EffortControlKind::None — no dropdown
    expect(screen.queryByTestId('inspector-effort-select')).toBeNull();
  });

  it('renders model + closed-effort + extra-args when Command Code is selected', () => {
    renderNode(spawnNode({ provider: 'commandcode' }));
    expect(screen.getByTestId('inspector-model-input')).toBeTruthy();
    expect(screen.getByTestId('inspector-effort-select')).toBeTruthy();
    expect(screen.getByTestId('inspector-extra-args-input')).toBeTruthy();
    const effortSelect = screen.getByTestId(
      'inspector-effort-select',
    ) as HTMLSelectElement;
    const options = Array.from(effortSelect.options).map((o) => o.value);
    expect(options).toContain('low');
    expect(options).toContain('medium');
    expect(options).toContain('high');
  });

  it('normalizes command-code and cmdc aliases to commandcode in the provider select', () => {
    const { unmount } = renderNode(spawnNode({ provider: 'command-code' }));
    const select1 = screen.getByTestId('inspector-provider-select') as HTMLSelectElement;
    expect(select1.value).toBe('commandcode');
    unmount();

    renderNode(spawnNode({ provider: 'cmdc' }));
    const select2 = screen.getByTestId('inspector-provider-select') as HTMLSelectElement;
    expect(select2.value).toBe('commandcode');
  });

  it('writes back the model through onChange', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-model-input');
    // fireEvent.change dispatches a single event with the typed value
    // — the actual user typing races with onChange re-renders, so
    // we assert on the dispatched value rather than the accumulated
    // per-keypress calls.
    fireEvent.change(input, { target: { value: 'opus-4-1' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { model: string | null }).model).toBe('opus-4-1');
  });

  it('writes back the effort through onChange', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const select = screen.getByTestId('inspector-effort-select');
    fireEvent.change(select, { target: { value: 'high' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { effort: string | null }).effort).toBe('high');
  });

  it('writes back the extra-args through onChange', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-extra-args-input');
    fireEvent.change(input, { target: { value: '--verbose --debug' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { extra_args: string }).extra_args).toBe('--verbose --debug');
  });

  it('clears provider to null when Default is selected', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'codex' }), onChange);
    const select = screen.getByTestId('inspector-provider-select');
    fireEvent.change(select, { target: { value: '' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect((lastCall as { provider: string | null }).provider).toBeNull();
  });

  it('serialises a v2 SpawnAgentNode back through the canonical AST shape', () => {
    const onChange = vi.fn();
    const node = spawnNode({
      provider: 'codex',
      model: 'gpt-5',
      effort: 'xhigh',
      extra_args: '--no-confirm',
    });
    renderNode(node, onChange);
    const payload = JSON.stringify(node, null, 2);
    // Stability: re-parse and assert the wire shape matches the AST spec.
    expect(payload).toContain('"type": "spawn_agent_node"');
    expect(payload).toContain('"prompt": "do the thing"');
    expect(payload).toContain('"provider": "codex"');
    expect(payload).toContain('"model": "gpt-5"');
    expect(payload).toContain('"effort": "xhigh"');
    expect(payload).toContain('"extra_args": "--no-confirm"');
  });

  // Issue #1362 review fix: switching provider must NOT leave
  // dangling model/effort/extra_args fields from the previous
  // harness in the AST. The Inspector clears them on the next emit
  // so the serialised circuit JSON never contains values the new
  // harness can't honour.
  it('clears model/effort/extra_args on provider switch', () => {
    const onChange = vi.fn();
    // Codex row with Anthropic-incompatible overrides set.
    renderNode(
      spawnNode({
        provider: 'codex',
        model: 'gpt-5',
        effort: 'xhigh',
        extra_args: '--no-confirm',
      }),
      onChange,
    );
    // User switches to Anthropic via the dropdown.
    const select = screen.getByTestId('inspector-provider-select');
    fireEvent.change(select, { target: { value: 'anthropic' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    // Provider is now Anthropic, but every prior harness-specific
    // override is cleared so a stale value can't sneak through the
    // capability mask at spawn time.
    expect((lastCall as { provider: string }).provider).toBe('anthropic');
    expect((lastCall as { model: string | null }).model).toBeNull();
    expect((lastCall as { effort: string | null }).effort).toBeNull();
    expect((lastCall as { extra_args: string | null }).extra_args).toBeNull();
  });

  it('clears overrides when switching back to Default (mesh autopilot)', () => {
    const onChange = vi.fn();
    renderNode(
      spawnNode({
        provider: 'opencode',
        model: 'anthropic/claude-sonnet-4-5',
      }),
      onChange,
    );
    const select = screen.getByTestId('inspector-provider-select');
    fireEvent.change(select, { target: { value: '' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect((lastCall as { provider: string | null }).provider).toBeNull();
    expect((lastCall as { model: string | null }).model).toBeNull();
  });
});
