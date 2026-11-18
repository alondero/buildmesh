import { arrayMove } from '@dnd-kit/sortable';
import type { LaunchTarget } from '../types/generated/LaunchTarget';
import type { SpawnConfiguration } from '../types/generated/SpawnConfiguration';

export interface LaunchConfigurationGroup {
  harnessId: string;
  harnessName: string;
  configurations: SpawnConfiguration[];
}

/** Resolve the owning harness profile id of a Launch Configuration. */
export function configurationHarnessId(
  configuration: Pick<SpawnConfiguration, 'harness_id' | 'spawn_option_id'>,
): string {
  if (configuration.harness_id && configuration.harness_id.length > 0) {
    return configuration.harness_id;
  }
  return configuration.spawn_option_id.split(':')[0] ?? '';
}

/**
 * Pure: move `activeId` to where `overId` sits, returning the new id order.
 * Mirrors `HarnessOrderList.reorderIds` and `HarnessCard.reorderProxiedIds`.
 */
export function reorderConfigurationIds(
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
 * Pure: apply a per-harness ordered id list into a flat configuration array in
 * place. Mirrors `preferences::spawn_configurations::reorder_in_preferences`:
 * configurations of other harnesses and any unmentioned configurations of the
 * same harness stay pinned at their existing indices.
 */
export function applyHarnessConfigurationOrder(
  values: SpawnConfiguration[],
  harnessId: string,
  orderedIds: string[],
): SpawnConfiguration[] {
  const seen = new Set<string>();
  const validIds: string[] = [];
  for (const id of orderedIds) {
    if (seen.has(id)) continue;
    seen.add(id);
    if (values.some((c) => c.id === id && configurationHarnessId(c) === harnessId)) {
      validIds.push(id);
    }
  }
  if (validIds.length < 2) return values;

  const activeSet = new Set(validIds);
  const slotIndices: number[] = [];
  const byId = new Map<string, SpawnConfiguration>();
  values.forEach((value, idx) => {
    if (activeSet.has(value.id)) {
      slotIndices.push(idx);
      byId.set(value.id, value);
    }
  });
  if (
    slotIndices.length === validIds.length &&
    slotIndices.every((slotIdx, i) => values[slotIdx].id === validIds[i])
  ) {
    return values;
  }
  const next = [...values];
  slotIndices.forEach((slotIdx, i) => {
    const item = byId.get(validIds[i]);
    if (item) next[slotIdx] = item;
  });
  return next;
}

/**
 * Group Launch Configurations by harness while preserving configuration order
 * within each harness. Harnesses follow the order announced by `targets`
 * (which carries display labels), with any remaining harnesses (e.g. an
 * uninstalled harness) appended in their order of first appearance.
 */
export function groupConfigurationsByHarness(
  values: SpawnConfiguration[],
  targets: LaunchTarget[],
): LaunchConfigurationGroup[] {
  const harnessNameById = new Map<string, string>();
  const targetOrder: string[] = [];
  for (const target of targets) {
    if (!harnessNameById.has(target.harness_id)) {
      harnessNameById.set(target.harness_id, target.harness_name || target.harness_id);
      targetOrder.push(target.harness_id);
    }
  }

  const buckets = new Map<string, SpawnConfiguration[]>();
  const appearanceOrder: string[] = [];
  for (const value of values) {
    const harnessId = configurationHarnessId(value);
    let bucket = buckets.get(harnessId);
    if (!bucket) {
      bucket = [];
      buckets.set(harnessId, bucket);
      appearanceOrder.push(harnessId);
    }
    bucket.push(value);
  }

  const orderedHarnessIds = [
    ...targetOrder.filter((id) => buckets.has(id)),
    ...appearanceOrder.filter((id) => !targetOrder.includes(id)),
  ];

  return orderedHarnessIds.map((harnessId) => ({
    harnessId,
    harnessName: harnessNameById.get(harnessId) ?? harnessId,
    configurations: buckets.get(harnessId) ?? [],
  }));
}
