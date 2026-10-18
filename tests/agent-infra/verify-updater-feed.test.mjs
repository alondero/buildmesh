import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  assetNameFromUrl,
  normaliseAssets,
  parseArgs,
  resolveAssets,
  signatureProblems,
  verifyFeed,
} from '../../scripts/ci/verify-updater-feed.mjs';

const SCRIPT = fileURLToPath(new URL('../../scripts/ci/verify-updater-feed.mjs', import.meta.url));

// A minisign signature: two base64 payload lines bracketed by comment lines,
// which is what `tauri build` writes to `<archive>.sig` and what tauri-action
// copies into latest.json. The payloads are base64-encoded rather than
// hand-written so their length is always a multiple of four — the checker
// rejects a payload that is not, exactly as a real truncated signature would.
function payload(text) {
  return Buffer.from(text, 'utf8').toString('base64');
}

function minisign() {
  return [
    'untrusted comment: signature from minisign secret key',
    payload('ed25519 signature over the updater archive'),
    'trusted comment: timestamp:1755500000\tfile:Buildmesh.app.tar.gz',
    payload('global signature over the trusted comment line'),
  ].join('\n');
}

const ENTRIES = {
  'windows-x86_64': {
    signature: minisign(),
    url: 'https://api.github.com/repos/alondero/buildmesh/releases/assets/11',
  },
  'darwin-aarch64': {
    signature: minisign(),
    url: 'https://api.github.com/repos/alondero/buildmesh/releases/assets/22',
  },
  'darwin-x86_64': {
    signature: minisign(),
    url: 'https://api.github.com/repos/alondero/buildmesh/releases/assets/33',
  },
};

const REQUIRED = ['windows-x86_64', 'darwin-aarch64', 'darwin-x86_64'];

function feed(platforms = ENTRIES, overrides = {}) {
  return {
    version: '1.5.0',
    notes: 'Buildmesh v1.5.0',
    pub_date: '2026-10-18T10:00:00.000Z',
    platforms,
    ...overrides,
  };
}

