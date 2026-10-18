#!/usr/bin/env node
// verify-updater-feed.mjs — prove the draft release's `latest.json` is a complete,
// usable updater feed before anyone publishes it (issue #2483).
//
// Why this exists: the release now has three packaging jobs that all write the
// same `latest.json` asset. `tauri-action` merges rather than overwrites (it
// re-reads the published asset and keeps its `platforms` object before writing
// its own keys), and `release.yml` serialises the writers so two jobs never
// race on the delete-then-upload. Merge plus serialisation is upstream
// behaviour we can rely on but not unit-test from this repository — so the
// guarantee is enforced here instead, at the boundary that matters: the feed a
// running install would actually download.
//
// The failure this catches is the one that ships silently. A macOS leg that
// uploaded its `.app.tar.gz` but whose `darwin-aarch64` entry never made it
// into the merged feed leaves a perfectly green release that macOS installs
// cannot update from, and the release notes promise an updater that is not
// there. Every check below is about a feed that would break an install.
//
// Usage:
//   node scripts/ci/verify-updater-feed.mjs <feed.json> \
//     --expect-version 1.5.0 \
//     --require windows-x86_64 --require darwin-aarch64 --require darwin-x86_64 \
//     [--assets assets.json]
//
// `--assets` takes what `gh release view --json assets` produces, and
// additionally proves each feed entry resolves to an artifact this release
// really carries. It has to handle two URL forms, because tauri-action v1.0.0
// changed which one it writes: `latest.json` entries are currently the API form
// `…/releases/assets/{numeric id}` rather than a filename, so a check that
// compared filenames against those urls would match nothing and fail every
// release. Matching the trailing segment covers both — an id against the
// release's asset ids, a filename against its asset names.
//
// Exit codes: 0 the feed is complete and well-formed; 1 it is not.

import fs from 'node:fs';
import { pathToFileURL } from 'node:url';

// A minisign signature is two base64 payload lines bracketed by `untrusted
// comment:` and `trusted comment:` lines. Requiring the base64 *and* the
// brackets catches a truncated or placeholder signature without pinning the
// comment text, which Tauri does not guarantee.
const BASE64_LINE = /^[A-Za-z0-9+/]+={0,2}$/;
const COMMENT_PREFIX = /^(untrusted|trusted) comment:/;

