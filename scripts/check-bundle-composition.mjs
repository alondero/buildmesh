#!/usr/bin/env node
// Issue #2409 - desktop bundle composition gate.
//
// `scripts/check-bundle-size.mjs` bounds *how large* the desktop entry chunk
// may get, but a budget alone cannot say *why* it grew. Re-importing a heavy
// module eagerly stays under a raised cap for months while charging every
// launch for bytes the user may never need, so this gate names the modules
// that must remain split out of the entry chunk and fails the build when one
// of them is pulled back in.
//
// The forbidden list is taken from the dynamic-import sites that currently keep
// them split, not from guesswork:
//
//   - `src/components/Terminal/TerminalRegistry.ts` and
//     `src/components/Terminal/BuildRunTerminalRegistry.ts` `await import()`
//     `@xterm/xterm` plus the addons; `loadWebglRenderer.ts` dynamic-imports
//     `@xterm/addon-webgl`; and the lazy `./loadUnicode11Widths` wrapper, which
//     statically imports `@xterm/addon-unicode11` and so carries it.
//   - `src/components/Probe/ProbePanel.tsx` `lazy()`s every Probe tab.
//
// How the gate reads the build. Rollup erases bare module specifiers when it
// bundles - the string `@xterm/addon-fit` appears nowhere in `dist/`, so a
// grep for the import cannot work - but a module reached through `import()`
// is emitted as its **own chunk named after it**, while one reached through a
// static `import` is inlined into the importer and gets no chunk of its own.
// The built output therefore carries the answer in two observable facts:
//
//   1. the forbidden module still has its own chunk (it is still a dynamic
//     import), and
//   2. that chunk is not reachable from `dist/assets/index-*.js` through
//      *static* `import` edges (only `import()` may reach it).
//
// Both are checked rather than assumed: as built today the desktop entry has
// no static import edges at all, so the second fact is decided by walking the
// static import closure, which stays correct if a `manualChunks` config ever
// introduces shared static chunks.
//
// When a chunk has gone missing the gate names the eager import that inlined
// it, by scanning `src/` for a *static* import of the same specifier. That is
// the file a reviewer has to change.
//
// Exit codes:
//   0 = every forbidden module is still split out of the entry chunk
//   1 = a forbidden module was pulled into the entry chunk
//   2 = config / CLI / dist error
//
// Dependency-free (Node built-ins only) so it runs in CI and in
// `scripts/check.ps1 all-ts` without an extra install step.

import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const defaultDistDir = join(root, 'dist');

/**
 * The modules that must not be reachable from the desktop entry chunk.
 *
 * `specifier` is what a developer types in an import and what the failure
 * message names. `stem` is the chunk filename stem Rollup derives from the
 * module (the emitted file is `<stem>-<hash>.js`) - that is what the built
 * output is actually matched against.
 *
 * Every entry is a module reached through `import()`, so specifier and stem
 * always describe the same module and the failure message points at the import
 * that has to change. `@xterm/addon-unicode11` is therefore listed as the lazy
 * `./loadUnicode11Widths` wrapper that carries it: the addon itself is never
 * dynamically imported, so it has no chunk of its own to gate, and pairing its
 * specifier with the wrapper's stem would both miss an entry-level import of the
 * package and blame the wrapper for someone else's eager import.
 */
