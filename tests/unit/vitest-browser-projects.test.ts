// The browser-owning tests run in their own Vitest project, after the jsdom
// pool drains, one file at a time. That layout is the fix for issue #2538: a
// browser file that runs beside hundreds of component transforms makes each
// Chromium launch and close slow enough to cross the suite budget, and the
// failure it produces is a bare "Test timed out in 30000ms" that names no
// cause.
//
// The layout lives in `vitest.config.ts`, and this file is the check that
// reads it: a later edit — a new browser test file filed under the wrong
// project, a dropped `groupOrder`, a renamed project — would otherwise
// restore the flake with no test failing anywhere. This reads the resolved
// config together with the suite's own sources, so the partition is proved
// rather than trusted: the same reason the CI shard gate reads the workflow
// matrix instead of believing it.
//
// Both projects name the browser files as concrete paths, so the partition
// below is a plain membership check. The set of process-owning files is small
// and worth reading in full, and a glob would hide which files own a browser.

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import { describe, expect, it } from 'vitest';
import vitestConfig from '../../vitest.config';

const root = resolve(import.meta.dirname, '../..');

type ProjectOptions = {
  name?: string;
  include?: string[];
  exclude?: string[];
  fileParallelism?: boolean;
  sequence?: { groupOrder?: number };
  testTimeout?: number;
};

/** Vitest wraps each project's own options in a `test` block. */
const projects = ((vitestConfig.test?.projects ?? []) as unknown as { test?: ProjectOptions }[])
  .map(project => project.test)
  .filter((options): options is ProjectOptions => options !== undefined);

const parallel = projects.find(options => options.name === 'jsdom');
const browser = projects.find(options => options.name === 'browser-tools');

/**
 * The budget a project actually runs under: its own `testTimeout`, or the
 * root's when it declares none. The parallel project inherits the root's
 * 30s, so comparing the browser project's declared value against the
 * parallel project's *declared* value would compare against 0 and pass for a
 * budget too small to supervise anything.
 */
const rootTimeout = (vitestConfig.test?.testTimeout ?? 0) as number;
const effectiveTimeout = (options: ProjectOptions | undefined) => options?.testTimeout ?? rootTimeout;

/**
 * A test file that owns a real browser or dev-server process. The markers are
 * matched against the file's code after `stripComments` removes the comments,
 * so a doc comment does not reach the markers:
 * `tests/unit/tauri-mock-harness.test.ts` names `scripts/ui-shot.mjs` in a
 * doc comment and owns no process, so a comment that read as a marker would
 * file a harmless test under the browser project.
 *
 * The playwright marker wants a value import. A type-only import names the
 * package without launching anything, and asking such a file to sit in the
 * browser project would send a harmless file down the slow lane.
 */
const PROCESS_OWNING_MARKERS = [
  /ui-shot(?:-server)?\.mjs/,
  /(?:^|\n)\s*import\s+(?!type\b)[^;]*?from\s+['"]playwright['"]/,
];

function stripComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, ' ')
    .replace(/(^|[^:\\])\/\/[^\n]*/g, '$1');
}

function testFilesUnder(directory: string): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) found.push(...testFilesUnder(path));
    else if (/\.test\.(ts|tsx)$/.test(entry.name)) found.push(path);
  }
  return found;
}

/** The repo-relative POSIX path of a file, the way the config names them. */
function repoPath(path: string): string {
  return relative(root, path).split(sep).join('/');
}

const thisFile = resolve(import.meta.dirname, 'vitest-browser-projects.test.ts');

const processOwningFiles = [
  ...testFilesUnder(join(root, 'tests', 'unit')),
  ...testFilesUnder(join(root, 'tests', 'integration')),
]
  .filter(path => path !== thisFile)
  .filter(path => PROCESS_OWNING_MARKERS.some(marker => marker.test(stripComments(readFileSync(path, 'utf8')))))
  .map(repoPath)
  .sort();

describe('the Vitest project layout', () => {
  it('declares the parallel project and the browser project', () => {
    expect(parallel, 'vitest.config.ts must keep the jsdom project').toBeDefined();
    expect(browser, 'vitest.config.ts must keep the browser-tools project').toBeDefined();
  });

  it('runs the browser project one file at a time, after the parallel pool', () => {
    // One browser file at a time: two Chromium owners side by side is the
    // contention the sequencing exists to remove.
    expect(browser?.fileParallelism).toBe(false);
    // After the parallel pool: Vitest runs project groups in `groupOrder`, so
    // a browser file that shares group 0 still competes with the transforms.
    expect(browser?.sequence?.groupOrder ?? 0).toBeGreaterThan(parallel?.sequence?.groupOrder ?? 0);
  });

  it('gives the browser project a budget above the one it supervises', () => {
    expect(effectiveTimeout(browser)).toBeGreaterThan(effectiveTimeout(parallel));
  });

  it('keeps each discovered process-owning test file out of the parallel project', () => {
    expect(processOwningFiles.length).toBeGreaterThan(0);
    for (const file of processOwningFiles) {
      expect(browser?.include, `${file} owns a browser process, so the browser project must name it`).toContain(file);
      expect(parallel?.exclude, `${file} owns a browser process, so the parallel project must exclude it`).toContain(file);
    }
  });

  it('names real files in the browser project, so the partition above stays checkable', () => {
    for (const entry of browser?.include ?? []) {
      expect(
        existsSync(resolve(root, entry)),
        `${entry} is not a file on disk — name the browser files as concrete paths so this gate can compare them`,
      ).toBe(true);
    }
  });

  it('covers both suites in the parallel project, so the partition is not limited to one of them', () => {
    const include = parallel?.include ?? [];
    expect(include.some(pattern => pattern.startsWith('tests/unit/'))).toBe(true);
    expect(include.some(pattern => pattern.startsWith('tests/integration/'))).toBe(true);
  });
});
