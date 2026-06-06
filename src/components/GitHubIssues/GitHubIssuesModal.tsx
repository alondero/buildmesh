import { useState, useEffect } from 'react';
import { getRepoIssues, spawnIssueAgent, type GitHubIssue } from '../../lib/tauri';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { ProviderDropdown, type ProviderEntry } from '../Sidebar/ProviderDropdown';

interface GitHubIssuesModalProps {
  meshId: number;
  meshPath: string;
  providerList: ProviderEntry[];
  getDefaultProvider: (meshId: number) => Promise<string>;
  onClose: () => void;
}

export function GitHubIssuesModal({ meshId, meshPath, providerList, getDefaultProvider, onClose }: GitHubIssuesModalProps) {
  const [issues, setIssues] = useState<GitHubIssue[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [spawning, setSpawning] = useState<number | null>(null);
  // Only one dropdown open at a time, keyed by issue number — mirrors the
  // SessionBrowserModal pattern so the click-outside handling stays simple.
  const [openDropdown, setOpenDropdown] = useState<number | null>(null);

  useEffect(() => {
    const load = async () => {
      try {
        const result = await getRepoIssues(meshId);
        setIssues(result);
      } catch (e) {
        console.error('Failed to load issues:', e);
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    };
    load();
  }, [meshId]);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [onClose]);

  // Close the provider dropdown when clicking outside of it. The dropdown
  // container carries a `data-dropdown-for` attribute set to the issue number.
  useEffect(() => {
    if (openDropdown === null) return;
    const handleClick = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (!target.closest(`[data-dropdown-for="${openDropdown}"]`)) {
        setOpenDropdown(null);
      }
    };
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [openDropdown]);

  const handleSpawn = async (issue: GitHubIssue, providerId: string) => {
    setSpawning(issue.number);
    try {
      await spawnIssueAgent(meshId, issue.number, issue.title, providerId);
      setOpenDropdown(null);
      await useAgentNodeStore.getState().fetchAgentNodes();
      onClose();
    } catch (e) {
      console.error('Failed to spawn issue agent:', e);
      setSpawning(null);
    }
  };

  // Primary "Spawn" button uses the mesh's resolved default provider —
  // explicit > per-mesh > app-wide > "anthropic" fallback is enforced
  // server-side by spawn_new_agent_impl when we pass `provider`.
  // We mark `spawning` BEFORE awaiting getDefaultProvider so the split
  // button's `disabled` immediately blocks a second click on the same
  // issue (e.g. picking a different provider in the still-open dropdown)
  // from racing with the in-flight default-resolution IPC.
  const handleDefaultSpawn = async (issue: GitHubIssue) => {
    setSpawning(issue.number);
    try {
      const defaultProvider = await getDefaultProvider(meshId);
      await spawnIssueAgent(meshId, issue.number, issue.title, defaultProvider);
      setOpenDropdown(null);
      await useAgentNodeStore.getState().fetchAgentNodes();
      onClose();
    } catch (e) {
      console.error('Failed to spawn issue agent:', e);
      setSpawning(null);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" onClick={onClose}>
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/70" />

      {/* Modal */}
      <div
        className="relative bg-bg-overlay border border-border-default rounded-lg shadow-2xl w-full max-w-2xl max-h-[70vh] flex flex-col"
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-border-subtle">
          <div>
            <h2 className="text-sm font-semibold text-text-primary">GitHub Issues</h2>
            <p className="text-[10px] text-text-muted mt-0.5">{meshPath}</p>
          </div>
          <button
            onClick={onClose}
            className="text-text-muted hover:text-text-secondary text-lg leading-none"
          >
            ×
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex flex-col items-center justify-center py-8 gap-3">
              <div className="animate-spin w-5 h-5 border border-accent-cyan border-t-transparent rounded-full" />
              <span className="text-xs text-text-muted">Loading issues...</span>
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-8">
              <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-red-400 mb-2">
                <circle cx="12" cy="12" r="10"/>
                <line x1="15" y1="9" x2="9" y2="15"/>
                <line x1="9" y1="9" x2="15" y2="15"/>
              </svg>
              <span className="text-xs text-red-400">Failed to load issues</span>
              <span className="text-[10px] text-text-muted mt-1 max-w-[280px] text-center">{error}</span>
            </div>
          ) : issues.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-8">
              <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-text-muted mb-2">
                <circle cx="12" cy="12" r="10"/>
                <line x1="12" y1="8" x2="12" y2="12"/>
                <line x1="12" y1="16" x2="12.01" y2="16"/>
              </svg>
              <span className="text-xs text-text-muted">No open issues</span>
            </div>
          ) : (
            <div className="space-y-1">
              {issues.map(issue => (
                <div
                  key={issue.number}
                  className="flex items-center gap-2 px-3 py-2 rounded hover:bg-bg-card transition-colors"
                >
                  <div className="flex-1 min-w-0">
                    <div>
                      <span className="text-xs text-accent-cyan font-mono">#{issue.number}</span>
                      <span className="text-sm text-text-primary ml-2">{issue.title}</span>
                    </div>
                    {issue.body && (
                      <p className="text-[10px] text-text-muted mt-1 line-clamp-2">{issue.body}</p>
                    )}
                  </div>

                  {/* Split spawn button — primary uses default provider, ▾ opens picker */}
                  <div className="relative flex shrink-0" onMouseDown={e => e.stopPropagation()}>
                    <button
                      onClick={() => handleDefaultSpawn(issue)}
                      disabled={spawning !== null}
                      className="px-2.5 py-1 text-xs font-medium rounded-l bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                    >
                      {spawning === issue.number ? 'Spawning...' : 'Spawn'}
                    </button>
                    <button
                      onClick={() => setOpenDropdown(openDropdown === issue.number ? null : issue.number)}
                      disabled={spawning !== null}
                      className="px-1.5 py-1 text-xs font-medium rounded-r border-l border-accent-cyan/20 bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                      title="Choose provider"
                    >
                      ▾
                    </button>
                    {openDropdown === issue.number && (
                      <ProviderDropdown
                        meshId={issue.number}
                        providers={providerList}
                        onSelect={(providerId) => handleSpawn(issue, providerId)}
                      />
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
