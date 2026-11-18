import { useEffect, useMemo, useRef, useState } from 'react';
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
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import type { LaunchTarget } from '../../types/generated/LaunchTarget';
import type { ProviderPairing } from '../../types/generated/ProviderPairing';
import type { PairingVerification } from '../../types/generated/PairingVerification';
import {
  applyHarnessConfigurationOrder,
  configurationHarnessId,
  getLaunchConfigurationReorderCoordinator,
  groupConfigurationsByHarness,
  reorderConfigurationIds,
  type LaunchConfigurationGroup,
  type LaunchConfigurationReorderCoordinator,
} from '../../lib/launchConfigurationOrder';
import { ProviderIcon } from './ProviderIcon';
import { LaunchConfigurationEditor, cloneConfiguration } from './LaunchConfigurationEditor';

export interface LaunchConfigurationApi {
  list: () => Promise<SpawnConfiguration[]>;
  targets: () => Promise<LaunchTarget[]>;
  save: (value: SpawnConfiguration, route?: ProviderPairing) => Promise<SpawnConfiguration>;
  verify?: (value: SpawnConfiguration, route?: ProviderPairing) => Promise<PairingVerification>;
  remove: (id: string) => Promise<void>;
  reorder?: (harnessId: string, configurationIds: string[]) => Promise<void>;
}

function SortableLaunchConfigurationRow({
  value,
  harnessName,
  canReorder,
  disabled,
  onEdit,
  onClone,
}: {
  value: SpawnConfiguration;
  harnessName: string;
  canReorder: boolean;
  disabled: boolean;
  onEdit: () => void;
  onClone: () => void;
}) {
  const { setNodeRef, transform, transition, isDragging, attributes, listeners } = useSortable({
    id: value.id,
    disabled: !canReorder,
  });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  return (
    <li
      ref={setNodeRef}
      style={style}
      className="launch-config-item"
      data-testid={`launch-config-row-${value.id}`}
    >
      {canReorder && (
        <span
          {...attributes}
          {...listeners}
          tabIndex={0}
          role="button"
          aria-roledescription="sortable"
          aria-disabled={disabled || undefined}
          aria-label={`Reorder ${value.name} under ${harnessName}`}
          className="launch-config-drag-handle"
          title={disabled ? 'Settings must be recovered before the order can change' : 'Drag to reorder'}
        >
          ⋮⋮
        </span>
      )}
      <p className="launch-config-item-name">
        {value.name} · {value.generated && !value.generated.user_owned ? 'Generated' : 'User-owned'}
      </p>
      <div className="launch-config-actions">
        <button type="button" onClick={onEdit} aria-label={`Edit ${value.name}`}>Edit</button>
        <button type="button" onClick={onClone} aria-label={`Clone ${value.name}`}>Clone</button>
      </div>
    </li>
  );
}

function HarnessConfigurationSection({
  group,
  reorderEnabled,
  disabled,
  onEdit,
  onClone,
  onReorder,
}: {
  group: LaunchConfigurationGroup;
  reorderEnabled: boolean;
  disabled: boolean;
  onEdit: (value: SpawnConfiguration) => void;
  onClone: (value: SpawnConfiguration) => void;
  onReorder: (harnessId: string, orderedIds: string[]) => void;
}) {
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  const canReorder = reorderEnabled && !disabled && group.configurations.length >= 2;
  const ids = useMemo(() => group.configurations.map((c) => c.id), [group.configurations]);

  const handleDragEnd = (event: DragEndEvent) => {
    if (!canReorder) return;
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const nextIds = reorderConfigurationIds(ids, String(active.id), String(over.id));
    if (nextIds !== ids) {
      onReorder(group.harnessId, nextIds);
    }
  };

  return (
    <section
      className="launch-config-group"
      aria-label={group.harnessName}
      data-testid={`launch-config-group-${group.harnessId}`}
    >
      <header className="launch-config-group-header">
        <ProviderIcon providerId={group.harnessId} className="h-4 w-4 shrink-0" />
        <h4 className="launch-config-group-title">{group.harnessName}</h4>
      </header>
      <DndContext sensors={sensors} onDragEnd={handleDragEnd}>
        <SortableContext items={ids} strategy={verticalListSortingStrategy}>
          <ul className="launch-config-list">
            {group.configurations.map((value) => (
              <SortableLaunchConfigurationRow
                key={value.id}
                value={value}
                harnessName={group.harnessName}
                canReorder={canReorder}
                disabled={disabled}
                onEdit={() => onEdit(value)}
                onClone={() => onClone(value)}
              />
            ))}
          </ul>
        </SortableContext>
      </DndContext>
    </section>
  );
}