export const FORBIDDEN = [
  {
    specifier: '@xterm/xterm',
    stem: 'xterm',
    why: 'the terminal core (~330 kB minified) loads only when a terminal pane attaches',
  },
  { specifier: '@xterm/addon-fit', stem: 'addon-fit', why: 'loaded only with the terminal core' },
  {
    specifier: '@xterm/addon-serialize',
    stem: 'addon-serialize',
    why: 'loaded only with the terminal core',
  },
  { specifier: '@xterm/addon-search', stem: 'addon-search', why: 'loaded only with the terminal core' },
  {
    specifier: '@xterm/addon-web-links',
    stem: 'addon-web-links',
    why: 'loaded only with the terminal core',
  },
  {
    specifier: '@xterm/addon-webgl',
    stem: 'addon-webgl',
    why: 'carried by loadWebglRenderer, which WebglRendererPool dynamic-imports',
  },
  {
    specifier: './loadUnicode11Widths',
    stem: 'loadUnicode11Widths',
    why: 'carries @xterm/addon-unicode11; the wrapper is the lazy import, so it is what stays split',
  },
  ...[
    'ProjectFilesTab',
    'AgentChangesTab',
    'ProjectSettingsTab',
    'RepositoryTab',
    'CircuitsProbeTab',
    'GitIssuesTab',
    'GitPullRequestsTab',
    'AgentHistoryTab',
    'ScratchpadTab',
    'UsageTab',
    'NotificationsTab',
  ].map((tab) => ({
    specifier: `./${tab}`,
    stem: tab,
    why: 'ProbePanel lazy-loads it so the tab costs nothing until the user opens it',
  })),
];

function parseArgs(argv) {
  const out = { dist: defaultDistDir };
  for (let i = 2; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--dist') {
      const next = argv[++i];
      if (!next) {
        console.error('--dist requires a path');
        process.exit(2);
      }
      out.dist = resolve(next);
    } else if (arg === '--help' || arg === '-h') {
      printHelp();
      process.exit(0);
    } else {
      console.error(`Unknown argument: ${arg}`);
      printHelp();
      process.exit(2);
    }
  }
  return out;
}

function printHelp() {
  console.log(`Usage: node scripts/check-bundle-composition.mjs [options]

Fails when a forbidden module (xterm + its addons, the Probe tabs) is no longer
split into its own lazy chunk, which means its code rides along with every
launch inside the desktop entry chunk.

Options:
  --dist <path>     Path to Vite output (default: dist/)
  -h, --help        Show this help`);
}

// `dist/` holds the desktop build; `dist/mobile/` is the separate mobile bundle
// (its own entry, with its own legitimate static imports) and is never scanned.
function assetDirs(distDir) {
  return [distDir, join(distDir, 'assets')].filter((d) => existsSync(d));
}

function listJsChunks(distDir) {
  const out = [];
  for (const dir of assetDirs(distDir)) {
    for (const name of readdirSync(dir)) {
      const full = join(dir, name);
      if (!/\.js$/.test(name) || !statSync(full).isFile()) continue;
      out.push(full);
    }
  }
  return out;
}

// The desktop entry chunk, named the same way check-bundle-size.mjs names it so
// both gates measure the same file.
function findEntryChunk(distDir) {
  const matches = listJsChunks(distDir).filter((p) => /^index-[^/\\]+\.js$/.test(p.split(/[\\/]/).pop()));
  if (matches.length === 0) {
    throw new Error(`No entry chunk matching index-*.js in ${distDir} or ${distDir}/assets\nRun \`npm run build\` first.`);
  }
  if (matches.length > 1) {
    throw new Error(`Multiple entry chunks matched index-*.js: ${matches.map((p) => p.split(/[\\/]/).pop()).join(', ')}`);
  }
  return matches[0];
}

/**
 * Static (non-dynamic) `import`/`export ... from` specifiers in a built chunk.
 *
 * Matched as `from "..."` plus bare `import "..."` rather than as one
 * `import ... from "..."` pattern: a dynamic call is `import("./x.js")` with no
 * `from`, so scanning for `from` is not fooled by one, and the clause body
 * cannot run past a dynamic import into a later `from`. Only relative
 * specifiers are kept: Vite inlines this build's dependencies, so a bare
 * specifier in the output would not resolve to a chunk here anyway.
 */
export function staticImportSpecifiers(source) {
  const found = new Set();
  for (const re of [/\bfrom\s*["']([^"']+)["']/g, /\bimport\s*["']([^"']+)["']/g]) {
    for (const m of source.matchAll(re)) {
      const spec = m[1];
      if (spec.startsWith('./') || spec.startsWith('../')) found.add(spec);
    }
  }
  return [...found];
}

