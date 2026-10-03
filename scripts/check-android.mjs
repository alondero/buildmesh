#!/usr/bin/env node
import { readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { runGuarded } from './ci/run-guarded.mjs';

export function executedAndroidTests(reports) {
  let count = 0;
  for (const xml of reports) {
    const suite = xml.match(/<testsuite\b[^>]*>/)?.[0];
    if (!suite) throw new Error('Missing Android test suite');
    const number = key => Number(suite.match(new RegExp(`\\b${key}="(\\d+)"`))?.[1] ?? 0);
    if (number('failures') || number('errors') || number('skipped')) throw new Error('Android tests failed or were skipped');
    count += number('tests');
  }
  if (count === 0) throw new Error('No Android tests executed');
  return count;
}

async function main() {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const command = process.platform === 'win32'
    ? ['cmd.exe', '/d', '/c', 'android\\gradlew.bat', '-p', 'android']
    : ['bash', 'android/gradlew', '-p', 'android'];
  command.push('assembleDebug', 'testDebugUnitTest', 'lintDebug', 'assembleDebugAndroidTest', '--console=plain', '--rerun-tasks');
  const code = await runGuarded({ minutes: 20, killGraceSeconds: 10, label: 'Android checks', command, cwd: root, env: process.env });
  if (code !== 0) { process.exitCode = code; return; }
  const directory = new URL('../android/app/build/test-results/testDebugUnitTest/', import.meta.url);
  const reports = readdirSync(directory).filter(name => name.endsWith('.xml')).map(name => readFileSync(new URL(name, directory), 'utf8'));
  process.stdout.write(`Android tests: ${executedAndroidTests(reports)} passed\n`);
  const liveCommand = command.slice(0, process.platform === 'win32' ? 6 : 4);
  liveCommand.push('assembleLive', 'assembleLiveAndroidTest', '-Pbuildmesh.testBuildType=live', '--console=plain');
  const liveCode = await runGuarded({ minutes: 20, killGraceSeconds: 10, label: 'Live Android test compilation', command: liveCommand, cwd: root, env: process.env });
  if (liveCode !== 0) process.exitCode = liveCode;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
}
