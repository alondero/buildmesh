#!/usr/bin/env node
// Guard the GitHub PR creation paths (issue #2236).
//
// `commands/pr.rs` used to own repository identity and three create-PR
// paths. Two of them derived their source from the *project root* — the
// branch the mesh checkout happened to be sitting on — which is how a
// `main -> main` PR (or a PR from an unrelated feature branch) got
// published while the sheet showed the agent's own branch (issue #2024
// rank 4 / #1567).
//
// The guard pins four things:
//
//   1. No production module under `services/` or `circuit/` reaches the PR
//      command layer. A service that imports `commands::pr` depends upward
//      on the boundary that is supposed to be above it, and the step is
//      invisible: the call compiles and the workflow it skipped is simply
//      missing.
//   2. The project-root create-PR commands stay removed. `createPr` and
//      `createPrForMesh` had no callers, so they were deleted rather than
//      left as a way to publish the wrong branch again.
//   3. Every production function that calls `create_pull_request_idempotent`
//      resolves its source through a named resolver, and never through
//      `repo_info` — the read that answers "which branch is checked out at
//      this path". That is the read that produced the bug.
//   4. The node-scoped resolver reads the agent node's worktree, not the
//      mesh's root path.
//
// Usage:
//   node scripts/check-pr-creation-source-ownership.mjs
//
// Exit codes: 0 the guard holds; 1 it does not.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { scanRust, stripCfgTestItems } from '../tests/agent-infra/git2-ownership.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const RUST_SRC = 'src-tauri/src';

/**
 * The directories whose modules must reach the GitHub workflows through
 * `services/github/` rather than through the command layer.
 */
export const GUARDED_DIRS = ['services', 'circuit'];

/**
 * The source resolvers a create-PR path may use.
 *
 * Each entry names where the PR's `head` branch comes from, and each is
 * either an agent node's worktree or a branch the path created on purpose.
 * A new create-PR path has to name its resolver here, which is the point:
 * the choice becomes explicit instead of defaulting to "the branch the
 * project root is sitting on".
 */
export const SOURCE_RESOLVERS = [
  {
    name: 'resolve_pr_source_for_node',
    why: 'the agent node worktree branch, with the mesh base_ref as the target',
  },
  {
    name: 'observe_wrapup_git_state',
    why: 'the agent node branch observed at circuit wrap-up',
  },
  {
    name: 'build_portability_commit',
    why: 'a branch the portability flow creates and pushes itself',
  },
];

