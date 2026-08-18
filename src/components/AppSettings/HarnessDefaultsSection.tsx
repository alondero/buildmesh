import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ProviderInfo } from '../../lib/tauri';
import type { HarnessConfigValue } from '../../types/generated/HarnessConfigValue';
import type { EffortControlKind } from '../../types/generated/EffortControlKind';
import { ProviderIcon } from '../Providers/ProviderIcon';

/** A single harness's draft state. `committed` is the last value the
 *  backend confirmed saved; `draft` is the in-flight edit. `dirty` is the
 *  edit-diverged-from-committed signal that drives the modal's discard-
 *  confirm and the amber dot on the General pane nav rail. */
interface HarnessDraft {
  committed: HarnessConfigValue;
  draft: HarnessConfigValue;
  dirty: boolean;
}

interface HarnessDefaultsSectionProps {
  /** All Spawn-Menu rows (native + proxied). The section dedupes by
   *  `harness_id` so each Agent Harness renders exactly one card — a
   *  Proxied Provider pairing uses the parent's capability descriptor, so
   *  rendering it twice would only duplicate the same form. */
  providers: ProviderInfo[];
  /** The current sparse map keyed by harness profile id (matches the
   *  `harness_id` on the Spawn-Menu native row). An empty entry means
   *  "no application default" — the card starts blank + dirty=false. */
  defaults: Record<string, HarnessConfigValue>;
  /** Persist the new draft for `harnessId`. Returns `true` on success so
   *  the card can refresh its committed-snapshot; `false` on failure so
   *  the card rolls the draft back and lets the parent surface the
   *  error. */
  onChange: (harnessId: string, value: HarnessConfigValue) => Promise<boolean>;
  /** Clear the stored default for `harnessId`. Same return contract as
   *  `onChange`. */
  onReset: (harnessId: string) => Promise<boolean>;
  /** Mirror the dirty state of any card to the modal so an Escape or
   *  backdrop click is intercepted by the discard banner (issue #730). */
  onDirtyChange?: (dirty: boolean) => void;
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
 *  with a native row). Terminal still renders so the user sees the
 *  "no-configurable-defaults" state for it; otherwise the missing form
 *  would feel like a bug. */
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

/** App-wide Agent Harness defaults — one card per native Agent Harness
 *  (issue #1150 / #1148). The capability descriptor drives the visible
 *  controls: a harness with `supports_model_override = false` and
 *  `EffortControlKind::None` renders the no-configurable-defaults state
 *  rather than an empty form.
 *
 *  Save / reset semantics mirror the existing App Settings fields
 *  (issue #730 / #581): optimistic save with rollback on failure, save
 *  commits the draft into the `committed` snapshot the card renders
 *  from, and a reset drops the map entry via the same code path as the
 *  backend's `clear_harness_default` Tauri command. */
export function HarnessDefaultsSection({
  providers,
  defaults,
  onChange,
  onReset,
  onDirtyChange,
}: HarnessDefaultsSectionProps) {
  const harnesses = useMemo(() => uniqueNativeHarnesses(providers), [providers]);

  // Map: harness id → draft state. Re-keyed on `defaults` so an external
  // mutation (e.g. another component calling `clear_harness_default`) is
  // picked up: any draft that matches the new committed value clears
  // `dirty`, and any draft that doesn't is rolled back to the new
  // committed value.
  const [drafts, setDrafts] = useState<Record<string, HarnessDraft>>({});
  useEffect(() => {
    setDrafts((prev) => {
      const next: Record<string, HarnessDraft> = {};
      for (const h of harnesses) {
        const committed = defaults[h.harness_id] ?? EMPTY_DEFAULT;
        const existing = prev[h.harness_id];
        next[h.harness_id] = existing
          ? { ...existing, committed }
          : { committed, draft: committed, dirty: false };
      }
      return next;
    });
  }, [harnesses, defaults]);

  // Aggregate dirty → propagate to the modal. The function-form setState
  // bails out when the bool is unchanged, so a re-fire from a stable
  // callback is cheap.
  const dirtyRef = useRef(false);
  useEffect(() => {
    const anyDirty = Object.values(drafts).some((d) => d.dirty);
    if (dirtyRef.current === anyDirty) return;
    dirtyRef.current = anyDirty;
    onDirtyChange?.(anyDirty);
  }, [drafts, onDirtyChange]);

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
    async (harnessId: string) => {
      const current = drafts[harnessId];
      if (!current || !current.dirty) return;
      const ok = await onChange(harnessId, current.draft);
      if (ok) {
        setDrafts((prev) => ({
          ...prev,
          [harnessId]: { ...current, committed: current.draft, dirty: false },
        }));
      } else {
        // Roll back: keep the committed value visible, drop the dirty
        // signal so the modal's discard banner clears. The parent
        // already surfaced the error.
        setDrafts((prev) => ({
          ...prev,
          [harnessId]: { ...current, draft: current.committed, dirty: false },
        }));
      }
    },
    [drafts, onChange],
  );

  const reset = useCallback(
    async (harnessId: string) => {
      const current = drafts[harnessId];
      if (!current) return;
      const ok = await onReset(harnessId);
      const cleared: HarnessConfigValue = { model: null, effort: null };
      if (ok) {
        setDrafts((prev) => ({
          ...prev,
          [harnessId]: { committed: cleared, draft: cleared, dirty: false },
        }));
      }
      // On failure the parent surfaces the error; we keep the existing
      // draft intact so the user can retry rather than lose their edit.
    },
    [drafts, onReset],
  );

  return (
    <div className="space-y-4" data-testid="harness-defaults-section">
      <div>
        <h3 className="text-xl font-semibold text-text-primary mb-2">Agent Harness defaults</h3>
        <p className="text-base text-text-muted mb-4">
          Buildmesh-wide defaults for each Agent Harness. New Agent Nodes inherit these unless a
          Mesh or per-launch override wins the cascade. Resetting a harness removes its default —
          the harness then runs with its native configuration.
        </p>
      </div>
      {harnesses.map((h) => (
        <HarnessDefaultCard
          key={h.harness_id}
          provider={h}
          draft={drafts[h.harness_id] ?? { committed: EMPTY_DEFAULT, draft: EMPTY_DEFAULT, dirty: false }}
          onUpdate={(patch) => updateDraft(h.harness_id, patch)}
          onCommit={() => commit(h.harness_id)}
          onReset={() => reset(h.harness_id)}
        />
      ))}
    </div>
  );
}

