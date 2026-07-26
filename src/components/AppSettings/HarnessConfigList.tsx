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
  useSortable,
  verticalListSortingStrategy,
  arrayMove,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { ProviderIcon } from '../Providers/ProviderIcon';
import * as api from '../../lib/tauri';
import type { ProviderAccount, ProviderPairing, ApiSurface, ModelTiers } from '../../lib/tauri';

/** A harness that can host Proxied Providers (speaks a Compatible API surface). */
export interface ProxyHarness {
  id: string;
  label: string;
}

const SURFACE_LABEL: Record<ApiSurface, string> = {
  anthropic: 'Anthropic',
  openai: 'OpenAI',
};

const EMPTY_TIERS: ModelTiers = {
  default: null,
  small_fast: null,
  sonnet: null,
  opus: null,
  fable: null,
  haiku: null,
};

/** Claude / Anthropic-surface model tier fields (Harnesses attach/edit only). */
export const MODEL_TIER_FIELDS: { key: keyof ModelTiers; label: string }[] = [
  { key: 'default', label: 'Default model' },
  { key: 'fable', label: 'Fable' },
  { key: 'opus', label: 'Opus' },
  { key: 'sonnet', label: 'Sonnet' },
  { key: 'haiku', label: 'Haiku' },
  { key: 'small_fast', label: 'Small / fast' },
];

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

/**
 * Harness-centric config (ADR-0016 §5 / ADR-0025). Attach collects base URL
 * (first-class prefilled) and Anthropic model tiers; inline edit after attach.
 */
export function HarnessConfigList({
  harnesses,
  compatibleByHarness,
  pairings,
  storedKeys,
  accounts,
  onAttach,
  onUpdate,
  onDetach,
  onReorderProxied,
  onDirtyChange,
}: {
  harnesses: ProxyHarness[];
  compatibleByHarness: Record<string, ProviderAccount[]>;
  pairings: ProviderPairing[];
  storedKeys: Set<string>;
  accounts: ProviderAccount[];
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
  onReorderProxied?: (harnessId: string, providerIds: string[]) => void;
  onDirtyChange?: (harnessId: string, dirty: boolean) => void;
}) {
  const accountName = (id: string) => accounts.find((a) => a.id === id)?.name ?? id;
  const isKeyed = (id: string) => {
    const k = accounts.find((a) => a.id === id)?.api_key;
    return !!k && k.length > 0;
  };

  const visible = harnesses.filter(
    (h) =>
      (compatibleByHarness[h.id]?.length ?? 0) > 0 ||
      pairings.some((p) => p.harness_id === h.id),
  );

  if (visible.length === 0) {
    return (
      <p className="text-base text-text-muted">
        No harnesses on this host can host a proxied provider yet.
      </p>
    );
  }

  return (
    <div className="space-y-4" data-testid="harness-config-list">
      {visible.map((harness) => (
        <HarnessCard
          key={harness.id}
          harness={harness}
          compatible={compatibleByHarness[harness.id] ?? []}
          pairings={pairings.filter((p) => p.harness_id === harness.id)}
          storedKeys={storedKeys}
          accountName={accountName}
          isKeyed={isKeyed}
          onAttach={onAttach}
          onUpdate={onUpdate}
          onDetach={onDetach}
          onReorderProxied={onReorderProxied}
          onDirtyChange={onDirtyChange ? (d) => onDirtyChange(harness.id, d) : undefined}
        />
      ))}
    </div>
  );
}

