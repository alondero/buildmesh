import { useState, useEffect } from 'react';
import { getRepoIssues, type GitHubIssue } from '../../lib/tauri';

interface GitHubIssuesModalProps {
  meshId: number;
  meshPath: string;
  onClose: () => void;
}

export function GitHubIssuesModal({ meshId, meshPath, onClose }: GitHubIssuesModalProps) {
  const [issues, setIssues] = useState<GitHubIssue[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

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

  // Close on Escape
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [onClose]);

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
                  className="px-3 py-2 rounded hover:bg-bg-card cursor-pointer transition-colors"
                >
                  <span className="text-xs text-accent-cyan font-mono">#{issue.number}</span>
                  <span className="text-sm text-text-primary ml-2">{issue.title}</span>
                  {issue.body && (
                    <p className="text-[10px] text-text-muted mt-1 line-clamp-2">{issue.body}</p>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}