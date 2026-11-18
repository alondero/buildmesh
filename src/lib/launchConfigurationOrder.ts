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

export interface LaunchConfigurationReorderSubscriber {
  /** Called immediately when a harness fails and has no newer queued intent,
   *  passing that harness's last confirmed order (when known) so the subscriber
   *  can roll back ONLY that harness's slice synchronously. */
  onHarnessFailed?: (
    harnessId: string,
    confirmedIds: string[] | undefined,
    error: unknown,
  ) => void;
  /** Called once when the coordinator's queue drains after processing one or
   *  more requests. */
  onDrained?: (summary: { anySucceeded: boolean; anyFailed: boolean }) => void;
}

export interface LaunchConfigurationReorderCoordinator {
  /** Subscribe a mounted surface to failure and drain notifications. Returns an
   *  unsubscribe function; unsubscribing never drops queued/in-flight writes. */
  subscribe: (subscriber: LaunchConfigurationReorderSubscriber) => () => void;
  /** Record the authoritative per-harness order from a committed list snapshot. */
  syncConfirmed: (values: SpawnConfiguration[]) => void;
  /** Advance the mutation version (e.g. after a local save/delete) so any
   *  in-flight list read that started earlier will be recognized as stale. */
  bumpMutationVersion: () => void;
  /** Current monotonic mutation version. */
  getMutationVersion: () => number;
  /** True while any persistence request is in flight or queued. */
  isPending: () => boolean;
  /** Overlay any in-flight or queued per-harness target orders onto a freshly
   *  fetched snapshot so a reopened surface immediately shows accepted pending
   *  reorders while waiting for them to settle. */
  applyPendingOverlay: (values: SpawnConfiguration[]) => SpawnConfiguration[];
  /** Enqueue a desired order for one harness. Serializes all writes so older
   *  requests can never overwrite newer ones, coalesces multiple drags for the
   *  same harness, and tracks independent per-harness rollback state. */
  enqueue: (
    harnessId: string,
    confirmedFallbackIds: string[],
    nextIds: string[],
  ) => void;
}

/** Merge a subset/full reordered id list into an existing confirmed id list in
 *  place, preserving any ids not mentioned in `orderedIds` (e.g. configs hidden
 *  behind another Codex login). */
function mergeConfirmedIds(existing: string[] | undefined, orderedIds: string[]): string[] {
  if (!existing || existing.length === 0) return [...orderedIds];
  const activeSet = new Set(orderedIds);
  const slotIndices: number[] = [];
  existing.forEach((id, idx) => {
    if (activeSet.has(id)) slotIndices.push(idx);
  });
  if (slotIndices.length !== orderedIds.length) {
    // Some newly ordered id wasn't in `existing` yet; append unknowns at the end.
    const next = [...existing];
    let cursor = 0;
    for (const id of orderedIds) {
      if (cursor < slotIndices.length && existing.includes(id)) {
        next[slotIndices[cursor++]] = id;
      } else if (!next.includes(id)) {
        next.push(id);
      }
    }
    return next;
  }
  const next = [...existing];
  slotIndices.forEach((slotIdx, i) => {
    next[slotIdx] = orderedIds[i];
  });
  return next;
}

export type PersistLaunchConfigurationOrder = (
  harnessId: string,
  orderedIds: string[],
) => Promise<void>;

