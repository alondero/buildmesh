/**
 * MeshOverridesSection — the per-Mesh harness override list (issue #1151 /
 * slice 2 of #1148).
 *
 * Replaces the legacy Mesh-wide Model + Effort controls (issue #1151 step 8)
 * with a sparse override list:
 *
 *   - Empty state when no exceptions exist.
 *   - Add override action that lists only configurable, not-yet-overridden
 *     harnesses.
 *   - Capability-gated editor for model and effort (matches the
 *     HarnessDefaultsSection render).
 *   - Summary row showing harness name and explicit overridden values.
 *   - Independent Edit and Reset actions so changing one harness can't
 *     touch another.
 *   - Secondary Reset all overrides action.
 *
 * The cascade order at the spawn seam is now:
 *   explicit > mesh_override > mesh (legacy) > application > native
 * so an override on this Mesh wins over the application-level default for
 * the same harness; resetting an override restores inheritance rather
 * than persisting blank values (issue #1151 acceptance criteria 11).
 *
 * Save semantics mirror the App Settings `HarnessDefaultsSection`:
 * optimistic per-entry save with rollback on failure, save commits the
 * draft into the per-harness `committed` snapshot the row renders from,
 * and a reset drops the map entry via the same path the backend's
 * `remove_mesh_harness_override` / `clear_mesh_harness_overrides` Tauri
 * commands own.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ProviderInfo } from '../../lib/tauri';
import type { HarnessConfigValue } from '../../types/generated/HarnessConfigValue';
import type { EffortControlKind } from '../../types/generated/EffortControlKind';
import { ProviderIcon } from '../Providers/ProviderIcon';

type HarnessDraft = {
  committed: HarnessConfigValue;
  draft: HarnessConfigValue;
  dirty: boolean;
};

interface MeshOverridesSectionProps {
  /** All Spawn-Menu rows (native + proxied). The section dedupes by
   *  `harness_id` so each Agent Harness renders exactly one summary row —
   *  a Proxied Provider pairing uses the parent's capability descriptor, so
   *  rendering it twice would only duplicate the same form. */
  providers: ProviderInfo[];
  /** The current sparse map keyed by harness profile id. An empty entry
   *  means "no override on this Mesh" — the resolver falls through to the
   *  application default and then native behaviour for that harness. */
  overrides: Record<string, HarnessConfigValue>;
  /** Persist the new draft for `harnessId`. Returns `true` on success so
   *  the row can refresh its committed-snapshot; `false` on failure so the
   *  row rolls the draft back and lets the parent surface the error. */
  onChange: (harnessId: string, value: HarnessConfigValue) => Promise<boolean>;
  /** Clear the stored override for `harnessId`. Same return contract as
   *  `onChange`. */
  onReset: (harnessId: string) => Promise<boolean>;
  /** Reset every override on this Mesh. Same return contract. */
  onResetAll: () => Promise<boolean>;
}

const EMPTY_DEFAULT: HarnessConfigValue = { model: null, effort: null };

function effortAllowed(kind: EffortControlKind): string[] | null {
  switch (kind.kind) {
    case 'closed':
      return kind.allowed;
    case 'inline_config':
      return kind.allowed;
    case 'none':
    default:
      return null;
  }
}

function effortKey(kind: EffortControlKind): string | null {
  return kind.kind === 'inline_config' ? kind.key : null;
}

/** Stable merge: providers → unique native harness ids (preserve the
 *  Spawn-Menu's harness-order, drop proxied rows that share a harness id
 *  with a native row). Same dedupe pattern as
 *  `HarnessDefaultsSection::uniqueNativeHarnesses`. */
function uniqueNativeHarnesses(providers: ProviderInfo[]): ProviderInfo[] {
  const seen = new Set<string>();
  const out: ProviderInfo[] = [];
  for (const p of providers) {
    if (seen.has(p.harness_id)) continue;
    seen.add(p.harness_id);
    out.push(p);
  }
  return out;
}

