import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { createServer as createHttpServer } from 'node:http';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

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

function fixture({ blockEsbuild = false } = {}) {
  mkdirSync(join(root, '.tmp'), { recursive: true });
  const dir = mkdtempSync(join(root, '.tmp', `bm-dev-launch-${process.pid}-`));
  mkdirSync(join(dir, 'scripts'));
  for (const name of ['dev-server.mjs', 'run-dev.ps1', 'run-dev.sh', 'launcher-common.ps1', 'check-dev-state.ps1']) {
    copyFileSync(join(root, 'scripts', name), join(dir, 'scripts', name));
  }
  symlinkSync(join(root, 'node_modules'), join(dir, 'node_modules'), process.platform === 'win32' ? 'junction' : 'dir');
  if (blockEsbuild) {
    // A loader hook that makes the `esbuild` specifier unresolvable, which is
    // what a clean install of Vite 8 looks like: esbuild is an optional peer
    // and is not installed at all. Needed because a developer machine can have
    // esbuild resolvable from a parent directory tree, which would hide the
    // failure this test exists to catch.
    writeFileSync(join(dir, 'block-esbuild-hooks.mjs'), [
      'export async function resolve(specifier, context, next) {',
      "  if (specifier === 'esbuild') {",
      "    const error = new Error(\"Cannot find package 'esbuild'\");",
      "    error.code = 'ERR_MODULE_NOT_FOUND';",
      '    throw error;',
      '  }',
      '  return next(specifier, context);',
      '}',
      '',
    ].join('\n'));
    writeFileSync(join(dir, 'block-esbuild.mjs'), [
      "import { register } from 'node:module';",
      "register('./block-esbuild-hooks.mjs', import.meta.url);",
      '',
    ].join('\n'));
  }
  writeFileSync(join(dir, 'index.html'), '<html><body>detached fixture</body></html>');
  return dir;
}

