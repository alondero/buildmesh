// harness-check.mjs — the scoped check every harness can run.
//
// `npm run verify` is the completion gate and stays the full plan. This module
// is the fast, harness-independent sibling: it maps the changed files to the
// checks that can actually exercise them, so a Codex (or any non-Claude) session
// gets real behavioural and Rust coverage from the diff instead of running
// focused commands by hand. See docs/agents/development-harness.md.
//
// It is deliberately advisory. A scoped check that passes is not completion
// evidence; `npm run verify` still runs the full plan (issue #2221).

import { categorisePath } from './ci/changed-scope.mjs';

const RUST_SRC = 'src-tauri/src/';

// Lintable source that ESLint actually checks. Generated bindings are produced
// by `cargo test` and fixtures must keep violating the hooks rules, so neither
// is linted here.
export function lintablePaths(paths, exists = () => true) {
  return paths.filter(path =>
    /^(?:src|tests|scripts)\/.+\.(?:tsx?|m?js)$/.test(path) &&
    !path.startsWith('src/types/generated/') &&
    !path.startsWith('tests/lint-fixtures/') &&
    exists(path));
}

// The fast gates the Claude PostToolUse hook already runs. The hook and
// `harness -- check` share this table so the two cannot drift apart.
export function fastGates(paths, { exists = () => true } = {}) {
  const gates = [{ id: 'fast-agent-rules', command: ['node', 'scripts/check-agent-diff.mjs', '--base', '$BASE'], minutes: 1 }];
  const lintable = lintablePaths(paths, exists);
  if (lintable.length) gates.push({ id: 'fast-lint', command: ['node', 'node_modules/eslint/bin/eslint.js', '--max-warnings', '0', ...lintable], minutes: 2 });
  if (paths.some(path => /^src\/.+\.tsx?$/.test(path))) gates.push({ id: 'fast-types', command: ['node', 'node_modules/typescript/bin/tsc', '--noEmit'], minutes: 2 });
  return gates;
}

// Whether a frontend test could plausibly be affected. Deliberately narrower
// than `categorisePath`, which sends an unknown path to both product layers:
// this check is for a fast loop, and starting Vitest for a diff that no
// frontend test imports only costs a dependency-graph walk. Such a change stays
// the full plan's business. Generated wire types are spelled out because
// `categorisePath` calls them 'both' (ts-rs writes them during `cargo test`).
export function frontendInScope(paths) {
  return paths.some(path => {
    const category = categorisePath(path);
    return category === 'frontend' || path.startsWith('src/types/generated/');
  });
}

// Rust test modules attached to another file with `#[path = "..."]` (issue
// #2221). A plain path walk maps `src/git/mesh_health_tests.rs` to
// `git::mesh_health_tests`, but the module is declared inside `git/health.rs`,
// so its tests run under `git::health::mesh_health_tests` and the naive filter
// would match nothing. Each entry maps the included file to the module path
// that actually owns it. `tests/agent-infra/harness.test.mjs` pins this table
// against every `#[path]` declaration in the crate, so a new one fails loudly
// instead of silently selecting zero tests.
export const RUST_PATH_INCLUDES = {
  'src-tauri/src/agent/terminal_input.rs': 'agent::process::terminal_input',
  'src-tauri/src/git/fetch_origin_tests.rs': 'git::sync::fetch_origin_tests',
  'src-tauri/src/git/mesh_health_tests.rs': 'git::health::mesh_health_tests',
  'src-tauri/src/commands/prune_tests.rs': 'commands::prune::prune_tests',
  'src-tauri/src/services/legacy_retirement/retirement_crash_tests.rs': 'services::legacy_retirement::retirement_crash_tests',
};

// The `cargo test --lib` module prefix a changed Rust path belongs to, or null
// when no module filter can scope it (crate root, manifests, build script,
// integration tests, or anything outside `src-tauri/src`).
export function rustModule(path) {
  if (RUST_PATH_INCLUDES[path]) return RUST_PATH_INCLUDES[path];
  if (!path.startsWith(RUST_SRC)) return null;
  const rest = path.slice(RUST_SRC.length);
  // The crate root and the binary entry can reach every module.
  if (rest === 'lib.rs' || rest === 'main.rs' || !rest.endsWith('.rs')) return null;
  const parts = rest.slice(0, -'.rs'.length).split('/');
  if (parts[parts.length - 1] === 'mod') parts.pop();
  return parts.length ? parts.join('::') : null;
}

// Split changed paths into the Rust modules a `--lib` filter can select, and
// the ones it cannot: the crate root, a manifest, the build script, or an
// integration-test binary, which `cargo test --lib` does not run.
export function rustScope(paths) {
  const modules = new Set();
  const unscoped = [];
  for (const path of paths) {
    // `categorisePath` resolves everything under src-tauri/ (and the manifests
    // beside it) to 'rust', so that is the whole Rust surface here.
    if (categorisePath(path) !== 'rust') continue;
    const module = rustModule(path);
    if (module) modules.add(module);
    else unscoped.push(path);
  }
  return {
    modules: [...modules].sort(),
    // Any Rust path that no module filter can select — the crate root, a
    // manifest, the build script, an integration test — can change anything the
    // lib suite covers, so it never narrows to a module filter.
    whole: unscoped.length > 0,
    unscoped: unscoped.sort(),
  };
}

// Every scoped check for a diff, in run order. `$BASE` is substituted by the
// gate runner, so the plan stays a pure function of the changed paths. These
// gates declare no `ignores`: that list exists to key a cached PASS in
// `verify`, and `check` re-runs everything it plans, so the field would be inert.
// `vitest --changed` selects by what the diff touches, so an unrelated change
// legitimately matches nothing: `--passWithNoTests` lets the process succeed and
// `emptyOk` lets the classifier report that honestly instead of as a failure. The
// executed count is still printed, and zero executed tests is not evidence. The
// Rust module checks deliberately keep the strict zero-test rule — a filter that
// matched nothing there usually means the module mapping is wrong.
export function planChecks(paths, { exists = () => true } = {}) {
  const checks = fastGates(paths, { exists });
  if (frontendInScope(paths)) {
    checks.push({ id: 'check-frontend-tests', command: ['node', 'node_modules/vitest/vitest.mjs', 'run', '--changed', '$BASE', '--passWithNoTests'], tests: 'vitest', minutes: 10, emptyOk: true });
  }
  const rust = rustScope(paths);
  if (rust.whole) {
    checks.push({ id: 'check-rust-tests', command: ['cargo', 'test', '--lib'], cwd: 'src-tauri', rust: true, tests: 'rust', minutes: 30 });
  } else {
    rust.modules.forEach((module, index) => {
      checks.push({ id: `check-rust-${index + 1}`, command: ['cargo', 'test', '--lib', `${module}::`], cwd: 'src-tauri', rust: true, tests: 'rust', minutes: 30, module });
    });
  }
  return { checks, rust };
}