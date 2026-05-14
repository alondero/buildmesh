import type { DiffResult } from '../../lib/tauri';

interface DiffViewProps {
  diff: DiffResult;
}

export function DiffView({ diff }: DiffViewProps) {
  if (diff.files.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-text-muted text-xs">
        No changes
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-auto font-mono text-xs">
      {diff.files.map((file) => (
        <div key={file.path}>
          <div className="sticky top-0 bg-bg-overlay border-b border-border-subtle">
            <div className="px-3 py-1.5 text-[10px] text-text-muted">
              {file.path}
            </div>
          </div>
          <div className="py-1">
            {file.hunks.map((hunk, hi) => (
              <div key={hi} className="py-0.5">
                {hunk.lines.map((line, li) => (
                  <div
                    key={li}
                    className={`
                      px-3 py-px flex
                      ${line.line_type === 'add' ? 'bg-accent-green/10 text-accent-green' : ''}
                      ${line.line_type === 'remove' ? 'bg-accent-red/10 text-accent-red' : ''}
                      ${line.line_type === 'context' ? 'text-text-secondary' : ''}
                    `}
                  >
                    <span className="w-8 text-text-muted/50 select-none text-right mr-2 flex-shrink-0">
                      {line.old_num ?? ''}
                    </span>
                    <span className="w-8 text-text-muted/50 select-none text-right mr-2 flex-shrink-0">
                      {line.new_num ?? ''}
                    </span>
                    <span className="w-4 text-text-muted/50 select-none flex-shrink-0">
                      {line.line_type === 'add' ? '+' : line.line_type === 'remove' ? '-' : ' '}
                    </span>
                    <span className="whitespace-pre">{line.content}</span>
                  </div>
                ))}
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}