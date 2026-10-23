import { execFile } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');
const artifacts = resolve(root, '.tmp/ui-shot-errors');

async function capture(html: string, name: string, extra: string[] = []) {
  const server = createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'text/html' });
    response.end(html);
  });
  await new Promise<void>(done => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No server address');
  await mkdir(artifacts, { recursive: true });
  const runDirectory = await mkdtemp(resolve(artifacts, `${name}-`));
  const output = resolve(runDirectory, 'shot.png');
  const marker = resolve(runDirectory, 'steps-marker.json');
  try {
    return await new Promise<{ code: number | string; stdout: string; stderr: string; output: string; marker: string }>(done => {
      execFile(process.execPath, [resolve(root, 'scripts/ui-shot.mjs'),
        '--mock', '--mock-url', `http://127.0.0.1:${address.port}`, '--out', output, ...extra],
      { cwd: root, timeout: 60000, env: { ...process.env, UI_SHOT_MARKER_PATH: marker } }, (error, stdout, stderr) => {
        done({ code: error?.code ?? 0, stdout, stderr, output, marker });
      });
    });
  } finally {
    await new Promise<void>((done, reject) => server.close(error => error ? reject(error) : done()));
  }
}

const mounted = '<div id="root"><p>Mounted successfully</p></div>';

// Every case supplies only one fatal surface, so another detector cannot hide
// a missing branch. A populated root rules out the old mount-timeout check.
describe('ui-shot fatal browser diagnostics', () => {
  it('fails a mounted page on an uncaught page error', async () => {
    const result = await capture(`${mounted}<script>throw new Error('uncaught-render-control')</script>`, 'pageerror');
    expect(result.code, result.stderr).toBe(1);
    expect(result.stderr).toContain('[page error]');
    expect(result.stderr).toContain('uncaught-render-control');
  });

  it('fails a mounted page on an unexpected console error', async () => {
    const result = await capture(`${mounted}<script>console.error('unexpected-console-control')</script>`, 'console');
    expect(result.code, result.stderr).toBe(1);
    expect(result.stderr).toContain('[page console error]');
    expect(result.stderr).toContain('unexpected-console-control');
  });

  it('fails React warnings even at console.warn severity', async () => {
    // String classification is covered without six Chromium boots in the
    // collector unit tests. Keep one real console.warn -> CLI failure check.
    const diagnostic = 'Encountered two children with the same key, duplicate-control.';
    const result = await capture(`${mounted}<script>console.warn(${JSON.stringify(diagnostic)})</script>`, 'duplicate-key');
    expect(result.code, result.stderr).toBe(1);
    expect(result.stderr).toContain('[React warning]');
    expect(result.stderr).toContain(diagnostic);
  });

  it('fails an Error Boundary fallback even without browser error events', async () => {
    const result = await capture('<div id="root"><div><div><h1>Buildmesh hit a render error</h1></div><pre>TypeError: boundary-only-control</pre></div></div>', 'boundary');
    expect(result.code, result.stderr).toBe(1);
    expect(result.stderr).toContain('[Error Boundary]');
    expect(result.stderr).toContain('boundary-only-control');
  });

  it('consults diagnostics again after steps on an already mounted page', async () => {
    const healthy = await capture(mounted, 'post-step-screenshot-control');
    expect(healthy.code, healthy.stderr).toBe(0);
    expect((await readFile(healthy.output)).subarray(1, 4).toString()).toBe('PNG');
    const result = await capture(mounted, 'late-console', [
      '--steps', resolve(root, 'tests/integration/ui-shot-console-error.steps.mjs'),
    ]);
    expect(result.code, result.stderr).toBe(1);
    expect(result.stderr).toContain('[page console error]');
    expect(result.stderr).toContain('step-console-control');
    await expect(readFile(result.output).then(() => undefined)).rejects.toMatchObject({ code: 'ENOENT' });
  });

  it('stops mount errors before steps execute', async () => {
    const steps = ['--steps', resolve(root, 'tests/integration/ui-shot-recording.steps.mjs')];
    const healthy = await capture(mounted, 'mount-step-control', steps);
    expect(healthy.code, healthy.stderr).toBe(0);
    expect(JSON.parse(await readFile(healthy.marker, 'utf8'))).toMatchObject({ fromCli: true, hasPage: true });

    const result = await capture(`${mounted}<script>console.error('mount-before-steps-control')</script>`, 'mount-stops-steps', steps);
    expect(result.code, result.stderr).toBe(1);
    expect(result.stderr).toContain('mount-before-steps-control');
    await expect(readFile(result.marker).then(() => undefined)).rejects.toMatchObject({ code: 'ENOENT' });
  });

  it('fails diagnostics emitted during screenshot activity', async () => {
    const result = await capture(mounted, 'screenshot-error', [
      '--steps', resolve(root, 'tests/integration/ui-shot-screenshot-error.steps.mjs'),
    ]);
    expect(result.code, result.stderr).toBe(1);
    expect(result.stderr).toContain('[page console error]');
    expect(result.stderr).toContain('screenshot-console-control');
    expect((await readFile(result.output)).subarray(1, 4).toString()).toBe('PNG');
  });

  it('allows an ordinary warning and writes a real screenshot with the collector armed', async () => {
    const result = await capture(`${mounted}<script>console.warn('ordinary-warning-control')</script>`, 'healthy');
    expect(result.code, result.stderr).toBe(0);
    expect((await readFile(result.output)).subarray(1, 4).toString()).toBe('PNG');
    // The same CLI path must still fail when its console error collector fires.
    const control = await capture(`${mounted}<script>console.error('armed-collector-control')</script>`, 'armed');
    expect(control.code, control.stderr).toBe(1);
    expect(control.stderr).toContain('armed-collector-control');
  });
});
