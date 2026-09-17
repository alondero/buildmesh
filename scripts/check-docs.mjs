#!/usr/bin/env node
// Documentation contract gate. This is intentionally dependency-free so it can
// run before `npm ci` and from every supported shell.

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, isAbsolute, relative, resolve } from 'node:path';
import { parseHarnessLabel, parseProviderVariants } from './check-readme-drift.mjs';
import { hasDocumentationExemption, isBehaviorSensitivePath, isDocumentationPath } from '../.claude/hooks/guard-documentation.mjs';

const scriptDir = dirname(fileURLToPath(import.meta.url));
export const repoRoot = resolve(scriptDir, '..');

export const REQUIRED_PATHS = [
  'README.md',
  'CONTRIBUTING.md',
  'CONTEXT.md',
  'SECURITY.md',
  'CODE_OF_CONDUCT.md',
  'docs/README.md',
  'docs/documentation-standards.md',
  'docs/user-guide.md',
  'docs/troubleshooting.md',
  'docs/development/README.md',
  'docs/releases/README.md',
  'docs/adr/README.md',
  'docs/specs/README.md',
];

const DOC_HUB_LINKS = [
  'user-guide.md',
  'troubleshooting.md',
  'development/README.md',
  'releases/README.md',
  'documentation-standards.md',
  'adr/README.md',
  'specs/README.md',
];

function walkMarkdown(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(dir, entry.name);
    if (entry.isDirectory()) return walkMarkdown(path);
    return entry.isFile() && entry.name.toLowerCase().endsWith('.md') ? [path] : [];
  });
}

export function collectMarkdownFiles(root = repoRoot) {
  return [
    ...walkMarkdown(resolve(root, 'docs')),
    ...walkMarkdown(resolve(root, '.github')),
    ...['README.md', 'CONTRIBUTING.md', 'CONTEXT.md', 'SECURITY.md', 'CODE_OF_CONDUCT.md']
      .map((file) => resolve(root, file))
      .filter(existsSync),
  ];
}

export function pathExistsExactly(root, target) {
  const relativePath = relative(root, target);
  if (!relativePath || relativePath === '.') return existsSync(root);
  if (relativePath.startsWith('..') || isAbsolute(relativePath)) return false;
  let current = root;
  for (const part of relativePath.split(/[\\/]/).filter(Boolean)) {
    const entry = readdirSync(current).find((name) => name === part);
    if (!entry) return false;
    current = resolve(current, entry);
  }
  return existsSync(current);
}

export function githubAnchor(text) {
  return text
    .toLowerCase()
    .trim()
    .replace(/[`*_~]/g, '')
    // GitHub's generated anchor retains the two spaces surrounding a
    // standalone ampersand as two hyphens; preserve that observable form.
    .replace(/\s*&\s*/g, '--')
    .replace(/[^\p{L}\p{N}\s-]/gu, '')
    .replace(/\s+/g, '-')
    .replace(/-{3,}/g, '--');
}

export function stripFencedCode(markdown) {
  return markdown.replace(/^(`{3,}|~{3,})[^\n]*\n[\s\S]*?^\1\s*$/gm, '');
}

export function markdownHeadings(markdown) {
  return [...stripFencedCode(markdown).matchAll(/^(#{1,6})\s+(.+?)\s*#*\s*$/gm)].map((match) => ({
    level: match[1].length,
    text: match[2].trim(),
    anchor: githubAnchor(match[2]),
  }));
}

export function extractMarkdownLinks(markdown) {
  const links = [];
  const linkPattern = /(!?)\[([^\]]*)\]\((?:<([^>]+)>|([^\s)]+))(?:\s+[^)]*)?\)/g;
  for (const match of stripFencedCode(markdown).matchAll(linkPattern)) {
    const target = (match[3] ?? match[4]).trim();
    links.push({ target, image: match[1] === '!', alt: match[2] });
  }
  return links;
}

export function hasDocumentStatus(markdown) {
  return /^Status:\s*\S.+$/m.test(markdown)
    || /^##\s+Status\s*\n+\s*\S.+$/m.test(markdown);
}

