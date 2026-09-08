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
      // #1219: v3 added `timeout_seconds` to `SpawnAgentNode`. The
      // helper needs the field so individual tests can omit it
      // (passing it via `overrides`) or override it.
      timeout_seconds: null,
      ...overrides,
    },
  };
}

function renderNode(node: CircuitNode, onChange = vi.fn()) {
  return render(<InspectorPanel node={node} onChange={onChange} />);
}

describe('InspectorPanel — borrowed source targets', () => {
  it.each(['llm_turn_classifier', 'await_agent_turn', 'review_verdict'] as const)(
    'offers the triggering agent for %s', type => {
      renderNode({ id: 'gate', type: { type, target_node_id: null } });
      expect((screen.getByRole('option', { name: 'Triggering agent (node-started runs)' }) as HTMLOptionElement).value).toBe('$source');
    },
  );

  it('offers the source for injection but never for destructive actions', () => {
    const { rerender } = renderNode({ id: 'action', type: { type: 'inject_pty', target_node_id: null, prompt: 'Review this' } });
    expect(screen.getByRole('option', { name: 'Triggering agent (node-started runs)' })).not.toBeNull();
    for (const type of [
      { type: 'close_agent_node' as const, target_node_id: null },
      { type: 'set_node_status' as const, target_node_id: null, status: 'completed' },
    ]) {
      rerender(<InspectorPanel node={{ id: 'action', type }} onChange={vi.fn()} />);
      expect(screen.queryByRole('option', { name: 'Triggering agent (node-started runs)' })).toBeNull();
    }
  });
});

