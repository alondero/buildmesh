import { describe, it, expect } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import ts from 'typescript';
import { ESLint } from 'eslint';

// ADR-0010: facets own domain wrappers; _invoke owns error logging. The
// logging bridge is a deliberate peer because routing it back through the
// wrapper would introduce a logging cycle.
const RAW_INVOKE_OWNERS = new Set([
  'src/lib/frontendLog.ts',
  'src/lib/tauri/_invoke.ts',
]);
const SRC_DIR = join(process.cwd(), 'src');
const FACET_DIR = join(SRC_DIR, 'lib', 'tauri');

function tsFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = join(dir, entry.name);
    return entry.isDirectory() ? tsFiles(full) : /\.tsx?$/.test(entry.name) ? [full] : [];
  });
}

function rel(full: string): string {
  return relative(process.cwd(), full).split(sep).join('/');
}

function imports(full: string) {
  const source = readFileSync(full, 'utf8');
  const file = ts.createSourceFile(full, source, ts.ScriptTarget.Latest, true);
  const edges: { module: string; rawInvoke: boolean }[] = [];
  function visit(node: ts.Node) {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
        node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      const bindings = ts.isImportDeclaration(node)
        ? node.importClause?.namedBindings : node.exportClause;
      const rawInvoke = !bindings || ts.isNamespaceImport(bindings) || ts.isNamespaceExport(bindings) ||
        bindings.elements.some((e) => (e.propertyName ?? e.name).text === 'invoke');
      edges.push({ module: node.moduleSpecifier.text, rawInvoke });
    } else if (ts.isCallExpression(node) &&
        (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
          ts.isIdentifier(node.expression) && node.expression.text === 'require') &&
        node.arguments[0] && ts.isStringLiteral(node.arguments[0])) {
      edges.push({ module: node.arguments[0].text, rawInvoke: true });
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
  return edges;
}

describe('Tauri IPC seam (ADR-0010)', () => {
  const sources = tsFiles(SRC_DIR).map((full) => ({ full, name: rel(full), edges: imports(full) }));

  it('only the chokepoint and logging bridge reach raw invoke, including aliases and dynamic imports', () => {
    const owners = sources.filter(({ edges }) => edges.some((edge) =>
      edge.module === '@tauri-apps/api/core' && edge.rawInvoke)).map(({ name }) => name);
    expect(owners.sort()).toEqual([...RAW_INVOKE_OWNERS].sort());
  });

  it('consumers cannot reach around domain facets into the _invoke chokepoint', () => {
    const violations = sources.filter(({ full, edges }) => !full.startsWith(FACET_DIR + sep) &&
      edges.some(({ module }) => module.startsWith('.') &&
        resolve(full, '..', module).replace(/\.ts$/, '') === join(FACET_DIR, '_invoke')));
    expect(violations.map(({ name }) => name)).toEqual([]);
  });

  it('facets route through the chokepoint and cannot depend on the compatibility barrel', () => {
    const facets = sources.filter(({ full, name }) => full.startsWith(FACET_DIR + sep) && !name.endsWith('/_invoke.ts'));
    expect(facets.length).toBeGreaterThan(0);
    for (const { name, full, edges } of facets) {
      expect(edges.some(({ module }) => module === './_invoke'), name).toBe(true);
      expect(edges.some(({ module }) => module.startsWith('.') &&
        resolve(full, '..', module).replace(/\.ts$/, '') === join(SRC_DIR, 'lib', 'tauri')), name).toBe(false);
    }
  });

  it('the compatibility barrel contains only deprecated domain re-exports', () => {
    const full = join(SRC_DIR, 'lib', 'tauri.ts');
    const source = readFileSync(full, 'utf8');
    const file = ts.createSourceFile(full, source, ts.ScriptTarget.Latest, true);
    expect(source.split('\n').length).toBeLessThan(400);
    expect(file.statements.every(ts.isExportDeclaration)).toBe(true);
    for (const statement of file.statements) {
      expect(statement.getFullText(file)).toContain('@deprecated');
    }
  });
});

describe('IPC import lint enforcement', () => {
  const eslint = new ESLint();
  async function errors(code: string, filePath = 'src/components/NewFacetConsumer.tsx') {
    const [result] = await eslint.lintText(code, { filePath });
    return result.messages.filter((message) => message.ruleId === 'no-restricted-imports');
  }

  it.each(['../../lib/tauri', '../../lib/tauri.ts', '../lib/tauri', './tauri'])(
    'rejects new compatibility-barrel imports from %s', async (module) => {
      expect(await errors(`import { listAgentNodes } from '${module}'; listAgentNodes();`)).toHaveLength(1);
    },
  );
  it('permits domain imports and Channel without permitting raw invoke or private transport access', async () => {
    expect(await errors("import { listAgentNodes } from '../../lib/tauri/agentNode'; listAgentNodes();")).toEqual([]);
    expect(await errors("import { Channel } from '@tauri-apps/api/core'; new Channel();")).toEqual([]);
    expect(await errors("import { invoke as raw } from '@tauri-apps/api/core'; raw('x');")).toHaveLength(1);
    expect(await errors("import * as core from '@tauri-apps/api/core'; core.invoke('x');")).toHaveLength(1);
    expect(await errors("import { _invoke } from '../../lib/tauri/_invoke'; _invoke('x');")).toHaveLength(1);
    expect(await errors("export { _invoke } from '../../lib/tauri/_invoke';")).toHaveLength(1);
  });
  it('allows legacy consumers while still prohibiting private transport access there', async () => {
    expect(await errors("import { listAgentNodes } from '../lib/tauri'; listAgentNodes();", 'src/stores/agentNodeStore.ts')).toEqual([]);
    expect(await errors("import { toHostPath } from './tauri'; toHostPath('/repo');", 'src/lib/fileDropPaste.ts')).toEqual([]);
    expect(await errors("import { _invoke } from '../lib/tauri/_invoke'; _invoke('x');", 'src/stores/agentNodeStore.ts')).toHaveLength(1);
    expect(await errors("import { _invoke } from './_invoke'; _invoke('x');", 'src/lib/tauri/agentNode.ts')).toEqual([]);
  });
});
