// Gate tests for the desktop bundle composition gate (issue #2409).
//
// These test the *policy* in the gate, not the current repository state, and
// the negative fixtures are the point: a gate that has only ever been run
// against a healthy build is a gate nobody has watched reject anything. The
// fixtures are hand-written chunk graphs shaped like a real Vite build — an
// entry chunk that reaches the heavy modules through `import("./x.js")` —
// because the failure this gate exists for is precisely a change in *how* the
// entry reaches them.
//
// Run with: node --test tests/agent-infra/check-bundle-composition.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import {
  FORBIDDEN,
  evaluateComposition,
  findStaticImportSites,
  staticImportSpecifiers,
} from '../../scripts/check-bundle-composition.mjs';

// A stable hash-shaped suffix, so the chunk names look like Rollup's output.
const HASH = 'abcdefgh';
const chunkName = (stem) => `${stem}-${HASH}.js`;

/**
 * Build a fake `dist/` whose entry chunk reaches `missing` and `static` modules
 * the given way, and everything else lazily.
 */
function makeDist({ missing = [], static: staticallyImported = [] } = {}) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'bundle-composition-'));
  const assets = path.join(tmp, 'dist', 'assets');
  fs.mkdirSync(assets, { recursive: true });

  const dynamic = [];
  for (const item of FORBIDDEN) {
    if (missing.includes(item.specifier)) continue;
    fs.writeFileSync(path.join(assets, chunkName(item.stem)), `export const ${item.stem}=1;\n`);
    dynamic.push(`import("./${chunkName(item.stem)}")`);
  }
  const statics = staticallyImported.map((spec) => {
    const item = FORBIDDEN.find((f) => f.specifier === spec);
    return `import{"${item.stem}"}from"./${chunkName(item.stem)}";`;
  });

  fs.writeFileSync(path.join(assets, `index-${HASH}.js`), [...statics, ...dynamic.map((d) => `void ${d};`)].join('\n') + '\n');
  fs.mkdirSync(path.join(tmp, 'src'), { recursive: true });
  return { tmp, dist: path.join(tmp, 'dist'), src: path.join(tmp, 'src') };
}

// --------------------------------------------------------------- the policy

test('a build that keeps every forbidden module lazy passes', () => {
  const { dist, src } = makeDist();
  const report = evaluateComposition(dist, src);
  assert.deepEqual(report.violations, [], 'no forbidden module may be flagged');
  assert.equal(report.checks.length, FORBIDDEN.length);
});

test('the forbidden list covers the xterm packages and the Probe tabs', () => {
  const specifiers = FORBIDDEN.map((f) => f.specifier);
  for (const pkg of [
    '@xterm/xterm',
    '@xterm/addon-fit',
    '@xterm/addon-serialize',
    '@xterm/addon-search',
    '@xterm/addon-web-links',
    '@xterm/addon-webgl',
  ]) {
    assert.ok(specifiers.includes(pkg), `${pkg} must be gated`);
  }
  for (const tab of ['ProjectFilesTab', 'RepositoryTab', 'UsageTab', 'NotificationsTab']) {
    assert.ok(specifiers.includes(`./${tab}`), `./${tab} must be gated`);
  }
  // `@xterm/addon-unicode11` is never dynamically imported, so it has no chunk
  // of its own; the lazy wrapper that carries it is what must stay split.
  assert.ok(specifiers.includes('./loadUnicode11Widths'), './loadUnicode11Widths must be gated');
});

