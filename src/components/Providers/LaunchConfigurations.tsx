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
  groupConfigurationsByHarness,
  reorderConfigurationIds,
  type LaunchConfigurationGroup,
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
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  const valuesRef = useRef(values);
  valuesRef.current = values;
  const reorderSeqRef = useRef(0);

  useEffect(() => {
    let current = true;
    Promise.resolve().then(() => Promise.all([api.list(), api.targets()])).then(([list, options]) => {
      if (!Array.isArray(list) || !Array.isArray(options)) throw new Error('Launch Configurations could not be loaded');
      if (current) { setValues(list); setTargets(options); setLoading(false); setError(null); }
    }).catch((e: unknown) => { if (current) { setError(String(e)); setLoading(false); } });
    return () => { current = false; };
  }, [api, revision, refreshToken]);

  const groups = useMemo(
    () => groupConfigurationsByHarness(values, targets),
    [values, targets],
  );

  const handleReorder = async (harnessId: string, orderedIds: string[]) => {
    if (!api.reorder || disabled) return;
    const previous = valuesRef.current;
    const next = applyHarnessConfigurationOrder(previous, harnessId, orderedIds);
    if (next === previous) return;
    const seq = ++reorderSeqRef.current;
    valuesRef.current = next;
    setValues(next);
    setError(null);
    try {
      await api.reorder(harnessId, orderedIds);
      if (reorderSeqRef.current === seq) {
        onChanged?.();
      }
    } catch (e: unknown) {
      if (reorderSeqRef.current === seq) {
        valuesRef.current = previous;
        setValues(previous);
      }
      setError(String(e));
    }
  };

  const refresh = () => { setDraft(null); setRevision((v) => v + 1); onChanged?.(); };
  if (draft) return <LaunchConfigurationEditor key={draft.id} value={draft} targets={targets} onCancel={() => setDraft(null)}
    onDirtyChange={onDirtyChange}
    onVerify={api.verify}
    onSave={async (value, route) => { if (route) await api.save(value, route); else await api.save(value); refresh(); }}
    onDelete={draft.id ? async () => { await api.remove(draft.id); refresh(); } : undefined} />;
  return <div className="launch-config-form">
    {loading && <p role="status">Loading Launch Configurations…</p>}
    {error && <p role="alert">{error} <button type="button" onClick={() => setRevision((v) => v + 1)}>Retry</button></p>}
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
