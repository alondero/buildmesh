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
// `--assets` takes what `gh release view --json assets` produces — an object
// with an `assets` array — and additionally proves each feed entry resolves to
// an artifact this release really carries. It has to handle two URL forms,
// because tauri-action v1.0.0 changed which one it writes: `latest.json`
// entries are currently the API form `…/releases/assets/{numeric id}` rather
// than a filename, so a check that compared filenames against those urls would
// match nothing and fail every release.
//
// An unusable `--assets` file is an error, never a skipped check. The bug this
// guards is a verification step that quietly stops verifying.
//
// Exit codes: 0 the feed is complete and well-formed; 1 it is not.

import fs from 'node:fs';
import { pathToFileURL } from 'node:url';

const BASE64_LINE = /^[A-Za-z0-9+/]+={0,2}$/;
const COMMENT_PREFIX = /^(untrusted|trusted) comment:/;

/** True when `text` is a complete minisign signature: both comments and two base64 payload lines. */
function looksLikeMinisign(text) {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  if (!lines.some((line) => /^untrusted comment:/.test(line))) return false;
  if (!lines.some((line) => /^trusted comment:/.test(line))) return false;
  const payloads = lines.filter((line) => !COMMENT_PREFIX.test(line));
  if (payloads.length < 2) return false;
  return payloads.every((line) => BASE64_LINE.test(line) && line.length % 4 === 0);
}

/**
 * Problems with one feed entry's updater signature.
 *
 * The shape is not the two bare base64 lines minisign prints: Tauri stores the
 * signature as a *base64 encoding of the whole minisign output*, which is what
 * actually appears in a published `latest.json` (checked against the live
 * v1.4.1 feed, where the entry is a single unwrapped base64 blob). So both
 * forms are accepted — the encoded blob and, in case a future Tauri stores the
 * raw text, the plain form — and anything that is neither is rejected.
 *
 * Verifying against the real format matters more than it looks: a check that
 * only understood the plain form would fail every valid release, and the unit
 * fixtures would have kept passing.
 */
export function signatureProblems(signature, label) {
  if (typeof signature !== 'string' || signature.trim() === '') {
    return [`${label}: signature is missing or empty — the updater would reject this update`];
  }

  const asText = signature.trim();
  const decoded = Buffer.from(asText, 'base64').toString('utf8');
  const candidates = decoded === asText ? [asText] : [asText, decoded.trim()];

  if (candidates.some((candidate) => looksLikeMinisign(candidate))) {
    return [];
  }

  return [
    `${label}: signature is neither a base64 minisign signature nor raw minisign output. ` +
      'Expected the encoded form Tauri writes, which decodes to an "untrusted comment" line, ' +
      'a base64 payload, a "trusted comment" line and a global signature.',
  ];
}

function isPlainObject(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

// A literal BOM, written this way so the source file itself stays BOM-free.
const BYTE_ORDER_MARK = '﻿';

/**
 * Parse a JSON file, tolerating a UTF-8 BOM. `gh release view --json assets >
 * assets.json` is fine under the bash shell the workflow uses, but the same
 * redirect from PowerShell writes UTF-16 with a BOM, and `JSON.parse` throws on
 * that — which would read as a broken feed rather than an encoding mistake.
 */
function parseJsonFile(file, what) {
  try {
    const raw = fs.readFileSync(file, 'utf8').replace(BYTE_ORDER_MARK, '');
    return JSON.parse(raw);
  } catch (error) {
    // Covers a missing file and a malformed one alike: neither may reach the
    // caller as an uncaught ENOENT or a bare SyntaxError.
    fail(`could not read ${what} ${file}: ${error.message}`);
  }
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
 * The `{os}-{arch}` part of a feed key, dropping the trailing installer
 * variant. Tauri writes both forms: `windows-x86_64` for the primary artifact
 * and `windows-x86_64-msi` / `windows-x86_64-nsis` for the same archives under
 * their bundle names. The primary and its msi variant therefore point at one
 * url on purpose, and only a *different* base sharing a url is a defect.
 */
export function platformBase(key) {
  return String(key).split('-').slice(0, 2).join('-');
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

    // One artifact cannot serve two architectures — but it legitimately serves
    // the primary key and its own installer variant, so compare the `{os}-{arch}`
    // base rather than the full key.
    const owner = urlOwners.get(entry.url);
    if (typeof entry.url === 'string' && owner !== undefined && platformBase(owner) !== platformBase(key)) {
      problems.push(`${label}: shares its url with "${owner}" — one artifact cannot serve two architectures`);
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
 * The asset list out of whatever `--assets` pointed at. `gh release view
 * --json assets` writes an *envelope* — `{"assets":[…]}` — and handing that
 * object straight to the catalog builder would make it look like "no assets
 * given" and skip the check, so the envelope is unwrapped here. A bare array is
 * accepted too. Returns null when the shape is unrecognised, which the caller
 * treats as an error rather than as an absent check.
 */
export function extractAssetList(parsed) {
  if (Array.isArray(parsed)) return parsed;
  if (parsed && typeof parsed === 'object' && Array.isArray(parsed.assets)) return parsed.assets;
  return null;
}

/**
 * Index a release's assets by both things a `latest.json` entry might refer to
 * them as: the numeric id in the API url form, and the filename in the browser
 * download form. Returns null when there is nothing to check against.
 *
 * The numeric REST asset id is taken from `apiUrl` only. In the real
 * `gh release view --json assets` payload each asset also has an `id`, but that
 * is the *GraphQL node* id (`RA_kwDOSMYi3c4mDtm6`), not the number tauri-action
 * writes into `latest.json` — indexing by it would resolve nothing.
 */
export function resolveAssets(assets) {
  const list = extractAssetList(assets);
  if (list === null) return null;
  const ids = new Set();
  const names = new Set();
  for (const asset of list) {
    if (typeof asset === 'string') {
      if (asset !== '') names.add(asset);
      continue;
    }
    if (!asset || typeof asset !== 'object') continue;
    if (typeof asset.name === 'string' && asset.name !== '') names.add(asset.name);
    // apiUrl ends in the numeric id; url is the browser download form and ends
    // in the filename. Indexing both, discriminated by shape, covers whichever
    // form a feed entry happens to use.
    for (const url of [asset.apiUrl, asset.url]) {
      const segment = assetNameFromUrl(url);
      if (segment === null) continue;
      if (/^\d+$/.test(segment)) ids.add(segment);
      else names.add(segment);
    }
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

  const { expectedVersion, requiredEntries, assetPath } = options;
  const [feedPath] = options.positionals;
  if (!feedPath) {
    console.error(USAGE);
    process.exit(2);
  }

  if (requiredEntries.length === 0) {
    fail('--require must name at least one <os>-<arch> entry; a feed check with no required entries checks nothing');
  }

  let assets = null;
  if (assetPath) {
    // Read from `options.assetPath`, never from the positionals: `--assets` is a
    // valued flag, so it never appears there, and reading the wrong one silently
    // downgraded this to a feed check with no asset verification at all.
    assets = parseJsonFile(assetPath, '--assets');
    if (resolveAssets(assets) === null) {
      // An unrecognised shape must not read as "no assets supplied" — that is
      // the silent-skip this whole branch exists to prevent.
      fail(
        `--assets ${assetPath} is not a release asset list. Expected \`gh release view --json assets\` output ` +
          '(an object with an "assets" array) or a bare array. Refusing to run without the asset check.'
      );
    }
  }

  const feed = parseJsonFile(feedPath, 'the feed');

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