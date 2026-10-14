import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { createServer as createHttpServer } from 'node:http';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const ps = process.platform === 'win32' ? 'powershell.exe' : 'pwsh';
const environment = { ...process.env };
delete environment.FORCE_COLOR;

function run(command, args, options = {}) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, { env: environment, windowsHide: true, ...options });
    let output = '';
    let launchedAt;
    child.stdout.on('data', chunk => {
      output += chunk;
      if (!launchedAt && /Launched PID: \d+/.test(output)) launchedAt = Date.now();
    });
    child.stderr.on('data', chunk => { output += chunk; });
    const timer = setTimeout(() => {
      child.kill(); child.stdout.destroy(); child.stderr.destroy();
      reject(new Error(`CLI did not return: ${output}`));
    }, 60000);
    child.on('error', error => { clearTimeout(timer); reject(error); });
    child.on('close', code => { clearTimeout(timer); resolvePromise({ code, output, launchedAt, endedAt: Date.now() }); });
  });
}

function fixture() {
  mkdirSync(join(root, '.tmp'), { recursive: true });
  const dir = mkdtempSync(join(root, '.tmp', `bm-dev-launch-${process.pid}-`));
  mkdirSync(join(dir, 'scripts'));
  for (const name of ['dev-server.mjs', 'run-dev.ps1', 'run-dev.sh', 'launcher-common.ps1']) {
    copyFileSync(join(root, 'scripts', name), join(dir, 'scripts', name));
  }
  symlinkSync(join(root, 'node_modules'), join(dir, 'node_modules'), process.platform === 'win32' ? 'junction' : 'dir');
  writeFileSync(join(dir, 'index.html'), '<html><body>detached fixture</body></html>');
  return dir;
}

async function freePort() {
  const server = createServer();
  await new Promise((resolvePromise, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolvePromise);
  });
  const port = server.address().port;
  await new Promise(resolvePromise => server.close(resolvePromise));
  return port;
}

function alive(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}

