#!/usr/bin/env node
// changed-scope.mjs — classify the files a pull request changed into the two
// verification graphs the merge gate fans out into, and emit the result as
// GitHub Actions job outputs.
//
// Motivation (the "hours to mergeable" investigation): every pull request ran
// the full fan-out — one Rust compile, seven Rust shards, the bindings pass,
// and the frontend suites — even when the change touched one side only. The
// merge gate skips whole graphs from this job's outputs, so a Rust-only pull
// request no longer boots Chromium or runs vitest, a frontend-only pull
// request no longer compiles Rust, and a docs-only pull request runs neither.
// The static gates are the exception and always run: they live in
// `Quality gates (Linux)`, and the required `Quality (Linux)` aggregate fails
// when they do, so a docs-only pull request still cannot merge on a lint,
// docs, or bundle-budget failure.
//
// Classification is deliberately conservative: a path that is not clearly
// documentation and not clearly one graph runs BOTH graphs. Under-testing is
// a worse failure than a wasted runner-minute.
//
// Usage:
//   node scripts/ci/changed-scope.mjs --base <sha> [--paths-file <file>]
//
//   --base <sha>     diff this revision against HEAD (three-dot, so the
//                    merge base is used); omitted means "unknown scope" and
//                    both graphs are reported as true.
//   --paths-file     classify newline-separated paths from a file instead of
//                    running git (used by tests).
//
// Writes `rust=<bool>` and `frontend=<bool>` to $GITHUB_OUTPUT when set, and
// always prints a one-line summary.

import { spawnSync } from 'node:child_process';
import { appendFileSync, readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const USAGE = `Usage: node scripts/ci/changed-scope.mjs [--base <commit>] [--paths-file <file>]`;

const RUST_PREFIXES = ['src-tauri/'];
const RUST_FILES = /^(?:Cargo\.(?:toml|lock)|rust-toolchain(?:\.toml)?)$/;
const FRONTEND_PREFIXES = ['src/', 'mobile/', 'public/', 'tests/'];
const FRONTEND_FILES = /^(?:index\.html|package(?:-lock)?\.json|vite\.config\.ts|vitest\.config\.ts|playwright\.config(?:\.[a-z]+)?\.ts|tsconfig(?:\.[a-z]+)?\.json|eslint\.config\.js)$/;
const DOCS_PREFIXES = ['docs/', '.claude/', '.agents/', '.opencode/'];
const DOCS_FILES = /^(?:[^/]+\.md|LICENSE|NOTICE(?:\.[^/]+)?|\.gitignore|\.gitattributes)$/;

// Returns 'rust' | 'frontend' | 'both' | 'docs' for one repository path.
export function categorisePath(rawPath) {
  const path = String(rawPath).replaceAll('\\', '/').replace(/^\.\//, '').trim();
  if (path === '') return 'docs';
  if (DOCS_PREFIXES.some((prefix) => path.startsWith(prefix)) || DOCS_FILES.test(path)) return 'docs';
  if (RUST_PREFIXES.some((prefix) => path.startsWith(prefix)) || RUST_FILES.test(path)) return 'rust';
  // Generated wire types live under src/ but are produced by ts-rs during
  // `cargo test` and drift-gated only by the rust-bindings job; classifying
  // them as frontend alone would let a hand-edited binding through while
  // every Rust check reports green-by-skip (review of PR #1991).
  if (path.startsWith('src/types/generated/')) return 'both';
  // Native checks run in the Android workflow/harness; bundled terminal assets use the frontend dependencies.
  if (path.startsWith('android/') || /^scripts\/check-android(?:-live)?\.mjs$/.test(path) || path === '.github/workflows/android.yml') return 'frontend';
  if (FRONTEND_PREFIXES.some((prefix) => path.startsWith(prefix)) || FRONTEND_FILES.test(path)) return 'frontend';
  // .github/, scripts/, and anything unknown: gate-relevant, run everything.
  return 'both';
}

export function classifyPaths(paths) {
  let rust = false;
  let frontend = false;
  for (const path of paths) {
    const category = categorisePath(path);
    if (category === 'rust') rust = true;
    else if (category === 'frontend') frontend = true;
    else if (category === 'both') { rust = true; frontend = true; }
  }
  return { rust, frontend };
}

function parseArgs(argv) {
  let base = null;
  let pathsFile = null;
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const next = argv[i + 1];
    if (flag === '--base' || flag === '--paths-file') {
      if (next === undefined) throw new Error(`Missing value for ${flag}.\n${USAGE}`);
      i += 1;
      if (flag === '--base') base = next;
      else pathsFile = next;
    } else {
      throw new Error(`Unknown option: ${flag}\n${USAGE}`);
    }
  }
  return { base, pathsFile };
}

function changedPaths(base) {
  const result = spawnSync('git', ['diff', '--name-only', `${base}...HEAD`], { encoding: 'utf8' });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`git diff --name-only ${base}...HEAD exited ${result.status}: ${result.stderr.trim()}`);
  }
  return result.stdout.split('\n').filter((line) => line.trim() !== '');
}

function emit({ rust, frontend }, pathCount) {
  const summary = `rust=${rust} frontend=${frontend} (${pathCount} paths classified)`;
  console.log(`changed-scope: ${summary}`);
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `rust=${rust}\nfrontend=${frontend}\n`);
  }
}

function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (err) {
    process.stderr.write(`${err.message}\n`);
    process.exitCode = 2;
    return;
  }

  if (options.pathsFile) {
    const paths = readFileSync(options.pathsFile, 'utf8').split('\n').filter((line) => line.trim() !== '');
    emit(classifyPaths(paths), paths.length);
    return;
  }
  if (!options.base) {
    // Pushes, schedules, and dispatches run the full gate: only pull requests
    // are narrowed by their diff.
    emit({ rust: true, frontend: true }, 0);
    return;
  }
  const paths = changedPaths(options.base);
  emit(classifyPaths(paths), paths.length);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
