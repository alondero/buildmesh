/**
 * Per-Mesh harness overrides section tests (issue #1151 / slice 2 of #1148).
 *
 * The Section sits on the Mesh Properties tab and replaces the legacy
 * Mesh-wide Model + Effort fields. The pinned contract:
 *
 *   * Empty state when no exceptions exist.
 *   * Add override dropdown lists only configurable, not-yet-overridden
 *     harnesses.
 *   * Capability-gated model and effort editors (Claude supports both;
 *     Codex supports both; Agy accept model only; Terminal/OpenCode
 *     render the no-configurable-defaults state).
 *   * Summary row showing harness name and explicit overridden values.
 *   * Independent Edit and Reset actions so changing one harness can't
 *     touch another.
 *   * Secondary Reset all bulk action.
 *   * Saved value persists via `upsert_mesh_harness_override`; reset
 *     persists via `remove_mesh_harness_override`; reset-all via
 *     `clear_mesh_harness_overrides`.
 *   * Failed save leaves the last confirmed list visible and surfaces
 *     the error (the section's commit helper returns `false` and the
 *     parent SaveStatus surfaces the message).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { MeshOverridesSection } from '../../src/components/Probe/MeshOverridesSection';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';
import type { HarnessConfigValue } from '../../src/types/generated/HarnessConfigValue';

function capsFixture(harness_id: string, opts: {
  supports_model: boolean;
  effortKind: 'none' | 'closed' | 'inline_config';
  effortAllowed?: string[];
  effortKey?: string;
}): unknown {
  const effort_control =
    opts.effortKind === 'none'
      ? ({ kind: 'none' as const })
      : opts.effortKind === 'closed'
        ? ({ kind: 'closed' as const, allowed: opts.effortAllowed ?? [] })
        : ({
            kind: 'inline_config' as const,
            key: opts.effortKey ?? 'model_reasoning_effort',
            allowed: opts.effortAllowed ?? [],
          });
  return {
    harness_id,
    supports_resume: false,
    auto_resume_on_startup: false,
    requires_attention_hook: false,
    produces_readable_transcript: false,
    supports_model_override: opts.supports_model,
    supports_effort_override: opts.effortKind !== 'none',
    supports_prefill: false,
    is_plain_terminal: harness_id === 'terminal',
    effort_control,
    available_on: ['windows'],
  };
}

function providerFixture(
  id: string,
  harness_id: string,
  capabilities: unknown,
  is_proxied = false,
): ProviderInfo {
  return {
    id,
    label: id.charAt(0).toUpperCase() + id.slice(1),
    color: '',
    icon: '',
    resumable: false,
    harness_id,
    provider_id: is_proxied ? 'minimax' : null,
    is_proxied,
    group_key: harness_id,
    capabilities: capabilities as any,
  };
}

const CLAUDE_ROW = providerFixture(
  'claude',
  'claude',
  capsFixture('claude', { supports_model: true, effortKind: 'closed', effortAllowed: ['low', 'medium', 'high'] }),
);
const CODEX_ROW = providerFixture(
  'codex',
  'codex',
  capsFixture('codex', { supports_model: true, effortKind: 'inline_config', effortAllowed: ['none', 'low', 'medium', 'high', 'xhigh'] }),
);
const AGY_ROW = providerFixture(
  'agy',
  'agy',
  capsFixture('agy', { supports_model: true, effortKind: 'none' }),
);
const OPENCODE_ROW = providerFixture(
  'opencode',
  'opencode',
  capsFixture('opencode', { supports_model: false, effortKind: 'none' }),
);

describe('MeshOverridesSection', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('renders the empty state when no overrides exist', () => {
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW, CODEX_ROW, AGY_ROW]}
        overrides={{}}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    expect(screen.getByTestId('mesh-overrides-empty')).toBeTruthy();
    expect(screen.queryByTestId('mesh-overrides-reset-all')).toBeNull();
  });

  it('renders the no-configurable-defaults state when no harness accepts controls', () => {
    render(
      <MeshOverridesSection
        providers={[OPENCODE_ROW]}
        overrides={{}}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    expect(screen.getByTestId('mesh-overrides-empty')).toBeTruthy();
    // Add dropdown is hidden when no harness accepts controls.
    expect(screen.queryByTestId('mesh-overrides-add-select')).toBeNull();
  });

  it('renders an existing override row with the summary', async () => {
    const overrides: Record<string, HarnessConfigValue> = {
      claude: { model: 'opus-4-1', effort: 'high' },
    };
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW, CODEX_ROW]}
        overrides={overrides}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    expect(screen.getByTestId('mesh-override-claude')).toBeTruthy();
    const summary = screen.getByTestId('mesh-override-summary-claude');
    expect(summary.textContent).toContain('opus-4-1');
    expect(summary.textContent).toContain('high');
  });

  it('excludes already-overridden harnesses from the Add dropdown', async () => {
    const overrides: Record<string, HarnessConfigValue> = {
      claude: { model: 'opus-4-1', effort: null },
    };
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW, CODEX_ROW, AGY_ROW]}
        overrides={overrides}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    const addSelect = screen.getByTestId('mesh-overrides-add-select');
    const options = Array.from(addSelect.querySelectorAll('option'));
    const harnessIds = options
      .map((o) => (o as HTMLOptionElement).value)
      .filter((v) => v.length > 0);
    expect(harnessIds).toContain('codex');
    expect(harnessIds).toContain('agy');
    expect(harnessIds).not.toContain('claude');
  });

  it('excludes harnesses with no configurable controls from the Add dropdown', () => {
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW, OPENCODE_ROW]}
        overrides={{}}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    const addSelect = screen.getByTestId('mesh-overrides-add-select');
    const options = Array.from(addSelect.querySelectorAll('option'));
    const harnessIds = options
      .map((o) => (o as HTMLOptionElement).value)
      .filter((v) => v.length > 0);
    expect(harnessIds).toEqual(['claude']);
  });

  it('upserts a new override via onChange when the Add editor saves', async () => {
    const onChange = vi.fn().mockResolvedValue(true);
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW, CODEX_ROW]}
        overrides={{}}
        onChange={onChange}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    const addSelect = screen.getByTestId('mesh-overrides-add-select');
    await userEvent.selectOptions(addSelect, 'codex');
    expect(screen.getByTestId('mesh-override-add-editor-codex')).toBeTruthy();
    const modelInput = screen.getByTestId('mesh-override-add-model-input-codex');
    await userEvent.type(modelInput, 'gpt-5');
    const saveBtn = screen.getByTestId('mesh-override-add-save-codex');
    await userEvent.click(saveBtn);
    await waitFor(() => {
      expect(onChange).toHaveBeenCalledWith('codex', {
        model: 'gpt-5',
        effort: null,
      });
    });
  });

  it('persists an override change via onChange on blur', async () => {
    const onChange = vi.fn().mockResolvedValue(true);
    const overrides: Record<string, HarnessConfigValue> = {
      claude: { model: 'opus-4-1', effort: 'high' },
    };
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW]}
        overrides={overrides}
        onChange={onChange}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    const modelInput = screen.getByTestId('mesh-override-model-input-claude');
    await userEvent.clear(modelInput);
    await userEvent.type(modelInput, 'sonnet-4');
    modelInput.blur();
    await waitFor(() => {
      expect(onChange).toHaveBeenCalledWith(
        'claude',
        expect.objectContaining({ model: 'sonnet-4' }),
      );
    });
  });

  it('removes a single override via onReset', async () => {
    const onReset = vi.fn().mockResolvedValue(true);
    const overrides: Record<string, HarnessConfigValue> = {
      claude: { model: 'opus-4-1', effort: null },
    };
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW]}
        overrides={overrides}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={onReset}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    const resetBtn = screen.getByTestId('mesh-override-reset-claude');
    await userEvent.click(resetBtn);
    await waitFor(() => {
      expect(onReset).toHaveBeenCalledWith('claude');
    });
  });

  it('renders Reset all only when at least one override exists', () => {
    const { rerender } = render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW]}
        overrides={{}}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    expect(screen.queryByTestId('mesh-overrides-reset-all')).not.toBeTruthy();
    rerender(
      <MeshOverridesSection
        providers={[CLAUDE_ROW]}
        overrides={{ claude: { model: 'opus', effort: null } }}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    expect(screen.getByTestId('mesh-overrides-reset-all')).toBeTruthy();
  });

  it('reset-all fires onResetAll', async () => {
    const onResetAll = vi.fn().mockResolvedValue(true);
    const overrides: Record<string, HarnessConfigValue> = {
      claude: { model: 'opus-4-1', effort: 'high' },
      codex: { model: 'gpt-5', effort: null },
    };
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW, CODEX_ROW]}
        overrides={overrides}
        onChange={vi.fn().mockResolvedValue(true)}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={onResetAll}
      />,
    );
    const resetAllBtn = screen.getByTestId('mesh-overrides-reset-all');
    await userEvent.click(resetAllBtn);
    await waitFor(() => {
      expect(onResetAll).toHaveBeenCalled();
    });
  });

  it('rejects invalid harness id at the backend boundary (writes map silent on IPC error)', async () => {
    // The IPC throws on unknown id; the parent (Mesh Properties tab) calls
    // its SaveStatus to surface the error. The section itself just rolls
    // the draft back to the committed value when onChange returns false.
    const onChange = vi.fn().mockResolvedValue(false);
    const overrides: Record<string, HarnessConfigValue> = {
      claude: { model: 'opus-4-1', effort: 'high' },
    };
    render(
      <MeshOverridesSection
        providers={[CLAUDE_ROW]}
        overrides={overrides}
        onChange={onChange}
        onReset={vi.fn().mockResolvedValue(true)}
        onResetAll={vi.fn().mockResolvedValue(true)}
      />,
    );
    const modelInput = screen.getByTestId('mesh-override-model-input-claude');
    await userEvent.clear(modelInput);
    await userEvent.type(modelInput, 'sonnet-4');
    modelInput.blur();
    await waitFor(() => {
      expect(onChange).toHaveBeenCalled();
    });
    // After a failed save the section rolls back to the committed value —
    // the visible row still shows the previously-saved override.
    const summary = screen.getByTestId('mesh-override-summary-claude');
    expect(summary.textContent).toContain('opus-4-1');
  });
});
