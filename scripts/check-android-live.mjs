#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { runGuarded } from './ci/run-guarded.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const appId = 'dev.buildmesh.remote.live';
const usage = 'node scripts/check-android-live.mjs --device SERIAL --origin https://LAN-IP:2992 [--bridge http://127.0.0.1:2991] [--http http://127.0.0.1:2992]';

export function liveOptions(args) {
  const values = { bridge: 'http://127.0.0.1:2991', http: 'http://127.0.0.1:2992' };
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i].slice(2);
    if (!args[i].startsWith('--') || !['device', 'origin', 'bridge', 'http'].includes(key) || !args[i + 1] || args[i + 1].startsWith('--')) throw new Error(usage);
    values[key] = args[i + 1];
  }
  if (!values.device || !/^[\w.:-]+$/.test(values.device) || !values.origin) throw new Error(usage);
  for (const key of ['origin', 'bridge', 'http']) {
    const url = new URL(values[key]);
    if (url.username || url.password || url.search || url.hash || url.pathname !== '/') throw new Error(`${key} must be an origin without credentials, query or fragment`);
    if (key === 'origin') {
      if (url.protocol !== 'https:') throw new Error('Live Android acceptance requires HTTPS');
    } else if (url.protocol !== 'http:' || !['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname)) {
      throw new Error(`${key} must be a loopback HTTP origin`);
    }
    values[key] = url.origin;
  }
  return values;
}

export function liveResult(output) {
  const deviceId = Number(output.match(/INSTRUMENTATION_STATUS: buildmeshDeviceId=(\d+)/)?.[1]);
  return { deviceId: Number.isSafeInteger(deviceId) && deviceId > 0 ? deviceId : null, passed: /\bOK \(1 test\)/.test(output) && !/FAILURES!!!|INSTRUMENTATION_FAILED/.test(output) };
}

