import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ProviderInfo } from '../../lib/tauri';
import type { HarnessConfigValue } from '../../types/generated/HarnessConfigValue';
import type { EffortControlKind } from '../../types/generated/EffortControlKind';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { SettingsSection } from './SettingsRow';

/** A single harness's draft state. `committed` is the last value the
 *  backend confirmed saved; `draft` is the in-flight edit. `dirty` is the
 *  edit-diverged-from-committed signal that drives the modal's discard-
 *  confirm and the amber dot on the Launch Configurations nav item. */
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
  /** When `true`, every input / select / button is disabled. Used by
   *  the parent to gate the section on a failed preferences load
   *  (issue #1534) — without it, a `defaults = {}` placeholder would
   *  be writable as if it were the real persisted state. */
  disabled?: boolean;
}

const EMPTY_DEFAULT: HarnessConfigValue = { model: null, effort: null, permission_mode: null };

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

/** Whether Buildmesh can set anything for this harness: a model override, an
 *  effort, or a permission mode (a single enforced mode is shown read-only). */
function hasConfigurableDefaults(provider: ProviderInfo): boolean {
  const caps = provider.capabilities;
  return (
    caps.supports_model_override ||
    effortAllowed(caps.effort_control) !== null ||
    (caps.permission_modes ?? []).length > 0
  );
}

/** Stable merge: providers → unique native harness ids (preserve the
 *  Spawn-Menu's harness-order, drop proxied rows that share a harness id
 *  with a native row). A harness with nothing to configure — Terminal, a CLI
 *  that takes no overrides — gets no card: a row that only says "nothing to
 *  set here" is clutter, not a signal. */
function uniqueNativeHarnesses(providers: ProviderInfo[]): ProviderInfo[] {
  const seen = new Set<string>();
  const out: ProviderInfo[] = [];
  for (const p of providers) {
    if (seen.has(p.harness_id)) continue;
    seen.add(p.harness_id);
    if (hasConfigurableDefaults(p)) out.push(p);
  }
  return out;
}