test('every forbidden entry pairs a specifier with the chunk of that same module', () => {
  // A specifier whose stem names a different module both misses that module's
  // own leak and blames the wrong file, so the two must describe one module.
  for (const item of FORBIDDEN) {
    const stemOf = item.specifier.replace(/^.*\//, '');
    assert.equal(item.stem, stemOf, `${item.specifier} is gated against the wrong chunk stem`);
  }
});

// ------------------------------------------------------- the eager-import case

test('a forbidden module inlined into the entry chunk is rejected', () => {
  // This is the regression the issue describes: the lazy chunk for @xterm/xterm
  // is gone because something imported it eagerly, so its code now ships to
  // every launch.
  const { dist, src } = makeDist({ missing: ['@xterm/xterm'] });
  const report = evaluateComposition(dist, src);
  assert.equal(report.violations.length, 1);
  const [violation] = report.violations;
  assert.equal(violation.specifier, '@xterm/xterm');
  assert.equal(violation.chunk, null);
  assert.match(violation.detail, /inlined into index-/);
});

test('the rejection names the file holding the eager import', () => {
  const { dist, src } = makeDist({ missing: ['@xterm/addon-fit'] });
  fs.mkdirSync(path.join(src, 'components', 'Terminal'), { recursive: true });
  fs.writeFileSync(
    path.join(src, 'components', 'Terminal', 'TerminalRegistry.ts'),
    "import { Terminal } from '@xterm/xterm';\nimport FitAddon from '@xterm/addon-fit';\nexport { Terminal, FitAddon };\n",
  );
  const report = evaluateComposition(dist, src);
  const [violation] = report.violations;
  assert.equal(violation.specifier, '@xterm/addon-fit');
  assert.deepEqual(
    violation.sites,
    ['src/components/Terminal/TerminalRegistry.ts:2'],
    'the gate must point at the eager import, by file and line',
  );
});

test('a forbidden module reached by a static import edge is rejected', () => {
  // The chunk still exists, but the entry now imports it statically, so it is
  // loaded on first paint rather than on demand.
  const { dist, src } = makeDist({ static: ['@xterm/addon-webgl'] });
  const report = evaluateComposition(dist, src);
  assert.equal(report.violations.length, 1);
  const [violation] = report.violations;
  assert.equal(violation.specifier, '@xterm/addon-webgl');
  assert.equal(violation.chunk, chunkName('addon-webgl'));
  assert.match(violation.detail, /statically imported by index-/);
});

test('the two failure reasons stay distinct', () => {
  // The report drives two different remedies, so a missing chunk must not be
  // reported as a static edge (or the other way round): only the missing-chunk
  // case has an eager import in src/ to point the reviewer at.
  const inlinedBuild = makeDist({ missing: ['@xterm/xterm'] });
  const edgeBuild = makeDist({ static: ['@xterm/addon-fit'] });
  const lazyBuild = makeDist();

  assert.equal(evaluateComposition(inlinedBuild.dist, inlinedBuild.src).violations[0].reason, 'inlined');
  assert.equal(evaluateComposition(edgeBuild.dist, edgeBuild.src).violations[0].reason, 'static-edge');

  const lazy = evaluateComposition(lazyBuild.dist, lazyBuild.src).checks.find((c) => c.specifier === '@xterm/addon-fit');
  assert.equal(lazy.reason, 'lazy');
  assert.equal(lazy.ok, true);
});

test('a relative --dist still catches a static import edge', () => {
  // `staticClosure` keys are absolute, so a relative dist used to make every
  // `closure.get(chunk)` miss and report the edge as lazy — a silent pass on
  // exactly the build shape the gate exists to catch.
  const { tmp, dist, src } = makeDist({ static: ['@xterm/addon-webgl'] });
  const previous = process.cwd();
  process.chdir(tmp);
  try {
    const relativeReport = evaluateComposition(path.join('dist', 'assets'), src);
    const absoluteReport = evaluateComposition(dist, src);
    assert.equal(relativeReport.violations.length, 1, 'a relative dist must not hide the violation');
    assert.equal(relativeReport.violations[0].specifier, '@xterm/addon-webgl');
    assert.equal(relativeReport.violations[0].reason, 'static-edge');
    assert.deepEqual(
      relativeReport.violations.map((v) => v.specifier),
      absoluteReport.violations.map((v) => v.specifier),
      'relative and absolute paths must agree',
    );
  } finally {
    process.chdir(previous);
  }
});

// ------------------------------------------------------ the parsing it relies on

test('a dynamic import is not mistaken for a static one', () => {
  // If `import("./x.js")` counted as static, every lazy module would read as a
  // violation and the gate could never pass a real build.
  assert.deepEqual(staticImportSpecifiers('void import("./addon-fit-abcdefgh.js");'), []);
});

test('static import forms are recognised, dynamic ones are not', () => {
  const source = [
    'import{a}from"./one.js";',
    'import"./two.js";',
    'export{b}from"./three.js";',
    'void import("./four.js");',
    'void import("./five.js");',
  ].join('\n');
  assert.deepEqual(staticImportSpecifiers(source).sort(), ['./one.js', './three.js', './two.js']);
});

test('the attribution scan ignores the lazy sites that keep a module split', () => {
  const src = fs.mkdtempSync(path.join(os.tmpdir(), 'bundle-composition-src-'));
  fs.writeFileSync(
    path.join(src, 'TerminalRegistry.ts'),
    [
      "const [Terminal, FitAddon] = await Promise.all([import('@xterm/xterm'), import('@xterm/addon-fit')]);",
      "import type { Terminal } from '@xterm/xterm';",
      "import type { FitAddon } from '@xterm/addon-fit';",
      "import { type Terminal as T2 } from '@xterm/xterm';",
      "import { type FitAddon as F2 } from '@xterm/addon-fit';",
      '',
    ].join('\n'),
  );
  // `import type` and its inline `import { type X }` spelling are erased by the
  // compiler, and `import()` is the whole point of the code split, so none of
  // them is the file that pulled the module in.
  for (const specifier of ['@xterm/xterm', '@xterm/addon-fit']) {
    assert.deepEqual(findStaticImportSites(src, [specifier]), [], `${specifier} must not be attributed to a lazy site`);
  }
});

test('a side-effect static import is attributed to the file that pulled it in', () => {
  // `import '@xterm/xterm';` inlines the chunk exactly like a named import, so
  // it must be found; reporting "inlined through another lazy chunk" here would
  // send the reviewer looking in the wrong place.
  const src = fs.mkdtempSync(path.join(os.tmpdir(), 'bundle-composition-src-'));
  fs.writeFileSync(path.join(src, 'App.tsx'), "import '@xterm/addon-webgl';\nimport '@xterm/xterm';\n");
  assert.deepEqual(
    findStaticImportSites(src, ['@xterm/xterm']).map((s) => s.split('/').pop()),
    ['App.tsx:2'],
  );
  assert.deepEqual(
    findStaticImportSites(src, ['@xterm/addon-webgl']).map((s) => s.split('/').pop()),
    ['App.tsx:1'],
  );
});

test('attribution paths are relative to the tree that owns the source root', () => {
  // Reported against the module-level repo root these came back absolute,
  // because a fixture in os.tmpdir() cannot be reached from the repo's drive.
  // Paths stay `src/…`-shaped for the default root and for a fixture alike.
  const { src } = makeDist();
  fs.mkdirSync(path.join(src, 'components'), { recursive: true });
  fs.writeFileSync(path.join(src, 'components', 'Terminal.ts'), "import '@xterm/xterm';\n");
  assert.deepEqual(findStaticImportSites(src, ['@xterm/xterm']), ['src/components/Terminal.ts:1']);
});

test('a missing dist is an error, not a silent pass', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'bundle-composition-empty-'));
  assert.throws(() => evaluateComposition(path.join(tmp, 'nope'), path.join(tmp, 'src')));
});