function HarnessDefaultCard({
  provider,
  draft,
  onUpdate,
  onCommit,
  onReset,
}: {
  provider: ProviderInfo;
  draft: HarnessDraft;
  onUpdate: (patch: Partial<HarnessConfigValue>) => void;
  onCommit: () => void | Promise<void>;
  onReset: () => void | Promise<void>;
}) {
  const caps = provider.capabilities;
  const showModel = caps.supports_model_override;
  const allowed = effortAllowed(caps.effort_control);
  const showEffort = allowed !== null;
  const hasAnyControl = showModel || showEffort;
  const stored = draft.committed.model !== null || draft.committed.effort !== null;

  return (
    <div
      className="border border-border-subtle rounded-lg p-5"
      data-testid={`harness-default-${provider.harness_id}`}
      data-has-stored-default={stored ? 'true' : 'false'}
    >
      <div className="flex items-center gap-3 mb-3">
        <ProviderIcon providerId={provider.harness_id} className="h-6 w-6" />
        <span className="text-lg font-medium text-text-primary">{provider.label}</span>
        {stored && (
          <button
            type="button"
            onClick={() => void onReset()}
            className="ml-auto px-3 py-1 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25"
            aria-label={`Reset ${provider.label} defaults`}
            data-testid={`harness-default-reset-${provider.harness_id}`}
          >
            Reset
          </button>
        )}
      </div>

      {!hasAnyControl ? (
        <p className="text-base text-text-muted italic" data-testid={`harness-default-empty-${provider.harness_id}`}>
          {provider.label} does not accept model or effort overrides from Buildmesh — it uses its own native configuration.
        </p>
      ) : (
        <div className="space-y-3">
          {showModel && (
            <div>
              <label
                htmlFor={`harness-default-model-${provider.harness_id}`}
                className="block text-sm text-text-muted mb-1"
              >
                Default model
              </label>
              <input
                id={`harness-default-model-${provider.harness_id}`}
                type="text"
                value={draft.draft.model ?? ''}
                placeholder={provider.harness_id === 'codex' ? 'model id' : 'model id'}
                onChange={(e) => onUpdate({ model: e.target.value || null })}
                onBlur={() => void onCommit()}
                className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                aria-label={`${provider.label} default model`}
                data-testid={`harness-default-model-input-${provider.harness_id}`}
              />
            </div>
          )}
          {showEffort && allowed && (
            <div>
              <label
                htmlFor={`harness-default-effort-${provider.harness_id}`}
                className="block text-sm text-text-muted mb-1"
              >
                {effortKey(caps.effort_control) === 'model_reasoning_effort'
                  ? 'Reasoning effort'
                  : 'Effort'}
              </label>
              <select
                id={`harness-default-effort-${provider.harness_id}`}
                value={draft.draft.effort ?? ''}
                onChange={(e) => onUpdate({ effort: e.target.value || null })}
                onBlur={() => void onCommit()}
                className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                aria-label={`${provider.label} effort`}
                data-testid={`harness-default-effort-select-${provider.harness_id}`}
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
          {draft.dirty && (
            <p className="text-sm text-status-warning" data-testid={`harness-default-dirty-${provider.harness_id}`}>
              Unsaved changes — will save on blur.
            </p>
          )}
        </div>
      )}
    </div>
  );
}
