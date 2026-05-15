import { useState, useRef, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { openPath } from '@tauri-apps/plugin-opener';
import { AgentNode } from '../../stores/agentNodeStore';

interface BuildRunDropdownProps {
  node: AgentNode;
  onBuildRun: (nodeId: number, mode: 'build' | 'run') => void;
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

  const handleOpenConfig = async () => {
    setIsOpen(false);
    const configPath = await invoke<string>('ensure_mesh_config', { meshId: node.mesh_id });
    await openPath(configPath);
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
        <div className="absolute right-0 top-full mt-1 w-44 bg-bg-card border border-border-default rounded shadow-lg z-50">
          <button
            onClick={handleBuild}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Build from worktree' : 'Build'}
          </button>
          <button
            onClick={handleRun}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            {node.use_worktree ? 'Run from worktree' : 'Run'}
          </button>
          <div className="border-t border-border-default" />
          <button
            onClick={handleOpenConfig}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-muted hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Open mesh.toml
          </button>
        </div>
      )}
    </div>
  );
}
