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
import type { ProviderAccount, ProviderPairing, ApiSurface } from '../../lib/tauri';

/** A harness that can host Proxied Providers (speaks a Compatible API surface). */
export interface ProxyHarness {
  id: string;
  label: string;
}

const SURFACE_LABEL: Record<ApiSurface, string> = {
  anthropic: 'Anthropic',
  openai: 'OpenAI',
};

/** Stable key for a (harness, provider) pairing — matches the backend
 *  `(harness_id, provider_id)` identity used for detach. */
const pairKey = (harnessId: string, providerId: string) => `${harnessId}:${providerId}`;

/** Pure: move `activeId` to where `overId` sits, returning the new id order.
 *  Exposed so the reorder math can be unit-tested without simulating a drag
 *  (jsdom can't fire real pointer drags through dnd-kit). Mirrors
 *  `reorderIds` in `HarnessOrderList.tsx` — the harness-config analogue,
 *  scoped to one harness's children only. */
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
 * The harness-centric **config page** (ADR-0016 §5, issue #576). For each
 * proxy-capable **Agent Harness** it lists the **Proxied Providers** attached to
 * it (each with its **Compatible API surface** + endpoint) and offers
 * "Add proxied provider", which only surfaces providers whose surface that
 * harness speaks (surface-matched, AC3).
 *
 * The **API key is global** to the provider and edited on the Providers page;
 * the attach flow only seeds it *if absent* (so a provider with no key yet shows
 * an inline key field). The default Anthropic pairing for a keyed provider is
 * derived backend-side and shown as managed-on-Providers (not detachable here);
 * extra surfaces/harnesses the user attaches are detachable.
 *
 * **Issue #577**: each `HarnessCard` wraps its child list in its own
 * `<DndContext>` + `<SortableContext>`, so a draggable can only be dropped on
 * a sibling in the same card — cross-harness drag is structurally
 * disallowed. Only stored pairings are draggable; derived defaults are
 * managed on the Providers page and show the "Default · key on Providers"
 * placeholder without a reorder handle.
 */
export function HarnessConfigList({
  harnesses,
  compatibleByHarness,
  pairings,
  storedKeys,
  accounts,
  onAttach,
  onDetach,
  onReorderProxied,
  onDirtyChange,
}: {
  harnesses: ProxyHarness[];
  compatibleByHarness: Record<string, ProviderAccount[]>;
  pairings: ProviderPairing[];
  /** `${harness_id}:${provider_id}` of pairings that are user-stored (detachable). */
  storedKeys: Set<string>;
  accounts: ProviderAccount[];
  onAttach: (harnessId: string, providerId: string, apiKey: string | null) => Promise<void>;
  onDetach: (harnessId: string, providerId: string) => Promise<void>;
  /**
   * Issue #577 — called with `(harnessId, newProviderIds)` after the user
   * drops a child row inside a harness card. Only fires for stored pairings
   * (the rows that are draggable). Cross-harness drag is disallowed at the
   * UI layer (each card is its own `DndContext`), so `harnessId` here always
   * matches the card the drag started in.
   */
  onReorderProxied?: (harnessId: string, providerIds: string[]) => void;
  /**
   * Forwarded to each card so the parent can aggregate the modal-wide dirty
   * signal (issue #730). Each card's form opens independently; any one
   * having a half-typed key or a selected provider counts as dirty.
   */
  onDirtyChange?: (harnessId: string, dirty: boolean) => void;
}) {
  const accountName = (id: string) => accounts.find((a) => a.id === id)?.name ?? id;
  const isKeyed = (id: string) => {
    const k = accounts.find((a) => a.id === id)?.api_key;
    return !!k && k.length > 0;
  };

  // Only render harnesses that can actually host a provider (have ≥1 compatible
  // provider) or already have one attached — a native-only harness (Terminal,
  // Antigravity) has no proxy section.
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
  onAttach: (harnessId: string, providerId: string, apiKey: string | null) => Promise<void>;
  onDetach: (harnessId: string, providerId: string) => Promise<void>;
  /** Issue #577 — reorder handler (only stored pairings call it). */
  onReorderProxied?: (harnessId: string, providerIds: string[]) => void;
  /** Dirty = the inline "Add proxied provider" form is open AND has a
   *  selection or a half-typed key. See issue #730. */
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [selected, setSelected] = useState('');
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState(false);

  const attachedIds = new Set(pairings.map((p) => p.provider_id));
  // Offer only compatible providers not already attached to this harness.
  const offerable = compatible.filter((a) => !attachedIds.has(a.id));
  const needsKey = selected !== '' && !isKeyed(selected);

  // Dirty when the inline form is open and the user has entered either a
  // provider selection or a half-typed key. The form opens empty (just the
  // "+ Add proxied provider" button) — that's not dirty.
  const isDirty = adding && (selected !== '' || key.trim() !== '');
  // Only fire on dirty-state FLIPS — see AccountCard's comment.
  const lastReportedDirtyRef = useRef<boolean>(false);
  useEffect(() => {
    if (lastReportedDirtyRef.current === isDirty) return;
    onDirtyChange?.(isDirty);
    lastReportedDirtyRef.current = isDirty;
  }, [isDirty, onDirtyChange]);

  const reset = () => {
    setAdding(false);
    setSelected('');
    setKey('');
  };

  const submitAttach = async () => {
    if (!selected) return;
    setBusy(true);
    try {
      await onAttach(harness.id, selected, needsKey ? key.trim() || null : null);
      reset();
    } catch {
      // Parent surfaces the error toast; keep the form open so the user can retry.
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

  // Issue #577 — child reorder handler. Only stored pairings participate
  // (derived defaults are managed on the Providers page). `reorderProxiedIds`
  // is a no-op when active == over or either id is missing.
  const storedPairings = pairings.filter((p) => storedKeys.has(pairKey(p.harness_id, p.provider_id)));
  const handleDragEnd = (event: DragEndEvent) => {
    if (!onReorderProxied) return;
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    // Both ids are sortable child keys (`<provider_id>`), so they map to
    // a stored pairing under THIS harness by construction.
    const next = reorderProxiedIds(
      storedPairings.map((p) => p.provider_id),
      active.id as string,
      over.id as string,
    );
    onReorderProxied(harness.id, next);
  };

  // Mirror the `HarnessOrderList` keyboard-sensor pattern (issue #727) so
  // the drag handle is operable from the keyboard.
  const sensors = useSensors(
    useSensor(PointerSensor),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  return (
    <div className="border border-border-subtle rounded-lg p-5" data-testid={`harness-${harness.id}`}>
      <div className="flex items-center gap-3 mb-3">
        <ProviderIcon providerId={harness.id} className="h-6 w-6" />
        <span className="text-lg font-medium text-text-primary">{harness.label}</span>
      </div>

      {pairings.length === 0 ? (
        <p className="text-base text-text-muted">No proxied providers attached.</p>
      ) : (
        // Issue #577 — per-harness DndContext so a draggable can only be
        // dropped on a sibling in the same card (cross-harness drag is
        // structurally disallowed). The sortable ids are `provider_id`s
        // (not composite ids) so the reorder math is one-dimensional.
        <DndContext sensors={sensors} onDragEnd={handleDragEnd}>
          <SortableContext
            items={storedPairings.map((p) => p.provider_id)}
            strategy={verticalListSortingStrategy}
          >
            <ul className="flex flex-col gap-2">
              {pairings.map((p) => {
                const detachable = storedKeys.has(pairKey(p.harness_id, p.provider_id));
                return (
                  <ProxiedChildRow
                    key={p.provider_id}
                    pairing={p}
                    harnessLabel={harness.label}
                    detachable={detachable}
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
              <div className="flex gap-3">
                <button
                  onClick={submitAttach}
                  // A keyless provider needs a key to reach its endpoint, so gate
                  // Attach on the inline key when the provider has none stored.
                  disabled={busy || !selected || (needsKey && !key.trim())}
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

/**
 * One **Proxied Provider** child row (issue #577). For **stored** pairings
 * the row is sortable — dnd-kit's `useSortable` wires the drag handle
 * activator. For **derived** default pairings (the keyed-account Claude/
 * Anthropic default managed on the Providers page) the row renders without
 * a drag handle and shows the "Default · key on Providers" placeholder.
 *
 * The Detach button is the user-stored-only control, so it never renders
 * for derived defaults — the same `detachable` gate determines both.
 */
function ProxiedChildRow({
  pairing,
  harnessLabel,
  detachable,
  onDetach,
  accountName,
  busy,
}: {
  pairing: ProviderPairing;
  harnessLabel: string;
  detachable: boolean;
  onDetach: (providerId: string) => void;
  accountName: (id: string) => string;
  busy: boolean;
}) {
  // `useSortable` is unconditional so dnd-kit's hooks order is stable;
  // when the row isn't detachable the listener/handle aren't wired into
  // the DOM (see the conditional spread below), so the row is inert.
  const { setNodeRef, transform, transition, isDragging, attributes, listeners } =
    useSortable({ id: pairing.provider_id, disabled: !detachable });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  // The drag handle only exists on detachable rows — derived defaults
  // are managed on the Providers page and aren't orderable.
  const handleProps = detachable ? { ...attributes, ...listeners } : {};

  return (
    <li
      ref={setNodeRef}
      style={style}
      className="flex items-center gap-3 border border-border-subtle rounded-md px-3 py-2"
      data-testid={`pairing-${pairing.harness_id}-${pairing.provider_id}`}
      data-spawn-harness={pairing.harness_id}
      data-spawn-id={`${pairing.harness_id}:${pairing.provider_id}`}
    >
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
        <span
          className="w-3.5 text-transparent select-none"
          aria-hidden="true"
        >
          ⋮⋮
        </span>
      )}
      <ProviderIcon providerId={pairing.provider_id} className="h-5 w-5" />
      <div className="min-w-0 flex-1">
        <div className="text-base text-text-primary truncate">
          {accountName(pairing.provider_id)}
          <span className="ml-2 text-sm text-accent-cyan">{SURFACE_LABEL[pairing.surface]}</span>
        </div>
        {pairing.base_url && (
          <div className="text-sm text-text-secondary truncate font-mono">{pairing.base_url}</div>
        )}
      </div>
      {detachable ? (
        <button
          onClick={() => onDetach(pairing.provider_id)}
          disabled={busy}
          className="px-3 py-1 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25 disabled:opacity-50"
          aria-label={`Detach ${accountName(pairing.provider_id)} from ${harnessLabel}`}
        >
          Detach
        </button>
      ) : (
        <span className="text-sm text-text-muted whitespace-nowrap" title="The global key is managed on the Providers page">
          Default · key on Providers
        </span>
      )}
    </li>
  );
}
