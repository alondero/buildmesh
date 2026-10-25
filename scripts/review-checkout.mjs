#!/usr/bin/env node
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { linkDependencies, removeLinkIfPresent } from './worktree-deps.mjs';

const git = (root, ...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true }).trim();

export async function withReviewCheckout(root, ref, action, { dependencies = true } = {}) {
  root = resolve(root);
  const revision = git(root, 'rev-parse', '--verify', '--end-of-options', `${ref}^{commit}`);
  const parent = join(root, '.tmp', 'review-checkouts');
  mkdirSync(parent, { recursive: true });
  const owned = mkdtempSync(join(parent, 'review-'));
  const checkout = join(owned, 'tree');
  const links = [];
  let registered = false;
  let outcome;
  let failed = false;
  let failure;
  try {
    git(root, 'worktree', 'add', '--detach', checkout, revision);
    registered = true;
    if (dependencies) linkDependencies(root, checkout, links, { remedy: 'use --no-deps and install inside the isolated checkout.' });
    const temporary = join(checkout, '.tmp', 'review-runtime');
    mkdirSync(temporary, { recursive: true });
    const env = {
      ...process.env,
      CARGO_TARGET_DIR: join(temporary, 'cargo-target'),
      npm_config_cache: join(temporary, 'npm-cache'),
      TMP: temporary, TEMP: temporary, TMPDIR: temporary,
    };
    delete env.TS_RS_EXPORT_DIR;
    outcome = await action({ path: checkout, revision, env });
  } catch (error) {
    failed = true;
    failure = error;
  }
  // Remove our package links first; git's cleanup never needs to traverse a
  // junction into the dependency checkout. Every path was created under owned.
  // A link that dangles or was replaced by a real directory is handled by
  // removeLinkIfPresent; if a real link cannot be removed, leave the checkout in
  // place rather than let git's cleanup traverse it into the dependency checkout.
  let stuck;
  for (const path of links.reverse()) {
    try { removeLinkIfPresent(path); } catch (error) { stuck ??= { path, error }; }
  }
  if (stuck) throw new Error(`Could not remove dependency link ${stuck.path}; the review checkout ${owned} was left in place: ${stuck.error.message}`);
  if (registered) git(root, 'worktree', 'remove', '--force', checkout);
  rmSync(owned, { recursive: true, force: true });
  if (failed) throw failure;
  return outcome;
}

function commandFor(root, command) {
  if (command[0] === 'node') return [process.execPath, ...command.slice(1)];
  if (command[0] !== 'npm') return command;
  const candidates = [process.env.npm_execpath, join(dirname(process.execPath), 'node_modules/npm/bin/npm-cli.js'), join(root, 'node_modules/npm/bin/npm-cli.js')];
  const cli = candidates.find(path => path && existsSync(path));
  if (!cli) throw new Error('npm CLI not found; invoke its npm-cli.js with node.');
  return [process.execPath, cli, ...command.slice(1)];
}

async function main(args) {
  const separator = args.indexOf('--');
  const options = args.slice(0, separator);
  const command = args.slice(separator + 1);
  const dependencies = !options.includes('--no-deps');
  const refArgs = options.filter(arg => arg !== '--no-deps');
  if (separator === -1 || !command.length || (refArgs.length && (refArgs.length !== 2 || refArgs[0] !== '--ref'))) {
    throw new Error('Usage: node scripts/review-checkout.mjs [--ref <commit>] [--no-deps] -- <command> [args...]');
  }
  if (dependencies && command[0] === 'npm' && /^(?:ci|install|i|update|uninstall|rebuild)$/.test(command[1] ?? '')) {
    throw new Error('Dependency installation needs --no-deps; linked packages are shared with the source checkout.');
  }
  const root = git(process.cwd(), 'rev-parse', '--show-toplevel');
  const resolved = commandFor(root, command);
  return withReviewCheckout(root, refArgs[1] ?? 'HEAD', ({ path, revision, env }) => {
    console.log(`Review revision: ${revision}\nReview checkout: ${path}`);
    const result = spawnSync(resolved[0], resolved.slice(1), { cwd: path, env, stdio: 'inherit', windowsHide: true });
    if (result.error) throw result.error;
    return result.status ?? 1;
  }, { dependencies });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).then(code => { process.exitCode = code; }).catch(error => {
    console.error(`Review checkout failed: ${error.message}`);
    process.exitCode = 1;
  });
}
