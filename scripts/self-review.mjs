#!/usr/bin/env node
// Advisory self-review prompts. Reviewers keep finding the same few defect classes,
// and the cheapest time to fix them is before the first publish. This compares the
// working tree with a base commit and lists where to look: absolute claims in added
// prose (the most common reviewer finding), and feature commits that are mostly
// formatting. It never fails a gate; docs/agents/self-review.md owns the checklist.
//
// Usage: node scripts/self-review.mjs [--base <commit>] [--all]
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const toolRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const CHECKLIST = 'docs/agents/self-review.md';
const TEXT_FILES = ['*.md', '*.rs', '*.ts', '*.tsx', '*.js', '*.mjs', '*.ps1', '*.yml', '*.yaml'];
const FORMATTABLE_FILES = ['*.rs', '*.ts', '*.tsx', '*.js', '*.mjs'];
// History and machine-written files describe or contain text nobody edits for accuracy.
const SKIPPED = /(?:^|\/)(?:generated|node_modules|archive|releases)\/|(?:^|\/)\.harness\/|package-lock\.json$/;
// Words reviewers dispute: an absolute claim is only as good as the line that proves it.
const CLAIM = /\b(?:every|always|never|nothing|guarantee[sd]?|by construction|exactly|unchanged|identical|cannot|no longer|none of|no other|neither)\b/i;
const FORMAT_COMMIT = /^(?:style|format|fmt)\b|^chore\((?:fmt|format)\)/i;

const git = (cwd, ...args) => execFileSync('git', ['-c', 'core.quotepath=off', ...args], { cwd, encoding: 'utf8', maxBuffer: 128 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });

function resolveBase(cwd, base) {
  return git(cwd, 'rev-parse', '--verify', '--end-of-options', `${base}^{commit}`).trim();
}

// Added lines of a unified=0 diff, with their line number in the new file.
function parseAdded(diff) {
  const added = [];
  let file = null;
  let line = 0;
  for (const raw of diff.split('\n')) {
    if (raw.startsWith('+++ ')) { file = raw.startsWith('+++ b/') ? raw.slice(6) : null; continue; }
    const hunk = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
    if (hunk) { line = Number(hunk[1]); continue; }
    if (!file || !raw.startsWith('+') || raw.startsWith('+++')) continue;
    added.push({ file, line, text: raw.slice(1).replace(/\r$/, '') });
    line += 1;
  }
  return added;
}

// Lines added since `base` in documents and source files (not generated ones),
// whether committed, staged, unstaged or untracked.
export function addedLines(cwd, base) {
  const commit = resolveBase(cwd, base);
  const tracked = parseAdded(git(cwd, 'diff', '--no-renames', '--no-ext-diff', '--no-textconv', '--unified=0', commit, '--', ...TEXT_FILES));
  const untracked = git(cwd, 'ls-files', '--others', '--exclude-standard', '-z', '--', ...TEXT_FILES).split('\0').filter(Boolean)
    .flatMap(file => readFileSync(resolve(cwd, file), 'utf8').split('\n').map((text, index) => ({ file, line: index + 1, text: text.replace(/\r$/, '') })).filter(row => row.text !== ''));
  return [...tracked, ...untracked].filter(row => !SKIPPED.test(row.file));
}

// How a line reads to a person: 0 document, 1 doc comment, 2 test title, 3 other comment, null code.
function proseRank({ file, text }) {
  if (/\.md$/.test(file)) return /\S/.test(text) && !/^\s*(?:\||```|#|<!--)/.test(text) ? 0 : null;
  const slashes = /\.(?:rs|tsx?|jsx?|mjs)$/.test(file);
  if (slashes && /^\s*(?:\/\/[/!]|\/\*\*|\*(?!\/))/.test(text)) return 1;
  if (slashes && /\b(?:it|test|describe)(?:\.\w+)?\(\s*['"`]/.test(text)) return 2;
  if (slashes && /^\s*(?:\/\/|\/\*)/.test(text)) return 3;
  if (/\.(?:ps1|ya?ml)$/.test(file) && /^\s*#/.test(text)) return 3;
  return null;
}

export function findClaims(added, { limit = 12, perFile = 2 } = {}) {
  const claims = added.flatMap(row => {
    const rank = proseRank(row);
    return rank !== null && CLAIM.test(row.text) ? [{ ...row, rank }] : [];
  }).sort((a, b) => a.rank - b.rank || a.file.localeCompare(b.file) || a.line - b.line);
  // One large document must not use every slot: take a couple per file first, then top up.
  const perFileCount = new Map();
  const first = claims.filter(claim => {
    const seen = perFileCount.get(claim.file) ?? 0;
    perFileCount.set(claim.file, seen + 1);
    return seen < perFile;
  });
  const rest = claims.filter(claim => !first.includes(claim));
  return { shown: [...first, ...rest].slice(0, limit), total: claims.length };
}

// Whitespace, wrapping and trailing commas are treated as layout. That is wrong inside a string
// literal, but one such hunk cannot reach the threshold that makes a commit worth flagging.
const layoutFree = lines => lines.join('').replace(/\s+/g, '').replace(/,(?=[)\]}])/g, '').replace(/[,;]$/, '');

