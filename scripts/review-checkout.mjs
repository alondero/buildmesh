#!/usr/bin/env node
import { execFileSync, spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const git = (root, ...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true }).trim();

function linkDependencies(root, checkout, links) {
  const source = join(root, 'node_modules');
  if (!existsSync(source)) return;
  for (const file of ['package-lock.json', 'package.json']) {
    const original = join(root, file);
    const reviewed = join(checkout, file);
    if (existsSync(original) !== existsSync(reviewed)) throw new Error(`Dependency inputs differ (${file}); use --no-deps and install inside the isolated checkout.`);
    if (!existsSync(original)) continue;
    const bytes = path => readFileSync(path, 'utf8');
    const value = text => file === 'package.json'
      ? JSON.stringify(Object.fromEntries(['dependencies', 'devDependencies', 'optionalDependencies', 'overrides'].map(key => [key, JSON.parse(text)[key]])))
      : text;
    if (value(bytes(original)) !== value(bytes(reviewed))) throw new Error(`Dependency inputs differ (${file}); use --no-deps and install inside the isolated checkout.`);
  }
  const target = join(checkout, 'node_modules');
  mkdirSync(target, { recursive: true });
  const link = (from, to) => {
    symlinkSync(realpathSync(from), to, process.platform === 'win32' ? 'junction' : 'dir');
    links.push(to);
  };
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    const from = join(source, entry.name);
    const to = join(target, entry.name);
    if (entry.name === '.bin') cpSync(from, to, { recursive: true, verbatimSymlinks: true });
    else if (entry.name.startsWith('.')) continue; // .vite, .vite-temp and other caches stay local.
    else if (entry.name.startsWith('@')) {
      mkdirSync(to, { recursive: true });
      for (const name of readdirSync(from)) link(join(from, name), join(to, name));
    } else if (entry.isDirectory() || entry.isSymbolicLink()) link(from, to);
  }
}

export async function withReviewCheckout(root, ref, action, { dependencies = true } = {}) {
  root = resolve(root);
  const revision = git(root, 'rev-parse', '--verify', '--end-of-options', `${ref}^{commit}`);
  const parent = join(root, '.tmp', 'review-checkouts');
  mkdirSync(parent, { recursive: true });
  const owned = mkdtempSync(join(parent, 'review-'));
  const checkout = join(owned, 'tree');
  const links = [];
  let registered = false;
  try {
    git(root, 'worktree', 'add', '--detach', checkout, revision);
    registered = true;
    if (dependencies) linkDependencies(root, checkout, links);
    const temporary = join(checkout, '.tmp', 'review-runtime');
    mkdirSync(temporary, { recursive: true });
    const env = {
      ...process.env,
      CARGO_TARGET_DIR: join(temporary, 'cargo-target'),
      npm_config_cache: join(temporary, 'npm-cache'),
      TMP: temporary, TEMP: temporary, TMPDIR: temporary,
    };
    delete env.TS_RS_EXPORT_DIR;
    return await action({ path: checkout, revision, env });
  } finally {
    // Remove our package links first; git's cleanup never needs to traverse a
    // junction into the dependency checkout. Every path was created under owned.
    for (const path of links.reverse()) rmSync(path, { recursive: true, force: true });
    if (registered) git(root, 'worktree', 'remove', '--force', checkout);
    rmSync(owned, { recursive: true, force: true });
  }
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
