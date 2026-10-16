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
    '@xterm/addon-unicode11',
  ]) {
    assert.ok(specifiers.includes(pkg), `${pkg} must be gated`);
  }
  for (const tab of ['ProjectFilesTab', 'RepositoryTab', 'UsageTab', 'NotificationsTab']) {
    assert.ok(specifiers.includes(`./${tab}`), `./${tab} must be gated`);
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
    violation.sites.map((s) => s.split('/').slice(-2).join('/')),
    ['Terminal/TerminalRegistry.ts:2'],
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
      '',
    ].join('\n'),
  );
  // `import type` is erased by the compiler, and `import()` is the whole point
  // of the code split, so neither is the file that pulled the module in.
  for (const specifier of ['@xterm/xterm', '@xterm/addon-fit']) {
    assert.deepEqual(findStaticImportSites(src, [specifier]), [], `${specifier} must not be attributed to a lazy site`);
  }
});

test('a missing dist is an error, not a silent pass', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'bundle-composition-empty-'));
  assert.throws(() => evaluateComposition(path.join(tmp, 'nope'), path.join(tmp, 'src')));
});