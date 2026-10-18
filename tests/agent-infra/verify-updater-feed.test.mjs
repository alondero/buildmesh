import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  assetNameFromUrl,
  extractAssetList,
  parseArgs,
  platformBase,
  resolveAssets,
  signatureProblems,
  verifyFeed,
} from '../../scripts/ci/verify-updater-feed.mjs';

const SCRIPT = fileURLToPath(new URL('../../scripts/ci/verify-updater-feed.mjs', import.meta.url));

// A minisign signature, in the shape that actually ships. Tauri stores the
// signature as a base64 encoding of minisign's four-line output, which is what
// a published `latest.json` really contains — not the bare lines. The first
// fixture below is a byte-for-byte copy of the v1.4.1 feed's entry so the
// format is pinned to reality rather than to an assumption.
const REAL_SIGNATURE =
  'dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUZDBKcldqL2hVdDdZcWxJaHpVZVhMVVZqQ2gydzFqZDRkcUVRM3FiTWZOeVdscUE5SWZqeVZ5RHJyUGNIR3VnQjJlSm55OE93OWRaNHpEc3JBZURPbHdueDRzRjhlblFNPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkxOTMyMzQxCWZpbGU6QnVpbGRtZXNoXzEuNC4xX3g2NF9lbi1VUy5tc2kJdmVyc2lvbjoxLjQuMQpvNTVWYU1xemVUQUp4THIyUE5DMk1pT25uK3pvMWNzSUVpbWpMeDQvSExKdHFJbWxNdUY4UGdJb3dmcGZhMVNHdlVPT3lhUlN3TCtUU2NHTXphcmRCdz09Cg==';

function payload(text) {
  return Buffer.from(text, 'utf8').toString('base64');
}

// The plain minisign output, used to build the encoded form below.
function minisignText() {
  return [
    'untrusted comment: signature from minisign secret key',
    payload('ed25519 signature over the updater archive'),
    'trusted comment: timestamp:1755500000\tfile:Buildmesh.app.tar.gz',
    payload('global signature over the trusted comment line'),
  ].join('\n');
}

// What Tauri actually writes into latest.json: the whole thing, base64'd.
function minisign() {
  return Buffer.from(minisignText(), 'utf8').toString('base64');
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

test('a signature copied from the real v1.4.1 feed is accepted', () => {
  // The regression this pins: the checker used to require two bare base64
  // payload lines, which is minisign's *print* format. Tauri stores the whole
  // thing base64-encoded, so the real entry looked like one opaque line and
  // every published release would have failed.
  assert.deepEqual(signatureProblems(REAL_SIGNATURE, 'entry'), []);
  const decoded = Buffer.from(REAL_SIGNATURE, 'base64').toString('utf8');
  assert.match(decoded, /^untrusted comment:/);
  assert.match(decoded, /trusted comment: timestamp:1791932341/);
});

test('raw minisign output is accepted too, in case a future Tauri stores it', () => {
  assert.deepEqual(signatureProblems(minisignText(), 'entry'), []);
});

test('a truncated signature is rejected, not accepted as "some base64"', () => {
  const platforms = {
    ...ENTRIES,
    'darwin-x86_64': { ...ENTRIES['darwin-x86_64'], signature: 'untrusted comment: x\nQUJD' },
  };
  const problems = verifyFeed(feed(platforms), { expectedVersion: '1.5.0', requiredEntries: REQUIRED });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /neither a base64 minisign signature nor raw minisign output/);
});

test('a non-base64 payload is rejected', () => {
  const problems = signatureProblems(
    'untrusted comment: signature\nnot valid base64!!!\ntrusted comment: t\nalso not base64 %%%',
    'entry',
  );
  assert.equal(problems.length, 1);
  assert.match(problems[0], /neither a base64 minisign signature nor raw minisign output/);
});

test('base64 with no minisign comment framing is rejected', () => {
  const problems = signatureProblems('QUJDREVG\nR0hJSktM', 'entry');
  assert.equal(problems.length, 1);
  assert.match(problems[0], /neither a base64 minisign signature nor raw minisign output/);
});

