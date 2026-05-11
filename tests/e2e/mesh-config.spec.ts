/**
 * E2E tests for mesh_config module — mesh.toml and settings.json CRUD
 *
 * Tests `get_mesh_properties`, `update_mesh_field`, `update_worktree_base_ref`,
 * and `remove_worktree_base_ref` Tauri commands.
 *
 * These tests use invokeViaHttp to call the actual Tauri backend via HTTP on port 1991.
 * They require `npm run tauri dev` to be running.
 */
import { test, expect, beforeAll, afterAll } from '@playwright/test';
import { invokeViaHttp, cleanupTestProjects, waitForTauriReady } from './utils/tauri-http';
import * as fs from 'fs';
import * as path from 'path';
import { tmpdir } from 'os';

let meshIdsToCleanup: number[] = [];

test.describe('mesh_config commands', () => {
  test.beforeAll(async () => {
    const ready = await waitForTauriReady(30000);
    if (!ready) {
      throw new Error('Tauri backend not ready - ensure `npm run tauri dev` is running');
    }
  });

  test.afterAll(async () => {
    await cleanupTestProjects();
  });

  test('get_mesh_properties returns build_command, run_command, model, effort from mesh.toml', async () => {
    const meshPath = path.join(tmpdir(), `buildmesh-get-props-${Date.now()}`);
    fs.mkdirSync(meshPath, { recursive: true });
    fs.writeFileSync(
      path.join(meshPath, 'mesh.toml'),
      `[build]
command = "npm run build"

[run]
command = "npm run dev"

[agent]
model = "opus-4"
effort = "medium"
`
    );

    const mesh = await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_project',
      { name: `test-get-props-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    // Manually write the mesh.toml to the test project's path since create_test_project uses temp dir
    fs.writeFileSync(
      path.join(mesh.path, 'mesh.toml'),
      `[build]
command = "npm run build"

[run]
command = "npm run dev"

[agent]
model = "opus-4"
effort = "medium"
`
    );

    const config = await invokeViaHttp<{
      build_command: string | null;
      run_command: string | null;
      model: string | null;
      effort: string | null;
      base_ref: string | null;
    }>('get_mesh_properties', { mesh_id: mesh.id });

    expect(config.build_command).toBe('npm run build');
    expect(config.run_command).toBe('npm run dev');
    expect(config.model).toBe('opus-4');
    expect(config.effort).toBe('medium');
  });

  test('get_mesh_properties returns null for model/effort when not set in mesh.toml', async () => {
    const meshPath = path.join(tmpdir(), `buildmesh-no-agent-${Date.now()}`);
    fs.mkdirSync(meshPath, { recursive: true });
    fs.writeFileSync(
      path.join(meshPath, 'mesh.toml'),
      `[build]
command = "cargo build"

[run]
command = "cargo run"
`
    );

    const mesh = await invokeViaHttp<{ id: number }>(
      'create_test_project',
      { name: `test-no-agent-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    // Overwrite with our mesh.toml
    fs.writeFileSync(
      path.join(mesh.path, 'mesh.toml'),
      `[build]
command = "cargo build"

[run]
command = "cargo run"
`
    );

    const config = await invokeViaHttp<{
      model: string | null;
      effort: string | null;
    }>('get_mesh_properties', { mesh_id: mesh.id });

    expect(config.model).toBeNull();
    expect(config.effort).toBeNull();
  });

  test('get_mesh_properties throws when mesh.toml is missing', async () => {
    const mesh = await invokeViaHttp<{ id: number; path: string }>(
      'create_test_project',
      { name: `test-no-file-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    // Don't create mesh.toml

    await expect(
      invokeViaHttp('get_mesh_properties', { mesh_id: mesh.id })
    ).rejects.toThrow('mesh.toml not found');
  });

  test('update_mesh_field writes model to [agent] section in mesh.toml', async () => {
    const mesh = await invokeViaHttp<{ id: number; path: string }>(
      'create_test_project',
      { name: `test-update-model-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    fs.writeFileSync(
      path.join(mesh.path, 'mesh.toml'),
      `[build]
command = "npm run build"

[run]
command = "npm run dev"
`
    );

    await invokeViaHttp('update_mesh_field', {
      mesh_id: mesh.id,
      section: 'agent',
      key: 'model',
      value: 'sonnet-4',
    });

    const config = await invokeViaHttp<{ model: string | null }>('get_mesh_properties', { mesh_id: mesh.id });
    expect(config.model).toBe('sonnet-4');
  });

  test('update_mesh_field writes effort to [agent] section in mesh.toml', async () => {
    const mesh = await invokeViaHttp<{ id: number; path: string }>(
      'create_test_project',
      { name: `test-update-effort-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    fs.writeFileSync(
      path.join(mesh.path, 'mesh.toml'),
      `[build]
command = "npm run build"

[run]
command = "npm run dev"
`
    );

    await invokeViaHttp('update_mesh_field', {
      mesh_id: mesh.id,
      section: 'agent',
      key: 'effort',
      value: 'high',
    });

    const config = await invokeViaHttp<{ effort: string | null }>('get_mesh_properties', { mesh_id: mesh.id });
    expect(config.effort).toBe('high');
  });

  test('update_mesh_field fails when section does not exist in mesh.toml', async () => {
    const mesh = await invokeViaHttp<{ id: number; path: string }>(
      'create_test_project',
      { name: `test-bad-section-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    fs.writeFileSync(
      path.join(mesh.path, 'mesh.toml'),
      `[build]
command = "npm run build"
`
    );

    await expect(
      invokeViaHttp('update_mesh_field', {
        mesh_id: mesh.id,
        section: 'nonexistent',
        key: 'model',
        value: 'opus-4',
      })
    ).rejects.toThrow('[nonexistent] section not found');
  });

  test('update_worktree_base_ref writes baseRef to settings.json', async () => {
    const mesh = await invokeViaHttp<{ id: number; path: string }>(
      'create_test_project',
      { name: `test-base-ref-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    fs.writeFileSync(
      path.join(mesh.path, 'mesh.toml'),
      `[build]
command = "npm run build"
`
    );

    await invokeViaHttp('update_worktree_base_ref', { mesh_id: mesh.id, base_ref: 'fresh' });

    const settingsPath = path.join(mesh.path, '.claude', 'settings.json');
    expect(fs.existsSync(settingsPath)).toBe(true);

    const content = JSON.parse(fs.readFileSync(settingsPath, 'utf-8'));
    expect(content.worktree?.baseRef).toBe('origin/main');
  });

  test('update_worktree_base_ref with head writes HEAD', async () => {
    const mesh = await invokeViaHttp<{ id: number; path: string }>(
      'create_test_project',
      { name: `test-base-ref-head-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    fs.writeFileSync(
      path.join(mesh.path, 'mesh.toml'),
      `[build]
command = "npm run build"
`
    );

    await invokeViaHttp('update_worktree_base_ref', { mesh_id: mesh.id, base_ref: 'head' });

    const settingsPath = path.join(mesh.path, '.claude', 'settings.json');
    const content = JSON.parse(fs.readFileSync(settingsPath, 'utf-8'));
    expect(content.worktree?.baseRef).toBe('HEAD');
  });

  test('remove_worktree_base_ref removes baseRef from settings.json', async () => {
    const mesh = await invokeViaHttp<{ id: number; path: string }>(
      'create_test_project',
      { name: `test-remove-ref-${Date.now()}` }
    );
    meshIdsToCleanup.push(mesh.id);

    fs.mkdirSync(path.join(mesh.path, '.claude'), { recursive: true });
    fs.writeFileSync(
      path.join(mesh.path, 'mesh.toml'),
      `[build]
command = "npm run build"
`
    );
    fs.writeFileSync(
      path.join(mesh.path, '.claude', 'settings.json'),
      JSON.stringify({ worktree: { baseRef: 'origin/main' } }, null, 2)
    );

    await invokeViaHttp('remove_worktree_base_ref', { mesh_id: mesh.id });

    const settingsPath = path.join(mesh.path, '.claude', 'settings.json');
    const content = JSON.parse(fs.readFileSync(settingsPath, 'utf-8'));
    expect(content.worktree?.baseRef).toBeUndefined();
  });
});
