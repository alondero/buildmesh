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

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
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

// The trees Vitest actually runs over: the sources under test, the suites
// themselves, the shared setup file, the brand assets a test reads, and the
// build config. `categorisePath` calls every `tests/` path 'frontend', so using
// it here started Vitest for `tests/agent-infra/**`, `tests/e2e/**` and
// `tests/lint-fixtures/**` and then selected nothing — the scoped check burned a
// dependency-graph walk to execute zero tests (review of PR #2232). Generated
// wire types live under `src/types/generated/`, so `src/` already covers them.
const FRONTEND_TEST_ROOTS = [
  'src/',
  'mobile/',
  'public/',
  'tests/unit/',
  'tests/integration/',
  'tests/setup/',
  'docs/brand/',
  'vitest.config.ts',
  'vite.config.ts',
];

// Whether a frontend test could plausibly be affected. Deliberately narrower
// than `categorisePath`, which also sends an unknown path to both product
// layers: this check is a fast loop, and starting Vitest for a diff that no
// frontend test imports only costs a dependency-graph walk. Such a change stays
// the full plan's business.
export function frontendInScope(paths) {
  return paths.some(path => FRONTEND_TEST_ROOTS.some(root => path.startsWith(root)));
}

// Rust test modules attached to another file with `#[path = "..."]` (issue
// #2221). A plain path walk maps `src/git/mesh_health_tests.rs` to
// `git::mesh_health_tests`, but the module is declared inside `git/health.rs`,
// so its tests run under `git::health::mesh_health_tests` and the naive filter
// would match nothing. Each entry maps the included file to the module path
// that actually owns it.
//
// Both the agent-infra test and the `rust-paths` gate call
// `pathIncludeDrift` below, so a new `#[path]` fails the gate that runs on the
// Rust diff itself, not only the one CI happens to run (review of PR #2232).
export const RUST_PATH_INCLUDES = {
  'src-tauri/src/git/trust_tests.rs': 'git::trust::tests',
  'src-tauri/src/db/circuit/review_configuration_tests.rs': 'db::circuit::review_configuration::tests',
  'src-tauri/src/agent/terminal_input.rs': 'agent::process::terminal_input',
  'src-tauri/src/agent/process_tests.rs': 'agent::process::tests',
  'src-tauri/src/agent/provider_menu_tests.rs': 'agent::provider_menu::tests',
  'src-tauri/src/agent/spawn_environment_tests.rs': 'agent::spawn_environment::tests',
  'src-tauri/src/agent/attention_hooks_tests.rs': 'agent::attention_hooks::tests',
  'src-tauri/src/agent/provider/adapters/codex_tests.rs': 'agent::provider::adapters::codex::tests',
  'src-tauri/src/agent/provider/adapters/grok_tests.rs': 'agent::provider::adapters::grok::provisioning_tests',
  'src-tauri/src/agent/provider/adapters/mcode_tests.rs': 'agent::provider::adapters::mcode::tests',
  'src-tauri/src/circuit/delivery_tests.rs': 'circuit::delivery::tests',
  'src-tauri/src/commands/git_status_bench.rs': 'commands::git_tests',
  'src-tauri/src/commands/git_status_counts_tests.rs': 'commands::git_tests::status_counts',
  'src-tauri/src/commands/pr_tests.rs': 'commands::pr::tests',
  'src-tauri/src/db/circuit/evidence_tests.rs': 'db::circuit::evidence::tests',
  'src-tauri/src/db/circuit/evidence/prompt_delivery_tests.rs': 'db::circuit::evidence::prompt_delivery::tests',
  'src-tauri/src/services/circuit_prompt_tests.rs': 'services::circuit_prompt::tests',
  'src-tauri/src/git/fetch_origin_tests.rs': 'git::sync::fetch_origin_tests',
  'src-tauri/src/git/fetch_freshness_tests.rs': 'git::fetch_freshness::tests',
  'src-tauri/src/git/mesh_health_tests.rs': 'git::health::mesh_health_tests',
  'src-tauri/src/git/sha_tests.rs': 'git::sha::tests',
  'src-tauri/src/git/sync_lock_tests.rs': 'git::sync_lock::tests',
  'src-tauri/src/git/worktree/provision_tests.rs': 'git::worktree::provision::tests',
  'src-tauri/src/git/worktree/warm_tests.rs': 'git::worktree::warm::tests',
  'src-tauri/src/git/worktree/worktree_tests.rs': 'git::worktree::tests',
  'src-tauri/src/commands/prune_tests.rs': 'commands::prune::prune_tests',
  'src-tauri/src/http/ws_tests.rs': 'http::ws::tests',
  'src-tauri/src/http/read_status_tests.rs': 'http::read_status::tests',
  'src-tauri/src/http/mobile_wire_tests.rs': 'http::mobile_wire::tests',
  'src-tauri/src/http/routes/circuit_tests.rs': 'http::routes::circuit::tests',
  'src-tauri/src/services/mcode_session_tests.rs': 'services::mcode_session::tests',
  'src-tauri/src/services/freebuff_usage_tests.rs': 'services::freebuff_usage::tests',
  'src-tauri/src/services/freebuff_daily_tests.rs': 'services::freebuff_usage::daily_tests',
  'src-tauri/src/services/agent_node_tests.rs': 'services::agent_node::tests',
  'src-tauri/src/services/agent_node_discovery_tests.rs': 'services::agent_node_discovery::tests',
  'src-tauri/src/services/circuit_worker/dispatch_tests.rs': 'services::circuit_worker::dispatch::tests',
  'src-tauri/src/services/circuit_worker/native_hooks_tests.rs': 'services::circuit_worker::native_hooks::tests',
  'src-tauri/src/services/legacy_retirement/retirement_crash_tests.rs': 'services::legacy_retirement::retirement_crash_tests',
  'src-tauri/src/services/pool_worker_tests.rs': 'services::pool_worker::tests',
  'src-tauri/src/services/transcript_reader/report_snapshot_tests.rs': 'services::transcript_reader::report_snapshot::tests',
  'src-tauri/src/services/warm_pool_tests.rs': 'services::warm_pool::tests',
};