/** The command that creates a pull request on the client. */
const PR_CREATE_CALL = /(?:\.|::)\s*create_pull_request_idempotent\s*\(/;

/** The read that answers "which branch is checked out at this path". */
const PROJECT_ROOT_BRANCH_READ = /(?:\.|::|\b)repo_info\s*\(/;

/**
 * Whether a function body creates a pull request.
 *
 * A call needs a receiver — `client.create_pull_request_idempotent(…)` or
 * `GitHubClient::create_pull_request_idempotent(…)` — so the client's own
 * definition of the method is not a creation site.
 */
export function isPrCreateCall(body) {
  return PR_CREATE_CALL.test(body);
}

/**
 * Replace comments with spaces so a doc comment does not read as a call.
 * Re-exported from the circuit ownership guard so both read a Rust file
 * the same way.
 */
export function stripComments(source) {
  return scanRust(source).code;
}

/**
 * The functions a Rust source declares, with their bodies.
 *
 * A hand-written scan rather than a regex over the whole file: the guard
 * must know which function a call sits in, and a closure inside a function
 * belongs to that function.
 */
export function rustFunctions(code) {
  const functions = [];
  const declaration = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^>]*>)?\s*\(/g;
  let match;
  while ((match = declaration.exec(code)) !== null) {
    const name = match[1];
    let index = declaration.lastIndex;
    // The match already consumed the opening paren, so the scan starts
    // inside the parameter list.
    let parens = 1;
    let bodyStart = -1;
    while (index < code.length) {
      const ch = code[index];
      if (ch === '(') parens += 1;
      else if (ch === ')') parens -= 1;
      else if (ch === '{' && parens === 0) {
        bodyStart = index;
        break;
      } else if (ch === ';' && parens === 0) break;
      index += 1;
    }
    if (bodyStart === -1) continue;
    let braces = 0;
    let end = bodyStart;
    for (; end < code.length; end += 1) {
      if (code[end] === '{') braces += 1;
      else if (code[end] === '}') {
        braces -= 1;
        if (braces === 0) {
          end += 1;
          break;
        }
      }
    }
    functions.push({ name, body: code.slice(bodyStart, end), index: match.index });
    declaration.lastIndex = end;
  }
  return functions;
}

/**
 * The names of the `#[command]` functions a module declares.
 */
export function commandNames(source) {
  const stripped = stripComments(source);
  const names = new Set();
  const attribute = /#\s*\[(?:tauri::)?command(?:\([^)]*\))?\]/g;
  const after = /(?:pub(?:\s*\(\s*crate\s*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/;
  let match;
  while ((match = attribute.exec(stripped)) !== null) {
    const fn = after.exec(stripped.slice(match.index + match[0].length));
    if (fn) names.add(fn[1]);
    attribute.lastIndex = match.index + match[0].length;
  }
  return names;
}

function listRustFiles(dir) {
  const out = [];
  if (!fs.existsSync(dir)) return out;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listRustFiles(full));
    else if (entry.name.endsWith('.rs')) out.push(full);
  }
  return out;
}

function isTestOnlyFile(rel) {
  return rel.endsWith('_tests.rs') || rel.endsWith('/tests.rs') || rel === 'tests.rs';
}

/** The production Rust files under `src-tauri/src`, relative to it. */
function productionRustFiles(root) {
  const src = path.join(root, ...RUST_SRC.split('/'));
  return listRustFiles(src)
    .map((full) => path.relative(src, full).split(path.sep).join('/'))
    .filter((rel) => !isTestOnlyFile(rel));
}

/**
 * The production modules that reach the PR command layer, each as
 * `path:line` with the reference that reached it.
 */
export function commandLayerReaches(root = repoRoot) {
  const reaches = [];
  for (const dir of GUARDED_DIRS) {
    for (const rel of productionRustFiles(root).filter((file) => file.startsWith(`${dir}/`))) {
      const source = stripCfgTestItems(
        fs.readFileSync(path.join(root, ...RUST_SRC.split('/'), ...rel.split('/')), 'utf8'),
      );
      const { code } = scanRust(source);
      const pattern = /(?<![:\w])(?:crate::)?commands::pr::/g;
      let match;
      while ((match = pattern.exec(code)) !== null) {
        const line = code.slice(0, match.index).split('\n').length;
        reaches.push(`${rel}:${line}: reaches commands::pr`);
      }
    }
  }
  return reaches.sort();
}

/**
 * The create-PR paths that do not resolve their source through a named
 * resolver, or that read the branch checked out at a path directly.
 */
export function sourceOwnershipViolations(root = repoRoot) {
  const violations = [];
  const resolvers = SOURCE_RESOLVERS.map((resolver) => resolver.name);
  for (const rel of productionRustFiles(root)) {
    const source = stripCfgTestItems(
      fs.readFileSync(path.join(root, ...RUST_SRC.split('/'), ...rel.split('/')), 'utf8'),
    );
    const { code } = scanRust(source);
    for (const fn of rustFunctions(code)) {
      if (!isPrCreateCall(fn.body)) continue;
      const used = resolvers.filter((resolver) => fn.body.includes(resolver));
      if (used.length === 0) {
        violations.push(
          `${rel}: ${fn.name} creates a pull request but names no source resolver ` +
            `(${resolvers.join(', ')}) — it must say where the head branch comes from.`,
        );
      }
      if (PROJECT_ROOT_BRANCH_READ.test(fn.body)) {
        violations.push(
          `${rel}: ${fn.name} creates a pull request and reads a path's checked-out ` +
            'branch (`repo_info`) — the head must come from the agent node or a branch ' +
            'the path created itself.',
        );
      }
    }
  }
  return violations.sort();
}

/**
 * The node-scoped resolver must read the agent node's worktree, not the
 * mesh's root path.
 */
export function nodeResolverViolations(root = repoRoot) {
  const file = path.join(root, ...RUST_SRC.split('/'), 'services', 'github', 'pr_workflow.rs');
  if (!fs.existsSync(file)) {
    return ['services/github/pr_workflow.rs is missing; the guard cannot prove anything'];
  }
  const source = fs.readFileSync(file, 'utf8');
  const { code } = scanRust(source);
  const resolver = rustFunctions(code).find((fn) => fn.name === 'resolve_pr_source_for_node');
  if (!resolver) {
    return ['services/github/pr_workflow.rs no longer declares resolve_pr_source_for_node'];
  }
  const violations = [];
  if (!resolver.body.includes('node_git_path')) {
    violations.push(
      'resolve_pr_source_for_node no longer reads the agent node worktree (`node_git_path`)',
    );
  }
  if (/mesh\s*\.\s*path/.test(resolver.body)) {
    violations.push(
      'resolve_pr_source_for_node reads the mesh root path (`mesh.path`) — the source must ' +
        'come from the agent node worktree',
    );
  }
  return violations;
}

/**
 * The removed project-root create-PR commands must stay removed, on both
 * sides of the IPC boundary.
 */
export function removedCommandViolations(root = repoRoot) {
  const violations = [];
  const commandFile = path.join(root, ...RUST_SRC.split('/'), 'commands', 'pr.rs');
  const names = commandNames(fs.readFileSync(commandFile, 'utf8'));
  for (const removed of ['create_pr', 'create_pr_for_mesh']) {
    if (names.has(removed)) {
      violations.push(
        `commands/pr.rs declares \`${removed}\` — it derived its source from the project ` +
          'root, and it has no callers (issue #2236)',
      );
    }
  }
  const wrapperFile = path.join(root, 'src', 'lib', 'tauri', 'github.ts');
  const wrapper = fs.readFileSync(wrapperFile, 'utf8');
  for (const removed of ['createPr', 'createPrForMesh']) {
    if (new RegExp(`\\b${removed}\\b`).test(wrapper)) {
      violations.push(
        `src/lib/tauri/github.ts still exports \`${removed}\` — the command it called is gone ` +
          '(issue #2236)',
      );
    }
  }
  return violations;
}

/** The violations the guard found. */
export function prCreationSourceViolations(root = repoRoot) {
  return [
    ...commandLayerReaches(root),
    ...sourceOwnershipViolations(root),
    ...nodeResolverViolations(root),
    ...removedCommandViolations(root),
  ];
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const violations = prCreationSourceViolations();
  if (violations.length > 0) {
    console.error('PR creation source ownership (issue #2236):');
    for (const violation of violations) console.error(`  ${violation}`);
    console.error('');
    console.error('A create-PR path must say where its head branch comes from — the agent');
    console.error('node worktree, or a branch the path created itself — never the branch the');
    console.error('project root happens to be sitting on.');
    process.exit(1);
  }
  console.log(
    'PR creation source ownership: every create-PR path resolves its source from the ' +
      'node/mesh, and no service or circuit module reaches the PR command layer.',
  );
}
