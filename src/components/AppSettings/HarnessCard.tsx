import { useEffect, useRef, useState } from 'react';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  sortableKeyboardCoordinates,
  verticalListSortingStrategy,
  arrayMove,
} from '@dnd-kit/sortable';
import { ProviderIcon } from '../Providers/ProviderIcon';
import * as api from '../../lib/tauri';
import type {
  ProviderAccount,
  ProviderPairing,
  PairingVerification,
  ApiSurface,
  ModelTiers,
} from '../../lib/tauri';
import { ProxiedChildRow } from './ProxiedChildRow';
import { EMPTY_TIERS, MODEL_TIER_FIELDS } from './harnessVocabulary';
import type { ProxyHarness } from './HarnessConfigList';

const pairKey = (harnessId: string, providerId: string) => `${harnessId}:${providerId}`;

/** Pure: move `activeId` to where `overId` sits, returning the new id order. */
export function reorderProxiedIds(
  ids: string[],
  activeId: string,
  overId: string,
): string[] {
  const from = ids.indexOf(activeId);
  const to = ids.indexOf(overId);
  if (from === -1 || to === -1 || from === to) return ids;
  return arrayMove(ids, from, to);
}

export function HarnessCard({
  harness,
  compatible,
  pairings,
  verifications,
  supportsWsl,
  storedKeys,
  accountName,
  isKeyed,
  onAttach,
  onUpdate,
  onDetach,
  onVerify,
  onReorderProxied,
  onDirtyChange,
  disabled = false,
}: {
  harness: ProxyHarness;
  compatible: ProviderAccount[];
  pairings: ProviderPairing[];
  verifications: PairingVerification[];
  supportsWsl: boolean;
  storedKeys: Set<string>;
  accountName: (id: string) => string;
  isKeyed: (id: string) => boolean;
  onAttach: (
    harnessId: string,
    providerId: string,
    apiKey: string | null,
    baseUrl: string | null,
    modelTiers: ModelTiers | null,
  ) => Promise<void>;
  onUpdate?: (
    harnessId: string,
    providerId: string,
    baseUrl: string | null,
    modelTiers: ModelTiers | null,
  ) => Promise<void>;
  onDetach: (harnessId: string, providerId: string) => Promise<void>;
  onVerify: (harnessId: string, providerId: string, envType: api.EnvType) => Promise<void>;
  onReorderProxied?: (harnessId: string, providerIds: string[]) => void;
  onDirtyChange?: (dirty: boolean) => void;
  /** Issue #1523 — relayed from `HarnessConfigList`; folds into this card's
   *  `busy` so the attach form, every pairing row, and the reorder drag all
   *  lock from one gate. */
  disabled?: boolean;
}) {
  const [adding, setAdding] = useState(false);
  const [selected, setSelected] = useState('');
  const [key, setKey] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [tiers, setTiers] = useState<ModelTiers>(EMPTY_TIERS);
  const [surface, setSurface] = useState<ApiSurface | null>(null);
  // Issue #1523 — see the `disabled` prop: one gate, every control below.
  const [submitting, setBusy] = useState(false);
  const busy = submitting || disabled;
  const [editingId, setEditingId] = useState<string | null>(null);

  const attachedIds = new Set(pairings.map((p) => p.provider_id));
  const offerable = compatible.filter((a) => !attachedIds.has(a.id));
  const needsKey = selected !== '' && !isKeyed(selected);
  const showTiers = surface === 'anthropic';
  const modelFields = showTiers ? MODEL_TIER_FIELDS : MODEL_TIER_FIELDS.slice(0, 1);

  const isDirty =
    adding &&
    (selected !== '' ||
      key.trim() !== '' ||
      baseUrl.trim() !== '' ||
      Object.values(tiers).some((v) => v != null && v !== ''));
  const lastReportedDirtyRef = useRef<boolean>(false);
  useEffect(() => {
    if (lastReportedDirtyRef.current === isDirty) return;
    onDirtyChange?.(isDirty);
    lastReportedDirtyRef.current = isDirty;
  }, [isDirty, onDirtyChange]);

  // Prefill base URL + tiers from first-class defaults when the user picks a
  // provider. Clear synchronously on selection change so the previous
  // provider's URL/tiers never bleed into a fresh attach (a stale-URL race).
  useEffect(() => {
    setBaseUrl('');
    setTiers(EMPTY_TIERS);
    setSurface(null);
    if (!selected) return;
    let cancelled = false;
    (async () => {
      try {
        const defaults = await api.getPairingDefaults(harness.id, selected);
        if (cancelled) return;
        if (defaults) {
          setSurface(defaults.surface);
          setBaseUrl(defaults.base_url ?? '');
          setTiers(defaults.model_tiers ?? EMPTY_TIERS);
        }
      } catch {
        // Form stays empty; the user can type a URL.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [selected, harness.id]);

  const reset = () => {
    setAdding(false);
    setSelected('');
    setKey('');
    setBaseUrl('');
    setTiers(EMPTY_TIERS);
    setSurface(null);
  };

  const submitAttach = async () => {
    if (!selected || !baseUrl.trim()) return;
    setBusy(true);
    try {
      await onAttach(
        harness.id,
        selected,
        needsKey ? key.trim() || null : null,
        baseUrl.trim(),
        surface ? tiers : null,
      );
      reset();
    } catch {
      // Parent surfaces error; keep form open.
    } finally {
      setBusy(false);
    }
  };

  const detach = async (providerId: string) => {
    setBusy(true);
    try {
      await onDetach(harness.id, providerId);
    } catch {
      // Parent surfaces the error.
    } finally {
      setBusy(false);
    }
  };

  // ADR-0025: effective pairings are stored-only; every row is detachable.
  const handleDragEnd = (event: DragEndEvent) => {
    if (!onReorderProxied) return;
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const next = reorderProxiedIds(
      pairings.map((p) => p.provider_id),
      active.id as string,
      over.id as string,
    );
    onReorderProxied(harness.id, next);
  };

  const sensors = useSensors(
    useSensor(PointerSensor),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const setTier = (k: keyof ModelTiers, value: string) =>
    setTiers((prev) => ({ ...prev, [k]: value || null }));

  return (
    <div className="border border-border-subtle rounded-lg p-5" data-testid={`harness-${harness.id}`}>
      <div className="flex items-center gap-3 mb-3">
        <ProviderIcon providerId={harness.id} className="h-6 w-6" />
        <span className="text-lg font-medium text-text-primary">{harness.label}</span>
      </div>

      {pairings.length === 0 ? (
        <p className="text-base text-text-muted">No proxied providers attached.</p>
      ) : (
        <DndContext sensors={sensors} onDragEnd={handleDragEnd}>
          <SortableContext
            items={pairings.map((p) => p.provider_id)}
            strategy={verticalListSortingStrategy}
          >
            <ul className="flex flex-col gap-2">
              {pairings.map((p) => {
                const detachable =
                  storedKeys.size === 0 ||
                  storedKeys.has(pairKey(p.harness_id, p.provider_id));
                return (
                  <ProxiedChildRow
                    key={p.provider_id}
                    pairing={p}
                    verifications={verifications.filter((v) => v.provider_id === p.provider_id)}
                    supportsWsl={supportsWsl}
                    harnessLabel={harness.label}
                    detachable={detachable}
                    editing={editingId === p.provider_id}
                    onStartEdit={() => setEditingId(p.provider_id)}
                    onCancelEdit={() => setEditingId(null)}
                    onSaveEdit={
                      onUpdate
                        ? async (url, mt) => {
                            await onUpdate(harness.id, p.provider_id, url, mt);
                            setEditingId(null);
                          }
                        : undefined
                    }
                    onDetach={detach}
                    onVerify={(envType) => onVerify(harness.id, p.provider_id, envType)}
                    accountName={accountName}
                    busy={busy}
                  />
                );
              })}
            </ul>
          </SortableContext>
        </DndContext>
      )}

      {adding ? (
        <div className="mt-3 space-y-3 border-t border-border-subtle pt-3">
          {offerable.length === 0 ? (
            <p className="text-base text-text-muted">
              No more compatible providers to attach. Add one on the Providers page first.
            </p>
          ) : (
            <>
              <select
                value={selected}
                onChange={(e) => setSelected(e.target.value)}
                className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                aria-label={`Provider to attach to ${harness.label}`}
              >
                <option value="">Select a provider…</option>
                {offerable.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name}
                  </option>
                ))}
              </select>
              {needsKey && (
                <div>
                  <label className="block text-sm text-text-muted mb-1">
                    API key for {accountName(selected)} (saved globally, set once)
                  </label>
                  <input
                    type="password"
                    value={key}
                    onChange={(e) => setKey(e.target.value)}
                    placeholder="Enter API key…"
                    className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                    aria-label={`${accountName(selected)} API key`}
                  />
                </div>
              )}
              {selected && (
                <>
                  <div>
                    <label className="block text-sm text-text-muted mb-1">Base URL</label>
                    <input
                      type="text"
                      value={baseUrl}
                      onChange={(e) => setBaseUrl(e.target.value)}
                      placeholder="https://api.example.com/…"
                      className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                      aria-label={`Base URL for ${accountName(selected)} under ${harness.label}`}
                    />
                  </div>
                  {surface && (
                    <div>
                      <label className="block text-sm text-text-muted mb-1">Models</label>
                      <p className="text-sm text-text-muted mb-2">
                        {surface === 'openai'
                          ? 'Codex uses one explicit Responses model.'
                          : 'Which model backs each Claude tier. Background tasks use small / fast.'}
                      </p>
                      <div className="space-y-2">
                        {modelFields.map(({ key: tk, label }) => (
                          <div key={tk} className="flex items-center gap-3">
                            <span className="w-28 shrink-0 text-sm text-text-muted">{label}</span>
                            <input
                              type="text"
                              value={tiers[tk] ?? ''}
                              onChange={(e) => setTier(tk, e.target.value)}
                              placeholder="model id"
                              className="flex-1 min-w-0 bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                              aria-label={`${accountName(selected)} ${label} model`}
                            />
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                </>
              )}
              <div className="flex gap-3">
                <button
                  onClick={submitAttach}
                  disabled={
                    busy ||
                    !selected ||
                    !baseUrl.trim() ||
                    (surface === 'openai' && !tiers.default?.trim()) ||
                    (needsKey && !key.trim())
                  }
                  className="px-5 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
                >
                  {submitting ? 'Attaching…' : 'Attach'}
                </button>
                <button
                  onClick={reset}
                  disabled={busy}
                  className="px-5 py-2 text-base text-text-secondary border border-border-strong rounded-md hover:bg-bg-card-hover hover:text-text-primary disabled:opacity-50"
                >
                  Cancel
                </button>
              </div>
            </>
          )}
        </div>
      ) : (
        <button
          onClick={() => setAdding(true)}
          className="mt-3 text-base text-text-secondary hover:text-text-primary"
        >
          + Add proxied provider
        </button>
      )}
    </div>
  );
}
