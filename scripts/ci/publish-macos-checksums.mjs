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

export function createPublishedChecksumManifest({ tag, architecture, releaseAssets, downloadAsset }) {
  if (!Array.isArray(releaseAssets)) throw new Error('release assets JSON must contain an assets array');
  if (typeof downloadAsset !== 'function') throw new Error('downloadAsset must be a function');
  const releaseAssetNames = releaseAssets.map((asset) => asset?.name);
  if (releaseAssetNames.some((name) => typeof name !== 'string' || basename(name) !== name)) {
    throw new Error('release assets must have plain filenames');
  }
  const { dmg, updater } = macOSChecksumAssetNames({ tag, architecture, releaseAssetNames });
  const downloadedAssets = Object.fromEntries([dmg, updater].map((name) => [name, downloadAsset(name)]));
  return buildManifest({ tag, architecture, releaseAssets, downloadedAssets });
}

function main(argv) {
  const [tag, architecture, releaseAssetsPath, downloadDir, manifestPath, ...extra] = argv;
  if (!tag || !architecture || !releaseAssetsPath || !downloadDir || !manifestPath || extra.length) {
    throw new Error(USAGE);
  }

  const release = JSON.parse(readFileSync(releaseAssetsPath, 'utf8'));
  if (!Array.isArray(release.assets)) throw new Error('gh release view JSON did not contain an assets array');
  mkdirSync(downloadDir, { recursive: true });

  const manifest = createPublishedChecksumManifest({
    tag,
    architecture,
    releaseAssets: release.assets,
    downloadAsset(name) {
      execFileSync('gh', ['release', 'download', tag, '--pattern', name, '--dir', downloadDir], { stdio: 'inherit' });
      return readFileSync(join(downloadDir, name));
    },
  });
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