async function main() {
  const options = liveOptions(process.argv.slice(2));
  const identity = randomUUID();
  const directory = path.join(root, '.tmp', 'android-live', identity);
  const repo = path.join(directory, 'repo');
  const meshName = `Android live QA ${identity}`;
  const controllerLabel = `BM live QA ${identity.slice(0, 16)}`;
  mkdirSync(repo, { recursive: true });
  const log = path.join(directory, 'instrumentation.log');
  const adb = (...args) => execFileSync('adb', ['-s', options.device, ...args], { cwd: root, encoding: 'utf8', timeout: 30000, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  const git = (...args) => execFileSync('git', args, { cwd: repo, timeout: 30000, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  const invoke = async (cmd, args = {}) => {
    const response = await fetch(`${options.bridge}/invoke`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ cmd, args }), signal: AbortSignal.timeout(30000) });
    const result = await response.json();
    if (!response.ok || result.error || result.success === false) throw new Error(`Desktop bridge ${cmd} failed: ${result.error || response.status}`);
    return result.data;
  };
  let cookie;
  let controllerId;
  let nativeId;
  let mesh;
  let installed = false;
  let setupComplete = false;
  let failure;
  const admin = async (route, method = 'GET') => {
    const response = await fetch(`${options.http}${route}`, { method, headers: { Cookie: cookie }, signal: AbortSignal.timeout(15000) });
    if (!response.ok) throw new Error(`Desktop ${route} failed (${response.status})`);
    return response.status === 204 ? null : response.json();
  };
  const cleanupErrors = [];
  const cleanup = async (action) => { try { await action(); } catch (error) { cleanupErrors.push(error.message); } };
  try {
    if (adb('get-state').trim() !== 'device') throw new Error('The selected Android device is unavailable');
    // Refuse to overwrite another live QA installation or its private session.
    const packages = adb('shell', 'pm', 'list', 'packages', appId).trim().split(/\r?\n/);
    for (const id of [appId, `${appId}.test`]) {
      if (packages.includes(`package:${id}`)) throw new Error(`Remove the previous ${id} QA installation before starting another run`);
    }
    const command = process.platform === 'win32' ? ['cmd.exe', '/d', '/c', 'android\\gradlew.bat'] : ['bash', 'android/gradlew'];
    command.push('-p', 'android', 'assembleLive', 'assembleLiveAndroidTest', '-Pbuildmesh.testBuildType=live', '--console=plain');
    if (await runGuarded({ minutes: 20, killGraceSeconds: 10, label: 'Live Android build', command, cwd: root, env: process.env }) !== 0) throw new Error('Live Android build failed');
    git('init', '-b', 'main');
    git('config', 'user.name', 'Buildmesh live QA');
    git('config', 'user.email', 'buildmesh-qa@example.invalid');
    writeFileSync(path.join(repo, 'smoke.txt'), 'Initial direct LAN fixture\n');
    git('add', 'smoke.txt');
    git('commit', '-m', 'Initial live fixture');
    writeFileSync(path.join(repo, 'smoke.txt'), 'Android direct LAN verified\n');
    mesh = await invoke('create_test_mesh', { name: meshName, path: repo });
    if (mesh.name !== meshName || path.resolve(mesh.path) !== repo) throw new Error('Desktop returned an unexpected fixture mesh');
    await invoke('update_mesh_use_worktree', { mesh_id: mesh.id, use_worktree: false });
    const node = await invoke('create_agent_node', { meshId: mesh.id, name: 'android-live-terminal', path: repo, provider: 'terminal', useWorktree: false });
    await invoke('spawn_agent', { nodeId: node.id, rows: 24, cols: 120 });
    const controllerTicket = await invoke('create_pairing_ticket');
    const pair = await fetch(`${options.http}/api/pair`, { method: 'POST', headers: { Authorization: `Bearer ${controllerTicket.ticket}`, 'User-Agent': controllerLabel }, signal: AbortSignal.timeout(15000) });
    if (pair.status !== 204 || !pair.headers.get('set-cookie')) throw new Error('Live fixture controller pairing failed');
    cookie = pair.headers.get('set-cookie').split(';')[0];
    const devices = await admin('/admin/devices');
    const controllers = devices.filter(device => device.label === controllerLabel);
    if (controllers.length !== 1) throw new Error('Unable to identify this fixture controller');
    controllerId = controllers[0].id;
    const certificate = await fetch(`${options.http}/__certs/status`, { signal: AbortSignal.timeout(15000) });
    if (!certificate.ok) throw new Error('Desktop certificate status is unavailable');
    const { root_fingerprint_sha256: fingerprint } = await certificate.json();
    if (!fingerprint) throw new Error('Desktop root CA fingerprint is unavailable');
    const { ticket } = await invoke('create_pairing_ticket');
    const fixture = { meshId: mesh.id, nodeId: node.id, beforeDeviceIds: devices.map(device => device.id), marker: `BUILDMESH_LIVE_${Date.now()}`, invitation: `${options.origin}/#pair=${ticket}&ca=${encodeURIComponent(fingerprint)}` };
    adb('install', path.join(root, 'android/app/build/outputs/apk/live/app-live.apk'));
    installed = true;
    adb('install', path.join(root, 'android/app/build/outputs/apk/androidTest/live/app-live-androidTest.apk'));
    adb('shell', 'run-as', appId, 'mkdir', '-p', 'files');
    // Credentials travel through stdin into private app storage, never argv or a host file.
    execFileSync('adb', ['-s', options.device, 'shell', '-T', `run-as ${appId} sh -c 'cat > files/live-desktop.json'`], { input: JSON.stringify(fixture), timeout: 15000, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    setupComplete = true;
    const code = await runGuarded({ minutes: 3, killGraceSeconds: 5, label: 'Live Android acceptance', command: ['adb', '-s', options.device, 'shell', 'am', 'instrument', '-w', '-r', '-e', 'class', 'dev.buildmesh.remote.LiveDesktopTest', `${appId}.test/androidx.test.runner.AndroidJUnitRunner`], cwd: root, env: process.env, log });
    const result = liveResult(readFileSync(log, 'utf8'));
    nativeId = result.deviceId;
    if (code !== 0 || !result.passed || !nativeId) throw new Error(`Live Android acceptance failed; see ${log}`);
    process.stdout.write(`Direct HTTPS Android acceptance: 1 passed; ${log}\n`);
  } catch (error) {
    failure = error;
  } finally {
    if (cookie && !controllerId) await cleanup(async () => {
      const controllers = (await admin('/admin/devices')).filter(device => device.label === controllerLabel);
      if (controllers.length !== 1) throw new Error('Unable to confirm the unfinished QA controller for revocation');
      controllerId = controllers[0].id;
    });
    if (nativeId) await cleanup(async () => {
      if ((await admin('/admin/devices')).some(device => device.id === nativeId)) await admin(`/admin/devices/${nativeId}/revoke`, 'POST');
    });
    else if (setupComplete) cleanupErrors.push('No confirmed native device ID was reported; inspect Authorized Devices for an unfinished QA pairing');
    if (mesh) await cleanup(async () => {
      const ownMesh = (await invoke('list_meshes')).find(item => item.id === mesh.id);
      if (!ownMesh) return;
      if (ownMesh.name !== meshName || path.resolve(ownMesh.path) !== repo) throw new Error('Fixture mesh identity changed; cleanup refused');
      // Stop fixture PTYs through their owning delete command before the mesh cascade.
      for (const node of (await invoke('list_agent_nodes')).filter(item => item.mesh_id === mesh.id)) await invoke('delete_agent_node', { nodeId: node.id });
      await invoke('delete_mesh', { meshId: mesh.id });
    });
    if (controllerId) await cleanup(() => admin(`/admin/devices/${controllerId}/revoke`, 'POST'));
    if (installed) {
      await cleanup(() => { adb('uninstall', `${appId}.test`); });
      await cleanup(() => { adb('uninstall', appId); });
    }
  }
  if (cleanupErrors.length) throw new Error(`${failure ? `${failure.message}; ` : ''}Live QA cleanup requires attention: ${cleanupErrors.join('; ')}`);
  if (failure) throw failure;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
}
