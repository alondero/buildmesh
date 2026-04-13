import { useState, useEffect } from 'react';
import { diffWorkspaceCheckpoint } from '../../lib/tauri';
import type { DiffResult } from '../../stores/workspaceStore';

interface DiffViewerProps {
  workspaceId: number;
  checkpointId: number;
  onClose: () => void;
}

export function DiffViewer({ workspaceId, checkpointId, onClose }: DiffViewerProps) {
  const [diff, setDiff] = useState<DiffResult | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    diffWorkspaceCheckpoint(workspaceId, checkpointId)
      .then(setDiff)
      .catch(console.error)
      .finally(() => setLoading(false));
  }, [workspaceId, checkpointId]);

  return (
    <div className="fixed inset-0 bg-black/80 flex items-center justify-center z-50">
      <div className="bg-[#1a1a1a] border border-[#2a2a2a] rounded-lg w-[90vw] h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[#2a2a2a]">
          <h2 className="text-sm font-semibold">Diff Viewer</h2>
          <button
            onClick={onClose}
            className="text-[#888] hover:text-white text-lg"
          >
            ×
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-auto p-4">
          {loading ? (
            <p className="text-[#666]">Loading diff...</p>
          ) : diff?.files.length === 0 ? (
            <p className="text-[#666]">No changes</p>
          ) : (
            <div class="space-y-4">
              {diff?.files.map((file) => (
                <div key={file.path} className="border border-[#2a2a2a] rounded">
                  <div className="px-3 py-2 bg-[#111] text-xs text-[#888] border-b border-[#2a2a2a]">
                    {file.path}
                  </div>
                  <div className="font-mono text-xs">
                    {file.hunks.map((hunk, hi) => (
                      <div key={hi} className="py-1">
                        {hunk.lines.map((line, li) => (
                          <div
                            key={li}
                            className={`
                              px-3 py-0.5 flex
                              ${line.line_type === 'add' ? 'bg-[#22c55e20] text-[#22c55e]' : ''}
                              ${line.line_type === 'remove' ? 'bg-[#ef444420] text-[#ef4444]' : ''}
                              ${line.line_type === 'context' ? 'text-[#e0e0e0]' : ''}
                            `}
                          >
                            <span className="w-8 text-[#666] select-none">
                              {line.old_num || line.new_num || ''}
                            </span>
                            <span className="w-4 text-[#666] select-none">
                              {line.line_type === 'add' ? '+' : line.line_type === 'remove' ? '-' : ' '}
                            </span>
                            <span>{line.content}</span>
                          </div>
                        ))}
                      </div>
                    ))}
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
