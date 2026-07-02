import { useState, useRef, useEffect } from 'react';
import { AgentNode } from '../../stores/agentNodeStore';

interface BuildRunDropdownProps {
  node: AgentNode;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
}

export function BuildRunDropdown({ node, onBuildRun }: BuildRunDropdownProps) {
  const [isOpen, setIsOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    };

    if (isOpen) {
      document.addEventListener('mousedown', handleClickOutside);
    }

    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
    };
  }, [isOpen]);

  const handleBuild = async () => {
    setIsOpen(false);
    onBuildRun(node.id, 'build');
  };

  const handleRun = async () => {
    setIsOpen(false);
    onBuildRun(node.id, 'run');
  };

  const handleTerminal = async () => {
    setIsOpen(false);
    onBuildRun(node.id, 'terminal');
  };

  return (
    <div className="relative" ref={dropdownRef}>
      <button
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        aria-haspopup="menu"
        aria-expanded={isOpen}
        className="flex items-center gap-1 px-2 py-0.5 rounded-md text-2xs font-sans font-semibold tracking-wide text-accent-cyan bg-accent-cyan/10 hover:bg-accent-cyan/20 hover:text-accent-cyan border border-accent-cyan/30 hover:border-accent-cyan/60 transition-colors shadow-sm"
      >
        <span>Build</span>
        <span className="text-2xs leading-none">▼</span>
      </button>

      {isOpen && (
        <div role="menu" className="absolute right-0 top-full mt-1 w-44 bg-bg-card border border-border-default rounded-md shadow-md z-50 animate-scale-in origin-top-right">
          <button
            onClick={handleBuild}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Build from worktree' : 'Build'}
          </button>
          <button
            onClick={handleRun}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Run from worktree' : 'Run'}
          </button>
          <div className="my-1 border-t border-border-default" />
          <button
            onClick={handleTerminal}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Terminal in worktree' : 'Terminal'}
          </button>
        </div>
      )}
    </div>
  );
}