function HarnessCard({
  harness,
  compatible,
  pairings,
  storedKeys,
  accountName,
  isKeyed,
  onAttach,
  onUpdate,
  onDetach,
  onReorderProxied,
  onDirtyChange,
}: {
  harness: ProxyHarness;
  compatible: ProviderAccount[];
  pairings: ProviderPairing[];
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
  onReorderProxied?: (harnessId: string, providerIds: string[]) => void;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [selected, setSelected] = useState('');
  const [key, setKey] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [tiers, setTiers] = useState<ModelTiers>(EMPTY_TIERS);
  const [surface, setSurface] = useState<ApiSurface | null>(null);
  const [busy, setBusy] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);

  const attachedIds = new Set(pairings.map((p) => p.provider_id));
  const offerable = compatible.filter((a) => !attachedIds.has(a.id));
  const needsKey = selected !== '' && !isKeyed(selected);
  const showTiers = surface === 'anthropic';

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
        showTiers ? tiers : null,
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
                  {showTiers && (
                    <div>
                      <label className="block text-sm text-text-muted mb-1">Models</label>
                      <p className="text-sm text-text-muted mb-2">
                        Which model backs each Claude tier. Background tasks use small / fast.
                      </p>
                      <div className="space-y-2">
                        {MODEL_TIER_FIELDS.map(({ key: tk, label }) => (
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
                    (needsKey && !key.trim())
                  }
                  className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
                >
                  {busy ? 'Attaching…' : 'Attach'}
                </button>
                <button
                  onClick={reset}
                  disabled={busy}
                  className="px-5 py-2 text-base text-text-muted hover:text-text-secondary disabled:opacity-50"
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
          className="mt-3 text-base text-accent-cyan hover:text-accent-cyan/80"
        >
          + Add proxied provider
        </button>
      )}
    </div>
  );
}

function ProxiedChildRow({
  pairing,
  harnessLabel,
  detachable,
  editing,
  onStartEdit,
  onCancelEdit,
  onSaveEdit,
  onDetach,
  accountName,
  busy,
}: {
  pairing: ProviderPairing;
  harnessLabel: string;
  detachable: boolean;
  editing: boolean;
  onStartEdit: () => void;
  onCancelEdit: () => void;
  onSaveEdit?: (baseUrl: string | null, modelTiers: ModelTiers | null) => Promise<void>;
  onDetach: (providerId: string) => void;
  accountName: (id: string) => string;
  busy: boolean;
}) {
  const { setNodeRef, transform, transition, isDragging, attributes, listeners } =
    useSortable({ id: pairing.provider_id, disabled: !detachable || editing });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  const [editUrl, setEditUrl] = useState(pairing.base_url ?? '');
  const [editTiers, setEditTiers] = useState<ModelTiers>(pairing.model_tiers ?? EMPTY_TIERS);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (editing) {
      setEditUrl(pairing.base_url ?? '');
      setEditTiers(pairing.model_tiers ?? EMPTY_TIERS);
    }
  }, [editing, pairing]);

  const handleProps = detachable && !editing ? { ...attributes, ...listeners } : {};
  const showTiers = pairing.surface === 'anthropic';

  const saveEdit = async () => {
    if (!onSaveEdit || !editUrl.trim()) return;
    setSaving(true);
    try {
      await onSaveEdit(editUrl.trim(), showTiers ? editTiers : null);
    } catch {
      // Parent surfaces error.
    } finally {
      setSaving(false);
    }
  };

  return (
    <li
      ref={setNodeRef}
      style={style}
      className="border border-border-subtle rounded-md px-3 py-2"
      data-testid={`pairing-${pairing.harness_id}-${pairing.provider_id}`}
      data-spawn-harness={pairing.harness_id}
      data-spawn-id={`${pairing.harness_id}:${pairing.provider_id}`}
    >
      <div className="flex items-center gap-3">
        {detachable ? (
          <span
            {...handleProps}
            tabIndex={0}
            role="button"
            aria-roledescription="sortable"
            aria-label={`Reorder ${accountName(pairing.provider_id)} under ${harnessLabel}`}
            className="text-text-muted hover:text-text-secondary cursor-grab active:cursor-grabbing text-2xs select-none focus:outline-none focus-visible:ring-1 focus-visible:ring-accent-cyan rounded-sm"
            title="Drag to reorder"
          >
            ⋮⋮
          </span>
        ) : (
          <span className="w-3.5 text-transparent select-none" aria-hidden="true">
            ⋮⋮
          </span>
        )}
        <ProviderIcon providerId={pairing.provider_id} className="h-5 w-5" />
        <div className="min-w-0 flex-1">
          <div className="text-base text-text-primary truncate">
            {accountName(pairing.provider_id)}
            <span className="ml-2 text-sm text-accent-cyan">{SURFACE_LABEL[pairing.surface]}</span>
          </div>
          {!editing && pairing.base_url && (
            <div className="text-sm text-text-secondary truncate font-mono">{pairing.base_url}</div>
          )}
        </div>
        {detachable && !editing && (
          <>
            {onSaveEdit && (
              <button
                onClick={onStartEdit}
                disabled={busy}
                className="px-3 py-1 text-sm text-accent-cyan hover:text-accent-cyan/80 disabled:opacity-50"
                aria-label={`Edit ${accountName(pairing.provider_id)} under ${harnessLabel}`}
              >
                Edit
              </button>
            )}
            <button
              onClick={() => onDetach(pairing.provider_id)}
              disabled={busy}
              className="px-3 py-1 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25 disabled:opacity-50"
              aria-label={`Detach ${accountName(pairing.provider_id)} from ${harnessLabel}`}
            >
              Detach
            </button>
          </>
        )}
      </div>

      {editing && (
        <div className="mt-3 space-y-3 border-t border-border-subtle pt-3">
          <div>
            <label className="block text-sm text-text-muted mb-1">Base URL</label>
            <input
              type="text"
              value={editUrl}
              onChange={(e) => setEditUrl(e.target.value)}
              className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
              aria-label={`Edit base URL for ${accountName(pairing.provider_id)}`}
            />
          </div>
          {showTiers && (
            <div className="space-y-2">
              {MODEL_TIER_FIELDS.map(({ key: tk, label }) => (
                <div key={tk} className="flex items-center gap-3">
                  <span className="w-28 shrink-0 text-sm text-text-muted">{label}</span>
                  <input
                    type="text"
                    value={editTiers[tk] ?? ''}
                    onChange={(e) =>
                      setEditTiers((prev) => ({ ...prev, [tk]: e.target.value || null }))
                    }
                    className="flex-1 min-w-0 bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                    aria-label={`Edit ${label} model for ${accountName(pairing.provider_id)}`}
                  />
                </div>
              ))}
            </div>
          )}
          <div className="flex gap-3">
            <button
              onClick={saveEdit}
              disabled={saving || !editUrl.trim()}
              className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
            >
              {saving ? 'Saving…' : 'Save'}
            </button>
            <button
              onClick={onCancelEdit}
              disabled={saving}
              className="px-5 py-2 text-base text-text-muted hover:text-text-secondary disabled:opacity-50"
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </li>
  );
}
