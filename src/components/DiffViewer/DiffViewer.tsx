import { useState, useEffect } from 'react';
import { diffSessionCheckpoint } from '../../lib/tauri';
import type { DiffResult } from '../../lib/tauri';

interface DiffViewerProps {
  sessionId: number;
  checkpointId: number;
  onClose: () => void;
}

export function DiffViewer({ sessionId, checkpointId, onClose }: DiffViewerProps) {
  const [diff, setDiff] = useState<DiffResult | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    diffSessionCheckpoint(sessionId, checkpointId)
      .then(setDiff)
      .catch(console.error)
      .finally(() => setLoading(false));
  }, [sessionId, checkpointId]);

  return (
    <div className="fixed inset-0 bg-black/80 flex items-center justify-center z-50">
      <div className="bg-[#1a1a1a] border border-[#2a2a2a] rounded-lg w-[95vw] h-[85vh] flex flex-col">
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

        {/* Side-by-side diff content */}
        <div className="flex-1 overflow-auto">
          {loading ? (
            <div className="flex items-center justify-center h-full text-[#666]">
              Loading diff...
            </div>
          ) : diff?.files.length === 0 ? (
            <div className="flex items-center justify-center h-full text-[#666]">
              No changes
            </div>
          ) : (
            <div className="space-y-4 p-4">
              {diff?.files.map((file) => (
                <div key={file.path} className="border border-[#2a2a2a] rounded overflow-hidden">
                  <div className="px-3 py-2 bg-[#111] text-xs text-[#888] border-b border-[#2a2a2a] font-mono">
                    {file.path}
                  </div>

                  {/* Side-by-side layout */}
                  <div className="grid grid-cols-2 divide-x divide-[#2a2a2a]">
                    {/* Left: old/checkpoint version */}
                    <div className="bg-[#1a1a1a]">
                      <div className="px-2 py-1 bg-[#151515] text-[10px] text-[#666] border-b border-[#2a2a2a]">
                        Checkpoint
                      </div>
                      <div className="p-2">
                        {file.hunks.map((hunk, hi) => (
                          <div
                            key={hi}
                            className="font-mono text-xs leading-5 overflow-x-auto whitespace-pre"
                            dangerouslySetInnerHTML={{ __html: hunk.old_highlighted }}
                          />
                        ))}
                      </div>
                    </div>

                    {/* Right: new/current version */}
                    <div className="bg-[#0f0f0f]">
                      <div className="px-2 py-1 bg-[#0a0a0a] text-[10px] text-[#666] border-b border-[#2a2a2a]">
                        Current
                      </div>
                      <div className="p-2">
                        {file.hunks.map((hunk, hi) => (
                          <div
                            key={hi}
                            className="font-mono text-xs leading-5 overflow-x-auto whitespace-pre"
                            dangerouslySetInnerHTML={{ __html: hunk.new_highlighted }}
                          />
                        ))}
                      </div>
                    </div>
                  </div>

                  {/* Unified diff below */}
                  <details className="bg-[#111]">
                    <summary className="px-3 py-2 text-xs text-[#666] cursor-pointer hover:text-[#888]">
                      Unified diff
                    </summary>
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
                              <span className="w-8 text-[#666] select-none text-right mr-2">
                                {line.old_num || ''}
                              </span>
                              <span className="w-8 text-[#666] select-none text-right mr-2">
                                {line.new_num || ''}
                              </span>
                              <span className="w-4 text-[#666] select-none">
                                {line.line_type === 'add' ? '+' : line.line_type === 'remove' ? '-' : ' '}
                              </span>
                              <span className="whitespace-pre">{line.content}</span>
                            </div>
                          ))}
                        </div>
                      ))}
                    </div>
                  </details>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
