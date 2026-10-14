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

// The contract-test gates read far less of the tree than they used to claim, so
// they can be skipped when nothing they read has changed (issue #2221). Each
// list below was audited from the tests and the scripts they import; a gate that
// reads a path it does not list here is re-run too often, and a gate that lists
// a path it does read can reuse a stale PASS. Both are bugs, so the lists are
// conservative and `tests/agent-infra/harness.test.mjs` pins the mapping.
//
// - agent-tests reads the harness itself: `scripts/**`, `.claude/**`,
//   `.agents/**`, `.github/**` (workflows, actions, the pin allowlist, the
//   rustsec exceptions), the manifests, `tests/**` (harness.test.mjs reads every
//   `.ts/.tsx/.mjs` under tests/unit, tests/integration, tests/e2e and
//   tests/setup), and `docs/agents/self-review.md`, which self-review.test.mjs
//   digests. That last file is why `docs/` cannot be ignored wholesale.
// - docs-tests reads all of `docs/**` plus every `src-tauri/src/**/*.rs`
//   (git2-ownership), `package.json`, the generated bindings, and the existence
//   of every path a skill or Markdown link names — so it keeps reading the whole
//   tree and stays unconditional.
// - readme-tests reads exactly three files, so it skips on almost everything.
// - lint-tests and lint-fixtures share one list: the ESLint config, the two
//   deliberate hook-rule violation fixtures, and the checker itself.
//
// One rule cuts across all of them: a contract-test gate always reads its own
// implementation script and its own test file, because those two files *are*
// the gate. Auditing only the repo files a suite opens at runtime misses them —
// then a change to either skips the suite that would have caught it, and the
// gate reports PASS having never run (review of PR #2232).
const AGENT_TEST_IGNORES = [
  '^docs/(?!agents/self-review\\.md$)',
  '^src/',
  '^android/',
  '^src-tauri/(?!Cargo\\.(?:toml|lock)$|tauri\\.conf\\.json$)',
  '^(?:README|CONTEXT|CLAUDE|CONTRIBUTING|SECURITY|CODE_OF_CONDUCT)\\.md$',
  '^eslint\\.config\\.js$',
  '^tsconfig[^/]*\\.json$',
  '^(?:vite|vitest|playwright)[^/]*\\.config\\.ts$',
  '^index\\.html$',
];
const README_TEST_IGNORES = ['^(?!README\\.md$|src/types/generated/(?:Provider\\.ts|HarnessCapabilitiesTable\\.json)$|scripts/check-readme-drift\\.mjs$|tests/agent-infra/check-readme-drift\\.test\\.mjs$)'];
const LINT_FIXTURE_IGNORES = ['^(?!tests/lint-fixtures/|eslint\\.config\\.js$|scripts/check-eslint-fixtures\\.mjs$|tests/agent-infra/check-eslint-fixtures\\.test\\.mjs$)'];

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
  // A contract-test gate joins the plan only when a changed path is one of its
  // inputs. A gate with no `ignores` reads the whole tree, so `gateReads` is
  // true for every path and it stays unconditional (issue #2221). `--full` and
  // an empty diff run the whole plan rather than trusting a narrowed list.
  const scoped = (id, script, options = {}) => {
    const gate = { id, command: ['npm', 'run', script], minutes: 5, ...options };
    if (full || !paths.length || paths.some(path => gateReads(gate, path))) gates.push(gate);
  };
  node('whitespace', ['scripts/harness.mjs', 'gate', 'whitespace']);
  node('staged-content', ['scripts/harness.mjs', 'gate', 'staging']);
  node('agent-rules', ['scripts/check-agent-diff.mjs', '--base', '$BASE']);
  node('docs', ['scripts/check-docs.mjs', '--base', '$BASE']);
  node('readme', ['scripts/check-readme-drift.mjs']);
  node('process-spawns', ['scripts/check-process-spawn-discipline.mjs']);
  node('known-flakes', ['scripts/harness.mjs', 'gate', 'flakes']);
  scoped('agent-tests', 'test:agent', { tests: 'node', ignores: AGENT_TEST_IGNORES });
  scoped('docs-tests', 'test:docs', { tests: 'node' });
  scoped('readme-tests', 'test:readme', { tests: 'node', ignores: README_TEST_IGNORES });
  scoped('lint-tests', 'test:lint', { tests: 'node', ignores: LINT_FIXTURE_IGNORES });
  // ESLint already ignores docs/** and src-tauri/**; the Rust sources are not linted.
  npm('lint', 'lint', { ignores: [DOCS, '^src-tauri/'] });
  scoped('lint-fixtures', 'lint:fixtures', { ignores: LINT_FIXTURE_IGNORES });
  if (paths.some(path => path.startsWith('android/') || /^scripts\/check-android(?:-live)?\.mjs$/.test(path) || path === '.github/workflows/android.yml')) {
    node('android', ['scripts/check-android.mjs'], { minutes: 25, tests: 'android' });
  }
  // Everything above is quick and runs first, in order. The product gates split
  // into a frontend and a Rust lane that run concurrently (#2104), each keeping
  // its own order and fail-fast. `heavy` gates share a machine-wide slot limit.
  // A gate that produces the bundle under test declares `nodeEnv: 'production'`.
  // Vite resolves `process.env.NODE_ENV` when it builds, and a gate without
  // this property runs with the harness-wide `test` value, so an undeclared
  // build ships the React development runtime and `check:bundle` measures the
  // wrong artifact (issue #2013). The property travels with the gate rather
  // than living in a list of gate ids here, so a new build gate is visibly
  // undeclared instead of quietly defaulting to the test environment.
  if (frontend) {
    npm('frontend-build', 'build', { minutes: 10, ignores: FRONTEND_BUILD_IGNORES, lane: 'frontend', nodeEnv: 'production' });
    npm('bundle', 'check:bundle', { ignores: FRONTEND_BUILD_IGNORES, lane: 'frontend' });
    npm('frontend-tests', 'test', { tests: 'vitest', minutes: 10, ignores: FRONTEND_TEST_IGNORES, lane: 'frontend', heavy: true });
    node('browser-smoke', ['node_modules/@playwright/test/cli.js', 'test', '--project=verify-smoke', '--reporter=line'], { tests: 'playwright', browser: true, ignores: FRONTEND_BUILD_IGNORES, lane: 'frontend' });
  } else if (rust) {
    npm('mobile-build', 'build:mobile', { minutes: 10, ignores: FRONTEND_BUILD_IGNORES, lane: 'rust', nodeEnv: 'production' });
  }
  if (rust) {
    // `vite build` empties dist/ (including dist/mobile/), which the Rust crate
    // embeds, so compiling must not overlap the build. rust-format only parses.
    const built = frontend ? ['frontend-build'] : [];
    const cargo = (id, args, options = {}) => gates.push({ id, command: ['cargo', ...args, '--manifest-path', 'Cargo.toml'], cwd: 'src-tauri', minutes: 30, rust: true, ignores: RUST_IGNORES, lane: 'rust', ...options });
    // The crate is rustfmt-clean (#2022), so any diff here is a regression the
    // change under review introduced, not inherited debt.
    cargo('rust-format', ['fmt', '--all', '--check']);
    cargo('rust-clippy', ['clippy', '--locked', '--all-targets', '--message-format=json'], { warnings: true, after: built });
    // All targets include the desktop binary (compile smoke); isolated failures
    // are rerun after both lanes drain.
    gates.push({ id: 'rust-tests', command: ['node', 'scripts/rust-test-shards.mjs'], minutes: 30, rust: true, tests: 'rust', ignores: RUST_IGNORES, lane: 'rust', heavy: true, after: built });
    node('binding-drift', ['scripts/harness.mjs', 'gate', 'bindings'], { ignores: RUST_IGNORES, lane: 'rust' });
  }
  return gates;
}

// `cargo fmt --check` prints `Diff in <absolute path>:<line>:` per hunk, with a
// `\\?\` prefix on Windows. Returns the distinct repo-relative files rustfmt
// would rewrite, so a failure can name them; an empty list means rustfmt failed
// for some other reason and the log is the only evidence.
export function formatDiffPaths(output) {
  const relative = [...stripVTControlCharacters(output).matchAll(/^Diff in (.+?):\d+:\s*$/gm)]
    .map(match => {
      const absolute = match[1].replace(/^\\\\\?\\/, '').replaceAll('\\', '/');
      const at = absolute.lastIndexOf('src-tauri/');
      return at === -1 ? absolute : absolute.slice(at);
    });
  // One file usually produces many hunks; naming it once per hunk buries the rest.
  return [...new Set(relative)];
}

export function isHarnessPath(path) {
  return /^(?:scripts\/harness(?:-plan|-lanes|-check|-vitest-isolate)?\.mjs|scripts\/self-review\.mjs|scripts\/known-flakes\.json|scripts\/harness-corpus\.json|scripts\/ci\/run-guarded\.mjs|tests\/agent-infra\/|\.claude\/|\.agents\/|\.github\/PULL_REQUEST_TEMPLATE\.md$)/.test(path);
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
