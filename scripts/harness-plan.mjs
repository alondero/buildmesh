import { categorisePath } from './ci/changed-scope.mjs';
import { stripVTControlCharacters } from 'node:util';

// A single local verification plan. Unknown inputs retain CI's conservative scope.
export function planGates(paths, { full = false } = {}) {
  let frontend = full;
  let rust = full;
  for (const path of paths) {
    if (isHarnessPath(path)) continue;
    const category = categorisePath(path);
    frontend ||= category === 'frontend' || category === 'both';
    rust ||= category === 'rust' || category === 'both';
  }
  const gates = [];
  const node = (id, args, options = {}) => gates.push({ id, command: ['node', ...args], minutes: 5, ...options });
  const npm = (id, script, options = {}) => gates.push({ id, command: ['npm', 'run', script], minutes: 5, ...options });
  node('whitespace', ['scripts/harness.mjs', 'gate', 'whitespace']);
  node('staged-content', ['scripts/harness.mjs', 'gate', 'staging']);
  node('agent-rules', ['scripts/check-agent-diff.mjs', '--base', '$BASE']);
  node('docs', ['scripts/check-docs.mjs', '--base', '$BASE']);
  node('readme', ['scripts/check-readme-drift.mjs']);
  node('process-spawns', ['scripts/check-process-spawn-discipline.mjs']);
  npm('agent-tests', 'test:agent', { tests: 'node' });
  npm('docs-tests', 'test:docs', { tests: 'node' });
  npm('readme-tests', 'test:readme', { tests: 'node' });
  npm('lint-tests', 'test:lint', { tests: 'node' });
  npm('lint', 'lint');
  npm('lint-fixtures', 'lint:fixtures');
  if (paths.some(path => path.startsWith('android/') || /^scripts\/check-android(?:-live)?\.mjs$/.test(path) || path === '.github/workflows/android.yml')) {
    node('android', ['scripts/check-android.mjs'], { minutes: 25, tests: 'android' });
  }
  // Everything above is quick and runs first, in order. The product gates split
  // into a frontend and a Rust lane that run concurrently (#2104), each keeping
  // its own order and fail-fast. `heavy` gates share a machine-wide slot limit.
  if (frontend) {
    npm('frontend-build', 'build', { minutes: 10, lane: 'frontend' });
    npm('bundle', 'check:bundle', { lane: 'frontend' });
    npm('frontend-tests', 'test', { tests: 'vitest', minutes: 10, lane: 'frontend', heavy: true });
    node('browser-smoke', ['node_modules/@playwright/test/cli.js', 'test', '--project=verify-smoke', '--reporter=line'], { tests: 'playwright', browser: true, lane: 'frontend' });
  } else if (rust) {
    npm('mobile-build', 'build:mobile', { minutes: 10, lane: 'rust' });
  }
  if (rust) {
    // `vite build` empties dist/ (including dist/mobile/), which the Rust crate
    // embeds, so compiling must not overlap the build. rust-format only parses.
    const built = frontend ? ['frontend-build'] : [];
    const cargo = (id, args, options = {}) => gates.push({ id, command: ['cargo', ...args, '--manifest-path', 'Cargo.toml'], cwd: 'src-tauri', minutes: 30, rust: true, lane: 'rust', ...options });
    // The crate has a formatting backlog (#2022); like Clippy, only diffs in
    // touched files fail, and the remaining count stays visible.
    cargo('rust-format', ['fmt', '--all', '--check'], { touchedFormat: true });
    cargo('rust-clippy', ['clippy', '--locked', '--all-targets', '--message-format=json'], { warnings: true, after: built });
    // All targets include the desktop binary (compile smoke). Tests stay
    // serial within a process (process-global DB, see CLAUDE.md), but the CI
    // shards run as concurrent processes so the suite is not single-core.
    gates.push({ id: 'rust-tests', command: ['node', 'scripts/rust-test-shards.mjs'], minutes: 30, rust: true, tests: 'rust', lane: 'rust', heavy: true, after: built });
    node('binding-drift', ['scripts/harness.mjs', 'gate', 'bindings'], { lane: 'rust' });
  }
  return gates;
}

// `cargo fmt --check` prints `Diff in <absolute path>:<line>:` per hunk, with a
// `\\?\` prefix on Windows. Returns the touched paths that have a diff and the
// total hunk count; a total of 0 means rustfmt failed for another reason.
export function touchedFormatDiffs(output, paths) {
  const files = [...stripVTControlCharacters(output).matchAll(/^Diff in (.+?):\d+:\s*$/gm)]
    .map(match => match[1].replace(/^\\\\\?\\/, '').replaceAll('\\', '/').toLowerCase());
  const touched = paths.filter(path => files.some(file => file.endsWith(`/${path.toLowerCase()}`)));
  return { touched, total: files.length };
}

export function isHarnessPath(path) {
  return /^(?:scripts\/harness(?:-plan|-lanes)?\.mjs|scripts\/harness-corpus\.json|scripts\/ci\/run-guarded\.mjs|tests\/agent-infra\/|\.claude\/|\.agents\/|\.github\/PULL_REQUEST_TEMPLATE\.md$)/.test(path);
}

export function executedTests(kind, output) {
  output = stripVTControlCharacters(output);
  if (kind === 'node') return Number(output.match(/(?:#|ℹ)\s+pass (\d+)/)?.[1] ?? 0);
  if (kind === 'android') return Number(output.match(/Android tests: (\d+) passed/)?.[1] ?? 0);
  if (kind === 'vitest') return Number(output.match(/Tests\s+(\d+) passed/)?.[1] ?? 0);
  if (kind === 'playwright') return Number(output.match(/(\d+) passed(?:\s|\()/)?.[1] ?? 0);
  if (kind === 'rust') return [...output.matchAll(/test result: ok\. (\d+) passed/g)].reduce((sum, match) => sum + Number(match[1]), 0);
  return null;
}
