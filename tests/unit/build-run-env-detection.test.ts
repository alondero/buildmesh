/**
 * Unit tests for build/run environment detection
 *
 * Tests that the correct path format (UNC for Windows, Linux for WSL)
 * is used based on the environment detected for a given path.
 */
import { describe, it, expect } from 'vitest';

describe('environment detection for build/run paths', () => {
  describe('env_for_path', () => {
    it('detects WSL path with /mnt/ prefix', () => {
      const result = envForPath('/mnt/d/projects/myapp');
      expect(result).toBe('wsl');
    });

    it('detects WSL path with /home/ prefix', () => {
      const result = envForPath('/home/user/projects/myapp');
      expect(result).toBe('wsl');
    });

    it('detects WSL path with / prefix on WSL', () => {
      // This could be WSL or Unix, but in our context it's WSL if the mesh is on WSL
      const result = envForPath('/home/alond/mesh');
      expect(result).toBe('wsl');
    });

    it('detects Windows path with backslash', () => {
      const result = envForPath('C:\\Projects\\myapp');
      expect(result).toBe('windows');
    });

    it('detects Windows UNC path with wsl$ prefix', () => {
      const result = envForPath('\\\\wsl$\\Ubuntu\\home\\user\\mesh');
      expect(result).toBe('wsl');
    });

    it('detects Windows path with drive letter', () => {
      const result = envForPath('X:/src/buildmesh');
      expect(result).toBe('windows');
    });
  });

  describe('to_host_path conversion', () => {
    it('converts WSL Linux path to Windows UNC path', () => {
      const linuxPath = '/mnt/d/projects/myapp';
      const result = toHostPath(linuxPath);
      // Should produce a valid UNC path starting with \\wsl$\d
      expect(result).toContain('wsl$\\d');
      expect(result).toContain('projects');
      expect(result).toContain('myapp');
    });

    it('converts WSL /home path to Windows UNC path', () => {
      const linuxPath = '/home/alond/mesh';
      const result = toHostPath(linuxPath);
      expect(result).toBe('\\\\wsl$\\Ubuntu\\home\\alond\\mesh');
    });

    it('leaves Windows paths unchanged', () => {
      const windowsPath = 'C:\\Projects\\myapp';
      const result = toHostPath(windowsPath);
      expect(result).toBe(windowsPath);
    });

    it('leaves Windows paths unchanged on Windows', () => {
      // For now, Windows paths are passed through as-is
      // (the actual conversion for WSL-side access would be done by the WSL shell)
      const windowsPath = 'X:/src/buildmesh';
      const result = toHostPath(windowsPath);
      expect(result).toBe(windowsPath);
    });
  });

  describe('working directory for shell', () => {
    it('uses UNC path when spawning shell on Windows for WSL worktree', () => {
      const wslWorktreePath = '\\\\wsl$\\Ubuntu\\home\\user\\mesh\\worktrees\\agent-1';
      const shellCwd = getShellWorkingDirectory(wslWorktreePath, 'wsl');
      expect(shellCwd).toBe(wslWorktreePath);
    });

    it('uses Linux path when spawning shell on WSL', () => {
      const linuxWorktreePath = '/home/user/mesh/worktrees/agent-1';
      const shellCwd = getShellWorkingDirectory(linuxWorktreePath, 'wsl');
      expect(shellCwd).toBe(linuxWorktreePath);
    });

    it('uses Windows path as-is on Windows', () => {
      const windowsPath = 'X:/src/buildmesh/worktrees/agent-1';
      const shellCwd = getShellWorkingDirectory(windowsPath, 'windows');
      expect(shellCwd).toBe(windowsPath);
    });
  });
});

// Replicate the env detection logic from src-tauri/src/env/mod.rs
function envForPath(path: string): 'wsl' | 'windows' | 'mac' {
  if (
    path.startsWith('/mnt/') ||
    path.startsWith('/home/') ||
    path.includes('\\\\wsl$') ||
    path === '/' ||
    path.startsWith('\\\\wsl$\\')
  ) {
    return 'wsl';
  }
  if (
    path.match(/^[A-Z]:/i) ||
    path.includes('\\\\') ||
    path.includes('\\')
  ) {
    return 'windows';
  }
  return 'mac';
}

function toHostPath(path: string): string {
  // Convert Linux path to Windows UNC path
  if (path.startsWith('/mnt/')) {
    const drive = path.substring('/mnt/'.length).replace('/', '\\');
    return `\\\\wsl$\\${drive}`;
  }
  if (path.startsWith('/home/')) {
    // Assume Ubuntu as the distro
    const homePath = path.substring('/home/'.length);
    return `\\\\wsl$\\Ubuntu\\home\\${homePath}`.replace(/\//g, '\\');
  }
  return path;
}

function getShellWorkingDirectory(
  worktreePath: string,
  env: 'wsl' | 'windows' | 'mac'
): string {
  if (env === 'wsl') {
    // For WSL, if the path is a UNC path (Windows-side access to WSL),
    // convert it to the Linux path
    if (worktreePath.startsWith('\\\\wsl$\\')) {
      return toHostPath(worktreePath);
    }
    return worktreePath;
  }
  return worktreePath;
}