export function LaunchConfigurations({
  api,
  disabled = false,
  onDirtyChange,
  onChanged,
  refreshToken,
}: {
  api: LaunchConfigurationApi;
  disabled?: boolean;
  onDirtyChange?: (dirty: boolean) => void;
  onChanged?: () => void;
  refreshToken?: unknown;
}) {
  const [values, setValues] = useState<SpawnConfiguration[]>([]);
  const [targets, setTargets] = useState<LaunchTarget[]>([]);
  const [draft, setDraft] = useState<SpawnConfiguration | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [reorderError, setReorderError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  const valuesRef = useRef(values);
  valuesRef.current = values;
  const onChangedRef = useRef(onChanged);
  onChangedRef.current = onChanged;
  const hasLoadedRef = useRef(false);
  const needsTrailingRefreshRef = useRef(false);

  const coordinator = useMemo<LaunchConfigurationReorderCoordinator | null>(
    () => (api.reorder ? getLaunchConfigurationReorderCoordinator(api.reorder) : null),
    [api.reorder],
  );

  useEffect(() => {
    hasLoadedRef.current = false;
    needsTrailingRefreshRef.current = false;
  }, [api]);

  useEffect(() => {
    if (!coordinator) return;
    return coordinator.subscribe({
      onHarnessFailed: (harnessId, confirmedIds, err) => {
        if (confirmedIds) {
          const rolledBack = coordinator.applyPendingOverlay(
            applyHarnessConfigurationOrder(valuesRef.current, harnessId, confirmedIds),
          );
          valuesRef.current = rolledBack;
          setValues(rolledBack);
        }
        setReorderError(String(err));
      },
      onDrained: ({ anySucceeded, anyFailed }) => {
        if (anySucceeded) {
          onChangedRef.current?.();
        }
        if (needsTrailingRefreshRef.current || anyFailed) {
          needsTrailingRefreshRef.current = false;
          setRevision((v) => v + 1);
        }
      },
    });
  }, [coordinator]);

  useEffect(() => {
    let current = true;
    const readVersion = coordinator?.getMutationVersion() ?? 0;
    Promise.resolve().then(() => Promise.all([api.list(), api.targets()])).then(([list, options]) => {
      if (!Array.isArray(list) || !Array.isArray(options)) throw new Error('Launch Configurations could not be loaded');
      if (!current) return;
      setTargets(options);
      setLoading(false);
      setLoadError(null);
      // Issue #2326 snapshot-authority pattern:
      // 1. If a reorder is still in flight or queued, overlay pending orders on
      //    first load (so reopening while pending shows accepted drags) or drop
      //    subsequent stale reads, and let `onDrained` run one trailing read.
      // 2. If the queue already drained while this stale read was in flight,
      //    drop the stale snapshot and schedule the trailing read immediately.
      if (coordinator?.isPending()) {
        if (!hasLoadedRef.current) {
          const overlaid = coordinator.applyPendingOverlay(list);
          valuesRef.current = overlaid;
          setValues(overlaid);
          hasLoadedRef.current = true;
        }
        needsTrailingRefreshRef.current = true;
        return;
      }
      if (coordinator && coordinator.getMutationVersion() !== readVersion) {
        needsTrailingRefreshRef.current = false;
        setRevision((v) => v + 1);
        return;
      }
      valuesRef.current = list;
      setValues(list);
      hasLoadedRef.current = true;
      coordinator?.syncConfirmed(list);
    }).catch((e: unknown) => { if (current) { setLoadError(String(e)); setLoading(false); } });
    return () => { current = false; };
  }, [api, revision, refreshToken, coordinator]);

  const groups = useMemo(
    () => groupConfigurationsByHarness(values, targets),
    [values, targets],
  );

  const handleReorder = (harnessId: string, orderedIds: string[]) => {
    if (!coordinator || disabled) return;
    const previous = valuesRef.current;
    const previousHarnessIds = previous
      .filter((c) => configurationHarnessId(c) === harnessId)
      .map((c) => c.id);
    const next = applyHarnessConfigurationOrder(previous, harnessId, orderedIds);
    if (next === previous) return;
    valuesRef.current = next;
    setValues(next);
    setReorderError(null);
    coordinator.enqueue(harnessId, previousHarnessIds, orderedIds);
  };

  const error = loadError ?? reorderError;
  const refresh = () => {
    coordinator?.bumpMutationVersion();
    setDraft(null);
    setReorderError(null);
    setRevision((v) => v + 1);
    onChanged?.();
  };
  if (draft) return <LaunchConfigurationEditor key={draft.id} value={draft} targets={targets} onCancel={() => setDraft(null)}
    onDirtyChange={onDirtyChange}
    onVerify={api.verify}
    onSave={async (value, route) => { if (route) await api.save(value, route); else await api.save(value); refresh(); }}
    onDelete={draft.id ? async () => { await api.remove(draft.id); refresh(); } : undefined} />;
  return <div className="launch-config-form">
    {loading && <p role="status">Loading Launch Configurations…</p>}
    {error && <p role="alert">{error} <button type="button" onClick={() => { setLoadError(null); setReorderError(null); setRevision((v) => v + 1); }}>Retry</button></p>}
    {!loading && !error && values.length === 0 && <p>No saved configurations</p>}
    {groups.map((group) => (
      <HarnessConfigurationSection
        key={group.harnessId}
        group={group}
        reorderEnabled={Boolean(api.reorder)}
        disabled={disabled}
        onEdit={(value) => setDraft(value)}
        onClone={(value) => setDraft(cloneConfiguration(value))}
        onReorder={(harnessId, orderedIds) => { void handleReorder(harnessId, orderedIds); }}
      />
    ))}
    <button type="button" disabled={loading || !targets.length} onClick={() => setDraft({ id: '', name: '', spawn_option_id: targets[0].id, model: null, effort: null, extra_args: null })}>New Launch Configuration</button>
  </div>;
}