/** A harness the user can add as a new override — must (a) have at
 *  least one configurable control (model or effort) and (b) not already
 *  be overridden on this Mesh. The returned list is the Add-override
 *  dropdown's options. Defensive about a missing `capabilities` (a
 *  pre-#1149 Spawn Menu row didn't carry the descriptor) — a missing
 *  descriptor hides the harness from the Add dropdown rather than
 *  crashing the render. */
function addableHarnesses(
  harnesses: ProviderInfo[],
  overrides: Record<string, HarnessConfigValue>,
): ProviderInfo[] {
  return harnesses.filter((h) => {
    if (overrides[h.harness_id]) return false;
    const caps = h.capabilities;
    if (!caps) return false;
    const hasAnyControl =
      caps.supports_model_override ||
      effortAllowed(caps.effort_control) !== null;
    if (!hasAnyControl) return false;
    return true;
  });
}

/** Summary text shown on a saved override row — the explicit overridden
 *  values the user can audit without opening the editor. */
function summary(committed: HarnessConfigValue): string {
  const parts: string[] = [];
  if (committed.model) parts.push(`model: ${committed.model}`);
  if (committed.effort) parts.push(`effort: ${committed.effort}`);
  return parts.length > 0 ? parts.join(' · ') : '(no values)';
}

