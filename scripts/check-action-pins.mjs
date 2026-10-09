#!/usr/bin/env node
// Enforce SHA-pinned third-party GitHub Actions (issue #1541).
//
// A `uses: owner/action@v4` reference is a *mutable tag*. Whoever controls the
// tag can repoint it at new code, and every pull request in this repository
// would then execute that code with this repository's credentials — including
// the release job, which holds `contents: write` and the updater signing keys.
// Pinning to a full commit SHA makes the executed code an explicit, reviewable
// property of the diff; the `# vX.Y.Z` comment beside it keeps the human-
// readable version, and Dependabot (see .github/dependabot.yml) rewrites both.
//
// What this gate checks, over `.github/workflows/*.yml` and
// `.github/actions/*/action.yml`:
//   * every third-party `uses:` reference is a full 40-character lowercase hex
//     commit SHA (local `./...` actions and `docker://` refs are exempt);
//   * every pinned reference carries a `# vX.Y.Z`-style version comment, so the
//     pin stays readable and has something for Dependabot to update;
//   * a re-pinned SHA actually exists in that repository and still points at a
//     commit, so a typo or a truncated SHA fails here instead of at run time
//     (with --verify, which CI and `npm run check:actions` pass; the default
//     run is offline so local checks and unit tests need no network).
//
// Deliberately *not* checked: that the pinned commit is on the upstream default
// branch, or that the comment matches upstream's latest tag. Both change
// constantly, and a gate that fails for those reasons trains people to bypass
// it. Age and provenance are reviewed when the pin is written or bumped.
//
// Exit codes: 0 clean, 1 violations found, 2 the tree could not be read.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

export const FULL_SHA = /^[0-9a-f]{40}$/;
// A version comment is the human-readable half of the pin: `# v4` and
// `# v4.2.1` are both fine. `splitComment` has already removed the `#`, so this
// matches the bare comment text.
//
// The channel names are for the one action whose upstream "version" is a moving
// channel rather than a release: `dtolnay/rust-toolchain@stable` publishes no
// tags, so `# stable` is the accurate label for that pin. It is still pinned to
// an immutable commit SHA, which is what actually matters.
export const VERSION_COMMENT = /^(?:v?\d[\w.+-]*|stable|beta|nightly)$/;

const WORKFLOWS_DIR = path.join(repoRoot, '.github', 'workflows');
const ACTIONS_DIR = path.join(repoRoot, '.github', 'actions');

/**
 * Collect the gate's input files. Missing directories are not an error: a
 * clone without `.github/workflows` has nothing to enforce, and failing here
 * would break unrelated tooling.
 */
