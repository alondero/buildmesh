import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, rmdirSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

// Dependency linking shared by `harness link-deps` and scripts/review-checkout.mjs.
// Packages are junctioned one by one under a private node_modules root so that
// `.bin` shims and bundler caches stay out of the source checkout, and links are
// removed with a non-recursive unlink (removeLink) rather than a directory delete.

export const WORKTREE_DEPS_USAGE = 'link-deps [--from <path>] [--cargo-target] | unlink-deps';
const MARKER = '.buildmesh-linked-deps.json';
const LINK_KIND = process.platform === 'win32' ? 'junction' : 'dir';

const git = (cwd, ...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true }).trim();
const lstatOrNull = path => { try { return lstatSync(path); } catch { return null; } };
const realPath = path => { try { return realpathSync.native(path); } catch { return null; } };

/** Removes a link (symlink or Windows junction) itself, not the directory it points at; refuses a non-link. */
export function removeLink(path) {
  if (!lstatSync(path).isSymbolicLink()) throw new Error(`Refusing to unlink ${path}: it is not a link.`);
  try { unlinkSync(path); } catch (error) {
    if (process.platform !== 'win32') throw error;
    rmdirSync(path); // a junction that unlink rejects: rmdir still removes only the junction.
  }
}

function assertDependencyInputsMatch(source, target, remedy) {
  for (const file of ['package-lock.json', 'package.json']) {
    const original = join(source, file);
    const candidate = join(target, file);
    if (existsSync(original) !== existsSync(candidate)) throw new Error(`Dependency inputs differ (${file}); ${remedy}`);
    if (!existsSync(original)) continue;
    const bytes = path => readFileSync(path, 'utf8');
    const value = text => file === 'package.json'
      ? JSON.stringify(Object.fromEntries(['dependencies', 'devDependencies', 'optionalDependencies', 'overrides'].map(key => [key, JSON.parse(text)[key]])))
      : text;
    if (value(bytes(original)) !== value(bytes(candidate))) throw new Error(`Dependency inputs differ (${file}); ${remedy}`);
  }
}

/**
 * Links `source/node_modules` packages into `target/node_modules`, appending each
 * created link to `links` as it goes so a caller can roll back a partial failure.
 */
export function linkDependencies(source, target, links, { remedy }) {
  const from = join(source, 'node_modules');
  if (!existsSync(from)) return;
  assertDependencyInputsMatch(source, target, remedy);
  const to = join(target, 'node_modules');
  mkdirSync(to, { recursive: true });
  const link = (packageDir, destination) => {
    symlinkSync(realpathSync(packageDir), destination, LINK_KIND);
    links.push(destination);
  };
  for (const entry of readdirSync(from, { withFileTypes: true })) {
    const packageDir = join(from, entry.name);
    const destination = join(to, entry.name);
    if (entry.name === '.bin') cpSync(packageDir, destination, { recursive: true, verbatimSymlinks: true });
    else if (entry.name.startsWith('.')) continue; // .vite, .vite-temp and other caches stay local.
    else if (entry.name.startsWith('@')) {
      mkdirSync(destination, { recursive: true });
      for (const name of readdirSync(packageDir)) link(join(packageDir, name), join(destination, name));
    } else if (entry.isDirectory() || entry.isSymbolicLink()) link(packageDir, destination);
  }
}

/** `git worktree list` prints the main checkout first. */
export function mainCheckout(root) {
  const first = git(root, 'worktree', 'list', '--porcelain').split(/\r?\n/).find(line => line.startsWith('worktree '));
  if (!first) throw new Error('git worktree list reported no main checkout; pass --from <path>.');
  return resolve(first.slice('worktree '.length));
}

function linkCargoTarget(source, root, created) {
  const from = join(source, 'src-tauri', 'target');
  if (!existsSync(from)) throw new Error(`${from} does not exist; build in the source checkout first or omit --cargo-target.`);
  const to = join(root, 'src-tauri', 'target');
  const stat = lstatOrNull(to);
  if (stat?.isSymbolicLink()) {
    if (realPath(to) === realPath(from)) return;
    throw new Error(`${to} already links elsewhere; run unlink-deps first.`);
  }
  if (stat) {
    if (!stat.isDirectory() || readdirSync(to).length) throw new Error(`${to} already exists with content; remove it deliberately or omit --cargo-target.`);
    rmdirSync(to);
  }
  mkdirSync(dirname(to), { recursive: true });
  symlinkSync(realpathSync(from), to, LINK_KIND);
  created.push(to);
}

function readMarker(modules) {
  try { return JSON.parse(readFileSync(join(modules, MARKER), 'utf8')); } catch { return null; }
}

/** Removes `path` only when it is a link; returns whether it did. */
export function removeLinkIfPresent(path) {
  if (!lstatOrNull(path)?.isSymbolicLink()) return false;
  removeLink(path);
  return true;
}

