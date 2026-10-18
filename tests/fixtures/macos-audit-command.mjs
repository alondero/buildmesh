import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import process from 'node:process';

function valueAfter(args, flag) {
  const index = args.indexOf(flag);
  if (index < 0 || !args[index + 1]) throw new Error(`missing ${flag}`);
  return args[index + 1];
}

export function runMock(command, args) {
  const arch = process.env.AUDIT_TEST_ARCH;
  const version = process.env.AUDIT_TEST_VERSION;
  switch (command) {
    case 'gh': {
      if (args[0] === 'release' && args[1] === 'download') {
        const dir = valueAfter(args, '--dir');
        const dmgName = `Buildmesh_${version}_${arch === 'x86_64' ? 'x64' : arch}.dmg`;
        const manifestName = `SHA256SUMS-macos-${arch}.txt`;
        const dmgPath = path.join(dir, dmgName);
        const digest = createHash('sha256').update('mock DMG bytes').digest('hex');
        writeFileSync(dmgPath, 'mock DMG bytes');
        writeFileSync(path.join(dir, manifestName), `${digest}  ${dmgName}\n`);
        return;
      }
      if (args[0] === 'release' && args[1] === 'upload') {
        const source = args.find((arg) => arg.endsWith('.json'));
        if (!source) throw new Error('release upload did not receive its audit JSON');
        const destination = path.join(process.env.AUDIT_TEST_UPLOAD_DIR, path.basename(source));
        copyFileSync(source, destination);
        return;
      }
      throw new Error(`unexpected gh invocation: ${args.join(' ')}`);
    }
    case 'hdiutil': {
      if (args[0] === 'attach') {
        const mount = valueAfter(args, '-mountpoint');
        const app = path.join(mount, 'Buildmesh.app');
        const contents = path.join(app, 'Contents');
        mkdirSync(path.join(contents, 'MacOS'), { recursive: true });
        writeFileSync(path.join(contents, 'MacOS', 'buildmesh'), 'mock executable');
        writeFileSync(path.join(contents, 'Info.plist'), [
          '<?xml version="1.0" encoding="UTF-8"?>',
          '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
          '<plist version="1.0"><dict>',
          '<key>CFBundleIdentifier</key><string>com.alond.buildmesh</string>',
          `<key>CFBundleShortVersionString</key><string>${version}</string>`,
          '<key>CFBundleExecutable</key><string>buildmesh</string>',
          `<key>LSMinimumSystemVersion</key><string>${arch === 'aarch64' ? '11.0' : '10.13'}</string>`,
          '</dict></plist>',
        ].join('\n'));
        return;
      }
      if (args[0] === 'detach') return;
      throw new Error(`unexpected hdiutil invocation: ${args.join(' ')}`);
    }
    case 'codesign':
      if (args[0] === '-dv') process.stderr.write('Executable=/Buildmesh.app/Contents/MacOS/buildmesh\nSignature=adhoc\n');
      return;
    case 'find':
      process.stdout.write(path.join(args[0], 'Buildmesh.app'));
      return;
    case 'lipo':
      process.stdout.write(arch === 'aarch64' ? 'arm64' : 'x86_64');
      return;
    case 'shasum': {
      const file = args.at(-1);
      const digest = createHash('sha256').update(readFileSync(file)).digest('hex');
      process.stdout.write(`${digest}  ${file}\n`);
      return;
    }
    case 'sw_vers':
      process.stdout.write('15.0\n');
      return;
    case 'uname':
      process.stdout.write(arch === 'aarch64' ? 'arm64\n' : 'x86_64\n');
      return;
    default:
      throw new Error(`unexpected mocked command: ${command}`);
  }
}