function isPlainObject(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function signatureProblems(signature, label) {
  if (typeof signature !== 'string' || signature.trim() === '') {
    return [`${label}: signature is missing or empty — the updater would reject this update`];
  }
  const lines = signature.trim().split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const payloads = lines.filter((line) => !COMMENT_PREFIX.test(line));
  if (payloads.length < 2) {
    return [`${label}: signature has ${payloads.length} payload line(s), expected a minisign signature (2 base64 lines)`];
  }
  const malformed = payloads.filter((line) => !BASE64_LINE.test(line) || line.length % 4 !== 0);
  if (malformed.length > 0) {
    return [`${label}: signature payload is not valid base64 minisign output`];
  }
  if (!lines.some((line) => /^untrusted comment:/.test(line))) {
    return [`${label}: signature has no 'untrusted comment:' line, so it is not a minisign signature`];
  }
  return [];
}

function urlProblems(entry, label) {
  if (typeof entry.url !== 'string' || entry.url.trim() === '') {
    return [`${label}: url is missing or empty`];
  }
  if (!/^https:\/\//.test(entry.url)) {
    return [`${label}: url is not https (${entry.url})`];
  }
  return [];
}

/** Basename of an asset API/download URL, or null when the URL has no filename. */
export function assetNameFromUrl(url) {
  if (typeof url !== 'string') return null;
  const withoutQuery = url.split(/[?#]/)[0];
  const lastSegment = withoutQuery.slice(withoutQuery.lastIndexOf('/') + 1);
  return lastSegment === '' ? null : decodeURIComponent(lastSegment);
}

/**
 * Every way `feed` fails to be a complete, updater-usable release feed.
 * Pure: returns a list of problem strings, empty when the feed is good.
 */
export function verifyFeed(feed, { expectedVersion, requiredEntries = [], assets = null } = {}) {
  const problems = [];

  if (!isPlainObject(feed)) {
    return ['latest.json is not a JSON object'];
  }

  if (typeof feed.version !== 'string' || feed.version.trim() === '') {
    problems.push('latest.json has no "version"');
  } else if (expectedVersion && feed.version !== expectedVersion) {
    problems.push(`latest.json reports version "${feed.version}", expected "${expectedVersion}"`);
  }

  if (typeof feed.pub_date !== 'string' || Number.isNaN(Date.parse(feed.pub_date))) {
    problems.push(`latest.json has an unparseable "pub_date" (${JSON.stringify(feed.pub_date)})`);
  }

  const platforms = feed.platforms;
  if (!isPlainObject(platforms)) {
    problems.push('latest.json has no "platforms" object — the updater cannot resolve any target');
    return problems;
  }

  const keys = Object.keys(platforms);

  for (const required of requiredEntries) {
    if (!keys.includes(required)) {
      problems.push(`latest.json is missing the "${required}" entry — an install on that target cannot update (present: ${keys.join(', ') || 'none'})`);
    }
  }

  // Validate every entry that is present, not only the required ones: a
  // half-written entry for a target nobody listed would otherwise ship.
  const catalog = resolveAssets(assets);
  const urlOwners = new Map();

  for (const key of keys) {
    const entry = platforms[key];
    const label = `latest.json "${key}"`;
    if (!isPlainObject(entry)) {
      problems.push(`${label}: entry is not an object`);
      continue;
    }
    problems.push(...urlProblems(entry, label));
    problems.push(...signatureProblems(entry.signature, label));

    if (catalog && typeof entry.url === 'string' && entry.url.trim() !== '') {
      const target = assetNameFromUrl(entry.url);
      const known = target === null
        ? false
        : /^\d+$/.test(target) ? catalog.ids.has(target) : catalog.names.has(target);
      if (!known) {
        problems.push(`${label}: points at "${target}", which is not an asset on this release — the updater would 404`);
      }
    }

    if (typeof entry.url === 'string' && urlOwners.has(entry.url)) {
      problems.push(`${label}: shares its url with "${urlOwners.get(entry.url)}" — one artifact cannot serve two architectures`);
    } else if (typeof entry.url === 'string') {
      urlOwners.set(entry.url, key);
    }
  }

  return problems;
}

const USAGE = `Usage: node scripts/ci/verify-updater-feed.mjs <feed.json> \\
  --expect-version <semver> [--require <os>-<arch>]... [--assets assets.json]`;

function fail(message) {
  console.error(`::error::${message}`);
  process.exit(1);
}

/**
 * `gh release view --json assets` yields objects; a bare array of names is
 * accepted too. Anything that has no usable `name` becomes null rather than
 * `undefined`, so the comparisons below cannot match by accident.
 */
export function normaliseAssets(assets) {
  if (!Array.isArray(assets)) return null;
  return assets.map((asset) => (typeof asset === 'string' ? asset : asset?.name ?? null));
}

/**
 * Index a release's assets by both things a `latest.json` entry might refer to
 * them as: the numeric id in the API url form, and the filename in the browser
 * download form. Returns null when there is nothing to check against.
 */
export function resolveAssets(assets) {
  if (!Array.isArray(assets)) return null;
  const ids = new Set();
  const names = new Set();
  for (const asset of assets) {
    if (typeof asset === 'string') {
      if (asset !== '') names.add(asset);
      continue;
    }
    if (!asset || typeof asset !== 'object') continue;
    if (typeof asset.name === 'string' && asset.name !== '') names.add(asset.name);
    const id = assetNameFromUrl(asset.url);
    if (id !== null && /^\d+$/.test(id)) ids.add(id);
  }
  return { ids, names };
}

/** Split `argv` into positionals and flags; every flag here takes exactly one value. */
export function parseArgs(argv) {
  const VALUED_FLAGS = new Set(['--expect-version', '--require', '--assets']);
  const positionals = [];
  const values = new Map([['--require', []]]);
  const unknown = [];

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith('--')) {
      positionals.push(arg);
      continue;
    }
    if (!VALUED_FLAGS.has(arg)) {
      unknown.push(arg);
      continue;
    }
    const value = argv[index + 1];
    if (value === undefined || value.startsWith('--')) {
      unknown.push(`${arg} (missing value)`);
      continue;
    }
    index += 1;
    if (arg === '--require') values.get('--require').push(value);
    else values.set(arg, value);
  }

  return {
    positionals,
    expectedVersion: values.get('--expect-version'),
    requiredEntries: values.get('--require'),
    assetPath: values.get('--assets'),
    unknown,
  };
}

function main(argv) {
  if (argv.includes('--help') || argv.includes('-h')) {
    console.log(USAGE);
    return;
  }

  const options = parseArgs(argv);
  if (options.unknown.length > 0) {
    console.error(USAGE);
    fail(`unrecognised argument(s): ${options.unknown.join(', ')}`);
  }

  const [feedPath, assetPath] = options.positionals;
  if (!feedPath) {
    console.error(USAGE);
    process.exit(2);
  }

  const { expectedVersion, requiredEntries } = options;
  if (requiredEntries.length === 0) {
    fail('--require must name at least one <os>-<arch> entry; a feed check with no required entries checks nothing');
  }

  let assets = null;
  if (assetPath) {
    try {
      assets = JSON.parse(fs.readFileSync(assetPath, 'utf8'));
    } catch (error) {
      fail(`could not read --assets ${assetPath}: ${error.message}`);
    }
    // `gh release view --json assets` yields objects, not bare names.
    if (Array.isArray(assets)) {
      assets = assets.map((asset) => (typeof asset === 'string' ? asset : asset?.name));
    }
  }

  let feed;
  try {
    feed = JSON.parse(fs.readFileSync(feedPath, 'utf8'));
  } catch (error) {
    fail(`could not read ${feedPath} as JSON: ${error.message}`);
  }

  const problems = verifyFeed(feed, { expectedVersion, requiredEntries, assets });
  if (problems.length > 0) {
    // Report every problem, not just the first: a macOS leg that failed to
    // merge usually shows up as one missing entry *and* a malformed sibling,
    // and fixing them one release at a time is how this gate gets bypassed.
    for (const problem of problems) console.error(`::error::${problem}`);
    process.exit(1);
  }

  const found = Object.keys(feed.platforms ?? {}).sort();
  console.log(`latest.json ${feed.version} verified: ${found.length} entries (${found.join(', ')}).`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2));
}