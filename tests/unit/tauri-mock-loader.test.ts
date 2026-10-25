import { execFile } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { copyFile, mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { promisify } from 'node:util';
import { expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');
const validSource = readFileSync(resolve(root, 'scripts/ui-mock/default-fixtures.ts'), 'utf8');
const run = promisify(execFile);

async function loadSource(source: string) {
  // Isolate the real loader and its input without changing tracked fixtures.
  const artifacts = resolve(root, '.tmp/ui-mock-loader');
  await mkdir(artifacts, { recursive: true });
  const folder = await mkdtemp(resolve(artifacts, 'source-'));
  const loader = resolve(folder, 'tauri-mock.mjs');
  await copyFile(resolve(root, 'scripts/ui-mock/tauri-mock.mjs'), loader);
  await writeFile(resolve(folder, 'default-fixtures.ts'), source);
  return run(process.execPath, ['--input-type=module', '-e', `
    const mock = await import(process.argv[1]);
    globalThis.window = {};
    eval(mock.buildInitScript());
    console.log(JSON.stringify(await window.__TAURI_INTERNALS__.invoke('list_providers')));
  `, pathToFileURL(loader).href], { cwd: root, timeout: 15000 });
}

it('loads valid fixtures but rejects recoverable TypeScript syntax errors with a source diagnostic', async () => {
  const valid = await loadSource(validSource);
  expect(JSON.parse(valid.stdout)).toEqual([
    expect.objectContaining({ label: 'Claude', capabilities: expect.objectContaining({ supports_model_override: true }) }),
  ]);

  const malformed = validSource.replace('const meshDefaults = {', 'const meshDefaults: = {');
  expect(malformed).not.toBe(validSource);
  await expect(loadSource(malformed).then(() => undefined)).rejects.toThrow(/default-fixtures\.ts[\s\S]*TS1110[\s\S]*Type expected/);
});
