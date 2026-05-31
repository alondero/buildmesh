import { useState, useEffect, useMemo } from 'react';
import { discoverSessions, importDiscoveredSession, type DiscoveredSession } from '../../lib/tauri';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';

interface SessionBrowserModalProps {
  meshId: number;
  meshPath: string;
  providerList: Array<{ id: string; label: string; color: string }>;
  onClose: () => void;
}

function timeAgo(isoString: string): string {
  const now = Date.now();
  const then = new Date(isoString).getTime();
  const diffMs = now - then;
  const minutes = Math.floor(diffMs / 60000);
  if (minutes < 1) return 'just now';
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(isoString).toLocaleDateString();
}

export function SessionBrowserModal({ meshId, meshPath, providerList, onClose }: SessionBrowserModalProps) {
  const [sessions, setSessions] = useState<DiscoveredSession[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [resuming, setResuming] = useState<string | null>(null);
  const [openDropdown, setOpenDropdown] = useState<string | null>(null);

  const fetchAgentNodes = useAgentNodeStore(state => state.fetchAgentNodes);
  const spawnAgent = useAgentNodeStore(state => state.spawnAgent);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);
  const selectMesh = useMeshStore(state => state.selectMesh);
  const getDefaultProvider = useMeshStore(state => state.getDefaultProvider);

  useEffect(() => {
    const load = async () => {
      try {
        const result = await discoverSessions(meshId, meshPath);
        setSessions(result);
      } catch (e) {
        console.error('Failed to discover sessions:', e);
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    };
    load();
  }, [meshId, meshPath]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return sessions;
    return sessions.filter(s =>
      s.first_message.toLowerCase().includes(q) ||
      (s.branch && s.branch.toLowerCase().includes(q)) ||
      (s.worktree_name && s.worktree_name.toLowerCase().includes(q))
    );
  }, [search, sessions]);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [onClose]);

  // Close dropdown on outside click
  useEffect(() => {
    if (!openDropdown) return;
    const handleClick = () => setOpenDropdown(null);
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [openDropdown]);

  const handleResume = async (session: DiscoveredSession, providerId: string) => {
    setResuming(session.session_id);
    setOpenDropdown(null);
    try {
      const node = await importDiscoveredSession(
        meshId,
        meshPath,
        session.session_id,
        session.branch || 'main',
        session.worktree_name,
        providerId,
      );
      await fetchAgentNodes();
      await setActiveNode(node.id);
      selectMesh(meshId);
      await spawnAgent(node.id, providerId);
      onClose();
    } catch (e) {
      console.error('Failed to resume session:', e);
      setResuming(null);
    }
  };

  const handleDefaultResume = async (session: DiscoveredSession) => {
    const defaultProvider = await getDefaultProvider(meshId);
    await handleResume(session, defaultProvider);
  };

  // Filter providers to only those that support resume (Claude Code-backed)
  const resumableProviders = providerList.filter(p =>
    ['anthropic', 'minimax', 'kimi'].includes(p.id)
  );

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" onClick={onClose}>
      <div className="absolute inset-0 bg-black/70" />

      <div
        className="relative bg-bg-overlay border border-border-default rounded-lg shadow-2xl w-full max-w-2xl max-h-[70vh] flex flex-col"
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-border-subtle">
          <div>
            <h2 className="text-sm font-semibold text-text-primary">Previous Sessions</h2>
            <p className="text-[10px] text-text-muted mt-0.5">{meshPath}</p>
          </div>
          <button
            onClick={onClose}
            className="text-text-muted hover:text-text-secondary text-lg leading-none"
          >
            ×
          </button>
        </div>

        {/* Search */}
        <div className="px-4 py-2 border-b border-border-subtle">
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Filter by message, branch, or worktree…"
            className="w-full bg-bg-card border border-border-default rounded px-3 py-1.5 text-xs text-text-primary placeholder:text-text-muted focus:outline-none focus:border-accent-cyan"
            autoFocus
          />
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex flex-col items-center justify-center py-8 gap-3">
              <div className="animate-spin w-5 h-5 border border-accent-cyan border-t-transparent rounded-full" />
              <span className="text-xs text-text-muted">Scanning sessions…</span>
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-8">
              <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-red-400 mb-2">
                <circle cx="12" cy="12" r="10"/>
                <line x1="15" y1="9" x2="9" y2="15"/>
                <line x1="9" y1="9" x2="15" y2="15"/>
              </svg>
              <span className="text-xs text-red-400">Failed to discover sessions</span>
              <span className="text-[10px] text-text-muted mt-1 max-w-[280px] text-center">{error}</span>
            </div>
          ) : filtered.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-8">
              <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-text-muted mb-2">
                <circle cx="12" cy="12" r="10"/>
                <line x1="12" y1="8" x2="12" y2="12"/>
                <line x1="12" y1="16" x2="12.01" y2="16"/>
              </svg>
              <span className="text-xs text-text-muted">
                {sessions.length === 0 ? 'No previous sessions found' : 'No matches'}
              </span>
            </div>
          ) : (
            <div className="space-y-1">
              {filtered.map(session => (
                <div
                  key={session.session_id}
                  className="flex items-center gap-2 px-3 py-2 rounded hover:bg-bg-card transition-colors"
                >
                  <div className="flex-1 min-w-0">
                    <div className="text-sm text-text-primary truncate">{session.first_message}</div>
                    <div className="flex items-center gap-2 mt-0.5">
                      {session.branch && (
                        <span className="text-[10px] text-accent-cyan font-mono">{session.branch}</span>
                      )}
                      {session.worktree_name && (
                        <span className="text-[10px] text-accent-purple font-mono">{session.worktree_name}</span>
                      )}
                      {session.timestamp && (
                        <span className="text-[10px] text-text-muted">{timeAgo(session.timestamp)}</span>
                      )}
                    </div>
                  </div>

                  {/* Split resume button */}
                  <div className="relative flex shrink-0" onMouseDown={e => e.stopPropagation()}>
                    <button
                      onClick={() => handleDefaultResume(session)}
                      disabled={resuming !== null}
                      className="px-2.5 py-1 text-xs font-medium rounded-l bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                    >
                      {resuming === session.session_id ? 'Resuming…' : 'Resume'}
                    </button>
                    <button
                      onClick={() => setOpenDropdown(openDropdown === session.session_id ? null : session.session_id)}
                      disabled={resuming !== null}
                      className="px-1.5 py-1 text-xs font-medium rounded-r border-l border-accent-cyan/20 bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                    >
                      ▾
                    </button>
                    {openDropdown === session.session_id && (
                      <div className="absolute right-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded shadow-lg py-1 min-w-[120px]">
                        {resumableProviders.map(p => (
                          <button
                            key={p.id}
                            onClick={() => handleResume(session, p.id)}
                            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
                          >
                            <span className={`w-2 h-2 rounded-full ${p.color}`} />
                            {p.label}
                          </button>
                        ))}
                      </div>
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