/** Chunk files reachable from the entry chunk through static import edges. */
export function staticClosure(entryPath) {
  const reached = new Map([[entryPath, null]]);
  const queue = [entryPath];
  while (queue.length > 0) {
    const current = queue.shift();
    const source = readFileSync(current, 'utf8');
    for (const spec of staticImportSpecifiers(source)) {
      const target = resolve(dirname(current), spec);
      if (!reached.has(target) && existsSync(target)) {
        reached.set(target, current);
        queue.push(target);
      }
    }
  }
  return reached;
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function findChunk(chunks, stem) {
  // Rollup emits `<stem>-<hash>.js` with a base64url-ish hash.
  const re = new RegExp(`^${escapeRegExp(stem)}-[A-Za-z0-9_-]{6,}\\.js$`);
  const matches = chunks.filter((p) => re.test(p.split(/[\\/]/).pop()));
  if (matches.length > 1) {
    // Two chunks for one stem means the build shape changed under us; picking
    // the first would silently check the wrong file.
    throw new Error(`Multiple chunks matched "${stem}-*.js": ${matches.map((p) => p.split(/[\\/]/).pop()).join(', ')}\nUpdate the FORBIDDEN list in scripts/check-bundle-composition.mjs.`);
  }
  return matches[0] ?? null;
}

/**
 * Whether an import clause pulls values at runtime, or is erased by the compiler.
 *
 * Covers both erased spellings: `import type { X } from 'y'` and the inline
 * `import { type X } from 'y'`. An empty clause is the bare `import 'y'` form,
 * which is a real side-effect import and is kept.
 */
function isValueImportClause(clause) {
  const trimmed = String(clause).trim();
  if (trimmed === '') return true;
  if (/^type\b/.test(trimmed)) return false;
  const bindings = trimmed
    .replace(/^\{/, '')
    .replace(/\}$/, '')
    .split(',')
    .map((part) => part.trim())
    .filter(Boolean);
  if (bindings.length === 0) return true;
  return bindings.some((binding) => !/^type\b/.test(binding));
}

/**
 * Static imports of `specifier` under `srcRoot`, as `file:line` strings.
 *
 * Used only to explain a failure: when a forbidden module's chunk has vanished,
 * some file must have imported it eagerly, and the reviewer needs to be told
 * which. Both eager spellings are matched — `import … from 'y'` and the bare
 * side-effect `import 'y'` — and erased type imports are skipped, so neither the
 * lazy sites that keep the module split nor the `import type` declarations this
 * codebase uses at module scope are reported as the culprits.
 *
 * Paths are reported relative to the directory that owns `srcRoot`, so the
 * default gives repo-relative `src/components/...` and a fixture tree gives a
 * stable path of its own. Using the module-level repo root instead would return
 * an absolute path whenever the two live on different Windows drives.
 */
export function findStaticImportSites(srcRoot, specifiers) {
  const sites = [];
  if (!existsSync(srcRoot)) return sites;
  const base = dirname(resolve(srcRoot));
  const stems = specifiers.map((s) => s.replace(/^\.\//, ''));
  const stack = [resolve(srcRoot)];
  while (stack.length > 0) {
    const dir = stack.pop();
    for (const name of readdirSync(dir)) {
      const full = join(dir, name);
      if (statSync(full).isDirectory()) {
        stack.push(full);
        continue;
      }
      if (!/\.tsx?$/.test(name)) continue;
      const source = readFileSync(full, 'utf8');
      for (const stem of stems) {
        const tail = stem.startsWith('@') ? stem : `[^'"]*${escapeRegExp(stem)}`;
        const patterns = [
          // `import … from 'y'` / `export … from 'y'`
          new RegExp(`\\b(?:import|export)\\s+([^;'"]*?)\\bfrom\\s*['"]${tail}['"]`, 'g'),
          // bare side-effect `import 'y'`, which has no clause to inspect
          new RegExp(`\\bimport\\s*['"]${tail}['"]`, 'g'),
        ];
        for (const re of patterns) {
          for (const m of source.matchAll(re)) {
            if (!isValueImportClause(m[1])) continue;
            const line = source.slice(0, m.index).split('\n').length;
            sites.push(`${relative(base, full).replace(/\\/g, '/')}:${line}`);
          }
        }
      }
    }
  }
  return [...new Set(sites)].sort();
}

export function evaluateComposition(distDir, srcRoot = join(root, 'src')) {
  // `staticClosure` keys are absolute (it resolves each import target), so the
  // chunk paths this function compares against them must be absolute too. A
  // relative `--dist dist` would otherwise make every `closure.get(chunk)` miss
  // and report a static import edge as lazy — a silent pass on broken code.
  const distRoot = resolve(distDir);
  const entryPath = findEntryChunk(distRoot);
  const entry = entryPath.split(/[\\/]/).pop();
  const chunks = listJsChunks(distRoot);
  const closure = staticClosure(entryPath);

  const checks = FORBIDDEN.map((item) => {
    const chunk = findChunk(chunks, item.stem);
    const chunkName = chunk ? chunk.split(/[\\/]/).pop() : null;
    if (!chunk) {
      return {
        ...item,
        chunk: null,
        ok: false,
        reason: 'inlined',
        detail: `no ${item.stem}-*.js chunk was emitted, so its code is inlined into ${entry}`,
        sites: findStaticImportSites(srcRoot, [item.specifier]),
      };
    }
    const pulledInBy = closure.get(chunk) ?? null;
    return {
      ...item,
      chunk: chunkName,
      ok: !pulledInBy,
      reason: pulledInBy ? 'static-edge' : 'lazy',
      detail: pulledInBy
        ? `${chunkName} is statically imported by ${pulledInBy.split(/[\\/]/).pop()}`
        : `lazy chunk ${chunkName}`,
      sites: [],
    };
  });

  return { entry, checks, violations: checks.filter((c) => !c.ok) };
}

function main() {
  const args = parseArgs(process.argv);
  if (!existsSync(args.dist)) {
    console.error(`dist directory not found: ${args.dist}\nRun \`npm run build\` first.`);
    process.exit(2);
  }
  let report;
  try {
    report = evaluateComposition(args.dist);
  } catch (e) {
    console.error(e.message);
    process.exit(2);
  }

  console.log('Bundle composition report');
  console.log('=========================');
  console.log(`Entry chunk: ${report.entry}`);
  console.log('');
  for (const c of report.checks) {
    console.log(`[${c.ok ? 'ok  ' : 'FAIL'}] ${c.specifier.padEnd(26)} ${c.detail}`);
  }
  console.log('-'.repeat(72));

  if (report.violations.length === 0) {
    console.log('Bundle composition OK: xterm, its addons and the Probe tabs are all split out of the entry chunk.');
    process.exit(0);
  }

  console.error('\nForbidden import pulled into the entry chunk:');
  for (const v of report.violations) {
    console.error(`  - ${v.specifier} (${v.why})`);
    console.error(`      ${v.detail}.`);
    // Only the inlined case has an eager import to point at; a static import
    // edge already names the chunk that introduced it.
    if (v.reason === 'inlined') {
      if (v.sites.length > 0) {
        for (const site of v.sites) console.error(`      static import at ${site}`);
      } else {
        console.error('      no static import of it under src/ — it is inlined through another lazy chunk.');
      }
    }
  }
  console.error(
    '\nIssue #2409: keep these imports dynamic (`await import(...)` inside the function, or `lazy()` for a Probe tab). ' +
      'If a build-config change renamed the chunk, update the FORBIDDEN list in scripts/check-bundle-composition.mjs instead of raising a limit.',
  );
  process.exit(1);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}