// A Rust production file whose tests live in a sibling `<stem>_tests` module
// that the PARENT declares (`mod y_tests;`) rather than the file itself
// including (`#[path = "y_tests.rs"] mod tests;`).
//
// The path walk maps the production file to its own module, and that module
// holds no tests — a scoped `cargo test --lib` filter for it selects zero,
// which the strict zero-test rule reports as a failure that carries no
// information about the code. These entries point the filter at the module
// that holds the file's tests. A file that keeps BOTH an inline `mod tests`
// and a sibling keeps its own module, so its inline tests still run.
//
// `RUST_PATH_INCLUDES` covers the `#[path]` half of the same problem; the
// two tables cover different shapes, because a `#[path]`-included
// sibling is registered in that table and keeps its owner's filter.
//
// The entries below have the shape "the parent declares `mod <stem>_tests;`
// and the production file is that module's same-stem submodule". A production
// file that carries test items of its own needs no entry: a filter on its own
// module already selects them, and redirecting it would skip them.
export const RUST_SIBLING_TESTS = {
  'src-tauri/src/agent/spawn/command.rs': 'agent::spawn::command_tests',
  'src-tauri/src/agent/spawn/orchestrator.rs': 'agent::spawn::orchestrator_tests',
  'src-tauri/src/agent/spawn/preflight.rs': 'agent::spawn::preflight_tests',
  'src-tauri/src/agent/spawn/prepare.rs': 'agent::spawn::prepare_tests',
  'src-tauri/src/agent/spawn/provision.rs': 'agent::spawn::provision_tests',
  'src-tauri/src/agent/spawn/reader.rs': 'agent::spawn::reader_tests',
  'src-tauri/src/agent/spawn/streams.rs': 'agent::spawn::streams_tests',
  'src-tauri/src/db/circuit/operator_push.rs': 'db::circuit::operator_push_tests',
  'src-tauri/src/db/create_operation.rs': 'db::create_operation_tests',
  'src-tauri/src/db/mesh.rs': 'db::mesh_tests',
  'src-tauri/src/db/warm_pool.rs': 'db::warm_pool_tests',
  'src-tauri/src/services/github/pagination.rs': 'services::github::pagination_tests',
};

// Every `#[path = "..."]` module in the crate, as repo-relative paths. Entries
// in RUST_PATH_INCLUDES that no declaration backs are drift too: they would mask
// a file that stops being included.
export function pathIncludeDrift(root) {
  const declared = new Set();
  for (const file of execFileSync('git', ['ls-files', RUST_SRC], { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 }).split('\n')) {
    if (!file.endsWith('.rs')) continue;
    const source = readFileSync(join(root, file), 'utf8');
    for (const match of source.matchAll(/#\[path\s*=\s*"([^"]+)"\]/g)) {
      declared.add(join(file.replace(/\/[^/]+$/, ''), match[1]).replaceAll('\\', '/'));
    }
  }
  const missing = [...declared].filter(file => !RUST_PATH_INCLUDES[file]).sort();
  const stale = Object.keys(RUST_PATH_INCLUDES).filter(file => !declared.has(file)).sort();
  return { missing, stale };
}

// The `cargo test --lib` module prefix a changed Rust path belongs to, or null
// when no module filter can scope it (crate root, manifests, build script,
// integration tests, or anything outside `src-tauri/src`).
export function rustModule(path) {
  if (RUST_PATH_INCLUDES[path]) return RUST_PATH_INCLUDES[path];
  if (RUST_SIBLING_TESTS[path]) return RUST_SIBLING_TESTS[path];
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
//
// A path the diff DELETED is skipped: the file is gone, so the module it
// named is gone with it and no test can live under it any more. A file that
// moved is a deletion plus an addition, and the addition carries the filter
// for the module's new home — without this, each move would ask Cargo for
// tests under a module that has been deleted, and a filter that matches
// no test is reported as a failure rather than as the no-op it is.
export function rustScope(paths, { exists = () => true } = {}) {
  const modules = new Set();
  const unscoped = [];
  for (const path of paths) {
    // `categorisePath` resolves everything under src-tauri/ (and the manifests
    // beside it) to 'rust', so that is the whole Rust surface here.
    if (categorisePath(path) !== 'rust') continue;
    if (!exists(path)) continue;
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
  // agent-tests owns the #[path] drift guard but is skipped for a Rust-only
  // diff, which is exactly when a new #[path] is added. This runs the same
  // check on the Rust source the diff actually touches.
  if (paths.some(path => path.startsWith(RUST_SRC) && path.endsWith('.rs'))) {
    checks.push({ id: 'check-rust-paths', command: ['node', 'scripts/harness.mjs', 'gate', 'rust-paths'], minutes: 2 });
  }
  const rust = rustScope(paths, { exists });
  if (rust.whole) {
    checks.push({ id: 'check-rust-tests', command: ['cargo', 'test', '--lib'], cwd: 'src-tauri', rust: true, tests: 'rust', minutes: 30 });
  } else {
    rust.modules.forEach((module, index) => {
      checks.push({ id: `check-rust-${index + 1}`, command: ['cargo', 'test', '--lib', `${module}::`], cwd: 'src-tauri', rust: true, tests: 'rust', minutes: 30, module });
    });
  }
  return { checks, rust };
}
