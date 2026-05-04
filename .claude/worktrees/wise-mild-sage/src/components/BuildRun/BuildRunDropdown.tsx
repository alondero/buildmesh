import { useState, useRef, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AgentNode } from '../../stores/agentNodeStore';

interface BuildRunDropdownProps {
  node: AgentNode;
  onBuildRun: (nodeId: number, mode: 'build' | 'run') => void;
}

export function BuildRunDropdown({ node, onBuildRun }: BuildRunDropdownProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setIsOpen(false);
        setError(null);
      }
    };

    if (isOpen) {
      document.addEventListener('mousedown', handleClickOutside);
    }

    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
    };
  }, [isOpen]);

  const closeDropdown = () => {
    setIsOpen(false);
    setError(null);
  };

  const handleBuildRun = async (mode: 'build' | 'run') => {
    closeDropdown();
    try {
      await invoke('build_run', { nodeId: node.id, mode });
      onBuildRun(node.id, mode);
    } catch (e) {
      setError(String(e));
      console.error('[BuildRunDropdown] build/run failed:', e);
    }
  };

  const handleBuild = () => handleBuildRun('build');
  const handleRun = () => handleBuildRun('run');

  const handleOpenConfig = () => {
    closeDropdown();
    // Derive mesh root from worktree path by going up one level from worktrees/agent-{id}
    // The path is: mesh_root/worktrees/agent-{id}, so we need mesh_root
    const worktreesIndex = node.path.lastIndexOf('/worktrees/');
    const meshRoot = worktreesIndex !== -1 ? node.path.substring(0, worktreesIndex) : node.path;
    window.open(meshRoot + '/mesh.toml');
  };

  return (
    <div className="relative" ref={dropdownRef}>
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="flex items-center gap-0.5 px-1.5 py-0.5 rounded text-[10px] text-text-muted hover:text-accent-cyan hover:bg-bg-base transition-colors border border-transparent hover:border-border-default"
      >
        <span>Build</span>
        <span className="text-[8px]">▼</span>
      </button>

      {isOpen && (
        <div className="absolute right-0 top-full mt-1 w-44 bg-bg-elevated border border-border-default rounded shadow-lg z-50">
          <button
            onClick={handleBuild}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Build from worktree
          </button>
          <button
            onClick={handleRun}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Run from worktree
          </button>
          <div className="border-t border-border-default" />
          <button
            onClick={handleOpenConfig}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-muted hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Open mesh.toml
          </button>
          {error && (
            <div className="px-3 py-1.5 text-[10px] text-status-error border-t border-border-default">
              {error}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