export function checkDocumentationImpact({ changedFiles = [], commitMessages = [] } = {}) {
  const sensitive = changedFiles.filter(isBehaviorSensitivePath);
  if (sensitive.length === 0) return [];
  const hasDocumentation = changedFiles.some(isDocumentationPath);
  const hasExemption = commitMessages.some(hasDocumentationExemption);
  if (hasDocumentation || hasExemption) return [];
  return [
    `[documentation-impact] ${sensitive.join(', ')} changed without a documentation update or a `
      + '`docs: none — <reason>` commit message',
  ];
}

function isExternalTarget(target) {
  return /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i.test(target);
}

function resolveLocalTarget(source, target, root) {
  const withoutFragment = target.split('#', 1)[0].split('?', 1)[0];
  if (!withoutFragment) return source;
  let decoded = withoutFragment;
  try {
    decoded = decodeURIComponent(withoutFragment);
  } catch {
    // Keep the original path; the existence check below will give a useful error.
  }
  return resolve(dirname(source), decoded);
}

export function checkLocalLinks({ root = repoRoot, files = collectMarkdownFiles(root) } = {}) {
  const failures = [];
  for (const source of files) {
    const markdown = readFileSync(source, 'utf8');
    for (const { target, image, alt } of extractMarkdownLinks(markdown)) {
      if (isExternalTarget(target)) continue;
      const fragmentIndex = target.indexOf('#');
      const fragment = fragmentIndex === -1 ? '' : target.slice(fragmentIndex + 1);
      const targetPath = resolveLocalTarget(source, target, root);
      if (!pathExistsExactly(root, targetPath)) {
        failures.push(`${relative(root, source)}: broken local ${image ? 'image' : 'link'} "${target}"`);
        continue;
      }
      if (fragment && statSync(targetPath).isFile() && targetPath.toLowerCase().endsWith('.md')) {
        const anchors = new Set(markdownHeadings(readFileSync(targetPath, 'utf8')).map((heading) => heading.anchor));
        if (!anchors.has(fragment.toLowerCase())) {
          failures.push(`${relative(root, source)}: missing anchor "#${fragment}" in "${relative(root, targetPath)}"`);
        }
      }
      if (image && !alt.trim()) {
        failures.push(`${relative(root, source)}: image link has empty alt text "${target}"`);
      }
      if (image && !targetPath.toLowerCase().match(/\.(?:png|jpe?g|gif|svg|webp|avif)$/)) {
        failures.push(`${relative(root, source)}: image target is not a raster/vector image "${target}"`);
      }
    }
  }
  return failures;
}

