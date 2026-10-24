/**
 * Guards over the test tree itself (issue #2534).
 *
 * The Session → AgentNode rename deleted `src/stores/sessionStore` and left
 * `tests/integration/session-switching.test.tsx` importing it. No gate caught
 * that: `tsc --noEmit` only covers `src/` (see `tsconfig.json`'s `include`),
 * and the Vitest run does not fail on an unresolvable import either — the
 * module is only touched when the test body runs, and the test that owned the
 * import was quietly asserting a hand-written fake rather than the app.
 *
 * These two guards close that door:
 *
 *   1. each module specifier in `tests/` must resolve to a file that
 *      exists, checked with the TypeScript compiler's own resolver under the
 *      repo's `tsconfig.json` options;
 *   2. no file in `tests/` may assert a tautology (a value compared with
 *      itself), the shape the deleted session-switching test used.
 *
 * `src/` does not need guard 1: `npm run build` runs `tsc` over it, so a
 * broken import there fails the build.
 *
 * Run with: npm test -- --run tests/unit/test-tree-guards.test.ts
 */
import { describe, it, expect } from 'vitest';
import ts from 'typescript';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { builtinModules } from 'node:module';

const REPO_ROOT = resolve(__dirname, '..', '..');
const TESTS_ROOT = join(REPO_ROOT, 'tests');
const TS_CONFIG = join(REPO_ROOT, 'tsconfig.json');

