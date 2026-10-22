#!/usr/bin/env node
// apt-package-set-key.mjs — the cache identity of the apt .deb set.
//
// `apt-get install --print-uris` resolves an install without fetching
// anything and prints one line per .deb it would download:
//
//   'http://mirror/pool/main/z/zlib_1.3.1-1_amd64.deb' zlib_1.3.1-1_amd64.deb 91338 SHA256:...
//
// The filename is what apt's own archive cache stores and matches on (the
// output URL-encodes it: an epoch's `1:` prints as `1%3a`), so the
// digest of that set *is* the identity of the .deb set: the same digest
// names the same set of files, and a different digest names a different one.
//
// The key this replaced was the hash of the composite action's own file, which
// changed only when somebody edited it. It could not tell a 24.04 package set
// from a 26.04 one, so the entry the runner-image migration left behind was
// restored as a *hit* and then the new set was downloaded again: on run
// 39777829786 attempt 2 each of the nine Linux Rust jobs restored 135 MB of
// the old set and downloaded 77.7 MB of the new one, and apt's own "Need to
// get" and "Fetched" figures matched, so none of the restored files was
// reused.
//
// Usage:
//   apt-get install --print-uris -y <packages> \
//     | node scripts/ci/apt-package-set-key.mjs
//   node scripts/ci/apt-package-set-key.mjs --uris <file>
//
// Writes GitHub Actions step outputs to stdout:
//
//   digest=<sha256 over the sorted .deb filenames>
//   codename=<the OS release codename>
//   packages=<how many .debs the set holds>
//
// Diagnostics go to stderr, so stdout stays a clean key=value file for
// `$GITHUB_OUTPUT`.

import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const USAGE = `Usage: node scripts/ci/apt-package-set-key.mjs [--uris <file>] [--codename <name>] [--os-release <file>]

Reads \`apt-get install --print-uris\` output (stdin by default) and prints the
cache identity of the .deb set it names:

  digest=<sha256 over the sorted .deb filenames>
  codename=<the OS release codename>
  packages=<how many .debs the set holds>`;

/** One `--print-uris` line: a quoted URI, the filename, the size and a hash. */
const URI_LINE = /^'([^']+)'\s+(\S+)\s+(\d+)\s+(\S+)\s*$/;

/**
 * The .deb filenames an `--print-uris` output names, in the order apt printed
 * them. Lines that are not URI lines are ignored: apt may print a note about
 * the simulation, and a package that is already installed is not fetched at
 * all and so is not listed.
 */
export function parseUriFilenames(text) {
  const names = [];
  for (const line of text.split('\n')) {
    const match = line.trim().match(URI_LINE);
    if (match) names.push(match[2]);
  }
  return names;
}

/**
 * The digest of a .deb set: the sha256 over its sorted, deduplicated
 * filenames. Order does not matter (apt resolves in its own order and the set
 * is the same set), and a repeated filename is the same file.
 */
export function packageSetDigest(text) {
  const names = [...new Set(parseUriFilenames(text))].sort();
  return createHash('sha256').update(names.join('\n')).digest('hex');
}

/**
 * The OS release codename, which scopes the cache key to one Ubuntu release.
 * A runner-image migration moves the suite under the runner's feet, and the
 * package set of the old release is a different set from the new one: on run
 * 39777829786 none of the 135 MB restored from the 24.04 entry was reused by
 * the 26.04 install.
 *
 * `VERSION_CODENAME` is the modern field; `UBUNTU_CODENAME` is the older name
 * for the same thing and is still the only one some images carry.
 */
export function releaseCodename(osReleasePath = '/etc/os-release') {
  let text;
  try {
    text = readFileSync(osReleasePath, 'utf8');
  } catch {
    return '';
  }
  for (const field of ['VERSION_CODENAME', 'UBUNTU_CODENAME']) {
    const match = text.match(new RegExp(`^${field}=(.*)$`, 'm'));
    const value = (match?.[1] ?? '').trim().replace(/^["']|["']$/g, '');
    if (value) return value;
  }
  return '';
}

function parseArgs(argv) {
  const options = { uris: null, codename: null, osRelease: '/etc/os-release' };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const [flag, inline] = arg.includes('=') ? [arg.slice(0, arg.indexOf('=')), arg.slice(arg.indexOf('=') + 1)] : [arg, undefined];
    const value = inline ?? argv[i + 1];
    if (inline === undefined) i += 1;
    if (value === undefined) throw new Error(`Missing value for ${flag}.\n\n${USAGE}`);
    if (flag === '--uris') options.uris = value;
    else if (flag === '--codename') options.codename = value;
    else if (flag === '--os-release') options.osRelease = value;
    else throw new Error(`Unknown option: ${arg}\n\n${USAGE}`);
  }
  return options;
}

function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (err) {
    process.stderr.write(`${err.message}\n`);
    process.exitCode = 2;
    return;
  }

  let text;
  try {
    text = options.uris ? readFileSync(options.uris, 'utf8') : readFileSync(0, 'utf8');
  } catch (err) {
    process.stderr.write(`apt-package-set-key: cannot read the --print-uris output: ${err.message}\n`);
    process.exitCode = 1;
    return;
  }

  const filenames = parseUriFilenames(text);
  if (filenames.length === 0) {
    // An empty set means the resolution did not produce the list of .debs the
    // install needs. Failing here is right: the install would fail too, and a
    // cache key computed from an empty set would name an entry that holds
    // nothing.
    process.stderr.write('apt-package-set-key: no .deb filenames in the --print-uris output.\n');
    process.exitCode = 1;
    return;
  }

  const codename = options.codename ?? releaseCodename(options.osRelease);
  if (!codename) {
    // Without the release in the key, an entry from another Ubuntu release
    // can be restored as a hit that downloads the whole set again. That is
    // the failure this script exists to prevent, so it is not papered over.
    process.stderr.write(`apt-package-set-key: no VERSION_CODENAME in ${options.osRelease}.\n`);
    process.exitCode = 1;
    return;
  }

  process.stdout.write(
    `digest=${packageSetDigest(text)}\ncodename=${codename}\npackages=${filenames.length}\n`,
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
