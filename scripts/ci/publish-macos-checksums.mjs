#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { macOSChecksumAssetNames, macOSChecksumRows } from './macos-artifacts.mjs';

const USAGE = 'Usage: node scripts/ci/publish-macos-checksums.mjs <tag> <architecture> <release-assets.json> <download-dir> <manifest-path>';

export function buildManifest({ tag, architecture, releaseAssets, downloadedAssets }) {
  if (!Array.isArray(releaseAssets)) throw new Error('release assets JSON must contain an assets array');
  const releaseAssetNames = releaseAssets.map((asset) => asset?.name);
  if (releaseAssetNames.some((name) => typeof name !== 'string' || basename(name) !== name)) {
    throw new Error('release assets must have plain filenames');
  }
  const sha256ByName = Object.fromEntries(
    Object.entries(downloadedAssets).map(([name, bytes]) => [
      name,
      createHash('sha256').update(bytes).digest('hex'),
    ])
  );
  return `${macOSChecksumRows({ tag, architecture, releaseAssetNames, sha256ByName }).join('\n')}\n`;
}

function main(argv) {
  const [tag, architecture, releaseAssetsPath, downloadDir, manifestPath, ...extra] = argv;
  if (!tag || !architecture || !releaseAssetsPath || !downloadDir || !manifestPath || extra.length) {
    throw new Error(USAGE);
  }

  const release = JSON.parse(readFileSync(releaseAssetsPath, 'utf8'));
  if (!Array.isArray(release.assets)) throw new Error('gh release view JSON did not contain an assets array');
  const releaseAssetNames = release.assets.map((asset) => asset?.name);
  // Check the exact target names against GitHub's current release listing before
  // downloading anything. The checksum manifest must describe published files.
  if (releaseAssetNames.some((name) => typeof name !== 'string' || basename(name) !== name)) {
    throw new Error('release assets must have plain filenames');
  }
  mkdirSync(downloadDir, { recursive: true });

  // The names come from GitHub's release listing after checking them against
  // the canonical names for this architecture.
  const names = macOSChecksumAssetNames({ tag, architecture, releaseAssetNames });
  const wanted = [names.dmg, names.updater];
  for (const name of wanted) {
    execFileSync('gh', ['release', 'download', tag, '--pattern', name, '--dir', downloadDir], { stdio: 'inherit' });
  }

  const downloadedAssets = Object.fromEntries(wanted.map((name) => [name, readFileSync(join(downloadDir, name))]));
  const manifest = buildManifest({ tag, architecture, releaseAssets: release.assets, downloadedAssets });
  writeFileSync(manifestPath, manifest, 'utf8');
  process.stdout.write(manifest);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    const detail = error?.stderr?.toString().trim();
    process.stderr.write(`${detail || error.message}\n`);
    process.exitCode = 1;
  }
}