test('base64 that decodes to something without a trusted comment is rejected', () => {
  // The encoded form is accepted only when it *decodes* to a whole signature;
  // a blob that merely looks like base64 must not pass.
  const partial = Buffer.from('untrusted comment: x\nQUJDREVG\n', 'utf8').toString('base64');
  assert.equal(signatureProblems(partial, 'entry').length, 1);
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

// The real shape of `gh release view --json assets`, trimmed to the fields that
// matter. Captured from an actual release rather than invented, because the
// whole point of these tests is that the catalog matches what gh emits.
function ghAsset(name, numericId) {
  return {
    apiUrl: `https://api.github.com/repos/o/r/releases/assets/${numericId}`,
    contentType: 'application/octet-stream',
    createdAt: '2026-10-13T23:04:55Z',
    // Note: this is the GraphQL *node* id, not the numeric REST asset id. It is
    // present in the payload and must not be used to resolve a feed entry.
    id: 'RA_kwDOSMYi3c4mDtm6',
    label: '',
    name,
    size: 1024,
    state: 'uploaded',
    updatedAt: '2026-10-13T23:04:56Z',
    url: `https://github.com/o/r/releases/download/v1.5.0/${name}`,
  };
}

function ghAssetsEnvelope() {
  return {
    assets: [
      ghAsset('Buildmesh_1.5.0_x64-setup.exe', 11),
      ghAsset('Buildmesh_1.5.0_aarch64.app.tar.gz', 22),
      ghAsset('Buildmesh_1.5.0_x86_64.app.tar.gz', 33),
    ],
  };
}

test('the gh envelope is unwrapped rather than read as an absent catalog', () => {
  assert.equal(extractAssetList(ghAssetsEnvelope()).length, 3);
  assert.deepEqual(extractAssetList(['a.dmg']), ['a.dmg']);
  // An unrecognised shape is null, which the CLI treats as an error.
  assert.equal(extractAssetList({ nope: [] }), null);
  assert.equal(extractAssetList(null), null);
});

test('the catalog takes ids from apiUrl, not from the GraphQL node id', () => {
  const index = resolveAssets(ghAssetsEnvelope());
  // The numeric REST ids tauri-action writes into latest.json.
  assert.deepEqual([...index.ids].sort(), ['11', '22', '33']);
  assert.equal(index.ids.has('RA_kwDOSMYi3c4mDtm6'), false);
  // Names come from both `name` and the browser download url.
  assert.equal(index.names.has('Buildmesh_1.5.0_aarch64.app.tar.gz'), true);
});

test('a feed pointing at real gh asset ids verifies against real gh output', () => {
  const problems = verifyFeed(feed(), {
    expectedVersion: '1.5.0',
    requiredEntries: REQUIRED,
    assets: ghAssetsEnvelope(),
  });
  assert.deepEqual(problems, []);
});

test('an id the release does not carry is rejected against real gh output', () => {
  const envelope = ghAssetsEnvelope();
  envelope.assets = envelope.assets.filter((asset) => !asset.name.includes('x86_64'));
  const problems = verifyFeed(feed(), {
    expectedVersion: '1.5.0',
    requiredEntries: REQUIRED,
    assets: envelope,
  });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /"darwin-x86_64": points at "33", which is not an asset on this release/);
});

test('bare asset names and object assets both index', () => {
  const index = resolveAssets(['a.dmg', { name: 'b.dmg', apiUrl: 'https://api.github.com/repos/o/r/releases/assets/9' }]);
  assert.deepEqual([...index.ids], ['9']);
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

test('the primary entry and its installer variant may share one url', () => {
  // Taken from the real v1.4.1 feed, where tauri-action points
  // `windows-x86_64` and `windows-x86_64-msi` at the same .msi asset on purpose.
  // Comparing whole keys instead of the `{os}-{arch}` base flagged every valid
  // Windows release as a failure.
  const shared = 'https://api.github.com/repos/alondero/buildmesh/releases/assets/638495693';
  const platforms = {
    'windows-x86_64': { signature: minisign(), url: shared },
    'windows-x86_64-msi': { signature: minisign(), url: shared },
    'windows-x86_64-nsis': { signature: minisign(), url: `${shared}63` },
    'darwin-aarch64': { signature: minisign(), url: 'https://api.github.com/repos/alondero/buildmesh/releases/assets/638495700' },
  };
  const problems = verifyFeed(feed(platforms), {
    expectedVersion: '1.5.0',
    requiredEntries: ['windows-x86_64', 'darwin-aarch64'],
  });
  assert.deepEqual(problems, []);
  assert.equal(platformBase('windows-x86_64-msi'), 'windows-x86_64');
  assert.equal(platformBase('darwin-aarch64'), 'darwin-aarch64');
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

// --- end-to-end CLI coverage for --assets -------------------------------
//
// These three exist because the asset check shipped broken three ways at once
// and 44 in-memory assertions missed all of them: `--assets` was read from the
// wrong place, the gh envelope was never unwrapped, and the catalog looked for
// ids in the wrong field. Every one of them made the check silently do nothing
// or fail everything, and none was visible without running the real command
// line the workflow runs.

function runCli(args) {
  try {
    const stdout = execFileSync(process.execPath, [SCRIPT, ...args], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    return { status: 0, stdout, stderr: '' };
  } catch (error) {
    return { status: error.status, stdout: error.stdout ?? '', stderr: error.stderr ?? '' };
  }
}

function withTempDir(files, body) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'updater-feed-cli-'));
  const paths = {};
  for (const [name, content] of Object.entries(files)) {
    paths[name] = path.join(dir, name);
    fs.writeFileSync(paths[name], typeof content === 'string' ? content : JSON.stringify(content));
  }
  try {
    return body(paths);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

const REQUIRES = [
  '--expect-version', '1.5.0',
  '--require', 'windows-x86_64',
  '--require', 'darwin-aarch64',
  '--require', 'darwin-x86_64',
];

test('the CLI honours --assets with real gh output and passes a good feed', () => {
  withTempDir({ 'feed.json': feed(), 'assets.json': ghAssetsEnvelope() }, ({ 'feed.json': feedPath, 'assets.json': assetsPath }) => {
    const result = runCli([feedPath, ...REQUIRES, '--assets', assetsPath]);
    assert.equal(result.status, 0, `expected a clean run, got: ${result.stderr}`);
    assert.match(result.stdout, /latest\.json 1\.5\.0 verified/);
  });
});

test('the CLI fails when a feed entry points at an id the release lacks', () => {
  // The direct regression for the three reported defects: with `--assets` now
  // actually loaded, an unresolvable entry has to fail the step.
  withTempDir({ 'feed.json': feed(), 'assets.json': { assets: [] } }, ({ 'feed.json': feedPath, 'assets.json': assetsPath }) => {
    const result = runCli([feedPath, ...REQUIRES, '--assets', assetsPath]);
    assert.equal(result.status, 1, 'an unresolvable entry must fail the step');
    assert.equal(
      (result.stderr.match(/which is not an asset on this release/g) ?? []).length,
      3,
      'every entry should be reported, not just the first'
    );
  });
});

test('the CLI refuses to skip the asset check when --assets has the wrong shape', () => {
  // Silently degrading to "no asset check" is the failure mode being designed
  // out, so valid JSON of an unrecognised shape must be an error, not a pass.
  for (const bad of [{ nope: [] }, 42]) {
    withTempDir({ 'feed.json': feed(), 'assets.json': bad }, ({ 'feed.json': feedPath, 'assets.json': assetsPath }) => {
      const result = runCli([feedPath, ...REQUIRES, '--assets', assetsPath]);
      assert.equal(result.status, 1, `expected failure for assets.json = ${JSON.stringify(bad)}`);
      assert.match(result.stderr, /Refusing to run without the asset check/);
    });
  }
});

test('the CLI reports unparseable --assets rather than skipping the check', () => {
  withTempDir({ 'feed.json': feed(), 'assets.json': 'not json at all' }, ({ 'feed.json': feedPath, 'assets.json': assetsPath }) => {
    const result = runCli([feedPath, ...REQUIRES, '--assets', assetsPath]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /could not read --assets/);
  });
});

test('the CLI reports a missing --assets file rather than skipping the check', () => {
  withTempDir({ 'feed.json': feed() }, ({ 'feed.json': feedPath }) => {
    const result = runCli([feedPath, ...REQUIRES, '--assets', path.join(path.dirname(feedPath), 'nope.json')]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /could not read --assets/);
  });
});