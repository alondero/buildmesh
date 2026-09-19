// Keep the DLL and console host from one verified Microsoft package. The Windows
// inbox host can flush synchronized-update markers before its rendered output.
import { createHash } from 'node:crypto';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

export const VERSION = '1.24.260710001';
export const PACKAGE_SHA256 = '175640566a3b59c4b132070ee96c2c77e5ab7edd2e92732a5eb3610bbf63d90e';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');

export function verifyPackage(bytes) {
  if (createHash('sha256').update(bytes).digest('hex') !== PACKAGE_SHA256) {
    throw new Error(`ConPTY ${VERSION}: package checksum mismatch`);
  }
}

export function packageFiles(rustArch) {
  const arch = { x86_64: 'x64', aarch64: 'arm64', x86: 'x86' }[rustArch];
  if (!arch) throw new Error(`Unsupported Windows ConPTY architecture: ${rustArch}`);
  // Emulated x86/x64 processes need the native host on ARM64 as well. Ship
  // Microsoft's host layout so the DLL chooses the OS architecture itself.
  return [
    [`runtimes/win-${arch}/native/conpty.dll`, 'conpty.dll'],
    ...['x86', 'x64', 'arm64'].map(host => [
      `build/native/runtimes/${host}/OpenConsole.exe`, `${host}/OpenConsole.exe`,
    ]),
  ];
}

async function copyChanged(source, target) {
  const bytes = await readFile(source);
  try {
    if (bytes.equals(await readFile(target))) return;
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  await mkdir(dirname(target), { recursive: true });
  await copyFile(source, target);
}

export async function stageRuntime(extracted, rustArch, destinations) {
  const files = packageFiles(rustArch);
  // Validate the complete pair before changing any destination.
  await Promise.all(files.map(([source]) => readFile(join(extracted, source))));
  for (const destination of destinations) {
    for (const [source, target] of files) {
      await copyChanged(join(extracted, source), join(destination, target));
    }
    await copyChanged(join(root, 'scripts/conpty-LICENSE.txt'), join(destination, 'conpty-LICENSE.txt'));
  }
}

async function prepare(rustArch, profileDir) {
  packageFiles(rustArch);
  const cache = join(root, 'src-tauri/target/conpty', VERSION);
  await mkdir(cache, { recursive: true });
  const archive = join(cache, 'package.zip');
  let bytes;
  try {
    bytes = await readFile(archive);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    const url = `https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/${VERSION}/microsoft.windows.console.conpty.${VERSION}.nupkg`;
    const response = await fetch(url, { signal: AbortSignal.timeout(60000) });
    if (!response.ok) throw new Error(`ConPTY download failed: HTTP ${response.status}`);
    bytes = Buffer.from(await response.arrayBuffer());
    verifyPackage(bytes);
    await writeFile(archive, bytes);
  }
  verifyPackage(bytes);
  const extracted = join(cache, 'extracted');
  execFileSync('powershell.exe', ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command',
    '$ErrorActionPreference = "Stop"; Expand-Archive -LiteralPath $env:BUILDMESH_CONPTY_ARCHIVE -DestinationPath $env:BUILDMESH_CONPTY_EXTRACT -Force',
  ], {
    windowsHide: true,
    env: { ...process.env, BUILDMESH_CONPTY_ARCHIVE: archive, BUILDMESH_CONPTY_EXTRACT: extracted },
    stdio: 'inherit',
  });
  const resources = join(root, 'src-tauri/conpty/runtime');
  await stageRuntime(extracted, rustArch, [resources, profileDir, join(profileDir, 'deps')]);
  const arch = { x86_64: 'x64', aarch64: 'arm64', x86: 'x86' }[rustArch];
  await copyChanged(join(extracted, `runtimes/win-${arch}/lib/uap10.0/conpty.lib`), join(profileDir, 'conpty.lib'));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [, , arch, profile] = process.argv;
  if (!arch || !profile) throw new Error('Usage: node scripts/prepare-conpty.mjs <Rust architecture> <Cargo profile directory>');
  await prepare(arch, resolve(profile));
}