/** Extensions that can carry a module specifier worth checking. */
const SOURCE_FILE = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/;

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      walk(full, out);
    } else if (SOURCE_FILE.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

function relPath(absFile: string): string {
  return absFile.slice(REPO_ROOT.length + 1).replace(/\\/g, '/');
}

/** Node builtins are not files on disk — the runtime provides them. */
function isNodeBuiltin(specifier: string): boolean {
  if (specifier.startsWith('node:')) return true;
  // `assert/strict`, `fs/promises` and friends are the builtin plus a
  // subpath.
  const base = specifier.split('/')[0];
  return builtinModules.includes(specifier) || builtinModules.includes(base);
}

/**
 * Specifiers the bundler resolves but the TypeScript resolver does not:
 * style and asset imports (Vite handles those), and the virtual-module
 * prefixes Vite and Vitest provide. They are not TypeScript modules, so
 * failing on them would be a false positive.
 */
const NON_MODULE_SPECIFIER =
  /^(?:virtual|file|data|https?):|\.(?:css|scss|sass|less|styl|svg|png|jpe?g|gif|webp|avif|ico|woff2?|ttf|eot|wasm|json5)$/i;

function isNonModuleSpecifier(specifier: string): boolean {
  return NON_MODULE_SPECIFIER.test(specifier);
}

/**
 * The module specifiers a file imports: static imports, re-exports, dynamic
 * `import()` calls and `import('...')` type nodes. Specifiers that are plain
 * strings (a fixture written into a temp dir, say) are not imports and are
 * correctly left out.
 */
function moduleSpecifiers(fileName: string, text: string): string[] {
  // `createSourceFile` infers the script kind from the file name, so a .tsx
  // file is parsed as TSX and a .ts file as TS.
  const source = ts.createSourceFile(fileName, text, ts.ScriptTarget.ES2020, true);
  const found: string[] = [];
  const visit = (node: ts.Node): void => {
    if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier)) {
      found.push(node.moduleSpecifier.text);
    } else if (
      ts.isExportDeclaration(node) &&
      node.moduleSpecifier &&
      ts.isStringLiteral(node.moduleSpecifier)
    ) {
      found.push(node.moduleSpecifier.text);
    } else if (
      ts.isCallExpression(node) &&
      node.expression.kind === ts.SyntaxKind.ImportKeyword &&
      node.arguments.length > 0 &&
      ts.isStringLiteral(node.arguments[0])
    ) {
      found.push(node.arguments[0].text);
    } else if (
      ts.isImportTypeNode(node) &&
      ts.isLiteralTypeNode(node.argument) &&
      ts.isStringLiteral(node.argument.literal)
    ) {
      found.push(node.argument.literal.text);
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  return found;
}

interface UnresolvedImport {
  file: string;
  specifier: string;
}

/**
 * The compiler options the repo actually uses, so resolution here matches
 * what `tsc` would do. `allowJs` is on because the test tree imports the
 * repo's `.mjs` helper scripts.
 */
function repoCompilerOptions(): ts.CompilerOptions {
  const parsed = ts.getParsedCommandLineOfConfigFile(
    TS_CONFIG,
    {},
    {
      ...ts.sys,
      onUnRecoverableConfigFileDiagnostic: () => {},
    },
  );
  if (!parsed) {
    throw new Error(`could not read ${TS_CONFIG}`);
  }
  return { ...parsed.options, noEmit: true, allowJs: true, checkJs: false };
}

function findUnresolvedImports(files: string[], options: ts.CompilerOptions): UnresolvedImport[] {
  const unresolved: UnresolvedImport[] = [];
  for (const file of files) {
    const text = readFileSync(file, 'utf8');
    for (const specifier of moduleSpecifiers(file, text)) {
      if (isNodeBuiltin(specifier)) continue;
      if (isNonModuleSpecifier(specifier)) continue;
      const resolved = ts.resolveModuleName(specifier, file, options, ts.sys);
      if (!resolved.resolvedModule) {
        unresolved.push({ file: relPath(file), specifier });
      }
    }
  }
  return unresolved;
}

/**
 * A value compared with itself passes for any value. The backreference makes
 * the two sides of the comparison have to be the same expression.
 */
const TAUTOLOGY = new RegExp(
  String.raw`expect\(\s*([A-Za-z_$][\w$.]*)\s*\)\s*\.\s*to(?:Be|Equal|StrictEqual)\(\s*\1\s*\)`,
  'g',
);

interface Tautology {
  file: string;
  line: number;
  text: string;
}

function findTautologies(files: string[]): Tautology[] {
  const found: Tautology[] = [];
  for (const file of files) {
    const lines = readFileSync(file, 'utf8').split(/\r?\n/);
    lines.forEach((line, index) => {
      const match = new RegExp(TAUTOLOGY.source).exec(line);
      if (match) {
        found.push({ file: relPath(file), line: index + 1, text: match[0] });
      }
    });
  }
  return found;
}

describe('test tree guards (#2534)', () => {
  const files = walk(TESTS_ROOT);

  it('walks the test tree and finds source files', () => {
    // Sanity gate — if the walk or the extension filter is broken, the
    // assertions below pass on an empty list.
    expect(files.length).toBeGreaterThan(50);
  });

  it('each module specifier in tests/ resolves to a file that exists', () => {
    const unresolved = findUnresolvedImports(files, repoCompilerOptions());
    if (unresolved.length > 0) {
      const report = unresolved
        .map(u => `  ${u.file} imports '${u.specifier}'`)
        .join('\n');
      throw new Error(
        `${unresolved.length} import(s) in tests/ point at a module that does not exist:\n` +
        `${report}\n\n` +
        `A test that imports a deleted module never fails loudly — the import is only\n` +
        `resolved when the test body runs, so the file can sit in the tree asserting\n` +
        `nothing. Fix the import (or delete the file if it is dead test surface).\n` +
        `This is the guard that issue #2534 asked for: tsc --noEmit only covers src/.`,
      );
    }
  });

  it('no file in tests/ asserts a tautology', () => {
    const tautologies = findTautologies(files);
    if (tautologies.length > 0) {
      const report = tautologies
        .map(t => `  ${t.file}:${t.line}  ${t.text}`)
        .join('\n');
      throw new Error(
        `${tautologies.length} tautological assertion(s) in tests/:\n` +
        `${report}\n\n` +
        `Comparing a value with itself always passes, so the assertion checks\n` +
        `nothing. Assert against the real expected value instead.`,
      );
    }
  });
});
