import { execFileSync } from 'node:child_process';
import { lstatSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { basename, extname, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import treeSitter from '@vscode/tree-sitter-wasm';
import { CST, Parser as YamlParser } from 'yaml';

const { Language, Parser } = treeSitter;
const require = createRequire(import.meta.url);
const grammars = new Map(Object.entries({
  '.js': 'javascript', '.mjs': 'javascript', '.cjs': 'javascript', '.jsx': 'javascript',
  '.ts': 'typescript', '.mts': 'typescript', '.cts': 'typescript', '.tsx': 'tsx',
  '.jsonc': 'typescript', '.rs': 'rust', '.kt': 'kotlin', '.kts': 'kotlin',
  '.css': 'css', '.html': 'html',
  '.sh': 'bash', '.bash': 'bash', '.ps1': 'powershell', '.psm1': 'powershell', '.toml': 'toml',
}));
const batchComments = /"(?:\^.|[^"\n])*"|\^\r?\n|\^.|(?:^|[&|])[\t ]*((?:@?rem(?=[\t ]|$)|::)[^\n]*)/gim;
const xmlComments = /<!\[CDATA\[[\s\S]*?\]\]>|<\?[\s\S]*?\?>|(<!--[\s\S]*?-->)/g;
const lexicalPatterns = new Map([
  ['.sql', /'(?:''|[^'])*'|"(?:""|[^"])*"|`[^`]*`|\[[^\]]*\]|(--[^\n]*|\/\*[\s\S]*?\*\/)/g],
  ['.cmd', batchComments],
  ['.bat', batchComments],
  ['.xml', xmlComments],
  ['.svg', xmlComments],
  ['.gitignore', /^(#[^\n]*)/gm],
  ['.gitattributes', /^[\t ]*(#[^\n]*)/gm],
  ['.properties', /^[\t ]*([#!][^\n]*)/gm],
]);
const commentTypes = ['comment', 'line_comment', 'block_comment', 'multiline_comment'];
const treeLanguages = new Set(grammars.values());
const languages = new Map();

function languageFor(path) {
  if (path.startsWith('docs/') || path.startsWith('src/types/generated/')
      || ['android/gradlew', 'android/gradlew.bat'].includes(path)) return null;
  const extension = extname(path) || basename(path);
  if (lexicalPatterns.has(extension) || extension === '.yml' || extension === '.yaml') return extension;
  return grammars.get(extension) ?? null;
}

async function treeComments(source, name) {
  if (!languages.has(name)) {
    const directory = ['html', 'kotlin', 'toml'].includes(name)
      ? 'tree-sitter-wasms/out' : '@vscode/tree-sitter-wasm/wasm';
    languages.set(name, Parser.init().then(() =>
      Language.load(require.resolve(`${directory}/tree-sitter-${name}.wasm`))));
  }
  const language = await languages.get(name);
  const parser = new Parser();
  parser.setLanguage(language);
  const tree = parser.parse(source);
  try {
    const comments = tree.rootNode.descendantsOfType(commentTypes).map(node => {
      const range = { start: node.startIndex, end: node.endIndex };
      const parent = node.parent;
      if (parent?.type === 'jsx_expression' && parent.namedChildCount === 1) {
        range.wrapper = { start: parent.startIndex, end: parent.endIndex };
      }
      return range;
    });
    if (name === 'html') {
      for (const node of tree.rootNode.descendantsOfType('raw_text')) {
        const embedded = node.parent.type === 'script_element' ? 'javascript' : 'css';
        for (const range of await treeComments(node.text, embedded)) {
          comments.push({ start: node.startIndex + range.start, end: node.startIndex + range.end });
        }
      }
    }
    return comments;
  } finally {
    tree.delete();
    parser.delete();
  }
}

async function literalRunComments(block, language) {
  const lines = block.source.split('\n');
  const indentation = lines.filter(line => line.trim()).map(line => /^ */.exec(line)[0].length);
  if (!indentation.length) return [];
  const indent = Math.min(...indentation);
  const lastProperty = block.props.at(-1);
  let original = lastProperty.offset + lastProperty.source.length;
  let projected = 0;
  const offsets = [];
  const script = lines.map(line => {
    const prefix = Math.min(indent, line.length);
    const content = line.slice(prefix);
    offsets.push({ index: projected, delta: original + prefix - projected });
    original += line.length + 1;
    projected += content.length + 1;
    return content;
  }).join('\n');
  const toOriginal = index => index + offsets.findLast(offset => offset.index <= index).delta;
  return (await treeComments(script, language)).map(({ start, end }) => ({
    start: toOriginal(start), end: toOriginal(end),
  }));
}

async function yamlComments(path, source) {
  const comments = [];
  const runs = [];
  const actionsFile = /^\.github\/(?:workflows|actions)\//.test(path);
  function visit(value) {
    if (Array.isArray(value)) {
      value.forEach(visit);
    } else if (value && typeof value === 'object') {
      if (value.type === 'comment') {
        comments.push({ start: value.offset, end: value.offset + value.source.length });
      } else {
        if (actionsFile && value.type === 'block-map') {
          const field = name => value.items.find(item => CST.resolveAsScalar(item.key)?.value === name)?.value;
          const run = field('run');
          const shellValue = CST.resolveAsScalar(field('shell'))?.value ?? 'bash';
          const shell = shellValue.split(/\s+/)[0];
          if (run?.type === 'block-scalar' && run.props[0]?.source.startsWith('|')) {
            if (shell === 'bash' || shell === 'sh') runs.push(literalRunComments(run, 'bash'));
            if (shell === 'pwsh' || shell === 'powershell') runs.push(literalRunComments(run, 'powershell'));
          }
        }
        Object.values(value).forEach(visit);
      }
    }
  }
  for (const token of new YamlParser().parse(source)) visit(token);
  for (const ranges of await Promise.all(runs)) comments.push(...ranges);
  return comments;
}

export async function sourceComments(path, source) {
  const language = languageFor(path);
  let ranges;
  if (language === '.yml' || language === '.yaml') {
    ranges = await yamlComments(path, source);
  } else if (lexicalPatterns.has(language)) {
    ranges = [...source.matchAll(lexicalPatterns.get(language))]
      .filter(match => match[1] !== undefined)
      .map(match => {
        const end = match.index + match[0].length;
        return { start: end - match[1].length, end };
      });
  } else if (treeLanguages.has(language)) {
    ranges = await treeComments(source, language);
  } else {
    return [];
  }
  return ranges
    .filter(({ start }) => start !== 0 || !source.startsWith('#!') || language !== 'bash')
    .sort((a, b) => a.start - b.start)
    .map(range => ({ ...range, text: source.slice(range.start, range.end).replace(/\r\n/g, '\n').trimEnd() }));
}

function git(root, args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
}

function pathsFrom(output) {
  return output.split('\0').filter(Boolean);
}

export async function checkComments(root, base = 'HEAD') {
  const revision = git(root, ['rev-parse', '--verify', '--end-of-options', `${base}^{commit}`]).trim();
  const previousPaths = new Set(pathsFrom(git(root, ['ls-tree', '-r', '--name-only', '-z', revision])));
  const diffArgs = ['--name-only', '--no-renames', '-z', revision, '--'];
  const changed = pathsFrom(git(root, ['diff', ...diffArgs]));
  const untracked = pathsFrom(git(root, ['ls-files', '-z', '--others', '--exclude-standard']));
  const staged = new Set(pathsFrom(git(root, ['diff', '--cached', '--diff-filter=ACMT', ...diffArgs])));
  const regularIndexPaths = new Set(pathsFrom(git(root, ['ls-files', '--stage', '-z']))
    .filter(entry => /^100\d+ [0-9a-f]+ 0\t/.test(entry))
    .map(entry => entry.slice(entry.indexOf('\t') + 1)));
  const additions = new Set();
  for (const path of [...new Set([...changed, ...untracked, ...staged])].sort()) {
    if (!languageFor(path)) continue;
    const fullPath = join(root, path);
    const working = lstatSync(fullPath, { throwIfNoEntry: false })?.isFile() ? readFileSync(fullPath, 'utf8') : null;
    const indexed = staged.has(path) && regularIndexPaths.has(path) ? git(root, ['show', `:${path}`]) : null;
    const previousCounts = new Map();
    if (previousPaths.has(path)) {
      const previous = git(root, ['show', `${revision}:${path}`]);
      for (const { text } of await sourceComments(path, previous)) {
        previousCounts.set(text, (previousCounts.get(text) ?? 0) + 1);
      }
    }
    for (const source of new Set([working, indexed])) {
      if (source === null) continue;
      const available = new Map(previousCounts);
      for (const { text, start } of await sourceComments(path, source)) {
        const count = available.get(text) ?? 0;
        if (count > 0) {
          available.set(text, count - 1);
          continue;
        }
        const line = source.slice(0, start).split('\n').length;
        const snapshot = source === indexed && source !== working ? ' (staged)' : '';
        additions.add(`${path}:${line}: new source comment${snapshot}`);
      }
    }
  }
  return [...additions];
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const { values } = parseArgs({ options: { base: { type: 'string', default: process.env.COMMENT_BASE ?? 'HEAD' } } });
    const errors = await checkComments(process.cwd(), values.base);
    for (const error of errors) console.error(error);
    if (errors.length) process.exitCode = 1;
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