describe('InspectorPanel — OpenPr policy', () => {
  const openPrNode = (open_pr_policy: 'create_if_missing' | 'require_existing' | null = null): CircuitNode => ({
    id: 'open-pr',
    type: {
      type: 'github_action',
      action: 'open_pr',
      open_pr_policy,
      label: null,
      comment: null,
    },
  });

  it('shows a create-if-missing default only for OpenPr and emits policy changes', () => {
    const onChange = vi.fn();
    const { rerender } = renderNode(openPrNode(), onChange);
    const policy = screen.getByTestId('inspector-open-pr-policy') as HTMLSelectElement;
    expect(policy.value).toBe('create_if_missing');
    fireEvent.change(policy, { target: { value: 'require_existing' } });
    expect(onChange).toHaveBeenCalledWith({
      type: 'github_action',
      action: 'open_pr',
      open_pr_policy: 'require_existing',
      label: null,
      comment: null,
    });

    rerender(
      <InspectorPanel
        node={{
          id: 'label',
          type: {
            type: 'github_action',
            action: 'add_label',
            open_pr_policy: null,
            label: 'ready',
            comment: null,
          },
        }}
        onChange={onChange}
      />,
    );
    expect(screen.queryByTestId('inspector-open-pr-policy')).toBeNull();
  });

  it('preserves an explicit require-existing policy when the node is rendered', () => {
    renderNode(openPrNode('require_existing'));
    expect((screen.getByTestId('inspector-open-pr-policy') as HTMLSelectElement).value).toBe(
      'require_existing',
    );
  });
});

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
    // #1219 (review feedback): timeout is orchestrator policy, not a
    // harness capability, so it ALWAYS renders — even with no provider
    // selected. The previous `caps &&` gate hid the most-common
    // authoring mode behind a capability the user had to opt into.
    expect(screen.getByTestId('inspector-timeout')).toBeTruthy();
  });

  it('renders model + closed-effort + extra-args + timeout when Claude Code is selected', async () => {
    renderNode(spawnNode({ provider: 'anthropic' }));
    expect(screen.getByTestId('inspector-model-input')).toBeTruthy();
    expect(screen.getByTestId('inspector-effort-select')).toBeTruthy();
    expect(screen.getByTestId('inspector-extra-args-input')).toBeTruthy();
    // #1219: timeout always renders (orchestrator policy), independent
    // of harness selection.
    expect(screen.getByTestId('inspector-timeout')).toBeTruthy();
    // Closed vocabulary: low / medium / high
    const effortSelect = screen.getByTestId(
      'inspector-effort-select',
    ) as HTMLSelectElement;
    const options = Array.from(effortSelect.options).map((o) => o.value);
    expect(options).toContain('low');
    expect(options).toContain('medium');
    expect(options).toContain('high');
  });

  it('renders model + inline-config effort + extra-args + timeout when Codex is selected', () => {
    renderNode(spawnNode({ provider: 'codex' }));
    expect(screen.getByTestId('inspector-model-input')).toBeTruthy();
    expect(screen.getByTestId('inspector-effort-select')).toBeTruthy();
    expect(screen.getByTestId('inspector-extra-args-input')).toBeTruthy();
    // #1219: timeout always renders; the harness dropdown is unrelated
    // to its visibility.
    expect(screen.getByTestId('inspector-timeout')).toBeTruthy();
    const effortSelect = screen.getByTestId(
      'inspector-effort-select',
    ) as HTMLSelectElement;
    const options = Array.from(effortSelect.options).map((o) => o.value);
    // Codex vocabulary (inline_config): none | low | medium | high | xhigh
    expect(options).toEqual(
      expect.arrayContaining(['none', 'low', 'medium', 'high', 'xhigh']),
    );
  });

  it('renders model + extra-args + timeout but NO effort when OpenCode is selected (no effort control)', () => {
    renderNode(spawnNode({ provider: 'opencode' }));
    expect(screen.getByTestId('inspector-model-input')).toBeTruthy();
    expect(screen.getByTestId('inspector-extra-args-input')).toBeTruthy();
    // OpenCode has EffortControlKind::None — no dropdown, but timeout
    // (#1219) is independent of the effort capability and still renders.
    expect(screen.queryByTestId('inspector-effort-select')).toBeNull();
    expect(screen.getByTestId('inspector-timeout')).toBeTruthy();
  });

  it('renders model + closed-effort + extra-args + timeout when Command Code is selected', () => {
    renderNode(spawnNode({ provider: 'commandcode' }));
    expect(screen.getByTestId('inspector-model-input')).toBeTruthy();
    expect(screen.getByTestId('inspector-effort-select')).toBeTruthy();
    expect(screen.getByTestId('inspector-extra-args-input')).toBeTruthy();
    // #1219: timeout is independent of the harness capability set.
    expect(screen.getByTestId('inspector-timeout')).toBeTruthy();
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
  // Provider-specific overrides must not survive a harness switch
  // (#1362): the serialised circuit JSON would carry values the new
  // harness can't honour, and the capability mask would silently drop
  // them at spawn time. `timeout_seconds` is the exception — it's
  // orchestrator-level policy, not a harness capability, so the
  // authored budget must persist across switches.
  it('clears model/effort/extra_args but PRESERVES timeout_seconds on provider switch', () => {
    const onChange = vi.fn();
    // Codex row with Anthropic-incompatible overrides set.
    renderNode(
      spawnNode({
        provider: 'codex',
        model: 'gpt-5',
        effort: 'xhigh',
        extra_args: '--no-confirm',
        timeout_seconds: 1800,
      }),
      onChange,
    );
    // User switches to Anthropic via the dropdown.
    const select = screen.getByTestId('inspector-provider-select');
    fireEvent.change(select, { target: { value: 'anthropic' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    // Provider is now Anthropic; the harness-specific overrides are
    // cleared so a stale value can't sneak through the capability mask
    // at spawn time.
    expect((lastCall as { provider: string }).provider).toBe('anthropic');
    expect((lastCall as { model: string | null }).model).toBeNull();
    expect((lastCall as { effort: string | null }).effort).toBeNull();
    expect((lastCall as { extra_args: string | null }).extra_args).toBeNull();
    // #1219 (review feedback): timeout persists across the switch.
    // The future step-level watchdog is buildmesh-wide, so clearing
    // the value here would destroy harness-agnostic user data.
    expect((lastCall as { timeout_seconds: number | null }).timeout_seconds).toBe(1800);
  });

  // #1219: the timeout input is nullable — clearing the field commits
  // `null` upward (inherit the orchestrator default), distinct from
  // "type a number". A populated value commits the number; an empty
  // input commits null. Both transitions must reach the AST.
  it('commits the timeout value upward when the user types a number', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '900' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { timeout_seconds: number | null }).timeout_seconds).toBe(900);
  });

  it('commits null when the user clears the timeout input', () => {
    const onChange = vi.fn();
    renderNode(
      spawnNode({ provider: 'anthropic', timeout_seconds: 900 }),
      onChange,
    );
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { timeout_seconds: number | null }).timeout_seconds).toBeNull();
  });

  // #1219 (review feedback): a typed `0` is the inspector's affordance
  // for "inherit default" — the resolver collapses `Some(0)` to `None`
  // at the seam (`resolve_circuit_spawn_inputs`). Without this commit
  // path the `min={1}` clamp would coerce the typed `0` to `1` behind
  // the user's back.
  it('commits null when the user types 0 in the timeout field', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '0' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { timeout_seconds: number | null }).timeout_seconds).toBeNull();
  });

  // #1219 (review feedback): values above MAX_STEP_TIMEOUT_SECONDS are
  // clamped at the inspector so the AST never carries a value the
  // Rust `validate()` would reject at save time.
  it('clamps the typed timeout to MAX_STEP_TIMEOUT_SECONDS', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '9999999999' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { timeout_seconds: number | null }).timeout_seconds).toBe(604_800);
  });

  // #1219 (review feedback): the input advertises `max=604800` so
  // assistive tech + form validation surface the upper bound, even
  // though our `onChange` clamp is the actual enforcement.
  it('advertises MAX_STEP_TIMEOUT_SECONDS as the max attribute', () => {
    renderNode(spawnNode({ provider: 'anthropic' }));
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    expect(input.max).toBe('604800');
  });

  // #1219 (review feedback, round 2): the inspector's onChange clamp
  // is the actual enforcement — `min={1}` and `max={...}` attributes
  // are advisory (typing bypasses them). Pin every boundary case so a
  // future regression that lets a value escape the clamp is caught at
  // the input layer, NOT at save time with a generic JSON error from
  // the Rust `validate()`.
  it('clamps a typed negative value up to the min (1)', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '-5' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { timeout_seconds: number | null }).timeout_seconds).toBe(1);
  });

  // Fractions (3.14) silently truncate to the integer floor so the AST
  // carries a value the Rust `Option<u32>` deserialiser accepts. Pin
  // the floor (3), not a round — half values round inconsistently
  // across implementations and the floor matches the inspector's
  // stated "integer" semantic.
  it('truncates a typed fractional value to its integer floor', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '3.14' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect((lastCall as { timeout_seconds: number | null }).timeout_seconds).toBe(3);
  });

  // Whitespace-only input is silently ignored — `Number(' ')` is 0,
  // which would route through the nullable+0 collapse to `null`. The
  // user-typed intent for a cleared whitespace string is "blank" not
  // "0", so reject the keystroke rather than collapse. The
  // `nullable && raw === ''` branch handles the explicit empty
  // string; this test pins the non-empty-but-whitespace stub.
  it('ignores a whitespace-only typed value (does not commit null or zero)', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '   ' } });
    expect(onChange).not.toHaveBeenCalled();
  });

  // #1219 (review feedback): the save-time collapse `Some(0) → None`
  // lives in `resolve_circuit_spawn_inputs` (Rust). The inspector
  // mirrors it at commit time so the typed 0 never reaches the AST.
  // Already covered by `commits null when the user types 0 in the
  // timeout field` above; this test pins the boundary at exactly 0
  // for the record.
  it('collapses typed zero to null (not Some(0))', () => {
    const onChange = vi.fn();
    renderNode(spawnNode({ provider: 'anthropic' }), onChange);
    const input = screen.getByTestId('inspector-timeout') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '0' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect((lastCall as { timeout_seconds: number | null }).timeout_seconds).toBeNull();
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

// ---------------------------------------------------------------------------
// #1219 review feedback: Trigger type select for root nodes.
//
// #1219 collapsed the Probe tab's New Circuit row to name + blueprint only,
// on the promise that trigger authoring moved "fully into the canvas
// inspector" — but the inspector's Trigger type select was missing. The
// review blueprint (and every non-Manual trigger) was therefore
// unreachable from the UI. These tests pin the inspector-level seam
// that fixes the dead end: root trigger nodes expose a Trigger type
// select, and switching it rewires the kind via `defaultKind`.
// ---------------------------------------------------------------------------

describe('InspectorPanel — Trigger type select for root nodes (#1219)', () => {
  function manualTriggerNode(): CircuitNode {
    return { id: 'trigger', type: { type: 'manual' } };
  }

  it('renders a Trigger type select on a Manual trigger', () => {
    renderNode(manualTriggerNode());
    const select = screen.getByTestId('inspector-trigger-type-select') as HTMLSelectElement;
    expect(select).toBeTruthy();
    expect(select.value).toBe('manual');
    const options = Array.from(select.options).map((o) => o.value);
    expect(options).toEqual([
      'manual',
      'interval',
      'github_issue_label',
      'github_pull_request_label',
    ]);
  });

  it('renders a Trigger type select on an Interval trigger', () => {
    renderNode({ id: 't', type: { type: 'interval', interval_seconds: 60 } });
    const select = screen.getByTestId('inspector-trigger-type-select') as HTMLSelectElement;
    expect(select.value).toBe('interval');
  });

  it('renders a Trigger type select on a github_issue_label trigger', () => {
    renderNode({ id: 't', type: { type: 'github_issue_label', label: 'buildmesh:run' } });
    const select = screen.getByTestId('inspector-trigger-type-select') as HTMLSelectElement;
    expect(select.value).toBe('github_issue_label');
  });

  it('renders a Trigger type select on a github_pull_request_label trigger', () => {
    renderNode({ id: 't', type: { type: 'github_pull_request_label', label: 'buildmesh:review' } });
    const select = screen.getByTestId('inspector-trigger-type-select') as HTMLSelectElement;
    expect(select.value).toBe('github_pull_request_label');
  });

  it('switching Manual → GithubIssueLabel rewires to a fresh default kind', () => {
    const onChange = vi.fn();
    renderNode(manualTriggerNode(), onChange);
    const select = screen.getByTestId('inspector-trigger-type-select') as HTMLSelectElement;
    fireEvent.change(select, { target: { value: 'github_issue_label' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toBeDefined();
    expect(lastCall).toEqual({ type: 'github_issue_label', label: '' });
  });

  it('switching Manual → Interval rewires to a fresh default kind', () => {
    const onChange = vi.fn();
    renderNode(manualTriggerNode(), onChange);
    const select = screen.getByTestId('inspector-trigger-type-select') as HTMLSelectElement;
    fireEvent.change(select, { target: { value: 'interval' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toEqual({ type: 'interval', interval_seconds: 300 });
  });

  it('switching Manual → GithubPullRequestLabel rewires to a fresh default kind', () => {
    const onChange = vi.fn();
    renderNode(manualTriggerNode(), onChange);
    const select = screen.getByTestId('inspector-trigger-type-select') as HTMLSelectElement;
    fireEvent.change(select, { target: { value: 'github_pull_request_label' } });
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(lastCall).toEqual({ type: 'github_pull_request_label', label: '' });
  });

  it('does NOT render the Trigger type select on non-trigger kinds', () => {
    // Spawn / action nodes don't have a trigger discriminator — the
    // Trigger type select would be misleading. Confirm it stays out of
    // every non-trigger category.
    const nonTriggerKinds: CircuitNode['type'][] = [
      { type: 'spawn_agent_node', prompt: 'p', name: null, provider: null, model: null, effort: null, extra_args: null, timeout_seconds: null },
      { type: 'inject_pty', prompt: 'p', target_node_id: null },
      { type: 'notify', message: 'm' },
      { type: 'set_node_status', status: 'completed', target_node_id: null },
      { type: 'close_agent_node', target_node_id: null },
      { type: 'all_completed' },
      { type: 'any_completed' },
    ];
    for (const kind of nonTriggerKinds) {
      const { unmount } = renderNode({ id: 'n', type: kind });
      expect(
        screen.queryByTestId('inspector-trigger-type-select'),
        `Trigger type select must not render for ${kind.type}`,
      ).toBeNull();
      unmount();
    }
  });

  it('after switching Manual → GithubIssueLabel, the label input is editable', () => {
    // The dead-end scenario from the review: user opens a circuit,
    // switches the trigger to GitHubIssueLabel, then needs to set the
    // label. Pin that the label input is editable after the switch.
    const onChange = vi.fn();
    renderNode(manualTriggerNode(), onChange);
    const select = screen.getByTestId('inspector-trigger-type-select') as HTMLSelectElement;
    fireEvent.change(select, { target: { value: 'github_issue_label' } });
    // The onChange call replaces the kind; re-render with the new kind
    // and the same onChange so the label-input change is captured.
    const newKind = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    renderNode({ id: 'trigger', type: newKind }, onChange);
    const labelInput = screen.getByTestId('inspector-trigger-label') as HTMLInputElement;
    expect(labelInput).toBeTruthy();
    fireEvent.change(labelInput, { target: { value: 'buildmesh:run' } });
    const labelCall = onChange.mock.calls[onChange.mock.calls.length - 1]?.[0];
    expect(labelCall).toEqual({ type: 'github_issue_label', label: 'buildmesh:run' });
  });
});
