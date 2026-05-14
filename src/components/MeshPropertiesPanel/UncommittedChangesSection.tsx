import { useState } from 'react';
import { createPrForMesh, type GitStatus } from '../../lib/tauri';

interface UncommittedChangesSectionProps {
  meshPath: string;
  meshName: string;
  files: GitStatus[];
  isAuthenticated: boolean;
  defaultBranch: string;
  onViewDiff: () => void;
  onRefresh: () => void;
}

const statusDots: Record<string, string> = {
  added: 'bg-green-400',
  modified: 'bg-amber-400',
  deleted: 'bg-red-400',
  renamed: 'bg-purple-400',
  untracked: 'bg-gray-400',
};

const statusPrefix: Record<string, string> = {
  added: 'A',
  modified: 'M',
  deleted: 'D',
  renamed: 'R',
  untracked: '??',
};

export function UncommittedChangesSection({
  meshPath,
  meshName,
  files,
  isAuthenticated,
  defaultBranch,
  onViewDiff,
  onRefresh,
}: UncommittedChangesSectionProps) {
  const [showPrForm, setShowPrForm] = useState(false);
  const [prTitle, setPrTitle] = useState(`Mesh: ${meshName} updates`);
  const [prBody, setPrBody] = useState(
    `Changed files:\n${files.map((f) => `- ${f.path}`).join('\n')}`
  );
  const [submitting, setSubmitting] = useState(false);
  const [prUrl, setPrUrl] = useState<string | null>(null);
  const [prError, setPrError] = useState<string | null>(null);

  const added = files.filter(
    (f) => f.status === 'added' || f.status === 'untracked'
  ).length;
  const modified = files.filter(
    (f) => f.status === 'modified' || f.status === 'renamed'
  ).length;
  const deleted = files.filter((f) => f.status === 'deleted').length;

  const handleCreatePr = async () => {
    setSubmitting(true);
    setPrError(null);
    try {
      const url = await createPrForMesh(meshPath, prTitle, prBody, defaultBranch);
      setPrUrl(url);
      onRefresh();
    } catch (e) {
      setPrError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  if (prUrl) {
    return (
      <div className="rounded border border-green-400/30 bg-green-400/10 p-3">
        <p className="text-xs text-green-400 font-medium mb-1">PR Created!</p>
        <a
          href={prUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="text-xs text-accent-cyan hover:underline break-all"
        >
          {prUrl}
        </a>
      </div>
    );
  }

  return (
    <div className="rounded border border-[#2a2a2a] p-3 space-y-3">
      <div className="space-y-1">
        <p className="text-xs font-medium text-[#e0e0e0]">Uncommitted Changes</p>
        <div className="space-y-0.5 max-h-32 overflow-y-auto">
          {files.map((file) => (
            <div key={file.path} className="flex items-center gap-2 text-xs">
              <span className="text-[10px] font-mono text-[#6b7280] w-4 text-right">
                {statusPrefix[file.status] ?? '??'}
              </span>
              <span
                className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${
                  statusDots[file.status] ?? 'bg-gray-400'
                }`}
              />
              <span className="text-[#d1d5db] truncate">{file.path}</span>
            </div>
          ))}
        </div>
      </div>

      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-[10px]">
          <span className="text-green-400">+{added}</span>
          <span className="text-amber-400">~{modified}</span>
          <span className="text-red-400">-{deleted}</span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={onViewDiff}
            className="text-[10px] text-[#9ca3af] hover:text-[#e0e0e0] transition-colors"
          >
            View Diff
          </button>
          {!isAuthenticated ? (
            <span className="text-[10px] text-[#ef4444]">
              Run `gh auth login` first
            </span>
          ) : (
            <button
              onClick={() => setShowPrForm(true)}
              className="text-[10px] text-accent-cyan hover:text-accent-cyan/80 transition-colors"
            >
              Create PR
            </button>
          )}
        </div>
      </div>

      {showPrForm && (
        <div className="space-y-2 pt-2 border-t border-[#2a2a2a]">
          <div>
            <label className="block text-[10px] text-[#9ca3af] mb-1">Title</label>
            <input
              type="text"
              value={prTitle}
              onChange={(e) => setPrTitle(e.target.value)}
              className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1 text-xs text-[#e0e0e0] focus:outline-none focus:border-accent-cyan"
            />
          </div>
          <div>
            <label className="block text-[10px] text-[#9ca3af] mb-1">
              Body
            </label>
            <textarea
              value={prBody}
              onChange={(e) => setPrBody(e.target.value)}
              rows={4}
              className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1 text-xs text-[#e0e0e0] focus:outline-none focus:border-accent-cyan resize-none"
            />
          </div>
          <div className="flex items-center gap-2">
            <span className="text-[10px] text-[#6b7280]">
              Base: <span className="font-mono text-[#9ca3af]">{defaultBranch}</span>
            </span>
          </div>
          {prError && (
            <p className="text-[10px] text-red-400">{prError}</p>
          )}
          <div className="flex items-center gap-2">
            <button
              onClick={handleCreatePr}
              disabled={submitting}
              className="flex-1 bg-accent-cyan/10 hover:bg-accent-cyan/20 text-accent-cyan text-xs py-1.5 rounded transition-colors disabled:opacity-50"
            >
              {submitting ? 'Creating...' : 'Submit PR'}
            </button>
            <button
              onClick={() => {
                setShowPrForm(false);
                setPrError(null);
              }}
              className="flex-1 bg-[#2a2a2a] hover:bg-[#3a3a3a] text-[#9ca3af] text-xs py-1.5 rounded transition-colors"
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}