export function createLaunchConfigurationReorderCoordinator(
  persist: PersistLaunchConfigurationOrder,
): LaunchConfigurationReorderCoordinator {
  const subscribers = new Set<LaunchConfigurationReorderSubscriber>();
  const confirmedByHarness = new Map<string, string[]>();
  const queuedByHarness = new Map<string, string[]>();
  let inFlightHarness: string | null = null;
  let inFlightIds: string[] | null = null;
  let anySucceededInBatch = false;
  let anyFailedInBatch = false;
  let mutationVersion = 0;

  const drain = async () => {
    while (queuedByHarness.size > 0) {
      const next = queuedByHarness.entries().next();
      if (next.done) break;
      const [harnessId, targetIds] = next.value;
      queuedByHarness.delete(harnessId);
      inFlightHarness = harnessId;
      inFlightIds = targetIds;

      try {
        await persist(harnessId, targetIds);
        confirmedByHarness.set(
          harnessId,
          mergeConfirmedIds(confirmedByHarness.get(harnessId), targetIds),
        );
        anySucceededInBatch = true;
      } catch (error: unknown) {
        anyFailedInBatch = true;
        // If a newer order for this same harness was queued while this request
        // was in flight, let the next iteration attempt the newer order instead
        // of flashing a premature rollback. Otherwise restore ONLY this harness
        // to its last confirmed order across every mounted subscriber.
        if (!queuedByHarness.has(harnessId)) {
          mutationVersion += 1;
          const confirmed = confirmedByHarness.get(harnessId);
          for (const subscriber of Array.from(subscribers)) {
            subscriber.onHarnessFailed?.(harnessId, confirmed, error);
          }
        }
      } finally {
        inFlightHarness = null;
        inFlightIds = null;
      }
    }

    const summary = {
      anySucceeded: anySucceededInBatch,
      anyFailed: anyFailedInBatch,
    };
    anySucceededInBatch = false;
    anyFailedInBatch = false;
    for (const subscriber of Array.from(subscribers)) {
      subscriber.onDrained?.(summary);
    }
  };

  return {
    subscribe(subscriber: LaunchConfigurationReorderSubscriber) {
      subscribers.add(subscriber);
      return () => {
        subscribers.delete(subscriber);
      };
    },
    syncConfirmed(values: SpawnConfiguration[]) {
      confirmedByHarness.clear();
      for (const value of values) {
        const harnessId = configurationHarnessId(value);
        const list = confirmedByHarness.get(harnessId);
        if (list) list.push(value.id);
        else confirmedByHarness.set(harnessId, [value.id]);
      }
    },
    bumpMutationVersion() {
      mutationVersion += 1;
    },
    getMutationVersion() {
      return mutationVersion;
    },
    isPending() {
      return inFlightHarness !== null || queuedByHarness.size > 0;
    },
    applyPendingOverlay(values: SpawnConfiguration[]) {
      let result = values;
      if (inFlightHarness !== null && inFlightIds !== null && !queuedByHarness.has(inFlightHarness)) {
        result = applyHarnessConfigurationOrder(result, inFlightHarness, inFlightIds);
      }
      for (const [harnessId, targetIds] of queuedByHarness.entries()) {
        result = applyHarnessConfigurationOrder(result, harnessId, targetIds);
      }
      return result;
    },
    enqueue(harnessId: string, confirmedFallbackIds: string[], nextIds: string[]) {
      if (!confirmedByHarness.has(harnessId)) {
        confirmedByHarness.set(harnessId, [...confirmedFallbackIds]);
      }
      mutationVersion += 1;
      queuedByHarness.set(harnessId, [...nextIds]);
      if (inFlightHarness === null) {
        void drain();
      }
    },
  };
}

let coordinatorsByPersist = new WeakMap<
  PersistLaunchConfigurationOrder,
  LaunchConfigurationReorderCoordinator
>();

/**
 * Return the long-lived reorder coordinator for a persistence transport
 * (`reorderSpawnConfigurations` on desktop, `launchConfigurationApi.reorder` on
 * mobile). Lives above mounted UI surfaces so closing a flyout or settings
 * modal never drops accepted queued writes, and reopening attaches to the same
 * serialized queue rather than racing an in-flight write.
 */
export function getLaunchConfigurationReorderCoordinator(
  persist: PersistLaunchConfigurationOrder,
): LaunchConfigurationReorderCoordinator {
  let coordinator = coordinatorsByPersist.get(persist);
  if (!coordinator) {
    coordinator = createLaunchConfigurationReorderCoordinator(persist);
    coordinatorsByPersist.set(persist, coordinator);
  }
  return coordinator;
}

/** Test-only: reset module-level coordinators between isolated test cases. */
export function __resetLaunchConfigurationReorderCoordinatorsForTests(): void {
  coordinatorsByPersist = new WeakMap();
}
