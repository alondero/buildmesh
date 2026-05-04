/**
 * Unit tests for mesh.toml config parsing
 *
 * Tests parsing build/run commands from mesh.toml configuration file.
 * Note: These tests verify the config parsing layer independently from the Tauri command.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import * as fs from 'fs';

describe('mesh.toml config parsing', () => {
  // Use vi.hoisted to ensure the mock is at the top level
  const { mockExistsSync, mockReadFileSync } = vi.hoisted(() => ({
    mockExistsSync: vi.fn(),
    mockReadFileSync: vi.fn(),
  }));

  vi.mock('fs', () => ({
    readFileSync: mockReadFileSync,
    existsSync: mockExistsSync,
  }));

  beforeEach(() => {
    vi.clearAllMocks();
    mockExistsSync.mockReturnValue(true);
  });

  describe('build command extraction', () => {
    it('parses build.command from valid mesh.toml', async () => {
      const tomlContent = `
[build]
command = "cargo build --release"

[run]
command = "./target/release/myapp"
`;
      mockReadFileSync.mockReturnValue(tomlContent);
      const config = await parseMeshConfig('/some/path/mesh.toml');
      expect(config.build.command).toBe('cargo build --release');
    });

    it('returns error when mesh.toml is missing', async () => {
      mockExistsSync.mockReturnValue(false);
      await expect(parseMeshConfig('/nonexistent/mesh.toml')).rejects.toThrow(
        'mesh.toml not found'
      );
    });

    it('returns error when build section is missing', async () => {
      const tomlContent = `
[run]
command = "./target/release/myapp"
`;
      mockReadFileSync.mockReturnValue(tomlContent);
      await expect(parseMeshConfig('/some/path/mesh.toml')).rejects.toThrow(
        'build command not configured'
      );
    });

    it('returns error when run section is missing', async () => {
      const tomlContent = `
[build]
command = "cargo build --release"
`;
      mockReadFileSync.mockReturnValue(tomlContent);
      await expect(parseMeshConfig('/some/path/mesh.toml')).rejects.toThrow(
        'run command not configured'
      );
    });
  });

  describe('command normalization', () => {
    it('trims whitespace from commands', async () => {
      const tomlContent = `
[build]
command = "  cargo build --release  "

[run]
command = "./target/release/myapp"
`;
      mockReadFileSync.mockReturnValue(tomlContent);
      const config = await parseMeshConfig('/some/path/mesh.toml');
      expect(config.build.command).toBe('cargo build --release');
    });
  });
});

// The function we're testing — will be implemented in build_run.rs
async function parseMeshConfig(meshPath: string): Promise<{
  build: { command: string };
  run: { command: string };
}> {
  if (!fs.existsSync(meshPath)) {
    throw new Error('mesh.toml not found');
  }
  const content = fs.readFileSync(meshPath, 'utf-8');
  // Simple TOML parsing for [build] and [run] sections
  const buildMatch = content.match(/\[build\]\s*command\s*=\s*"([^"]+)"/);
  const runMatch = content.match(/\[run\]\s*command\s*=\s*"([^"]+)"/);
  if (!buildMatch) throw new Error('build command not configured');
  if (!runMatch) throw new Error('run command not configured');
  return {
    build: { command: buildMatch[1].trim() },
    run: { command: runMatch[1].trim() },
  };
}
