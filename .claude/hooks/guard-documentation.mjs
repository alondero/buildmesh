#!/usr/bin/env node
// PreToolUse(Bash) guard: require a documentation decision before committing
// behavior-sensitive changes. It is deliberately commit-time rather than
// edit-time: the staged snapshot is the only reliable scope available here.
// Use `docs: none — <reason>` in the commit message when the change genuinely
// has no documentation impact. This is a prompt, not a substitute for CI's
// link and source-of-truth checks, and it fails open on git/parser errors.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

const SEGMENT_SEP = /&&|\|\||[;&|\n]/;
const GIT_COMMIT_RE = /^\s*git\s+(?:-[Cc]\s+\S+\s+|-\S+\s+)*commit\b/;
const GIT_ADD_RE = /^\s*git\s+(?:-[Cc]\s+\S+\s+|-\S+\s+)*(?:add|stage)\b/;

export function stripHeredocs(command) {
  return String(command ?? '').replace(
    /<<-?\s*(["']?)([A-Za-z_]\w*)\1[\s\S]*?(?:\n|^)[ \t]*\2(?=\s|$)/g,
    ' <<heredoc ',
  );
}

export function classifyCommitCommand(command) {
  const segments = stripHeredocs(command).split(SEGMENT_SEP);
  const commitIndex = segments.findIndex((segment) => GIT_COMMIT_RE.test(segment));
  if (commitIndex === -1) return { isCommit: false, isPlainCommit: false };
  const segment = segments[commitIndex];
  const hasAddBefore = segments.slice(0, commitIndex).some((part) => GIT_ADD_RE.test(part));
  const hasAutoStage = /\s--all\b/.test(segment) || /\s-[A-Za-z]*a[A-Za-z]*\b/.test(segment);
  const hasInlineStaging = hasAddBefore || hasAutoStage;
  return {
    isCommit: true,
    isPlainCommit: !hasInlineStaging && !/\s--allow-empty\b/.test(segment),
    hasAddBefore,
    hasAutoStage,
    hasAmend: /\s--amend\b/.test(segment),
  };
}

export function isDocumentationPath(path) {
  const normalised = String(path).replaceAll('\\', '/');
  return /^(?:docs\/.*\.md$|\.github\/.*\.md$|README\.md$|CONTRIBUTING\.md$|SECURITY\.md$|CODE_OF_CONDUCT\.md$|CHANGELOG\.md$)/i.test(normalised);
}

export function isBehaviorSensitivePath(path) {
  const normalised = String(path).replaceAll('\\', '/');
  if (isDocumentationPath(normalised)) return false;
  if (/^(?:tests\/|\.codex\/|src\/types\/generated\/)/i.test(normalised)) return false;
  if (/^(?:scripts\/check(?:\.ps1|-.*\.mjs)$|\.claude\/|\.github\/workflows\/|package(?:-lock)?\.json$|src-tauri\/(?:Cargo\.toml|tauri[^/]*\.json)$|(?:eslint|vite|tsconfig)[^/]*\.)/i.test(normalised)) return true;
  return /^(?:src\/|src-tauri\/src\/)/i.test(normalised);
}

export function hasDocumentationExemption(command) {
  const match = String(command ?? '').match(/docs:\s*none\b([\s\S]*)/i);
  return Boolean(match && /[A-Za-z0-9]/.test(match[1] ?? ''));
}

function commandContainsDocumentationPath(command) {
  return /(?:^|\s)(?:docs[\\/][^\s'\"]*|\.github[\\/][^\s'\"]+\.md|README\.md|CONTRIBUTING\.md|SECURITY\.md|CODE_OF_CONDUCT\.md|CHANGELOG\.md)(?:\s|$)/i.test(command);
}

export function decideDocumentation({ command, stagedFiles }) {
  const { isCommit, isPlainCommit, hasAddBefore, hasAutoStage, hasAmend } = classifyCommitCommand(command);
  const paths = stagedFiles ?? [];
  if (!isCommit || (!isPlainCommit && !hasAddBefore && !hasAmend && !hasAutoStage && paths.length === 0)) return null;

  const commandText = stripHeredocs(command);
  const inlineAdd = hasAddBefore
    ? commandText.split(SEGMENT_SEP).find((part) => GIT_ADD_RE.test(part)) ?? ''
    : '';
  const inlineAddsDocumentation = commandContainsDocumentationPath(inlineAdd);
  const inlineAddsBehavior = /(?:^|\s)(?:src[\\/]|src-tauri[\\/]src[\\/]|-A(?:\s|$)|--all(?:\s|$)|\.(?:\s|$))/i.test(inlineAdd);
  const sensitive = paths.filter(isBehaviorSensitivePath);
  if (hasAddBefore && !inlineAddsBehavior && !inlineAddsDocumentation) return null;
  if (hasAddBefore && inlineAddsBehavior && inlineAddsDocumentation) return null;
  if (hasAddBefore && inlineAddsBehavior) sensitive.push(`inline staging (${inlineAdd.trim()})`);
  if (hasAutoStage && sensitive.length === 0) sensitive.push('automatic staging (-a/--all)');
  if (sensitive.length === 0) return null;
  const hasDocumentation = paths.some(isDocumentationPath);
  if (hasDocumentation || hasDocumentationExemption(command)) return null;

  return {
    hookEventName: 'PreToolUse',
    permissionDecision: 'deny',
    permissionDecisionReason:
      'This commit contains behavior-sensitive source changes but no documentation decision. '
      + 'Stage the affected user/developer docs and CHANGELOG, or put `docs: none — <reason>` '
      + 'in the commit message when documentation is genuinely unnecessary. '
      + `Sensitive paths: ${sensitive.join(', ')}`,
  };
}

function readGitState(cwd) {
  const options = { cwd, encoding: 'utf8' };
  const staged = execFileSync('git', ['diff', '--staged', '--name-only', '-z'], options)
    .split('\0')
    .filter(Boolean);
  return { stagedFiles: staged };
}

function main() {
  let payload;
  try {
    const raw = readFileSync(0, 'utf8');
    if (!raw.trim()) return;
    payload = JSON.parse(raw);
  } catch {
    return;
  }
  if (payload?.tool_name !== 'Bash') return;
  try {
    const verdict = decideDocumentation({
      command: payload?.tool_input?.command ?? '',
      stagedFiles: readGitState(payload?.cwd || process.cwd()).stagedFiles,
    });
    if (verdict) process.stdout.write(JSON.stringify({ hookSpecificOutput: verdict }));
  } catch {
    // A hook must not wedge a user's shell because git or the payload changed.
  }
}

function isMain() {
  if (!process.argv[1]) return false;
  try {
    return resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (isMain()) main();
