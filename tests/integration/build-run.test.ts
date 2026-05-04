/**
 * Integration tests for build/run feature
 *
 * These tests call the actual Tauri backend via invoke().
 * They will fail until the build_run command is implemented.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

describe('build_run command', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe('build mode', () => {
    it('calls build_run with mode "build" and returns ok', async () => {
      // This will fail until build_run is implemented in the backend
      await expect(
        invoke('build_run', { nodeId: 1, mode: 'build' })
      ).resolves.toBeUndefined();
    });

    it('returns error when mesh.toml is missing', async () => {
      // Mock node that doesn't have a mesh with mesh.toml
      await expect(
        invoke('build_run', { nodeId: 9999, mode: 'build' })
      ).rejects.toThrow();
    });

    it('returns error when worktree does not exist', async () => {
      // Node with mesh.toml but no worktree created
      await expect(
        invoke('build_run', { nodeId: 9999, mode: 'build' })
      ).rejects.toThrow('Worktree not found');
    });
  });

  describe('run mode', () => {
    it('calls build_run with mode "run" and returns ok', async () => {
      await expect(
        invoke('build_run', { nodeId: 1, mode: 'run' })
      ).resolves.toBeUndefined();
    });

    it('returns error when run command is not configured', async () => {
      // mesh.toml with only [build] section, no [run]
      await expect(
        invoke('build_run', { nodeId: 1, mode: 'run' })
      ).rejects.toThrow('run command not configured');
    });
  });

  describe('get_mesh_config command', () => {
    it('returns parsed mesh.toml config for valid mesh', async () => {
      const config = await invoke<{
        build: { command: string };
        run: { command: string };
      }>('get_mesh_config', { meshId: 1 });

      expect(config).toHaveProperty('build');
      expect(config).toHaveProperty('run');
      expect(config.build).toHaveProperty('command');
      expect(config.run).toHaveProperty('command');
    });

    it('throws when mesh.toml is missing', async () => {
      await expect(
        invoke('get_mesh_config', { meshId: 9999 })
      ).rejects.toThrow('mesh.toml not found');
    });

    it('throws when build section is missing', async () => {
      await expect(
        invoke('get_mesh_config', { meshId: 1 })
      ).rejects.toThrow('build command not configured');
    });
  });
});
