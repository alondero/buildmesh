import { appendFileSync, copyFileSync, mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import process from 'node:process';

const ASSET_BYTES = new Map([
  ['Buildmesh_1.5.0_x64.dmg', 'published DMG bytes'],
  ['Buildmesh_1.5.0_x64.app.tar.gz', 'published updater bytes'],
]);

export function runMock(args) {
  const callsPath = process.env.CHECKSUM_TEST_CALLS;
  if (!callsPath) throw new Error('missing CHECKSUM_TEST_CALLS');
  appendFileSync(callsPath, `${JSON.stringify(args)}\n`);

  if (args[0] === 'release' && args[1] === 'download') {
    if (args.length !== 7 || args[2] !== process.env.CHECKSUM_TEST_TAG || args[3] !== '--pattern' || args[5] !== '--dir') {
      throw new Error(`unexpected gh release download arguments: ${args.join(' ')}`);
    }
    const name = args[4];
    const bytes = ASSET_BYTES.get(name);
    if (bytes === undefined) throw new Error(`unexpected release asset: ${name}`);
    if (name === process.env.CHECKSUM_TEST_FAIL_ASSET) throw new Error(`mock download failed for ${name}`);
    mkdirSync(args[6], { recursive: true });
    writeFileSync(path.join(args[6], name), bytes);
    return;
  }

  if (args[0] === 'release' && args[1] === 'upload') {
    if (args.length !== 5 || args[2] !== process.env.CHECKSUM_TEST_TAG || args[4] !== '--clobber') {
      throw new Error(`unexpected gh release upload arguments: ${args.join(' ')}`);
    }
    const uploadDir = process.env.CHECKSUM_TEST_UPLOADS;
    if (!uploadDir) throw new Error('missing CHECKSUM_TEST_UPLOADS');
    mkdirSync(uploadDir, { recursive: true });
    copyFileSync(args[3], path.join(uploadDir, path.basename(args[3])));
    return;
  }

  throw new Error(`unexpected gh invocation: ${args.join(' ')}`);
}
