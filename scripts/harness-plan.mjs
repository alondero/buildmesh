import { categorisePath } from './ci/changed-scope.mjs';
import { stripVTControlCharacters } from 'node:util';

// Input sets. A gate lists the paths it provably does NOT read (regex sources,
// so the plan stays JSON-serialisable); every other tracked or untracked path is
// an input, so a new or unknown path invalidates a reused PASS. A gate with no
// `ignores` reads the whole tree, which is the safe default.
//
// These lists come from auditing what each gate really reads, and several reads
// cross the obvious Rust/frontend boundary:
//  - Vitest reads a handful of src-tauri files (config JSON, Cargo.*, lib.rs,
//    the command and route sources, the opencode plugin) and docs/brand.
//  - Rust tests read src/ (generated bindings, vocabulary) and embed the built
//    dist/mobile, so frontend edits must re-run Rust. Rust never reads docs/,
//    android/ or the repo-root tests/ (its fixtures live in src-tauri/tests/).
//  - rust-tests and binding-drift MUST share one list: binding-drift only
//    checks what rust-tests regenerated, so it must never be reused alone.
const DOCS = '^docs/';
const FRONTEND_BUILD_IGNORES = [DOCS, '^src-tauri/'];
// A Vitest file that starts reading another src-tauri file must add it here
// (the drift guard in tests/agent-infra/harness.test.mjs flags new references).
export const VITEST_READS_FROM_SRC_TAURI = ['capabilities/', 'Cargo\\.', 'tauri[^/]*\\.json$', 'src/(?:commands/|http/routes/|lib\\.rs$|agent/provider/adapters/opencode_attention_plugin\\.js$)'];
const FRONTEND_TEST_IGNORES = ['^docs/(?!brand/)', `^src-tauri/(?!(?:${VITEST_READS_FROM_SRC_TAURI.join('|')}))`];
const RUST_IGNORES = [DOCS, '^android/', '^tests/'];
const NODE_CHECK_IGNORES = [DOCS];

// Whether `path` is an input of `gate`. Paths use forward slashes.
export function gateReads(gate, path) {
  if (!gate.ignores || gate.ignores.length === 0) return true;
  if (!gate._ignoreRegexes) {
    const compiled = gate.ignores.map(source => new RegExp(source));
    try {
      Object.defineProperty(gate, '_ignoreRegexes', { value: compiled, writable: true, enumerable: false });
    } catch {
      return !compiled.some(rx => rx.test(path));
    }
  }
  return !gate._ignoreRegexes.some(rx => rx.test(path));
}

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
  node('known-flakes', ['scripts/harness.mjs', 'gate', 'flakes']);
  npm('agent-tests', 'test:agent', { tests: 'node', ignores: NODE_CHECK_IGNORES });
  npm('docs-tests', 'test:docs', { tests: 'node' });
  npm('readme-tests', 'test:readme', { tests: 'node' });
  npm('lint-tests', 'test:lint', { tests: 'node', ignores: [DOCS, '^src-tauri/'] });
  // ESLint already ignores docs/** and src-tauri/**; the Rust sources are not linted.
  npm('lint', 'lint', { ignores: [DOCS, '^src-tauri/'] });
  npm('lint-fixtures', 'lint:fixtures', { ignores: [DOCS, '^src-tauri/'] });
  if (paths.some(path => path.startsWith('android/') || /^scripts\/check-android(?:-live)?\.mjs$/.test(path) || path === '.github/workflows/android.yml')) {
    node('android', ['scripts/check-android.mjs'], { minutes: 25, tests: 'android' });
  }
  // Everything above is quick and runs first, in order. The product gates split
  // into a frontend and a Rust lane that run concurrently (#2104), each keeping
  // its own order and fail-fast. `heavy` gates share a machine-wide slot limit.
  if (frontend) {
    npm('frontend-build', 'build', { minutes: 10, ignores: FRONTEND_BUILD_IGNORES, lane: 'frontend' });
    npm('bundle', 'check:bundle', { ignores: FRONTEND_BUILD_IGNORES, lane: 'frontend' });
    npm('frontend-tests', 'test', { tests: 'vitest', minutes: 10, ignores: FRONTEND_TEST_IGNORES, lane: 'frontend', heavy: true });
    node('browser-smoke', ['node_modules/@playwright/test/cli.js', 'test', '--project=verify-smoke', '--reporter=line'], { tests: 'playwright', browser: true, ignores: FRONTEND_BUILD_IGNORES, lane: 'frontend' });
  } else if (rust) {
    npm('mobile-build', 'build:mobile', { minutes: 10, ignores: FRONTEND_BUILD_IGNORES, lane: 'rust' });
  }
  if (rust) {
    // `vite build` empties dist/ (including dist/mobile/), which the Rust crate
    // embeds, so compiling must not overlap the build. rust-format only parses.
    const built = frontend ? ['frontend-build'] : [];
    const cargo = (id, args, options = {}) => gates.push({ id, command: ['cargo', ...args, '--manifest-path', 'Cargo.toml'], cwd: 'src-tauri', minutes: 30, rust: true, ignores: RUST_IGNORES, lane: 'rust', ...options });
    // The crate has a formatting backlog (#2022); like Clippy, only diffs in
    // touched files fail, and the remaining count stays visible.
    cargo('rust-format', ['fmt', '--all', '--check'], { touchedFormat: true });
    cargo('rust-clippy', ['clippy', '--locked', '--all-targets', '--message-format=json'], { warnings: true, after: built });
    // All targets include the desktop binary (compile smoke); isolated failures
    // are rerun after both lanes drain.
    gates.push({ id: 'rust-tests', command: ['node', 'scripts/rust-test-shards.mjs'], minutes: 30, rust: true, tests: 'rust', ignores: RUST_IGNORES, lane: 'rust', heavy: true, after: built });
    node('binding-drift', ['scripts/harness.mjs', 'gate', 'bindings'], { ignores: RUST_IGNORES, lane: 'rust' });
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
  return /^(?:scripts\/harness(?:-plan|-lanes|-vitest-isolate)?\.mjs|scripts\/known-flakes\.json|scripts\/harness-corpus\.json|scripts\/ci\/run-guarded\.mjs|tests\/agent-infra\/|\.claude\/|\.agents\/|\.github\/PULL_REQUEST_TEMPLATE\.md$)/.test(path);
}

export function executedTests(kind, output) {
  output = stripVTControlCharacters(output);
  if (kind === 'node') return Number(output.match(/(?:#|ℹ)\s+pass (\d+)/)?.[1] ?? 0);
  if (kind === 'android') return Number(output.match(/Android tests: (\d+) passed/)?.[1] ?? 0);
  if (kind === 'vitest') return Number(output.match(/Tests\s+(?:\d+ failed\s*\|\s*)?(\d+) passed/)?.[1] ?? 0);
  if (kind === 'playwright') return Number(output.match(/(\d+) passed(?:\s|\()/)?.[1] ?? 0);
  if (kind === 'rust') return [...output.matchAll(/test result: ok\. (\d+) passed/g)].reduce((sum, match) => sum + Number(match[1]), 0);
  return null;
}
