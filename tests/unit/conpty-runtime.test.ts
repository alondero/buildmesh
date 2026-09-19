import { afterEach, describe, expect, it } from 'vitest';
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { packageFiles, stageRuntime, verifyPackage } from '../../scripts/prepare-conpty.mjs';

const scratch: string[] = [];
afterEach(async () => {
  await Promise.all(scratch.splice(0).map(path => rm(path, { recursive: true, force: true })));
});

describe('Windows ConPTY distribution', () => {
  it('rejects an unverified archive and unsupported architecture', () => {
    expect(() => verifyPackage(Buffer.from('corrupt download'))).toThrow('checksum mismatch');
    expect(() => packageFiles('riscv64')).toThrow('Unsupported Windows ConPTY architecture');
  });

  it.each([
    ['x86_64', 'x64'], ['aarch64', 'arm64'], ['x86', 'x86'],
  ])('stages the %s DLL and native hosts for bundles and Cargo test binaries', async (rustArch, dllArch) => {
    const dir = await mkdtemp(join(tmpdir(), 'buildmesh-conpty-'));
    scratch.push(dir);
    const extracted = join(dir, 'package');
    for (const arch of ['x86', 'x64', 'arm64']) {
      const native = join(extracted, `runtimes/win-${arch}/native`);
      const host = join(extracted, `build/native/runtimes/${arch}`);
      await mkdir(native, { recursive: true });
      await mkdir(host, { recursive: true });
      await writeFile(join(native, 'conpty.dll'), `dll-${arch}`);
      await writeFile(join(host, 'OpenConsole.exe'), `host-${arch}`);
    }
    const bundle = join(dir, 'bundle');
    const profile = join(dir, 'debug');
    const destinations = [bundle, profile, join(profile, 'deps')];
    await stageRuntime(extracted, rustArch, destinations);
    for (const destination of destinations) {
      expect(await readFile(join(destination, 'conpty.dll'), 'utf8')).toBe(`dll-${dllArch}`);
      for (const arch of ['x86', 'x64', 'arm64']) {
        expect(await readFile(join(destination, arch, 'OpenConsole.exe'), 'utf8')).toBe(`host-${arch}`);
      }
      expect(await readFile(join(destination, 'conpty-LICENSE.txt'), 'utf8')).toContain('MIT License');
    }
    const dll = join(profile, 'conpty.dll');
    const before = (await stat(dll)).mtimeMs;
    await stageRuntime(extracted, rustArch, destinations);
    expect((await stat(dll)).mtimeMs).toBe(before);
    await rm(join(extracted, 'build/native/runtimes/arm64/OpenConsole.exe'));
    await writeFile(join(extracted, `runtimes/win-${dllArch}/native/conpty.dll`), 'incomplete update');
    await expect(stageRuntime(extracted, rustArch, destinations)).rejects.toThrow();
    expect(await readFile(dll, 'utf8')).toBe(`dll-${dllArch}`);
  });

  it('packages the runtime beside the executable on Windows only', async () => {
    const windows = JSON.parse(await readFile('src-tauri/tauri.windows.conf.json', 'utf8'));
    const common = JSON.parse(await readFile('src-tauri/tauri.conf.json', 'utf8'));
    expect(windows.bundle.resources).toEqual({ 'conpty/runtime/': './' });
    expect(common.bundle.resources).toBeUndefined();
  });
});
