import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { macOSDmgName, macOSLocalUpdaterArchiveName, macOSUpdaterArchiveName } from '../../scripts/ci/macos-artifacts.mjs';

const workflow = readFileSync(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8');
const verifyWorkflow = readFileSync(new URL('../../.github/workflows/verify.yml', import.meta.url), 'utf8');
const candidateAudit = readFileSync(new URL('../../scripts/ci/audit-macos-candidate.mjs', import.meta.url), 'utf8');
const candidateAuditPath = fileURLToPath(new URL('../../scripts/ci/audit-macos-candidate.mjs', import.meta.url));

/** The body of one top-level `job:` block, so one job's steps are not read as another's. */
function jobBlock(jobId) {
  const marker = `\n  ${jobId}:\n`;
  const start = workflow.indexOf(marker);
  assert.notEqual(start, -1, `release.yml has no \`${jobId}\` job`);
  const rest = workflow.slice(start + marker.length);
  const end = rest.search(/\n {2}[a-z][\w-]*:\n/);
  return end === -1 ? workflow.slice(start) : rest.slice(0, end);
}

/**
 * A job body with whole-line comments removed. Every assertion below is about
 * configuration, so scanning prose would make these tests fail when a comment
 * is reworded and pass when a setting is wrong — the exact inversion of what
 * they are for. Inline comments are kept, because `uses: x@sha # v1.0.0` is a
 * config line.
 */
function jobBody(jobId) {
  return jobBlock(jobId)
    .split('\n')
    .filter((line) => !/^\s*#/.test(line))
    .join('\n');
}

function readConfig(name) {
  return JSON.parse(readFileSync(new URL(`../../src-tauri/${name}`, import.meta.url), 'utf8'));
}

const ARM_CONFIG = 'tauri.macos-release.conf.json';
const INTEL_CONFIG = 'tauri.macos-release-intel.conf.json';

test('the macOS release job builds both release architectures', () => {
  const job = jobBody('macos-release');
  assert.match(job, /runs-on: \$\{\{ matrix\.runner \}\}/);
  assert.match(job, /target: aarch64-apple-darwin/);
  assert.match(job, /target: x86_64-apple-darwin/);
  // Each leg names its own runner and overlay; a matrix leg that inherited the
  // other's architecture would silently publish a duplicate under a wrong name.
  assert.match(job, /- runner: macos-15\n\s+target: aarch64-apple-darwin\n\s+config: src-tauri\/tauri\.macos-release\.conf\.json\n\s+arch: aarch64/);
  assert.match(job, /- runner: macos-15-intel\n\s+target: x86_64-apple-darwin\n\s+config: src-tauri\/tauri\.macos-release-intel\.conf\.json\n\s+arch: x86_64/);
});

test('the macOS release job uses the configured native runner labels', () => {
  const job = jobBody('macos-release');
  const runners = [...job.matchAll(/runner: (\S+)/g)].map((match) => match[1]);
  assert.deepEqual(runners.sort(), ['macos-15', 'macos-15-intel']);
});

test('published Intel filenames use Tauri x64 names while architecture evidence stays x86_64', () => {
  assert.equal(macOSDmgName('1.5.0', 'aarch64'), 'Buildmesh_1.5.0_aarch64.dmg');
  assert.equal(macOSDmgName('1.5.0', 'x86_64'), 'Buildmesh_1.5.0_x64.dmg');
  assert.equal(macOSUpdaterArchiveName('1.5.0', 'x86_64'), 'Buildmesh_1.5.0_x64.app.tar.gz');
  assert.equal(macOSLocalUpdaterArchiveName(), 'Buildmesh.app.tar.gz');
});

test('native PR verification exercises the audit CLI and reports real Intel bundle filenames', () => {
  const marker = '\n  macos-candidate-audit-test:\n';
  const start = verifyWorkflow.indexOf(marker);
  assert.notEqual(start, -1, 'verify.yml has no macOS candidate audit job');
  const job = verifyWorkflow.slice(start);
  assert.match(job, /if: github\.event_name == 'pull_request'/);
  assert.match(job, /runs-on: macos-15-intel/);
  assert.match(job, /node --test tests\/agent-infra\/macos-candidate-audit\.test\.mjs/);
  assert.match(job, /npm run tauri -- build --debug --target x86_64-apple-darwin/);
  assert.match(job, /Buildmesh_\$\{version\}_x64\.dmg/);
  assert.match(job, /Buildmesh\.app\.tar\.gz/);
});

// The load-bearing invariant of the whole change: three jobs write one
// latest.json, and tauri-action merges it with a delete-then-upload.
test('only one macOS leg builds at a time, so the shared feed cannot race', () => {
  const job = jobBody('macos-release');
  assert.match(job, /max-parallel: 1/);
  // macos-release is ordered behind the Windows job, which creates the draft.
  assert.match(job, /needs: release/);
});

test('the Windows job is ordered ahead of every other feed writer', () => {
  assert.match(jobBody('macos-release'), /needs: release/);
  assert.match(jobBody('updater-feed'), /needs: \[release, macos-release, macos-candidate-audit\]/);
});

test('the draft DMGs are downloaded and audited natively after upload', () => {
  const job = jobBody('macos-candidate-audit');
  assert.match(job, /needs: macos-release/);
  assert.match(job, /- runner: macos-15\n\s+arch: aarch64\n\s+minimum: "11\.0"/);
  assert.match(job, /- runner: macos-15-intel\n\s+arch: x86_64\n\s+minimum: "10\.13"/);
  assert.match(job, /node --test tests\/agent-infra\/macos-candidate-audit\.test\.mjs/);
  assert.match(job, /audit-macos-candidate\.mjs/);
  assert.match(candidateAudit, /gh', \[/);
  assert.match(candidateAudit, /'release', 'download'/);
  assert.match(candidateAudit, /'hdiutil'/);
  assert.match(candidateAudit, /'codesign'/);
  assert.match(candidateAudit, /'lipo'/);
  assert.match(candidateAudit, /'gh', \['release', 'upload'/);
  assert.match(candidateAudit, /browserDownloadAndQuarantine: 'MANUAL_REQUIRED/);
  assert.match(candidateAudit, /finderLaunch: 'MANUAL_REQUIRED/);
  assert.match(candidateAudit, /gatekeeperApproval: 'MANUAL_REQUIRED/);
  assert.doesNotMatch(job, /latest\.json|uploadUpdaterJson/);
});

test('candidate audit entrypoint rejects an unsupported host or architecture before release operations', () => {
  const expectedFailure = process.platform === 'darwin'
    ? /unsupported architecture invalid/
    : /must run on a native macOS runner/;
  const result = spawnSync(process.execPath, [
    candidateAuditPath,
    '--tag', 'v1.5.0',
    '--commit', 'a'.repeat(40),
    '--arch', 'invalid',
    '--minimum-macos', '11.0',
    '--repo', 'alondero/buildmesh',
  ], { encoding: 'utf8' });
  assert.equal(result.status, 1);
  assert.match(result.stderr, expectedFailure);
  assert.doesNotMatch(result.stderr, /\n\s+at .+\.mjs:/, 'CLI errors should not include an uncaught stack trace');
});

test('exactly the two jobs that merge the feed declare an updater json upload', () => {
  const writers = [...workflow.matchAll(/uploadUpdaterJson: (\w+)/g)].map((match) => match[1]);
  // A third writer would be another concurrent reader/writer of one asset.
  assert.deepEqual(writers, ['true', 'true'], 'expected the Windows job and the macOS job only');
});

test('the macOS job uses the same pinned tauri-action as the Windows job', () => {
  const pinned = /tauri-apps\/tauri-action@([0-9a-f]{40}) # v(\S+)/g;
  const mac = [...jobBody('macos-release').matchAll(pinned)];
  const windows = [...jobBody('release').matchAll(pinned)];
  assert.equal(mac.length, 1);
  assert.equal(windows.length, 1);
  assert.equal(mac[0][1], windows[0][1], 'the two jobs must run the same action build');
  assert.equal(mac[0][2], windows[0][2]);
});

test('the macOS job asks for the ad-hoc overlay and a native target', () => {
  const job = jobBody('macos-release');
  assert.match(job, /args: --target \$\{\{ matrix\.target \}\} --config \$\{\{ matrix\.config \}\}/);
});

test('the macOS job reuses the app-owned minisign key, not an Apple credential', () => {
  const job = jobBody('macos-release');
  assert.match(job, /TAURI_SIGNING_PRIVATE_KEY: \$\{\{ secrets\.TAURI_SIGNING_PRIVATE_KEY \}\}/);
  assert.match(job, /TAURI_SIGNING_PRIVATE_KEY_PASSWORD: \$\{\{ secrets\.TAURI_SIGNING_PRIVATE_KEY_PASSWORD \}\}/);
});

// "without Apple membership" is the whole point of the slice: a paid-credential
// requirement would make this unmergeable, and a stray one would make a release
// depend on an account nobody set up.
test('no job or config asks for an Apple credential, team or notarization', () => {
  const forbidden = [
    'APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID', 'APPLE_SIGNING_IDENTITY',
    'APPLE_API_KEY', 'APPLE_API_KEY_ID', 'APPLE_API_ISSUER', 'AC_PASSWORD',
    'notarization', 'appleId', 'teamId',
  ];
  for (const name of [ARM_CONFIG, INTEL_CONFIG]) {
    const raw = readFileSync(new URL(`../../src-tauri/${name}`, import.meta.url), 'utf8');
    for (const term of forbidden) {
      assert.ok(!raw.includes(term), `${name} must not reference ${term}`);
    }
  }
  for (const jobId of ['release', 'macos-release', 'android-release', 'updater-feed']) {
    const job = jobBody(jobId);
    for (const term of forbidden) {
      assert.ok(!job.includes(term), `release.yml job \`${jobId}\` must not reference ${term}`);
    }
  }
});

test('both macOS overlays sign ad-hoc and produce updater artifacts', () => {
  for (const name of [ARM_CONFIG, INTEL_CONFIG]) {
    const config = readConfig(name);
    assert.equal(config.bundle.createUpdaterArtifacts, true, `${name} must create the .app.tar.gz + .sig`);
    // `-` is the ad-hoc identity: a local signature with no certificate behind it.
    assert.equal(config.bundle.macOS.signingIdentity, '-', `${name} must sign ad-hoc`);
  }
});

test('each overlay declares the minimum macOS its architecture can actually target', () => {
  const arm = readConfig(ARM_CONFIG).bundle.macOS.minimumSystemVersion;
  const intel = readConfig(INTEL_CONFIG).bundle.macOS.minimumSystemVersion;
  // Apple Silicon hardware starts at macOS 11, so Tauri's 10.13 default floor is
  // not a linkable deployment target for that architecture.
  assert.equal(arm, '11.0');
  // Intel keeps Tauri's documented minimum.
  assert.equal(intel, '10.13');
});

test('the overlays are opt-in and do not turn signing on for ordinary CI', () => {
  // tauri-action auto-merges `tauri.<platform>.conf.json` for every macOS
  // build. These files are named `tauri.macos-release.*` on purpose, so the
  // weekly package smoke keeps building unsigned and updater-free.
  for (const name of ['tauri.macos.conf.json', 'tauri.macos.conf.json5', 'Tauri.macos.toml']) {
    assert.throws(
      () => readFileSync(new URL(`../../src-tauri/${name}`, import.meta.url), 'utf8'),
      `${name} would silently ad-hoc-sign and updater-sign every macOS build`
    );
  }
});

test('the Windows signing path is untouched by the macOS overlays', () => {
  const job = jobBody('release');
  assert.match(job, /args: --config src-tauri\/tauri\.release\.conf\.json/);
  assert.ok(!job.includes('macos-release'), 'the Windows job must not pick up the macOS overlay');
  // Windows still gets its own checksum manifest, unchanged.
  assert.match(job, /SHA256SUMS\.txt/);
});

test('checksum manifests are per-platform so no job can overwrite another', () => {
  const mac = jobBody('macos-release');
  // The Windows manifest keeps its established name; each macOS leg gets its own
  // arch-qualified file, so the legs cannot clobber each other or Windows.
  assert.match(jobBody('release'), /Set-Content SHA256SUMS\.txt/);
  assert.match(mac, /SHA256SUMS-macos-\$\{\{ matrix\.arch \}\}\.txt/);
  assert.equal(mac.includes('SHA256SUMS.txt'), false, 'the macOS job must never write the Windows manifest');
});

test('the macOS checksum step covers both the DMG and the updater archive', () => {
  const job = jobBody('macos-release');
  assert.match(job, /-name '\*\.dmg'/);
  assert.match(job, /-name '\*\.app\.tar\.gz'/);
  // shasum is the macOS spelling of sha256sum and emits the same two-space
  // format the Windows manifest uses.
  assert.match(job, /shasum -a 256/);
});

test('the macOS job asserts the signature is ad-hoc rather than trusting codesign', () => {
  const job = jobBody('macos-release');
  assert.match(job, /codesign --verify --deep --strict/);
  assert.match(job, /grep -q 'Signature=adhoc'/);
  // The declared minimum version is recorded as evidence, not assumed.
  assert.match(job, /LSMinimumSystemVersion/);
});

test('the feed check runs after every packaging job and names all three platforms', () => {
  const job = jobBody('updater-feed');
  assert.match(job, /needs: \[release, macos-release, macos-candidate-audit\]/);
  assert.match(job, /verify-updater-feed\.mjs/);
  for (const entry of ['windows-x86_64', 'darwin-aarch64', 'darwin-x86_64']) {
    assert.ok(job.includes(`--require ${entry}`), `updater-feed must require ${entry}`);
  }
  // The version comes from the tag, so a feed from the wrong release is caught.
  assert.match(job, /--expect-version "\$\{TAG#v\}"/);
});

test('the feed check reads the real draft feed rather than a rebuild', () => {
  const job = jobBody('updater-feed');
  assert.match(job, /gh release download "\$TAG" --pattern latest\.json/);
  // And cross-checks the feed's urls against the assets that actually exist.
  assert.match(job, /--assets "\$RUNNER_TEMP\/assets\.json"/);
});

test('the macOS job leaves the release body to the job that loaded it', () => {
  // The notes come from the Windows job's `release-notes` step, which the macOS
  // job does not have. Referencing it here would resolve to an empty string;
  // harmless while the release stays a draft, and it blanks the published notes
  // the day releaseDraft is turned off.
  assert.equal(jobBody('macos-release').includes('releaseBody'), false);
  assert.match(jobBody('release'), /releaseBody: \$\{\{ steps\.release-notes\.outputs\.body \}\}/);
});

test('the macOS steps avoid bash builtins macOS does not ship', () => {
  // /bin/bash on a macOS runner is still 3.2. `mapfile` and `declare -A` do not
  // exist there, so a step that uses one fails on the runner rather than in
  // review — and it fails after the build, which is the expensive place to
  // discover it.
  const job = jobBody('macos-release');
  for (const builtin of ['mapfile', 'readarray', 'declare -A', 'declare -a']) {
    assert.equal(job.includes(builtin), false, `macos-release must not use \`${builtin}\` (bash 3.2)`);
  }
  // The portable idiom it uses instead.
  assert.match(job, /while IFS= read -r asset; do/);
});

test('Android delivery is preserved', () => {
  const job = jobBody('android-release');
  assert.match(job, /needs: release/);
  assert.match(job, /Buildmesh_\$\{TAG#v\}_android\.apk/);
  assert.match(job, /\.sha256/);
});