test('a complete Windows + both-Mac feed verifies clean', () => {
  const problems = verifyFeed(feed(), { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  assert.deepEqual(problems, []);
});

test('a feed missing a macOS architecture names the entry it is missing', () => {
  // The failure this gate exists for: the macOS leg uploaded its archive but
  // its entry never made it into the merged feed.
  const platforms = { ...ENTRIES };
  delete platforms['darwin-aarch64'];
  const problems = verifyFeed(feed(platforms), { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /missing the "darwin-aarch64" entry/);
});

test('a Windows-only feed fails once macOS is required', () => {
  const platforms = { 'windows-x86_64': ENTRIES['windows-x86_64'] };
  const problems = verifyFeed(feed(platforms), { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  assert.equal(problems.filter((p) => /missing the "darwin-/.test(p)).length, 2);
});

test('a feed for the wrong version is rejected even when every entry is present', () => {
  const problems = verifyFeed(feed(ENTRIES, { version: '1.4.1' }), {
    expectedVersion: '1.5.0',
    requiredEntries: REQUIRED,
  });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /reports version "1.4.1", expected "1.5.0"/);
});

test('an entry with an empty signature is rejected', () => {
  const platforms = { ...ENTRIES, 'darwin-aarch64': { ...ENTRIES['darwin-aarch64'], signature: '' } };
  const problems = verifyFeed(feed(platforms), { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /signature is missing or empty/);
});

test('a truncated signature is rejected, not accepted as "some base64"', () => {
  const platforms = {
    ...ENTRIES,
    'darwin-x86_64': { ...ENTRIES['darwin-x86_64'], signature: 'untrusted comment: x\nQUJD' },
  };
  const problems = verifyFeed(feed(platforms), { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /expected a minisign signature \(2 base64 lines\)/);
});

test('a non-base64 payload is rejected', () => {
  const problems = signatureProblems(
    'untrusted comment: signature\nnot valid base64!!!\ntrusted comment: t\nalso not base64 %%%',
    'entry',
  );
  assert.equal(problems.length, 1);
  assert.match(problems[0], /not valid base64/);
});

test('base64 with no minisign comment framing is rejected', () => {
  const problems = signatureProblems('QUJDREVG\nR0hJSktM', 'entry');
  assert.equal(problems.length, 1);
  assert.match(problems[0], /no 'untrusted comment:' line/);
});

test('a non-https url is rejected', () => {
  const platforms = {
    ...ENTRIES,
    'darwin-aarch64': { ...ENTRIES['darwin-aarch64'], url: 'http://example.invalid/app.tar.gz' },
  };
  const problems = verifyFeed(feed(platforms), { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /url is not https/);
});

test('an entry pointing at an asset the release does not carry is rejected', () => {
  // Catches a signature that landed without its archive, or a url left over
  // from the previous release. tauri-action v1.0.0 writes the *api* form, whose
  // trailing segment is an asset id, so the catalog is indexed by id here.
  const assets = [
    { name: 'Buildmesh_1.5.0_x64-setup.exe', url: 'https://api.github.com/repos/o/r/releases/assets/11' },
    { name: 'Buildmesh_1.5.0_aarch64.dmg', url: 'https://api.github.com/repos/o/r/releases/assets/22' },
  ];
  const problems = verifyFeed(feed(), {
    expectedVersion: '1.5.0',
    requiredEntries: REQUIRED,
    assets,
  });
  // The catalog holds ids 11 and 22; darwin-x86_64 points at 33.
  assert.equal(problems.length, 1);
  assert.match(problems[0], /"darwin-x86_64": points at "33", which is not an asset on this release/);
});

test('an api-id entry resolves against the release ids, not its names', () => {
  // The regression this guards: matching the trailing segment against asset
  // *names* would reject every real v1.0.0 feed, whose urls end in an id.
  const assets = [
    { name: 'Buildmesh_1.5.0_x64-setup.exe', url: 'https://api.github.com/repos/o/r/releases/assets/11' },
    { name: 'Buildmesh_1.5.0_aarch64.app.tar.gz', url: 'https://api.github.com/repos/o/r/releases/assets/22' },
    { name: 'Buildmesh_1.5.0_x86_64.app.tar.gz', url: 'https://api.github.com/repos/o/r/releases/assets/33' },
  ];
  const problems = verifyFeed(feed(), { expectedVersion: '1.5.0', requiredEntries: REQUIRED, assets });
  assert.deepEqual(problems, []);
});

test('a browser-download filename entry resolves against the release names', () => {
  // The other form tauri-action has used, so the check survives the pinned
  // action changing which url it writes.
  const platforms = {
    'windows-x86_64': { signature: minisign(), url: 'https://github.com/o/r/releases/download/v1.5.0/Buildmesh_1.5.0_x64-setup.exe' },
    'darwin-aarch64': { signature: minisign(), url: 'https://github.com/o/r/releases/download/v1.5.0/Buildmesh_1.5.0_aarch64.app.tar.gz' },
    'darwin-x86_64': { signature: minisign(), url: 'https://github.com/o/r/releases/download/v1.5.0/Buildmesh_1.5.0_x86_64.app.tar.gz' },
  };
  const assets = [
    { name: 'Buildmesh_1.5.0_x64-setup.exe', url: 'https://api.github.com/repos/o/r/releases/assets/11' },
    { name: 'Buildmesh_1.5.0_aarch64.app.tar.gz', url: 'https://api.github.com/repos/o/r/releases/assets/22' },
    { name: 'Buildmesh_1.5.0_x86_64.app.tar.gz', url: 'https://api.github.com/repos/o/r/releases/assets/33' },
  ];
  const problems = verifyFeed(feed(platforms), { expectedVersion: '1.5.0', requiredEntries: REQUIRED, assets });
  assert.deepEqual(problems, []);
});

test('two architectures sharing one artifact are rejected', () => {
  const shared = 'https://api.github.com/repos/alondero/buildmesh/releases/assets/22';
  const platforms = {
    ...ENTRIES,
    'darwin-aarch64': { signature: minisign(), url: shared },
    'darwin-x86_64': { signature: minisign(), url: shared },
  };
  const problems = verifyFeed(feed(platforms), { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /shares its url with "darwin-aarch64"/);
});

test('an unparseable pub_date is rejected', () => {
  const problems = verifyFeed(feed(ENTRIES, { pub_date: 'not-a-date' }), {
    expectedVersion: '1.5.0',
    requiredEntries: REQUIRED,
  });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /unparseable "pub_date"/);
});

test('a feed with no platforms object stops there instead of inventing entry problems', () => {
  const problems = verifyFeed({ version: '1.5.0' }, { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  // The structural complaint is the useful one; there is nothing to say about
  // the entries once there is no platforms object to read them from.
  assert.equal(problems.length, 2, 'a missing pub_date and a missing platforms object are both real');
  assert.ok(problems.some((p) => /no "platforms" object/.test(p)));
  assert.ok(problems.every((p) => !/missing the "/.test(p)), 'no per-entry noise once there are no entries');
});

test('a non-object feed is rejected rather than throwing', () => {
  assert.deepEqual(verifyFeed(null, { requiredEntries: REQUIRED }), ['latest.json is not a JSON object']);
  assert.deepEqual(verifyFeed([], { requiredEntries: REQUIRED }), ['latest.json is not a JSON object']);
});

test('asset names are read from api and browser url forms alike', () => {
  assert.equal(assetNameFromUrl('https://api.github.com/repos/o/r/releases/assets/123'), '123');
  assert.equal(
    assetNameFromUrl('https://github.com/o/r/releases/download/v1.5.0/Buildmesh_1.5.0_aarch64.dmg'),
    'Buildmesh_1.5.0_aarch64.dmg',
  );
  assert.equal(assetNameFromUrl('https://example.invalid/a/b/app.tar.gz?token=x'), 'app.tar.gz');
});

test('gh release assets and bare names normalise to the same list', () => {
  assert.deepEqual(normaliseAssets([{ name: 'a.dmg' }, 'b.dmg']), ['a.dmg', 'b.dmg']);
  assert.equal(normaliseAssets(null), null);
});

test('the asset index carries both ids and names', () => {
  const index = resolveAssets([
    { name: 'a.dmg', url: 'https://api.github.com/repos/o/r/releases/assets/7' },
    'b.dmg',
  ]);
  assert.deepEqual([...index.ids], ['7']);
  assert.deepEqual([...index.names].sort(), ['a.dmg', 'b.dmg']);
  assert.equal(resolveAssets(null), null);
});

test('argv parsing separates positionals from repeated and single-value flags', () => {
  const options = parseArgs([
    'feed.json',
    '--expect-version', '1.5.0',
    '--require', 'windows-x86_64',
    '--require', 'darwin-aarch64',
    '--assets', 'assets.json',
  ]);
  assert.deepEqual(options.positionals, ['feed.json']);
  assert.equal(options.expectedVersion, '1.5.0');
  assert.deepEqual(options.requiredEntries, ['windows-x86_64', 'darwin-aarch64']);
  assert.equal(options.assetPath, 'assets.json');
  assert.deepEqual(options.unknown, []);
});

test('a flag with no value is reported rather than silently swallowing the next flag', () => {
  const options = parseArgs(['feed.json', '--require', '--assets', 'assets.json']);
  assert.deepEqual(options.requiredEntries, []);
  assert.deepEqual(options.unknown, ['--require (missing value)']);
});

test('the script exits 0 on a good feed and 1 on an incomplete one', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'updater-feed-'));
  const goodPath = path.join(dir, 'good.json');
  const badPath = path.join(dir, 'bad.json');
  try {
    fs.writeFileSync(goodPath, JSON.stringify(feed()));
    const platforms = { ...ENTRIES };
    delete platforms['darwin-aarch64'];
    fs.writeFileSync(badPath, JSON.stringify(feed(platforms)));

    const out = execFileSync(process.execPath, [
      SCRIPT, goodPath,
      '--expect-version', '1.5.0',
      '--require', 'windows-x86_64',
      '--require', 'darwin-aarch64',
      '--require', 'darwin-x86_64',
    ], { encoding: 'utf8' });
    assert.match(out, /latest\.json 1\.5\.0 verified/);
    assert.match(out, /darwin-aarch64/);

    let status = 0;
    let stderr = '';
    try {
      execFileSync(process.execPath, [
        SCRIPT, badPath,
        '--expect-version', '1.5.0',
        '--require', 'windows-x86_64',
        '--require', 'darwin-aarch64',
        '--require', 'darwin-x86_64',
      ], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
    } catch (error) {
      status = error.status;
      stderr = error.stderr;
    }
    assert.equal(status, 1, 'an incomplete feed must fail the step, not warn');
    assert.match(stderr, /::error::latest\.json is missing the "darwin-aarch64" entry/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('the script refuses to check nothing when no entry is required', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'updater-feed-'));
  const feedPath = path.join(dir, 'good.json');
  try {
    fs.writeFileSync(feedPath, JSON.stringify(feed()));
    let status = 0;
    let stderr = '';
    try {
      execFileSync(process.execPath, [SCRIPT, feedPath], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'pipe'],
      });
    } catch (error) {
      status = error.status;
      stderr = error.stderr;
    }
    assert.equal(status, 1);
    assert.match(stderr, /--require must name at least one/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});