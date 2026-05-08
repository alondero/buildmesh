import type { GitStatus, DiffResult } from '../../lib/tauri';

interface DiffViewerModalProps {
  file: GitStatus;
  diff: DiffResult;
  onClose: () => void;
}

export function DiffViewerModal({ file, diff, onClose }: DiffViewerModalProps) {
  return (
    <div className="fixed inset-0 bg-black/80 flex items-center justify-center z-50">
      <div className="bg-[#1a1a1a] border border-[#2a2a2a] rounded-lg w-[95vw] h-[85vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[#2a2a2a]">
          <div className="flex items-center gap-2">
            <span className={`w-2 h-2 rounded-full ${
              file.status === 'added' ? 'bg-green-400' :
              file.status === 'modified' ? 'bg-amber-400' :
              file.status === 'deleted' ? 'bg-red-400' :
              'bg-gray-400'
            }`} />
            <h2 className="text-sm font-semibold">{file.path}</h2>
          </div>
          <button
            onClick={onClose}
            className="text-[#888] hover:text-white text-lg"
          >
            ×
          </button>
        </div>

        {/* Diff content */}
        <div className="flex-1 overflow-auto">
          {diff.files.length === 0 ? (
            <div className="flex items-center justify-center h-full text-[#666]">
              No changes
            </div>
          ) : (
            <div className="space-y-4 p-4">
              {diff.files.map((file) => (
                <div key={file.path} className="border border-[#2a2a2a] rounded overflow-hidden">
                  <div className="px-3 py-2 bg-[#111] text-xs text-[#888] border-b border-[#2a2a2a] font-mono">
                    {file.path}
                  </div>

                  {/* Unified diff */}
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