/** App-wide Agent Harness defaults — one card per native Agent Harness
 *  (issue #1150 / #1148). The capability descriptor drives the visible
 *  controls: a harness with `supports_model_override = false`,
 *  `EffortControlKind::None` and no permission modes has nothing to set and
 *  gets no card at all (see `hasConfigurableDefaults`).
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
  disabled = false,
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
        (draft.effort ?? null) !== (current.committed.effort ?? null) ||
        (draft.permission_mode ?? null) !== (current.committed.permission_mode ?? null);
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
      const cleared: HarnessConfigValue = { model: null, effort: null, permission_mode: null };
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
    <SettingsSection
      title="Agent Harness defaults"
      testId="harness-defaults-section"
      description={
        <>
          Buildmesh-wide defaults for each Agent Harness. New Agent Nodes inherit
          these unless a Mesh or per-launch override wins the cascade. Resetting a
          harness removes its default — it then runs with its native configuration.
        </>
      }
    >
      <div className="space-y-3">
        {harnesses.map((h) => (
          <HarnessDefaultCard
            key={h.harness_id}
            provider={h}
            draft={drafts[h.harness_id] ?? { committed: EMPTY_DEFAULT, draft: EMPTY_DEFAULT, dirty: false }}
            onUpdate={(patch) => updateDraft(h.harness_id, patch)}
            onCommit={() => commit(h.harness_id)}
            onReset={() => reset(h.harness_id)}
            disabled={disabled}
          />
        ))}
      </div>
    </SettingsSection>
  );
}

function HarnessDefaultCard({
  provider,
  draft,
  onUpdate,
  onCommit,
  onReset,
  disabled,
}: {
  provider: ProviderInfo;
  draft: HarnessDraft;
  onUpdate: (patch: Partial<HarnessConfigValue>) => void;
  onCommit: () => void | Promise<void>;
  onReset: () => void | Promise<void>;
  disabled: boolean;
}) {
  const caps = provider.capabilities;
  const showModel = caps.supports_model_override;
  const allowed = effortAllowed(caps.effort_control);
  const showEffort = allowed !== null;
  // Issue #2151: per-harness permission mode. Two or more modes render a
  // select; a single enforced mode (mcode's Full Access pin) renders as
  // read-only text. A harness with no permission flag renders nothing — a
  // missing control needs no apology.
  const modes = caps.permission_modes ?? [];
  const showPermissionSelect = modes.length > 1;
  const enforcedMode = modes.length === 1 ? modes[0] : null;
  const stored =
    draft.committed.model !== null ||
    draft.committed.effort !== null ||
    draft.committed.permission_mode !== null;
  const defaultMode = modes.find((m) => m.id === caps.default_permission_mode) ?? modes[0] ?? null;
  const selectedMode = modes.find((m) => m.id === (draft.draft.permission_mode ?? '')) ?? defaultMode;

  return (
    <div
      className="border border-border-subtle rounded-lg px-4 py-2.5"
      data-testid={`harness-default-${provider.harness_id}`}
      data-has-stored-default={stored ? 'true' : 'false'}
    >
      <div className="flex items-center gap-3">
        <ProviderIcon providerId={provider.harness_id} className="h-5 w-5 shrink-0" />
        <span className="shrink-0 text-base font-medium text-text-primary">{provider.label}</span>

        <div className="flex min-w-0 flex-1 flex-wrap items-center justify-end gap-2">
          {showModel && (
            <input
              id={`harness-default-model-${provider.harness_id}`}
              type="text"
              value={draft.draft.model ?? ''}
              placeholder="model id"
              onChange={(e) => onUpdate({ model: e.target.value || null })}
              onBlur={() => void onCommit()}
              disabled={disabled}
              className="w-44 bg-bg-input border border-border-default rounded-md px-3 py-1.5 text-base text-text-primary focus:outline-none focus:border-border-active disabled:opacity-50"
              aria-label={`${provider.label} default model`}
              data-testid={`harness-default-model-input-${provider.harness_id}`}
            />
          )}
          {showEffort && allowed && (
            <select
              id={`harness-default-effort-${provider.harness_id}`}
              value={draft.draft.effort ?? ''}
              onChange={(e) => onUpdate({ effort: e.target.value || null })}
              onBlur={() => void onCommit()}
              disabled={disabled}
              className="w-32 bg-bg-input border border-border-default rounded-md px-3 py-1.5 text-base text-text-primary focus:outline-none focus:border-border-active disabled:opacity-50"
              aria-label={`${provider.label} ${
                effortKey(caps.effort_control) === 'model_reasoning_effort' ? 'reasoning effort' : 'effort'
              }`}
              data-testid={`harness-default-effort-select-${provider.harness_id}`}
            >
              <option value="">— none —</option>
              {allowed.map((v) => (
                <option key={v} value={v}>
                  {v}
                </option>
              ))}
            </select>
          )}
          {draft.dirty && (
            <span
              className="whitespace-nowrap text-sm text-status-warning"
              data-testid={`harness-default-dirty-${provider.harness_id}`}
            >
              Saves on blur
            </span>
          )}
          {stored && (
            <button
              type="button"
              onClick={() => void onReset()}
              disabled={disabled}
              className="shrink-0 px-3 py-1.5 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25 disabled:opacity-50"
              aria-label={`Reset ${provider.label} defaults`}
              data-testid={`harness-default-reset-${provider.harness_id}`}
            >
              Reset
            </button>
          )}
        </div>
      </div>

      {/* The permission mode gets its own full-width line: the select shows the
          harness's own flag text, which is too long to share a row with the
          model and effort inputs without being truncated. */}
      {(showPermissionSelect || enforcedMode) && (
        <div className="mt-2 pl-8">
          {showPermissionSelect && (
            <select
              id={`harness-default-permission-${provider.harness_id}`}
              value={draft.draft.permission_mode ?? ''}
              onChange={(e) => onUpdate({ permission_mode: e.target.value || null })}
              onBlur={() => void onCommit()}
              disabled={disabled}
              className="w-full bg-bg-input border border-border-default rounded-md px-3 py-1.5 text-base text-text-primary focus:outline-none focus:border-border-active disabled:opacity-50"
              aria-label={`${provider.label} permission mode`}
              title={selectedMode?.description ?? ''}
              data-testid={`harness-permission-select-${provider.harness_id}`}
            >
              <option value="">— harness default ({defaultMode?.label ?? ''}) —</option>
              {modes.map((m) => (
                <option key={m.id} value={m.id} title={m.description}>
                  {m.label}
                </option>
              ))}
            </select>
          )}
          {enforcedMode && (
            <span
              className="text-sm text-text-muted"
              title={enforcedMode.description}
              data-testid={`harness-permission-fixed-${provider.harness_id}`}
            >
              {enforcedMode.label}
            </span>
          )}
        </div>
      )}
    </div>
  );
}