export function MeshOverridesSection({
  providers,
  overrides,
  onChange,
  onReset,
  onResetAll,
}: MeshOverridesSectionProps) {
  const harnesses = useMemo(() => uniqueNativeHarnesses(providers), [providers]);
  const overriddenIds = useMemo(
    () => Object.keys(overrides).sort(),
    [overrides],
  );

  // Map: harness id → draft state. Re-keyed on `overrides` so an external
  // mutation (e.g. another component calling `clear_mesh_harness_overrides`)
  // is picked up: any draft that matches the new committed value clears
  // `dirty`, and any draft that doesn't is rolled back to the new committed
  // value.
  const [drafts, setDrafts] = useState<Record<string, HarnessDraft>>({});
  useEffect(() => {
    setDrafts((prev) => {
      const next: Record<string, HarnessDraft> = {};
      for (const harnessId of overriddenIds) {
        const committed = overrides[harnessId] ?? EMPTY_DEFAULT;
        const existing = prev[harnessId];
        next[harnessId] = existing
          ? { ...existing, committed }
          : { committed, draft: committed, dirty: false };
      }
      return next;
    });
  }, [overriddenIds, overrides]);

  // The active "Adding override" harness id. Empty string = no selection
  // (the Add override dropdown is showing the harnesses to pick from).
  const [adding, setAdding] = useState<string>('');

  const updateDraft = useCallback((harnessId: string, patch: Partial<HarnessConfigValue>) => {
    setDrafts((prev) => {
      const current = prev[harnessId];
      if (!current) return prev;
      const draft = { ...current.draft, ...patch };
      const isDirty =
        (draft.model ?? null) !== (current.committed.model ?? null) ||
        (draft.effort ?? null) !== (current.committed.effort ?? null);
      return { ...prev, [harnessId]: { ...current, draft, dirty: isDirty } };
    });
  }, []);

  const commit = useCallback(
    async (harnessId: string, overrideDraft?: HarnessConfigValue): Promise<boolean> => {
      const current = drafts[harnessId];
      // `overrideDraft` lets the caller supply the freshly-edited value
      // when the commit fires from the same event handler that updated
      // the draft — otherwise the closure-captured `drafts` is stale
      // (React state updates are deferred to the next render).
      const draftToSubmit = overrideDraft ?? current?.draft;
      // Skip the save only when the caller didn't supply an override AND
      // the captured draft is clean. A caller-supplied override always
      // wins — the user is editing inline and we trust the explicit
      // value. This is the same pattern as the App Settings
      // HarnessDefaultsSection (issue #1150).
      if (!overrideDraft && (!current || !current.dirty)) return false;
      if (!draftToSubmit) return false;
      const ok = await onChange(harnessId, draftToSubmit);
      if (ok) {
        setDrafts((prev) => ({
          ...prev,
          [harnessId]: current
            ? { ...current, committed: draftToSubmit, dirty: false }
            : { committed: draftToSubmit, draft: draftToSubmit, dirty: false },
        }));
      } else if (current) {
        // Roll back: keep the committed value visible, drop the dirty
        // signal. The parent already surfaced the error.
        setDrafts((prev) => ({
          ...prev,
          [harnessId]: { ...current, draft: current.committed, dirty: false },
        }));
      }
      return ok;
    },
    [drafts, onChange],
  );

  const reset = useCallback(
    async (harnessId: string) => {
      const current = drafts[harnessId];
      if (!current) return;
      const ok = await onReset(harnessId);
      if (ok) {
        // Drop the entry from the draft map so the row unmounts cleanly.
        setDrafts((prev) => {
          const { [harnessId]: _removed, ...rest } = prev;
          return rest;
        });
      }
      // On failure the parent surfaces the error; we keep the existing
      // draft intact so the user can retry rather than lose their edit.
    },
    [drafts, onReset],
  );

  const startAdding = useCallback((harnessId: string) => {
    if (!harnessId) return;
    setAdding(harnessId);
    // Seed an empty draft so the editor renders immediately.
    setDrafts((prev) => {
      if (prev[harnessId]) return prev;
      return {
        ...prev,
        [harnessId]: { committed: EMPTY_DEFAULT, draft: EMPTY_DEFAULT, dirty: true },
      };
    });
  }, []);

  const cancelAdd = useCallback((harnessId: string) => {
    setAdding('');
    setDrafts((prev) => {
      const { [harnessId]: _removed, ...rest } = prev;
      return rest;
    });
  }, []);

  const addable = useMemo(
    () => addableHarnesses(harnesses, overrides),
    [harnesses, overrides],
  );

  const hasNoConfigurableHarnesses = addable.length === 0 && overriddenIds.length === 0;

  return (
    <div className="space-y-3" data-testid="mesh-overrides-section">
      <div className="flex items-baseline gap-3">
        <div className="flex-1">
          <h3 className="text-base font-semibold text-text-primary">Per-harness overrides</h3>
          <p className="text-xs text-text-muted mt-1">
            Exceptions that override the application-level defaults for this Mesh only. Editing
            one harness does not affect another. Reset removes the exception and restores
            application inheritance.
          </p>
        </div>
        {overriddenIds.length > 0 && (
          <button
            type="button"
            onClick={() => void onResetAll()}
            className="px-3 py-1 bg-status-error/10 text-status-error text-xs rounded-md hover:bg-status-error/20"
            data-testid="mesh-overrides-reset-all"
          >
            Reset all
          </button>
        )}
      </div>

      {overriddenIds.length === 0 && !adding && hasNoConfigurableHarnesses ? (
        <p
          className="text-xs text-text-muted italic"
          data-testid="mesh-overrides-empty"
        >
          No Agent Harness accepts model or effort overrides from Buildmesh.
        </p>
      ) : overriddenIds.length === 0 && !adding ? (
        <p
          className="text-xs text-text-muted italic"
          data-testid="mesh-overrides-empty"
        >
          No overrides. This Mesh inherits every application-level default.
        </p>
      ) : null}

      {overriddenIds.map((harnessId) => {
        const draft = drafts[harnessId];
        if (!draft) return null;
        const provider = harnesses.find((h) => h.harness_id === harnessId);
        if (!provider) return null;
        return (
          <OverrideRow
            key={harnessId}
            provider={provider}
            draft={draft}
            onUpdate={(patch) => updateDraft(harnessId, patch)}
            onCommit={(overrideDraft) => void commit(harnessId, overrideDraft)}
            onReset={() => void reset(harnessId)}
          />
        );
      })}

      {adding && drafts[adding] && (
        <AddOverrideEditor
          provider={harnesses.find((h) => h.harness_id === adding)!}
          draft={drafts[adding]}
          onUpdate={(patch) => updateDraft(adding, patch)}
          onCommit={async () => {
            const ok = await commit(adding);
            if (ok) {
              setAdding('');
            }
          }}
          onCancel={() => cancelAdd(adding)}
        />
      )}

      {!adding && addable.length > 0 && (
        <div className="flex items-center gap-2">
          <label
            htmlFor="mesh-overrides-add-select"
            className="text-xs text-text-muted"
          >
            Add override
          </label>
          <select
            id="mesh-overrides-add-select"
            value=""
            onChange={(e) => startAdding(e.target.value)}
            className="bg-bg-overlay border border-border-subtle rounded-md px-2 py-1 text-xs text-text-primary focus:outline-none focus:border-accent-cyan"
            data-testid="mesh-overrides-add-select"
          >
            <option value="">Choose a harness…</option>
            {addable.map((h) => (
              <option key={h.harness_id} value={h.harness_id}>
                {h.label}
              </option>
            ))}
          </select>
        </div>
      )}
    </div>
  );
}