// `start` spawns the worker with the environment it inherited, so the hook has
// to be active when the server is launched, not when `stop` is called.
function blockEsbuildEnv(dir) {
  const hook = pathToFileURL(join(dir, 'block-esbuild.mjs')).href;
  return { ...environment, NODE_OPTIONS: [environment.NODE_OPTIONS, `--import ${hook}`].filter(Boolean).join(' ') };
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

test('stop shuts the server down even when the optional bundler is unresolvable', { timeout: 120000 }, async () => {
  // Vite 8 lists esbuild as an optional peer, so a clean install has no
  // `node_modules/esbuild`. The worker imported it unconditionally on `/stop`,
  // which returned HTTP 500, so `stop` exited 1 without unlinking the ownership
  // record and left the server running (issue #2247 review). With the specifier
  // blocked, a stop must still terminate the server and clear the record.
  const dir = fixture({ blockEsbuild: true });
  const cli = dir => join(dir, 'scripts', 'dev-server.mjs');
  const blocked = blockEsbuildEnv(dir);
  try {
    const port = await freePort();
    let result = await run(process.execPath, [cli(dir), 'start', '--port', String(port)], { env: blocked });
    assert.equal(result.code, 0, result.output);
    const owned = JSON.parse(readFileSync(join(dir, '.harness', 'dev-server.json'), 'utf8'));
    assert.equal(alive(owned.pid), true);

    result = await run(process.execPath, [cli(dir), 'stop'], { env: blocked });
    assert.equal(result.code, 0, result.output);
    assert.equal(alive(owned.pid), false, 'a missing optional bundler must not keep the server running');
    assert.equal(existsSync(join(dir, '.harness', 'dev-server.json')), false, 'stop must clear the ownership record');
  } finally {
    if (existsSync(join(dir, '.harness', 'dev-server.json'))) {
      // A stop that failed leaves the server holding the fixture directory, so
      // removing it can fail with EBUSY. The assertions above already report
      // why; letting that secondary error replace them would hide it.
      try {
        await run(process.execPath, [cli(dir), 'stop'], { env: blocked });
        await rm(dir, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
      } catch {
        // Already reported by the assertions above.
      }
    } else {
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

// Issue #2345 — a linked worktree is the fixture whose `.git` is a FILE (that is
// how `git worktree add` records the link), which is how both launchers tell a
// worktree from the main checkout. Every other fixture in this file is the main
// checkout and keeps the canonical dev profile and ports.
function worktreeFixture() {
  const dir = fixture();
  writeFileSync(join(dir, '.git'), 'gitdir: F:/src/buildmesh/.git/worktrees/fixture\n');
  return dir;
}

// A long-lived fixture app that binds the ports the launcher assigned it. The
// ports are held before it reports ready, so a second worktree's probe meets a
// real listener rather than an assumption.
//
// The fixture has to BE the worktree's executable on every platform, because
// that is how each launcher recognises its own instance: run-dev.ps1 compares
// `Get-Process .Path` and run-dev.sh greps `ps -o command=` for the binary path.
// A wrapper script that execs another binary leaves the live process naming the
// interpreter instead, and every ownership assertion then passes vacuously -
// the launcher reports its own instance as foreign and stops nothing. Copying
// node under the binary's name is the faithful stand-in; the Windows branch
// already did this, which is why the defect only ever showed up on POSIX.
function prepareFixtureBinary(binaryDir) {
  const binary = join(binaryDir, process.platform === 'win32' ? 'buildmesh-dev.exe' : 'buildmesh-dev');
  // Copied once per fixture: a re-launch stops the previous instance inside the
  // launcher, which runs after this fixture is prepared, and Windows refuses to
  // overwrite the image file of a running process.
  if (!existsSync(binary)) copyFileSync(process.execPath, binary);
  // copyFileSync does not carry an executable bit onto the copy on POSIX.
  if (process.platform !== 'win32') chmodSync(binary, 0o755);
}

function stubApp(dir, dataRoot, extraEnv = {}) {
  const binaryDir = join(dir, 'src-tauri', 'target', 'release-dev', 'release');
  mkdirSync(binaryDir, { recursive: true });
  prepareFixtureBinary(binaryDir);
  // node is launched with no script argument, exactly as the real launcher
  // launches the real binary. The preload comes through NODE_OPTIONS and its
  // setInterval holds the event loop open, so the process behaves like a
  // long-running app rather than a REPL that exits on an empty stdin.
  const preloadOption = `--require "${join(dir, 'app.cjs').replaceAll('\\', '/')}"`;
  const envPath = join(dir, 'launch-env.json');
  const preload = join(dir, 'app.cjs');
  writeFileSync(preload, `if (process.execPath.includes('buildmesh-dev') || process.platform !== 'win32') {
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');
const offset = Number(process.env.BUILDMESH_PORT_OFFSET || 0);
const profileDir = process.env.BUILDMESH_PROFILE_DIR;
fs.writeFileSync(${JSON.stringify(join(dir, 'app-pid'))}, String(process.pid));
const listening = [1991, 1992, 1993, 1994].map(base => {
  const server = net.createServer();
  return new Promise(resolve => server.on('error', () => resolve(false)).listen(base + offset, '127.0.0.1', () => resolve(true)));
});
Promise.all(listening).then(bound => {
  fs.writeFileSync(${JSON.stringify(envPath)}, JSON.stringify({
    backtrace: process.env.RUST_BACKTRACE,
    portOffset: process.env.BUILDMESH_PORT_OFFSET,
    profileDir,
    bound,
  }));
  fs.appendFileSync(path.join(profileDir, 'logs', 'buildmesh.log'), 'fixture ready\\n');
  setInterval(() => console.log('still alive'), 1000);
});
}
`);
  if (process.platform === 'win32') {
    // Build stubbed at the npm boundary; Start-Process/Get-Process adapted so the
    // console fixture does not open a visible window. Get-Process answers BOTH
    // -Id and -Name: the launcher's name sweep is how it finds an instance whose
    // ownership record was lost, so a mock that silently dropped -Name would
    // leave that path untested.
    //
    // The readiness window is a hang detector, not a load detector: the fixture
    // app has to start and bind its ports, and how long that takes is set by
    // the machine. Eight seconds was enough for an idle machine and not for a
    // busy one, so an overlapping check failed the launch rather than the
    // behaviour under test (issue #2254). The window is a maximum - a launch
    // that is ready returns as soon as it is ready - so a longer one costs
    // nothing on the happy path.
    const wrapper = join(dir, 'launch.ps1');
    const quote = value => `'${value.replaceAll("'", "''")}'`;
    writeFileSync(wrapper, `function npm { $global:LASTEXITCODE = 0 }\nfunction Get-Process { param($Name, $Id) if ($Id) { Microsoft.PowerShell.Management\\Get-Process -Id $Id -ErrorAction SilentlyContinue } elseif ($Name) { Microsoft.PowerShell.Management\\Get-Process -Name $Name -ErrorAction SilentlyContinue } }\nfunction Start-Process { param($FilePath, [switch]$PassThru, $WindowStyle) if ($WindowStyle -eq 'Hidden') { throw 'Dev GUI visibility must not be forced hidden' }; Microsoft.PowerShell.Management\\Start-Process -FilePath $FilePath -PassThru:$PassThru -WindowStyle Hidden }\n& ${quote(join(dir, 'scripts', 'run-dev.ps1'))} -Detach -ReadinessTimeoutSeconds 30\nexit $LASTEXITCODE\n`);
    return run(ps, ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', wrapper], {
      env: {
        ...environment,
        APPDATA: dataRoot,
        NODE_OPTIONS: preloadOption,
        // A caller who pins the identity must not have it re-derived.
        BUILDMESH_PORT_OFFSET: '',
        BUILDMESH_PROFILE_DIR: '',
        ...extraEnv,
      },
    });
  }
  const shellQuote = value => "'" + value.replaceAll("'", "'\\''") + "'";
  const wrapper = join(dir, 'launch.sh');
  writeFileSync(wrapper, `function npm() { return 0; }; export -f npm\nexec bash ${shellQuote(join(dir, 'scripts', 'run-dev.sh'))} --detach --readiness-timeout-seconds 30\n`);
  return run('bash', [wrapper], {
    env: {
      ...environment,
      HOME: join(dataRoot, 'home'),
      XDG_DATA_HOME: dataRoot,
      NODE_OPTIONS: preloadOption,
      BUILDMESH_PORT_OFFSET: '',
      BUILDMESH_PROFILE_DIR: '',
      ...extraEnv,
    },
  });
}

function instanceRecord(dir) {
  return JSON.parse(readFileSync(join(dir, '.tmp', 'dev-instance.json'), 'utf8'));
}

async function stopFixtureApp(dir) {
  const pidPath = join(dir, 'app-pid');
  if (!existsSync(pidPath)) return;
  const pid = Number(readFileSync(pidPath, 'utf8'));
  if (!alive(pid)) return;
  process.kill(pid);
  const deadline = Date.now() + 5000;
  while (alive(pid) && Date.now() < deadline) await new Promise(resolvePromise => setTimeout(resolvePromise, 50));
  assert.equal(alive(pid), false, `fixture app ${pid} must exit before its executable is deleted`);
}

test('two worktrees each run their own dev instance: distinct profile, distinct ports, neither stops the other', { timeout: 180000 }, async () => {
  const first = worktreeFixture();
  const second = worktreeFixture();
  // One data root, two worktrees: the second launch must move somewhere else
  // inside it, which is the whole point of the per-worktree profile.
  const dataRoot = mkdtempSync(join(root, '.tmp', `bm-dev-appdata-${process.pid}-`));
  const running = [];
  let failure;
  try {
    for (const dir of [first, second]) {
      const result = await stubApp(dir, dataRoot);
      assert.equal(result.code, 0, result.output);
      assert.match(result.output, /OK - Buildmesh Dev running detached/);
      const record = instanceRecord(dir);
      const env = JSON.parse(readFileSync(join(dir, 'launch-env.json'), 'utf8'));
      assert.equal(record.pid, Number(readFileSync(join(dir, 'app-pid'), 'utf8')));
      assert.equal(record.profileDir, env.profileDir);
      assert.equal(Number(record.portOffset), Number(env.portOffset));
      // The identity variables reach the child through the same launch-only
      // scope as RUST_BACKTRACE, and nothing the launcher derives leaks past it.
      assert.equal(env.backtrace, '1', result.output);
      // The child sees what the record says, and the ports it was told to use
      // are the ones it actually bound.
      assert.deepEqual(env.bound, [true, true, true, true], result.output);
      running.push({ dir, pid: record.pid, offset: Number(record.portOffset), profile: env.profileDir });
      // The other worktree's instance is untouched by this launch.
      for (const other of running.slice(0, -1)) assert.equal(alive(other.pid), true, result.output);
    }

    const [a, b] = running;
    assert.notEqual(a.profile, b.profile, 'two worktrees must not share a profile');
    assert.notEqual(a.offset, b.offset, 'two worktrees must not share a port range');
    assert.ok(a.offset >= 1000 && b.offset >= 1000);
    for (const entry of running) {
      assert.match(entry.profile, /com\.alond\.buildmesh\.dev[\\/]worktrees[\\/]/);
    }

    // Re-running one worktree replaces only its own instance.
    const again = await stubApp(first, dataRoot);
    assert.equal(again.code, 0, again.output);
    assert.match(again.output, new RegExp(`Stopping this worktree's buildmesh-dev \\(PID ${a.pid}`));
    assert.equal(alive(a.pid), false, 'the re-launched worktree stops its own old instance');
    assert.equal(alive(b.pid), true, "another worktree's instance must survive");
    const restarted = instanceRecord(first);
    assert.notEqual(restarted.pid, a.pid);
    assert.equal(alive(restarted.pid), true);
  } catch (error) {
    // Held so cleanup below cannot replace the real failure with a locked file.
    failure = error;
  } finally {
    for (const dir of [first, second]) await stopFixtureApp(dir);
  }
  for (const target of [first, second, dataRoot]) {
    await rm(target, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
  if (failure) throw failure;
});

// The record is a cache, not the only way back to the instance: an
// interrupted launch or a wiped .tmp leaves an orphaned buildmesh-dev that no
// PID points at. The launcher has to find it by executable path instead, and
// still leave another worktree's identical-named instance alone.
test('an instance whose ownership record was lost is still stopped, without touching another worktree', { timeout: 180000 }, async () => {
  const first = worktreeFixture();
  const second = worktreeFixture();
  const dataRoot = mkdtempSync(join(root, '.tmp', `bm-dev-appdata-${process.pid}-`));
  let failure;
  try {
    for (const dir of [first, second]) {
      const result = await stubApp(dir, dataRoot);
      assert.equal(result.code, 0, result.output);
    }
    const orphan = instanceRecord(first).pid;
    const bystander = instanceRecord(second).pid;
    assert.equal(alive(orphan), true);
    assert.equal(alive(bystander), true);

    await rm(join(first, '.tmp', 'dev-instance.json'), { force: true });
    const again = await stubApp(first, dataRoot);
    assert.equal(again.code, 0, again.output);
    assert.match(again.output, new RegExp(`Stopping this worktree's buildmesh-dev \\(PID ${orphan}`));
    assert.equal(alive(orphan), false, 'the orphaned instance must still be stopped');
    assert.equal(alive(bystander), true, 'another worktree has the same process name and must survive');
    assert.equal(instanceRecord(first).pid !== orphan, true);
  } catch (error) {
    failure = error;
  } finally {
    for (const dir of [first, second]) await stopFixtureApp(dir);
  }
  for (const target of [first, second, dataRoot]) {
    await rm(target, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
  if (failure) throw failure;
});

// A caller who pins the offset owns the decision: no probing, and the record
// has to say which range the instance is on, or a helper reading it reports an
// unknown port.
test('a pinned BUILDMESH_PORT_OFFSET is used verbatim and recorded', { timeout: 120000 }, async () => {
  const dir = worktreeFixture();
  const dataRoot = mkdtempSync(join(root, '.tmp', `bm-dev-appdata-${process.pid}-`));
  let failure;
  try {
    const result = await stubApp(dir, dataRoot, { BUILDMESH_PORT_OFFSET: '3300' });
    assert.equal(result.code, 0, result.output);
    assert.match(result.output, /Dev ports: offset 3300 \(from BUILDMESH_PORT_OFFSET\)/);
    const record = instanceRecord(dir);
    assert.equal(record.portOffset, 3300, 'the record must report the pinned offset');
    const env = JSON.parse(readFileSync(join(dir, 'launch-env.json'), 'utf8'));
    assert.equal(env.portOffset, '3300');
    // The app bound the pinned range, which is the point of pinning it.
    assert.deepEqual(env.bound, [true, true, true, true], result.output);
    // The main checkout's documented range stays free for it.
    assert.equal(alive(record.pid), true);
  } catch (error) {
    failure = error;
  } finally {
    await stopFixtureApp(dir);
  }
  for (const target of [dir, dataRoot]) {
    await rm(target, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
  if (failure) throw failure;
});

// A diagnostic that crashes is worse than one that says nothing: this is what a
// developer runs when the dev instance is NOT up, including before the first
// dev build, when src-tauri\target\release-dev does not exist on disk. A
// Resolve-Path on that directory threw ItemNotFoundException and aborted the
// script, so the clean-tree case is pinned here rather than left to review.
test('check-dev-state runs before the first dev build and resolves this worktree profile', { timeout: 60000, skip: process.platform === 'win32' ? false : 'check-dev-state.ps1 is the Windows diagnostic' }, async () => {
  const dir = worktreeFixture();
  const dataRoot = mkdtempSync(join(root, '.tmp', `bm-dev-appdata-${process.pid}-`));
  try {
    assert.equal(existsSync(join(dir, 'src-tauri', 'target', 'release-dev')), false, 'the fixture must have no build output');
    const result = await run(ps, ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(dir, 'scripts', 'check-dev-state.ps1')], {
      env: { ...environment, APPDATA: dataRoot },
      // cwd is the fixture, not the runner's directory: the whole point is that
      // the script must work where the build output does not exist, and a
      // repo-root cwd would hide a cwd-relative lookup behind this repo's own
      // target directory.
      cwd: dir,
    });
    assert.equal(result.code, 0, result.output);
    assert.doesNotMatch(result.output, /Resolve-Path|Cannot find path|Exception/, result.output);
    // No record, no pinned env var: the worktree's own profile is still derived,
    // which is the window a helper runs in before the first launch.
    assert.match(result.output, /=== DEV PROFILE: .*com\.alond\.buildmesh\.dev[\\/]worktrees[\\/]/, result.output);
    assert.match(result.output, new RegExp(escapeForRegExp(join(dataRoot, 'com.alond.buildmesh.dev'))));
    assert.match(result.output, /NO buildmesh-dev from this worktree running/);
  } finally {
    for (const target of [dir, dataRoot]) {
      await rm(target, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
    }
  }
});

function escapeForRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

test('a recorded PID that is not this worktree binary is reported and left running', { timeout: 120000 }, async () => {
  const dir = worktreeFixture();
  const dataRoot = mkdtempSync(join(root, '.tmp', `bm-dev-appdata-${process.pid}-`));
  let failure;
  try {
    // A PID this runner already owns, standing in for a recycled id: live, but
    // not the executable this worktree launched.
    mkdirSync(join(dir, '.tmp'), { recursive: true });
    writeFileSync(join(dir, '.tmp', 'dev-instance.json'), JSON.stringify({
      pid: process.pid, binary: join(dir, 'src-tauri', 'target', 'release-dev', 'release'), profileDir: 'F:/none', portOffset: 4200,
    }));
    const result = await stubApp(dir, dataRoot);
    assert.equal(result.code, 0, result.output);
    assert.match(result.output, new RegExp(`Left PID ${process.pid} alone`));
    assert.equal(alive(process.pid), true, 'an unrelated live process must not be stopped');
    assert.notEqual(instanceRecord(dir).pid, process.pid, 'the record now names the new instance');
  } catch (error) {
    failure = error;
  } finally {
    await stopFixtureApp(dir);
  }
  for (const target of [dir, dataRoot]) {
    await rm(target, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
  if (failure) throw failure;
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
        // A copied executable, not a wrapper script: see prepareFixtureBinary -
        // the launcher recognises its own instance by that path.
        prepareFixtureBinary(binaryDir);
        const shellQuote = value => "'" + value.replaceAll("'", "'\\''") + "'";
        const wrapper = join(dir, 'launch.sh');
        writeFileSync(wrapper, `function npm() { return 0; }; export -f npm\nexec bash ${shellQuote(join(dir, 'scripts', 'run-dev.sh'))} --detach --readiness-timeout-seconds ${readinessSeconds}\n`);
        result = await run('bash', [wrapper], {
          env: {
            ...environment,
            HOME: fixtureHome,
            XDG_DATA_HOME: dataDir,
            NODE_OPTIONS: `--require "${preload.replaceAll('\\', '/')}"`,
            CLAUDE_CODE_CHILD_SESSION: 'must-be-cleared',
          },
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