export function collectActionFiles(root = repoRoot) {
  const files = [];
  const workflows = path.join(root, '.github', 'workflows');
  if (fs.existsSync(workflows)) {
    for (const entry of fs.readdirSync(workflows, { withFileTypes: true })) {
      if (entry.isFile() && (entry.name.endsWith('.yml') || entry.name.endsWith('.yaml'))) {
        files.push(path.join(workflows, entry.name));
      }
    }
  }
  const actions = path.join(root, '.github', 'actions');
  if (fs.existsSync(actions)) {
    for (const entry of fs.readdirSync(actions, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      for (const candidate of ['action.yml', 'action.yaml']) {
        const file = path.join(actions, entry.name, candidate);
        if (fs.existsSync(file)) files.push(file);
      }
    }
  }
  return files;
}

// Strip a trailing `# comment` from a `uses:` value. The value can legitimately
// contain a `#` inside an expression, so cut at the first one that starts a
// comment (preceded by whitespace or at the start).
function splitComment(value) {
  const match = value.match(/\s+#\s*(.*?)\s*$/);
  return match ? { ref: value.slice(0, match.index).trim(), comment: match[1] } : { ref: value.trim(), comment: '' };
}

/**
 * Parse `uses:` references out of one workflow/action file.
 *
 * Only `uses:` keys are considered, and only at a step position. A `uses:` under
 * `jobs.<id>.uses:` is a *reusable-workflow* call (`./.github/workflows/x.yml`
 * or `owner/repo/.github/workflows/x.yml@v1`) — local ones are exempt and
 * third-party ones are still pinned, which is why the distinction is made on the
 * reference shape rather than on indentation.
 */
export function parseUses(markdown) {
  const lines = markdown.split(/\r?\n/);
  const found = [];
  lines.forEach((rawLine, index) => {
    const line = rawLine.replace(/\s+$/, '');
    // Ignore commented-out references: a workflow that documents the old tag in
    // prose must not fail the gate.
    const match = line.match(/^\s*(?:-\s*)?uses:\s*(.+?)\s*$/);
    if (!match) return;
    const value = match[1];
    if (value.startsWith('#')) return;
    const { ref, comment } = splitComment(value);
    if (!ref || ref.startsWith('#')) return;
    found.push({ ref, comment, line: index + 1 });
  });
  return found;
}

function classify(reference) {
  if (reference.startsWith('./') || reference.startsWith('../') || reference.startsWith('.')) {
    return { kind: 'local', owner: null, version: null };
  }
  if (reference.startsWith('docker://')) return { kind: 'docker', owner: null, version: null };
  const at = reference.lastIndexOf('@');
  if (at === -1) return { kind: 'third-party', owner: reference, version: null };
  const owner = reference.slice(0, at);
  const version = reference.slice(at + 1);
  // `owner/repo/path@ref` is still third-party and still needs a SHA.
  return { kind: 'third-party', owner, version };
}

/**
 * Check one file's contents. Returns a list of violation strings (empty when
 * clean) so a caller can aggregate across the tree.
 */
export function checkActionFile(markdown, { file = 'workflow.yml', allow = new Set() } = {}) {
  const violations = [];
  for (const { ref, comment, line } of parseUses(markdown)) {
    const { kind, owner, version } = classify(ref);
    if (kind !== 'third-party') continue;
    const at = `at ${file}:${line}`;
    if (version === null) {
      violations.push(`${at}: \`${ref}\` has no version or SHA after '@'. Pin it to a full commit SHA.`);
      continue;
    }
    // A Dependabot-updated pin may legitimately be exempted by an explicit
    // allowlist entry, which is how a repository can land an action it cannot
    // yet pin (an action with no releases, for example) without disabling the
    // gate.
    const allowKey = `${owner}@${version}`;
    if (allow.has(allowKey)) continue;
    if (!FULL_SHA.test(version)) {
      violations.push(
        `${at}: \`${owner}\` is pinned to the mutable ref \`${version}\`, not a commit SHA. `
          + 'Use a full 40-character commit SHA with a `# vX.Y.Z` comment.',
      );
      continue;
    }
    if (!VERSION_COMMENT.test(comment)) {
      violations.push(
        `${at}: \`${owner}@${version}\` is SHA-pinned but has no version comment. `
          + 'Add `# vX.Y.Z` after the SHA so the pin is readable and Dependabot can update it.',
      );
    }
  }
  return violations;
}

function readAllowList(root) {
  const file = path.join(root, '.github', 'action-pin-allowlist.txt');
  if (!fs.existsSync(file)) return new Set();
  return new Set(
    fs
      .readFileSync(file, 'utf8')
      .split(/\r?\n/)
      .map((line) => line.trim())
      // Blank lines and `#` comments only; anything else is an `owner@ref`.
      .filter((line) => line && !line.startsWith('#')),
  );
}

/**
 * Verify that each pinned SHA still names a commit in its repository, so a
 * truncated or mistyped SHA fails here instead of at run time. Requires network
 * and is opt-in (`--verify`), which is what CI and `npm run check:actions` use.
 *
 * Resolution failures are reported but do not throw: a transient API failure
 * must not read as "the repository is unpinned". An *unauthenticated* failure
 * is called out separately, because that is a wiring mistake rather than an
 * upstream problem — `gh` refuses to call the API at all inside a workflow
 * without a token, and the resulting message is otherwise a wall of the same
 * auth hint repeated once per reference.
 */
export async function verifyShas(references, { fetchJson } = {}) {
  const problems = [];
  const authFailures = [];
  const byOwner = new Map();
  for (const { owner, sha } of references) {
    if (!byOwner.has(owner)) byOwner.set(owner, new Set());
    byOwner.get(owner).add(sha);
  }
  for (const [owner, shas] of byOwner) {
    for (const sha of shas) {
      try {
        const commit = await fetchJson(`repos/${owner}/commits/${sha}`);
        if (!commit || commit.sha !== sha) {
          problems.push(`${owner}@${sha}: the API did not resolve this SHA to a commit.`);
        }
      } catch (error) {
        const message = error.message ?? String(error);
        if (/GH_TOKEN|authentication|not logged in|gh auth/i.test(message)) {
          authFailures.push(`${owner}@${sha}: ${message.split('\n')[0]}`);
        } else {
          problems.push(`${owner}@${sha}: could not verify (${message}).`);
        }
      }
    }
  }
  if (authFailures.length > 0) {
    problems.push(
      `GitHub API authentication failed for ${authFailures.length} reference(s) — this is a wiring problem, not an unpinned action.\n`
      + `  First: ${authFailures[0]}\n`
      + "  Set GH_TOKEN for this step (in a workflow, `env: GH_TOKEN: ${{ github.token }}`); "
      + 'locally, run `gh auth login`. First error: ' + authFailures.slice(1, 3).join(' | '),
    );
  }
  return problems;
}

/** Every third-party reference in the tree, deduplicated, for `--verify`. */
export function collectReferences(root = repoRoot) {
  const allow = readAllowList(root);
  const references = [];
  for (const file of collectActionFiles(root)) {
    const markdown = fs.readFileSync(file, 'utf8');
    for (const { ref } of parseUses(markdown)) {
      const { kind, owner, version } = classify(ref);
      if (kind !== 'third-party' || version === null || !FULL_SHA.test(version)) continue;
      const key = `${owner}@${version}`;
      if (allow.has(key)) continue;
      references.push({ owner, sha: version, key });
    }
  }
  const seen = new Map();
  for (const reference of references) seen.set(reference.key, reference);
  return [...seen.values()];
}

async function main(argv) {
  const verify = argv.includes('--verify');
  const root = repoRoot;
  let files;
  try {
    files = collectActionFiles(root);
  } catch (error) {
    process.stderr.write(`::error::Could not read the workflow tree: ${error.message}\n`);
    return 2;
  }
  const allow = readAllowList(root);
  const violations = files.flatMap((file) =>
    checkActionFile(fs.readFileSync(file, 'utf8'), {
      file: path.relative(root, file).split(path.sep).join('/'),
      allow,
    }),
  );

  if (verify) {
    const { execFile } = await import('node:child_process');
    const fetchJson = (endpoint) =>
      new Promise((resolve, reject) => {
        execFile(
          'gh',
          ['api', endpoint, '--jq', '.sha'],
          { encoding: 'utf8', timeout: 30_000 },
          (error, stdout) => (error ? reject(error) : resolve({ sha: stdout.trim() })),
        );
      });
    violations.push(...(await verifyShas(collectReferences(root), { fetchJson })));
  }

  if (violations.length > 0) {
    process.stderr.write('Third-party actions must be pinned to a full commit SHA (issue #1541):\n');
    for (const violation of violations) process.stderr.write(`  ${violation}\n`);
    process.stderr.write(
      '\nTo pin an action, resolve the tag with '
        + '`gh api repos/<owner>/<repo>/git/ref/tags/<tag>` (follow `object.url` '
        + 'once for an annotated tag) and write '
        + '`uses: <owner>/<repo>@<40-char-sha> # vX.Y.Z`.\n',
    );
    return 1;
  }
  process.stdout.write(`All third-party actions are SHA-pinned (${files.length} file(s) checked).\n`);
  return 0;
}

// Only run when invoked directly, so the unit tests can import the pure helpers.
if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  });
}