function formatHunkStats(diff) {
  const files = new Map();
  let file = null;
  let hunk = null;
  const close = () => {
    if (!hunk || !file) return;
    const stat = files.get(file) ?? { formatOnly: 0, hunks: 0 };
    stat.hunks += 1;
    if (hunk.removed.length && hunk.added.length && layoutFree(hunk.removed) === layoutFree(hunk.added)) stat.formatOnly += 1;
    files.set(file, stat);
    hunk = null;
  };
  for (const raw of diff.split('\n')) {
    if (raw.startsWith('diff --git ')) { close(); file = raw.split(' b/').pop(); continue; }
    if (raw.startsWith('@@')) { close(); hunk = { removed: [], added: [] }; continue; }
    if (!hunk) continue;
    if (raw.startsWith('-') && !raw.startsWith('---')) hunk.removed.push(raw.slice(1));
    else if (raw.startsWith('+') && !raw.startsWith('+++')) hunk.added.push(raw.slice(1));
  }
  close();
  const total = [...files.values()].reduce((sum, stat) => ({ formatOnly: sum.formatOnly + stat.formatOnly, hunks: sum.hunks + stat.hunks }), { formatOnly: 0, hunks: 0 });
  const top = [...files].filter(([, stat]) => stat.formatOnly).sort((a, b) => b[1].formatOnly - a[1].formatOnly).slice(0, 3).map(([name, stat]) => `${name} ${stat.formatOnly}`);
  return { ...total, files: top };
}

// Commits since `base`, plus uncommitted work, that are mostly reformatting but not labelled as such.
export function findFormatChurn(cwd, base, { threshold = 10 } = {}) {
  const commit = resolveBase(cwd, base);
  const diffArgs = ['--no-renames', '--format=', '--unified=0', '--no-ext-diff', '--no-textconv'];
  const candidates = git(cwd, 'rev-list', '--reverse', '--no-merges', `${commit}..HEAD`).split('\n').filter(Boolean).map(sha => ({
    sha: sha.slice(0, 8),
    subject: git(cwd, 'log', '-1', '--format=%s', sha).trim(),
    diff: () => git(cwd, 'show', ...diffArgs, sha, '--', ...FORMATTABLE_FILES),
  }));
  candidates.push({ sha: 'uncommitted', subject: 'uncommitted changes', diff: () => git(cwd, 'diff', ...diffArgs, 'HEAD', '--', ...FORMATTABLE_FILES) });
  return candidates.filter(candidate => !FORMAT_COMMIT.test(candidate.subject)).flatMap(candidate => {
    const { formatOnly, hunks, files } = formatHunkStats(candidate.diff());
    return formatOnly >= threshold ? [{ sha: candidate.sha, subject: candidate.subject, formatOnly, hunks, files }] : [];
  });
}

// The first sentence of each numbered item in the checklist, as "1. Claims — ...".
export function checklistDigest(root = toolRoot) {
  const path = resolve(root, CHECKLIST);
  if (!existsSync(path)) return [];
  return readFileSync(path, 'utf8').split('\n').flatMap(line => {
    const item = /^(\d+)\. \*\*(.+?)\.\*\* (.+)$/.exec(line.replace(/\r$/, ''));
    return item ? [`${item[1]}. ${item[2]} — ${item[3].split(/\.(?:\s|$)/)[0]}`] : [];
  });
}

const clip = (text, length) => (text.length > length ? `${text.slice(0, length - 1)}…` : text);

export function selfReviewReport(cwd, base, { root = toolRoot, all = false, limit = 12, threshold = 10 } = {}) {
  const lines = [`Self-review prompts (advisory; these never fail a gate). Compared with ${resolveBase(cwd, base).slice(0, 8)}.`];
  const churn = findFormatChurn(cwd, base, { threshold });
  if (churn.length) {
    lines.push('', 'Formatting-only hunks inside behaviour commits. Split them into their own `style(...)` commit (behaviour first, then format) so the behaviour change stays reviewable:');
    for (const entry of churn) lines.push(`  ${entry.sha} ${clip(entry.subject, 60)}: ${entry.formatOnly} of ${entry.hunks} hunks only reformat (${entry.files.join(', ')})`);
  }
  const { shown, total } = findClaims(addedLines(cwd, base), { limit: all ? Infinity : limit });
  if (total) {
    lines.push('', `Absolute claims in added comments and docs (${shown.length} of ${total}${shown.length < total ? '; --all lists the rest' : ''}). Name the line or test that makes each true, or soften it:`);
    for (const claim of shown) lines.push(`  ${claim.file}:${claim.line}  ${clip(claim.text.trim(), 140)}`);
  }
  const digest = checklistDigest(root);
  lines.push('', `Before you commit (${CHECKLIST}):`, ...(digest.length ? digest.map(item => `  ${item}`) : ['  checklist missing']));
  return { text: lines.join('\n'), flagged: churn.length + total };
}

export function selfReview(cwd, base, options) {
  return selfReviewReport(cwd, base, options).text;
}

function defaultBase(cwd) {
  for (const candidate of ['origin/main', 'main']) {
    try { return git(cwd, 'merge-base', 'HEAD', candidate).trim(); } catch { /* try the next name */ }
  }
  throw new Error('no --base given and neither origin/main nor main exists');
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const args = process.argv.slice(2);
    const all = args.includes('--all');
    const rest = args.filter(arg => arg !== '--all');
    if (rest.length && (rest.length !== 2 || rest[0] !== '--base')) throw new Error('Usage: node scripts/self-review.mjs [--base <commit>] [--all]');
    const cwd = process.cwd();
    console.log(selfReview(cwd, rest[1] ?? defaultBase(cwd), { all }));
  } catch (error) {
    console.error(`Self-review failed: ${error.message.split('\n')[0]}`);
    process.exitCode = 1;
  }
}