export function checkDocumentation({ root = repoRoot, files = collectMarkdownFiles(root) } = {}) {
  const failures = [];
  const add = (anchor, message) => failures.push(`[${anchor}] ${message}`);

  for (const path of REQUIRED_PATHS) {
    if (!existsSync(resolve(root, path))) add('required-file', `missing required documentation file: ${path}`);
  }

  const hubPath = resolve(root, 'docs/README.md');
  if (existsSync(hubPath)) {
    const hub = readFileSync(hubPath, 'utf8');
    for (const link of DOC_HUB_LINKS) {
      if (!hub.includes(`](${link})`)) add('documentation-hub', `docs/README.md must link to ${link}`);
    }
  }

  for (const source of files.filter((path) => path.startsWith(resolve(root, 'docs')))) {
    const headings = markdownHeadings(readFileSync(source, 'utf8'));
    const relativePath = relative(root, source);
    const h1Count = headings.filter((heading) => heading.level === 1).length;
    if (h1Count === 0) {
      add('top-level-heading', `${relativePath} must have a level-one heading`);
    } else if (h1Count > 1) {
      add('top-level-heading', `${relativePath} must have exactly one level-one heading (found ${h1Count})`);
    }
  }

  for (const source of files) {
    const relativePath = relative(root, source).replaceAll('\\', '/');
    const releaseMatch = relativePath.match(/^docs\/releases\/(v\d+\.\d+\.\d+)\.md$/i);
    if (releaseMatch) {
      const headings = markdownHeadings(readFileSync(source, 'utf8'));
      const expectedTitle = `Buildmesh ${releaseMatch[1]}`;
      if (!headings.some(({ level, text }) => level === 1 && text === expectedTitle)) {
        add('release-note', `${relativePath} must have the title "${expectedTitle}"`);
      }
    } else if (relativePath.startsWith('docs/releases/') && !/\/README\.md$/i.test(relativePath)) {
      add('release-note', `${relativePath} must be named vX.Y.Z.md`);
    }
    if (!/^docs\/(?:adr|specs)\/[^/]+\.md$/i.test(relativePath) || /\/README\.md$/i.test(relativePath)) continue;
    const markdown = readFileSync(source, 'utf8');
    if (!hasDocumentStatus(markdown)) {
      add('document-status', `${relativePath} must declare a current/proposed/superseded/historical status`);
    }
  }

  const harnessPath = resolve(root, 'src/components/Circuits/harnessCapabilities.ts');
  const userGuidePath = resolve(root, 'docs/user-guide.md');
  if (existsSync(harnessPath) && existsSync(userGuidePath)) {
    const labels = parseHarnessLabel(readFileSync(harnessPath, 'utf8')) ?? {};
    const guide = readFileSync(userGuidePath, 'utf8');
    for (const label of Object.values(labels)) {
      if (!guide.includes(`| ${label} |`)) {
        add('harness-guide-coverage', `docs/user-guide.md is missing the ${label} harness row`);
      }
    }
  }

  const providerPath = resolve(root, 'src/types/generated/Provider.ts');
  if (existsSync(providerPath) && existsSync(harnessPath)) {
    const variants = parseProviderVariants(readFileSync(providerPath, 'utf8'));
    const labels = parseHarnessLabel(readFileSync(harnessPath, 'utf8')) ?? {};
    for (const variant of variants) {
      if (!(variant in labels)) add('harness-source-coverage', `Provider variant ${variant} has no HARNESS_LABEL entry`);
    }
  }

  for (const failure of checkLocalLinks({ root, files })) add('local-links', failure);
  return failures;
}

function isMain() {
  if (!process.argv[1]) return false;
  try {
    return resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

function parseBaseArgument() {
  const args = process.argv.slice(2);
  if (args.length === 0) return null;
  if (args.length !== 2 || args[0] !== '--base') {
    throw new Error('Usage: node scripts/check-docs.mjs [--base <base-commit>]');
  }
  return args[1];
}

export function changedFilesSince(root, base) {
  const options = { cwd: root, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 };
  const requestedBase = String(base ?? '').trim();
  const candidate = !requestedBase || /^0+$/.test(requestedBase) ? 'HEAD' : requestedBase;
  try {
    const commit = execFileSync(
      'git',
      ['rev-parse', '--verify', '--end-of-options', `${candidate}^{commit}`],
      { ...options, stdio: ['ignore', 'pipe', 'ignore'] },
    ).trim();
    const changed = execFileSync('git', ['diff', '--name-only', '-z', commit, '--'], options).split('\0').filter(Boolean);
    const messages = execFileSync('git', ['log', `${commit}..HEAD`, '--format=%B%x00'], options);
    return {
      changedFiles: changed,
      commitMessages: messages.split('\0').filter(Boolean),
      base: candidate,
      skipped: false,
    };
  } catch (error) {
    return {
      changedFiles: [],
      commitMessages: [],
      base: candidate,
      skipped: true,
      reason: error.message,
    };
  }
}

if (isMain()) {
  let failures;
  try {
    const base = parseBaseArgument();
    failures = checkDocumentation();
    if (base) {
      const impact = changedFilesSince(repoRoot, base);
      if (impact.skipped) {
        console.warn(`Documentation impact check skipped for unavailable base "${impact.base}".`);
      } else {
        failures.push(...checkDocumentationImpact(impact));
      }
    }
  } catch (error) {
    console.error(`Documentation check failed: ${error.message}`);
    process.exitCode = 1;
    failures = null;
  }
  if (!failures) process.exitCode = 1;
  else if (failures.length > 0) {
    console.error(`Documentation check failed (${failures.length} issue${failures.length === 1 ? '' : 's'}):`);
    for (const failure of failures) console.error(`  - ${failure}`);
    process.exitCode = 1;
  } else {
    console.log(`Documentation check passed (${collectMarkdownFiles().length} Markdown files checked).`);
  }
}