function OverrideRow({
  provider,
  draft,
  onUpdate,
  onCommit,
  onReset,
}: {
  provider: ProviderInfo;
  draft: HarnessDraft;
  onUpdate: (patch: Partial<HarnessConfigValue>) => void;
  /** Pass an optional overrideDraft to submit the freshly-edited value
   *  when the commit fires from the same event handler that updated
   *  the draft. The closure-captured `drafts` is stale at that point. */
  onCommit: (overrideDraft?: HarnessConfigValue) => void | Promise<void>;
  onReset: () => void | Promise<void>;
}) {
  const caps = provider.capabilities;
  const showModel = caps.supports_model_override;
  const allowed = effortAllowed(caps.effort_control);
  const showEffort = allowed !== null;
  const hasAnyControl = showModel || showEffort;
  const summaryText = summary(draft.committed);

  return (
    <div
      className="border border-border-subtle rounded-lg p-3"
      data-testid={`mesh-override-${provider.harness_id}`}
    >
      <div className="flex items-center gap-2 mb-2">
        <ProviderIcon providerId={provider.harness_id} className="h-4 w-4" />
        <span className="text-sm font-medium text-text-primary">{provider.label}</span>
        <span
          className="text-xs text-text-muted"
          data-testid={`mesh-override-summary-${provider.harness_id}`}
        >
          {summaryText}
        </span>
        <button
          type="button"
          onClick={() => void onReset()}
          className="ml-auto px-2 py-1 bg-status-error/10 text-status-error text-xs rounded-md hover:bg-status-error/20"
          aria-label={`Reset ${provider.label} override`}
          data-testid={`mesh-override-reset-${provider.harness_id}`}
        >
          Reset
        </button>
      </div>

      {!hasAnyControl ? (
        <p className="text-xs text-text-muted italic">
          {provider.label} does not accept model or effort overrides from Buildmesh.
        </p>
      ) : (
        <div className="space-y-2">
          {showModel && (
            <div>
              <label
                htmlFor={`mesh-override-model-${provider.harness_id}`}
                className="block text-xs text-text-muted mb-1"
              >
                Model
              </label>
              <input
                id={`mesh-override-model-${provider.harness_id}`}
                type="text"
                value={draft.draft.model ?? ''}
                placeholder="model id"
                onChange={(e) => onUpdate({ model: e.target.value || null })}
                onBlur={() => void onCommit()}
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-xs text-text-primary focus:outline-none focus:border-accent-cyan"
                data-testid={`mesh-override-model-input-${provider.harness_id}`}
              />
            </div>
          )}
          {showEffort && allowed && (
            <div>
              <label
                htmlFor={`mesh-override-effort-${provider.harness_id}`}
                className="block text-xs text-text-muted mb-1"
              >
                {effortKey(caps.effort_control) === 'model_reasoning_effort'
                  ? 'Reasoning effort'
                  : 'Effort'}
              </label>
              <select
                id={`mesh-override-effort-${provider.harness_id}`}
                value={draft.draft.effort ?? ''}
                onChange={(e) => {
                  const next = { ...draft.draft, effort: e.target.value || null } as HarnessConfigValue;
                  onUpdate({ effort: e.target.value || null });
                  void onCommit(next);
                }}
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-xs text-text-primary focus:outline-none focus:border-accent-cyan"
                data-testid={`mesh-override-effort-select-${provider.harness_id}`}
              >
                <option value="">— inherit —</option>
                {allowed.map((v) => (
                  <option key={v} value={v}>
                    {v}
                  </option>
                ))}
              </select>
            </div>
          )}
          {draft.dirty && (
            <p
              className="text-xs text-status-warning"
              data-testid={`mesh-override-dirty-${provider.harness_id}`}
            >
              Unsaved changes — will save on blur.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

function AddOverrideEditor({
  provider,
  draft,
  onUpdate,
  onCommit,
  onCancel,
}: {
  provider: ProviderInfo;
  draft: HarnessDraft;
  onUpdate: (patch: Partial<HarnessConfigValue>) => void;
  onCommit: () => void | Promise<void>;
  onCancel: () => void;
}) {
  const caps = provider.capabilities;
  const showModel = caps.supports_model_override;
  const allowed = effortAllowed(caps.effort_control);
  const showEffort = allowed !== null;

  return (
    <div
      className="border border-accent-cyan/40 rounded-lg p-3 bg-accent-cyan/5"
      data-testid={`mesh-override-add-editor-${provider.harness_id}`}
    >
      <div className="flex items-center gap-2 mb-2">
        <ProviderIcon providerId={provider.harness_id} className="h-4 w-4" />
        <span className="text-sm font-medium text-text-primary">{provider.label}</span>
        <span className="text-xs text-text-muted">new override</span>
      </div>
      <div className="space-y-2">
        {showModel && (
          <div>
            <label
              htmlFor={`mesh-override-add-model-${provider.harness_id}`}
              className="block text-xs text-text-muted mb-1"
            >
              Model
            </label>
            <input
              id={`mesh-override-add-model-${provider.harness_id}`}
              type="text"
              value={draft.draft.model ?? ''}
              placeholder="model id"
              onChange={(e) => onUpdate({ model: e.target.value || null })}
              className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-xs text-text-primary focus:outline-none focus:border-accent-cyan"
              data-testid={`mesh-override-add-model-input-${provider.harness_id}`}
            />
          </div>
        )}
        {showEffort && allowed && (
          <div>
            <label
              htmlFor={`mesh-override-add-effort-${provider.harness_id}`}
              className="block text-xs text-text-muted mb-1"
            >
              {effortKey(caps.effort_control) === 'model_reasoning_effort'
                ? 'Reasoning effort'
                : 'Effort'}
            </label>
            <select
              id={`mesh-override-add-effort-${provider.harness_id}`}
              value={draft.draft.effort ?? ''}
              onChange={(e) => onUpdate({ effort: e.target.value || null })}
              className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-xs text-text-primary focus:outline-none focus:border-accent-cyan"
              data-testid={`mesh-override-add-effort-select-${provider.harness_id}`}
            >
              <option value="">— none —</option>
              {allowed.map((v) => (
                <option key={v} value={v}>
                  {v}
                </option>
              ))}
            </select>
          </div>
        )}
        <div className="flex items-center gap-2 pt-1">
          <button
            type="button"
            onClick={() => void onCommit()}
            className="px-3 py-1 bg-accent-cyan text-bg-base text-xs font-medium rounded-md hover:bg-accent-cyan/90"
            data-testid={`mesh-override-add-save-${provider.harness_id}`}
          >
            Save override
          </button>
          <button
            type="button"
            onClick={onCancel}
            className="px-3 py-1 bg-bg-overlay text-text-muted text-xs rounded-md hover:bg-bg-overlay/70"
            data-testid={`mesh-override-add-cancel-${provider.harness_id}`}
          >
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
