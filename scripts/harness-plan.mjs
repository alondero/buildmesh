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
  if (frontend) {
    npm('frontend-build', 'build', { minutes: 10 });
    npm('bundle', 'check:bundle');
    npm('frontend-tests', 'test', { tests: 'vitest', minutes: 10 });
    node('browser-smoke', ['node_modules/@playwright/test/cli.js', 'test', '--project=verify-smoke', '--reporter=line'], { tests: 'playwright', browser: true });
  } else if (rust) {
    npm('mobile-build', 'build:mobile', { minutes: 10 });
  }
  if (rust) {
    const cargo = (id, args, options = {}) => gates.push({ id, command: ['cargo', ...args, '--manifest-path', 'Cargo.toml'], cwd: 'src-tauri', minutes: 30, rust: true, ...options });
    cargo('rust-format', ['fmt', '--all', '--check']);
    cargo('rust-clippy', ['clippy', '--locked', '--all-targets', '--message-format=json'], { warnings: true });
    // All targets include the desktop binary (compile smoke); serialized tests
    // avoid the process-global DB races documented in CLAUDE.md.
    gates.push({ id: 'rust-tests', command: ['cargo', 'test', '--locked', '--', '--test-threads=1'], cwd: 'src-tauri', minutes: 30, rust: true, tests: 'rust' });
    node('binding-drift', ['scripts/harness.mjs', 'gate', 'bindings']);
  }
  return gates;
}

export function isHarnessPath(path) {
  return /^(?:scripts\/harness(?:-plan)?\.mjs|scripts\/harness-corpus\.json|scripts\/ci\/run-guarded\.mjs|tests\/agent-infra\/|\.claude\/|\.agents\/|\.github\/PULL_REQUEST_TEMPLATE\.md$)/.test(path);
}

export function executedTests(kind, output) {
  output = stripVTControlCharacters(output);
  if (kind === 'node') return Number(output.match(/(?:#|ℹ)\s+pass (\d+)/)?.[1] ?? 0);
  if (kind === 'vitest') return Number(output.match(/Tests\s+(\d+) passed/)?.[1] ?? 0);
  if (kind === 'playwright') return Number(output.match(/(\d+) passed(?:\s|\()/)?.[1] ?? 0);
  if (kind === 'rust') return [...output.matchAll(/test result: ok\. (\d+) passed/g)].reduce((sum, match) => sum + Number(match[1]), 0);
  return null;
}