// Removes what an earlier (possibly interrupted) link-deps run left in a marked
// node_modules. It sweeps the links actually on disk instead of trusting paths
// recorded in the marker, so a stale or edited marker cannot point it elsewhere.
function removeOwnedLinks(modules) {
  let removed = 0;
  for (const entry of readdirSync(modules, { withFileTypes: true })) {
    if (entry.name.startsWith('.')) continue; // .bin, caches and the marker are handled below.
    const path = join(modules, entry.name);
    if (removeLinkIfPresent(path)) removed += 1;
    else if (entry.name.startsWith('@') && entry.isDirectory()) {
      for (const name of readdirSync(path)) if (removeLinkIfPresent(join(path, name))) removed += 1;
      try { rmdirSync(path); } catch { /* holds something link-deps did not create */ }
    }
  }
  const bin = join(modules, '.bin');
  if (!removeLinkIfPresent(bin)) rmSync(bin, { recursive: true, force: true }); // our copy of the shims
  rmSync(join(modules, MARKER), { force: true });
  try { rmdirSync(modules); } catch { /* local caches remain */ }
  return removed;
}

export function linkWorktreeDependencies(root, { from, cargoTarget = false } = {}) {
  root = resolve(root);
  const source = from ? resolve(from) : mainCheckout(root);
  if (!existsSync(source)) throw new Error(`Dependency source ${source} does not exist.`);
  if (realPath(source) === realPath(root)) {
    throw new Error(from
      ? `Dependency source ${source} is this checkout; pass another checkout with --from.`
      : 'This is the main checkout; install packages here with npm ci.');
  }
  if (!existsSync(join(source, 'node_modules'))) throw new Error(`${source} has no node_modules; run npm ci there first, or run npm ci in this worktree.`);
  assertDependencyInputsMatch(source, root, 'run npm ci in this worktree instead of linking.');

  const modules = join(root, 'node_modules');
  const stat = lstatOrNull(modules);
  const marker = stat?.isDirectory() ? readMarker(modules) : null;
  if (stat?.isSymbolicLink()) throw new Error(`${modules} is a link; run unlink-deps first.`);
  if (stat && !marker && readdirSync(modules).length) throw new Error(`${modules} already exists and was not created by link-deps; keep that install, or remove it deliberately and rerun.`);

  const created = [];
  const links = [];
  let started = false;
  try {
    // Check the Cargo target before dropping a working link set to refresh it.
    if (cargoTarget) linkCargoTarget(source, root, created);
    started = true;
    if (marker) removeOwnedLinks(modules);
    // The marker goes down before the first link so an interrupted run stays unlinkable.
    mkdirSync(modules, { recursive: true });
    writeFileSync(join(modules, MARKER), JSON.stringify({ source }, null, 2));
    linkDependencies(source, root, links, { remedy: 'run npm ci in this worktree instead of linking.' });
  } catch (error) {
    // Roll back a partial link: drop the links made so far, then the Cargo link.
    if (started && readMarker(modules)) removeOwnedLinks(modules);
    for (const path of created) { try { removeLink(path); } catch { /* already gone */ } }
    throw error;
  }
  return { source, linked: links.length, cargoTarget };
}

export function unlinkWorktreeDependencies(root) {
  root = resolve(root);
  const modules = join(root, 'node_modules');
  const target = join(root, 'src-tauri', 'target');
  let removed = 0;
  const stat = lstatOrNull(modules);
  if (stat?.isSymbolicLink()) { removeLink(modules); removed += 1; } // a manual whole-directory junction
  else if (stat?.isDirectory()) {
    if (readMarker(modules)) removed += removeOwnedLinks(modules);
  }
  if (lstatOrNull(target)?.isSymbolicLink()) { removeLink(target); removed += 1; }
  return { removed };
}

export function runWorktreeDeps(root, args) {
  const [action, ...flags] = args;
  const options = {};
  for (let i = 0; i < flags.length; i += 1) {
    if (action === 'link-deps' && flags[i] === '--cargo-target') options.cargoTarget = true;
    else if (action === 'link-deps' && flags[i] === '--from' && flags[i + 1]) options.from = flags[++i];
    else throw new Error(`Usage: ${WORKTREE_DEPS_USAGE}`);
  }
  if (action === 'unlink-deps') {
    const { removed } = unlinkWorktreeDependencies(root);
    return removed ? `Removed ${removed} dependency link(s); the source checkout was not modified.` : 'No dependency links to remove.';
  }
  const { source, linked, cargoTarget } = linkWorktreeDependencies(root, options);
  return `Linked ${linked} package(s) from ${source}${cargoTarget ? ' and src-tauri/target' : ''}. Do not run npm install here; undo with: npm run harness -- unlink-deps`;
}