test('detached Vite returns, survives its CLI, and stops only its worktree server', { timeout: 120000 }, async () => {
  const first = fixture();
  const second = fixture();
  const cli = dir => join(dir, 'scripts', 'dev-server.mjs');
  const state = dir => JSON.parse(readFileSync(join(dir, '.harness', 'dev-server.json'), 'utf8'));
  try {
    const port = await freePort();
    let result = await run(process.execPath, [cli(first), 'start', '--port', String(port)]);
    assert.equal(result.code, 0, result.output);
    assert.match(result.output, /OK - Vite ready/);
    const owned = state(first);
    assert.equal(alive(owned.pid), true);
    assert.match(await (await fetch(owned.url)).text(), /detached fixture/);

    result = await run(process.execPath, [cli(first), 'start', '--port', String(port)]);
    assert.equal(result.code, 0, result.output);
    assert.equal(state(first).pid, owned.pid, 'repeat start reuses the recorded owner');

    result = await run(process.execPath, [cli(first), 'start', '--port', String(await freePort())]);
    assert.equal(result.code, 1, result.output);
    assert.match(result.output, /run npm run dev:stop first/);
    assert.equal(state(first).pid, owned.pid, 'a confirmed owner must retain its record');

    mkdirSync(join(second, '.harness'));
    writeFileSync(join(second, '.harness', 'dev-server.json'), JSON.stringify({
      pid: owned.pid, controlPort: await freePort(), token: 'stale', port,
    }));

    result = await run(process.execPath, [cli(second), 'start', '--port', String(port)]);
    assert.equal(result.code, 1, 'an occupied port cannot be adopted');
    assert.match(result.output, /Discarding unconfirmed dev server record/);
    assert.match(result.output, /Starting Vite PID:/);
    assert.equal(existsSync(join(second, '.harness', 'dev-server.json')), false);
    assert.equal(alive(owned.pid), true);

    result = await run(process.execPath, [cli(second), 'start', '--port', String(await freePort())]);
    assert.equal(result.code, 0, result.output);
    const other = state(second);
    result = await run(process.execPath, [cli(first), 'stop']);
    assert.equal(result.code, 0, result.output);
    assert.equal(alive(owned.pid), false);
    assert.equal(alive(other.pid), true, 'stop leaves the other worktree running');
    assert.equal((await fetch(other.url)).ok, true);
  } finally {
    for (const dir of [first, second]) {
      const stopped = await run(process.execPath, [cli(dir), 'stop']);
      assert.equal(stopped.code, 0, stopped.output);
      // Let pending child-process handle cleanup run during bounded removal.
      await rm(dir, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
    }
  }
});

test('a stale PID record cannot stop an unrelated live process', { timeout: 60000 }, async () => {
  const dir = fixture();
  try {
    mkdirSync(join(dir, '.harness'));
    writeFileSync(join(dir, '.harness', 'dev-server.json'), JSON.stringify({
      pid: process.pid, controlPort: await freePort(), token: 'stale', port: 3999,
    }));
    const result = await run(process.execPath, [join(dir, 'scripts', 'dev-server.mjs'), 'stop']);
    assert.equal(result.code, 1, result.output);
    assert.equal(alive(process.pid), true);
    assert.equal(JSON.parse(readFileSync(join(dir, '.harness', 'dev-server.json'), 'utf8')).pid, process.pid);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

for (const failure of ['unavailable', 'forbidden', 'invalid JSON', 'wrong owner', 'timeout']) {
  test(`unconfirmed live PID recovery: ${failure}`, { timeout: 90000 }, async () => {
    const dir = fixture();
    const cli = join(dir, 'scripts', 'dev-server.mjs');
    const statePath = join(dir, '.harness', 'dev-server.json');
    const endpoint = createHttpServer((_request, response) => {
      if (failure === 'timeout') return;
      if (failure === 'forbidden') response.writeHead(403).end();
      else if (failure === 'invalid JSON') response.end('invalid JSON');
      else response.end(JSON.stringify({ pid: process.pid, token: 'wrong' }));
    });
    try {
      let controlPort = await freePort();
      if (failure !== 'unavailable') {
        await new Promise(resolvePromise => endpoint.listen(0, '127.0.0.1', resolvePromise));
        controlPort = endpoint.address().port;
      }
      const port = await freePort();
      mkdirSync(join(dir, '.harness'));
      writeFileSync(statePath, JSON.stringify({ pid: process.pid, controlPort, token: 'stale', port }));
      // Timeout is covered by start below; avoid spending a second timeout here.
      if (failure !== 'timeout') {
        const stopped = await run(process.execPath, [cli, 'stop']);
        assert.equal(stopped.code, 1, stopped.output);
        assert.match(stopped.output, /ownership could not be confirmed for stop/);
        if (failure === 'forbidden') assert.match(stopped.output, /HTTP 403/);
        if (failure === 'invalid JSON') assert.match(stopped.output, /invalid JSON response/);
        assert.doesNotMatch(stopped.output, /Unexpected end of JSON/);
        assert.equal(JSON.parse(readFileSync(statePath, 'utf8')).pid, process.pid);
      }
      const started = await run(process.execPath, [cli, 'start', '--port', String(port)]);
      assert.equal(started.code, 0, started.output);
      assert.match(started.output, /Discarding unconfirmed dev server record/);
      assert.match(started.output, /ownership could not be confirmed for status/);
      const replacement = JSON.parse(readFileSync(statePath, 'utf8'));
      assert.notEqual(replacement.pid, process.pid);
      assert.equal(alive(process.pid), true, 'recovery must leave the unrelated PID alive');
      assert.equal(alive(replacement.pid), true);
      assert.match(await (await fetch(replacement.url)).text(), /detached fixture/);
      const stopped = await run(process.execPath, [cli, 'stop']);
      assert.equal(stopped.code, 0, stopped.output);
      assert.equal(alive(replacement.pid), false);
      assert.equal(alive(process.pid), true);
    } finally {
      if (existsSync(statePath) && JSON.parse(readFileSync(statePath, 'utf8')).pid !== process.pid) {
        const stopped = await run(process.execPath, [cli, 'stop']);
        assert.equal(stopped.code, 0, stopped.output);
      }
      endpoint.closeAllConnections();
      await new Promise(resolvePromise => endpoint.close(resolvePromise));
      await rm(dir, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
    }
  });
}

test('out-of-range ports are invalid values for a known option', async () => {
  const result = await run(process.execPath, [join(root, 'scripts', 'dev-server.mjs'), 'start', '--port', '70000']);
  assert.equal(result.code, 1, result.output);
  assert.match(result.output, /Invalid value for --port/);
  assert.doesNotMatch(result.output, /Unknown option/);
});

test('Vite readiness timeout kills its new child and leaves no ownership record', { timeout: 60000 }, async () => {
  const dir = fixture();
  try {
    // The real Vite config boundary stays pending, so no listen receipt arrives.
    writeFileSync(join(dir, 'vite.config.mjs'), `import { writeFileSync } from 'node:fs';\nwriteFileSync(${JSON.stringify(join(dir, 'worker-pid'))}, String(process.pid));\nexport default async () => { await new Promise(() => {}); };\n`);
    const result = await run(process.execPath, [join(dir, 'scripts', 'dev-server.mjs'), 'start', '--port', String(await freePort()), '--timeout-seconds', '3']);
    assert.equal(result.code, 1, result.output);
    assert.match(result.output, /readiness timed out/);
    const pid = Number(result.output.match(/Starting Vite PID: (\d+)/)?.[1]);
    assert.ok(pid, result.output);
    assert.equal(alive(pid), false);
    assert.throws(() => readFileSync(join(dir, '.harness', 'dev-server.json')), { code: 'ENOENT' });
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

// Exercise the shipped launcher and a long-lived fixture executable. The build
// is stubbed: this proves process/handle/readiness behavior without native UI.
const launchOutcomes = ['ready', 'rotated', 'timeout', 'panic'];
if (process.platform === 'win32') launchOutcomes.push('interactive');
for (const outcome of launchOutcomes) {
  test(`run-dev ${outcome === 'interactive' ? 'interactive' : `detached ${outcome}`}: bounded return and launched PID lifecycle`, { timeout: 90000 }, async () => {
    const dir = fixture();
    const readinessSeconds = 6;
    let pid;
    try {
      const binaryDir = join(dir, 'src-tauri', 'target', 'release-dev', 'release');
      const fixtureHome = join(dir, 'home');
      const dataDir = process.platform === 'darwin' ? join(fixtureHome, 'Library', 'Application Support') : join(dir, 'data');
      const logDir = join(dataDir, 'com.alond.buildmesh.dev', 'logs');
      mkdirSync(binaryDir, { recursive: true });
      mkdirSync(logDir, { recursive: true });
      if (outcome === 'rotated') writeFileSync(join(logDir, 'buildmesh.log'), 'old startup ready\nold data\nold tail\n');
      const preload = join(dir, 'app.cjs');
      writeFileSync(preload, `if (process.execPath.includes('buildmesh-dev') || process.platform !== 'win32') {\nconst fs = require('node:fs');\nfs.writeFileSync(${JSON.stringify(join(dir, 'app-pid'))}, String(process.pid));\nfs.writeFileSync(${JSON.stringify(join(dir, 'launch-env.json'))}, JSON.stringify({backtrace: process.env.RUST_BACKTRACE, marker: process.env.CLAUDE_CODE_CHILD_SESSION}));\n${['ready', 'interactive'].includes(outcome) ? `fs.appendFileSync(${JSON.stringify(join(logDir, 'buildmesh.log'))}, 'fixture ready\\n');` : outcome === 'rotated' ? `fs.writeFileSync(${JSON.stringify(join(logDir, 'buildmesh.log'))}, 'fixture ready\\n');` : outcome === 'panic' ? `fs.appendFileSync(${JSON.stringify(join(logDir, 'panic.log'))}, 'fixture panic\\n');` : ''}\nsetInterval(() => console.log('still alive'), 1000);\n}\n`);
      let result;
      if (process.platform === 'win32') {
        copyFileSync(process.execPath, join(binaryDir, 'buildmesh-dev.exe'));
        const wrapper = join(dir, 'launch.ps1');
        const quote = value => `'${value.replaceAll("'", "''")}'`;
        // Assert normal app visibility at the real launch boundary, then hide
        // only this console fixture through the cmdlet adapter.
        writeFileSync(wrapper, `function npm { $global:LASTEXITCODE = 0 }\nfunction Get-Process { param($Name, $Id) if ($Id) { Microsoft.PowerShell.Management\\Get-Process -Id $Id -ErrorAction SilentlyContinue } }\nfunction Start-Process { param($FilePath, [switch]$PassThru, $WindowStyle) if ($WindowStyle -eq 'Hidden') { throw 'Dev GUI visibility must not be forced hidden' }; Microsoft.PowerShell.Management\\Start-Process -FilePath $FilePath -PassThru:$PassThru -WindowStyle Hidden }\n& ${quote(join(dir, 'scripts', 'run-dev.ps1'))} ${outcome === 'interactive' ? '' : '-Detach'} -ReadinessTimeoutSeconds ${readinessSeconds}\nexit $LASTEXITCODE\n`);
        result = await run(ps, ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', wrapper], {
          env: { ...environment, APPDATA: join(dir, 'data'), NODE_OPTIONS: `--require "${preload.replaceAll('\\', '/')}"`, CLAUDE_CODE_CHILD_SESSION: 'must-be-cleared' },
        });
      } else {
        // npm is a shell function so the build is replaced at the executable boundary.
        const binary = join(binaryDir, 'buildmesh-dev');
        const shellQuote = value => "'" + value.replaceAll("'", "'\\''") + "'";
        writeFileSync(binary, `#!/usr/bin/env bash\nexec ${shellQuote(process.execPath)} --require ${shellQuote(preload)}\n`, { mode: 0o755 });
        const wrapper = join(dir, 'launch.sh');
        writeFileSync(wrapper, `function npm() { return 0; }; export -f npm\nexec bash ${shellQuote(join(dir, 'scripts', 'run-dev.sh'))} --detach --readiness-timeout-seconds ${readinessSeconds}\n`);
        result = await run('bash', [wrapper], {
          env: { ...environment, HOME: fixtureHome, XDG_DATA_HOME: dataDir, CLAUDE_CODE_CHILD_SESSION: 'must-be-cleared' },
        });
      }
      assert.match(result.output, /Launched PID: /);
      const stderrPath = join(dir, '.harness', 'logs', 'dev-app.stderr.log');
      assert.equal(existsSync(join(dir, 'app-pid')), true, result.output + (existsSync(stderrPath) ? readFileSync(stderrPath, 'utf8') : ''));
      pid = Number(readFileSync(join(dir, 'app-pid'), 'utf8'));
      assert.match(result.output, new RegExp(`Launched PID: ${pid}`));
      assert.match(result.output, /Log: /);
      const ready = ['ready', 'rotated', 'interactive'].includes(outcome);
      assert.ok(result.launchedAt, 'the launcher must report its PID');
      // Configured readiness budget, three-second Unix cleanup budget, and
      // one second for host scheduling; successful readiness needs no cleanup.
      assert.ok(result.endedAt - result.launchedAt < (readinessSeconds + (ready ? 1 : 4)) * 1000, result.output);
      assert.equal(result.code, ready ? 0 : 1, result.output);
      assert.equal(alive(pid), ready, result.output);
      const env = JSON.parse(readFileSync(join(dir, 'launch-env.json'), 'utf8'));
      assert.equal(env.backtrace, '1');
      assert.equal(env.marker, undefined);
      if (ready) assert.match(result.output, outcome === 'interactive' ? /OK - Buildmesh Dev running/ : /OK - Buildmesh Dev running detached/);
      else assert.match(result.output, outcome === 'panic' ? /panic detected/ : /readiness timed out/);
    } finally {
      if (!pid && existsSync(join(dir, 'app-pid'))) pid = Number(readFileSync(join(dir, 'app-pid'), 'utf8'));
      if (pid && alive(pid)) {
        process.kill(pid);
        const deadline = Date.now() + 5000;
        while (alive(pid) && Date.now() < deadline) await new Promise(resolvePromise => setTimeout(resolvePromise, 50));
        assert.equal(alive(pid), false, 'fixture app must exit before deleting its executable');
      }
      await rm(dir, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
    }
  });
}
