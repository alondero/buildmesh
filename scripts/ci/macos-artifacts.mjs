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

export function macOSLocalUpdaterArchiveName(architecture) {
  return `Buildmesh_${releaseArch(architecture)}.app.tar.gz`;
}
