import type { ProbeTab } from './probeContext';

export const PROBE_GROUPS = [
  { id: 'files', label: 'Files & changes', tabs: ['files', 'review'] },
  { id: 'issues', label: 'GitHub', tabs: ['issues', 'pulls'] },
] as const;

export function probeGroup(tab: ProbeTab) {
  return PROBE_GROUPS.find(group => group.tabs.some(value => value === tab));
}

export function probeGroupId(tab: ProbeTab): ProbeTab {
  return probeGroup(tab)?.id ?? tab;
}

export function groupedProbeTabs(tabs: readonly ProbeTab[], active: ProbeTab, mru: readonly ProbeTab[]): ProbeTab[] {
  const seen = new Set<ProbeTab>();
  return tabs.flatMap(tab => {
    const id = probeGroupId(tab);
    if (seen.has(id)) return [];
    seen.add(id);
    return [probeGroupId(active) === id ? active : mru.find(value => probeGroupId(value) === id) ?? tab];
  });
}

export function rememberProbeSubview(tab: ProbeTab): void {
  const group = probeGroup(tab);
  if (!group) return;
  try { localStorage.setItem(`buildmesh.probe-subview.${group.id}`, tab); } catch { /* Session navigation still works. */ }
}

export function restoredProbeSubview(tab: ProbeTab): ProbeTab {
  const group = probeGroup(tab);
  if (!group) return tab;
  try {
    const saved = localStorage.getItem(`buildmesh.probe-subview.${group.id}`);
    return group.tabs.find(value => value === saved) ?? tab;
  } catch { return tab; }
}
