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
        aria-label="Open build menu"
        title="Build / Run / Terminal"
        // Icon-only trigger — saves ~34 px vs the former "Build ▼" pill,
        // and reads as a peer of the close + expand buttons because all
        // three share `h-7` + bg-bg-base/60 + border-border-default with no
        // shadow, so the trio looks like one control group against the
        // mesh-tinted header.
        className="flex items-center gap-0.5 h-7 px-1.5 rounded-md bg-bg-base/60 border border-border-default text-accent-cyan hover:bg-accent-cyan/15 hover:border-accent-cyan/60 transition-colors"
      >
        {/* Wrench — "build / tools" semantic, the common IDE shorthand for
            the "execute build or run" family. 12 px matches the close /
            expand icon sizes so the three controls are visually balanced. */}
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
        </svg>
        <svg width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className="opacity-70">
          <path d="M6 9l6 6 6-6" />
        </svg>
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
