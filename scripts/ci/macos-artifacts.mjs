const RELEASE_ARCH = Object.freeze({
  aarch64: 'aarch64',
  x86_64: 'x64',
});

function releaseArch(architecture) {
  const arch = RELEASE_ARCH[architecture];
  if (!arch) throw new Error(`unsupported macOS artifact architecture ${architecture}`);
  return arch;
}

export function macOSDmgName(version, architecture) {
  return `Buildmesh_${version}_${releaseArch(architecture)}.dmg`;
}

export function macOSUpdaterArchiveName(version, architecture) {
  return `Buildmesh_${version}_${releaseArch(architecture)}.app.tar.gz`;
}

export function macOSLocalUpdaterArchiveName() {
  return 'Buildmesh.app.tar.gz';
}

export function macOSChecksumAssetNames({ tag, architecture, releaseAssetNames }) {
  if (!/^v\d+\.\d+\.\d+$/.test(tag)) throw new Error(`unsupported macOS release tag ${tag}`);
  if (!Array.isArray(releaseAssetNames)) throw new Error('release assets must be an array of names');
  if (releaseAssetNames.some((name) => typeof name !== 'string')) throw new Error('release asset names must be strings');

  const listedAssets = new Set(releaseAssetNames);
  const expectedDmg = macOSDmgName(tag.slice(1), architecture);
  const expectedUpdater = macOSUpdaterArchiveName(tag.slice(1), architecture);
  const dmg = [...listedAssets].find((name) => name === expectedDmg);
  const updater = [...listedAssets].find((name) => name === expectedUpdater && name.endsWith('.app.tar.gz'));

  if (!dmg) throw new Error(`release ${tag} does not list expected DMG ${expectedDmg}`);
  if (!updater) {
    const listedUpdaterAssets = [...listedAssets].filter((name) => typeof name === 'string' && name.endsWith('.app.tar.gz'));
    throw new Error(`release ${tag} does not list expected updater archive ${expectedUpdater}; listed: ${listedUpdaterAssets.join(', ') || '(none)'}`);
  }

  return { dmg, updater };
}

export function macOSChecksumRows({ tag, architecture, releaseAssetNames, sha256ByName }) {
  const { dmg, updater } = macOSChecksumAssetNames({ tag, architecture, releaseAssetNames });
  return [dmg, updater].map((name) => {
    const digest = sha256ByName?.[name]?.toLowerCase();
    if (!/^[a-f\d]{64}$/.test(digest ?? '')) throw new Error(`missing SHA-256 digest for published asset ${name}`);
    return `${digest}  ${name}`;
  });
}
