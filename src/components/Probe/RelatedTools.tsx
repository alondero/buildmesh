import { useUIStore } from '../../stores/uiStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { PROBE_TAB_DEFINITIONS } from '../../lib/probeContext';
import { probeGroup } from '../../lib/probeGroups';

export function RelatedTools() {
  const tab = useUIStore(s => s.probeTab);
  const open = useUIStore(s => s.openProbeTab);
  const context = useProbeContext();
  const node = useAgentNodeStore(s => context.activeNodeId === null ? undefined : s.nodesById[context.activeNodeId]);
  const mesh = useMeshStore(s => context.activeMeshId === null ? undefined : s.meshesById.get(context.activeMeshId));
  const group = probeGroup(tab);
  if (!group) return null;
  return <div className="shrink-0 min-w-0 border-b border-border-subtle p-2">
    <p className="mb-1 text-2xs text-text-secondary">{group.label}</p>
    <div className="flex gap-1" role="group" aria-label={`${group.label} views`}>
      {group.tabs.map((value, index) => <button key={value} type="button" aria-pressed={tab === value}
        onClick={() => open(value)} onKeyDown={e => {
          const next = e.key === 'ArrowRight' ? (index + 1) % group.tabs.length
            : e.key === 'ArrowLeft' ? (index + group.tabs.length - 1) % group.tabs.length
            : e.key === 'Home' ? 0 : e.key === 'End' ? group.tabs.length - 1 : null;
          if (next === null) return;
          e.preventDefault(); open(group.tabs[next]);
          e.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('button')[next]?.focus();
        }} className={`min-h-[24px] flex-1 min-w-0 rounded-md px-2 py-1 text-xs ${tab === value ? 'bg-bg-card text-text-primary' : 'text-text-secondary hover:bg-bg-card-hover'}`}>
        {value === 'files' ? 'Explorer' : value === 'review' ? 'Agent changes' : value === 'issues' ? 'Issues' : 'Pull requests'}
      </button>)}
    </div>
    {group.id === 'files' && <p className="mt-1 break-all text-2xs text-text-secondary" data-testid="files-baseline">
      {tab === 'files' ? 'Working tree vs HEAD' : `Agent changes vs merge base (${mesh?.base_ref || node?.branch || 'base ref unavailable'})`}
      {' · '}{PROBE_TAB_DEFINITIONS[tab].lens === 'agent' ? 'Agent scope' : 'Repository scope'}
    </p>}
  </div>;
}
