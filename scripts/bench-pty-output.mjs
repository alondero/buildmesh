// Native WebView2 probe of the production subscriber and Rust output sink.
// Launch scripts/run-dev.ps1 -Detach -CdpPort 9237 first, then:
// node scripts/bench-pty-output.mjs 9237
// The injected hold substitutes one body-read promise; Channel IPC stays real.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { build } from 'rolldown';
import { chromium } from 'playwright';

const port = Number(process.argv[2] ?? 9237);
// A baseline run needs the baseline native app plus the dev-only pump command.
const baseline = process.argv[3];
const stressSmall = process.env.BUILDMESH_PTY_PROBE_PATTERN === 'small';
const floodFrames = Number(process.env.BUILDMESH_PTY_PROBE_FRAMES ?? 20_001);
assert(Number.isSafeInteger(floodFrames) && floodFrames > 64);
let expectedFrames = 0;
let expectedBytes = 0;
for (let sequence = stressSmall ? 0 : 1; expectedFrames < 64; sequence++) {
  const size = stressSmall ? (sequence === 0 ? 2048 : 512) : [512, 2048, 32768, 3, 1][sequence % 5];
  if (expectedBytes + size > 256 * 1024) break;
  expectedFrames++;
  expectedBytes += size;
}
const bundle = await build({
  input: 'pty-probe-entry',
  plugins: [{ name: 'probe-entry', resolveId(id) {
    if (id === 'pty-probe-entry') return '\0pty-probe-entry';
  }, load(id) {
    if (id === '\0pty-probe-entry') return `export * from ${JSON.stringify(path.resolve('src/lib/tauri/agent.ts').replaceAll('\\', '/'))};
      export { Terminal } from '@xterm/xterm';`;
  } }, { name: 'observe-real-invoke', transform(code, id) {
    if (baseline && id.replaceAll('\\', '/').endsWith('/tauri/agent.ts')) {
      return execFileSync('git', ['show', `${baseline}:src/lib/tauri/agent.ts`], { encoding: 'utf8' });
    }
    if (id.replaceAll('\\', '/').endsWith('/tauri/_invoke.ts')) {
      return code.replace('  try {', '  globalThis.__ptyProbeOnInvoke?.(cmd, args);\n  try {');
    }
  } }],
  output: { format: 'iife', name: '__ptyProbeTransport' },
  write: false,
});
const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
let restorePage;
try {
  const page = browser.contexts()[0].pages().find(p => !p.url().startsWith('devtools:'));
  assert(page, 'Native app page not found');
  const originalUrl = page.url();
  // Keep the real native WebView/IPC while isolating fixture terminals from
  // React auto-attachment, which would replace the probe's subscription.
  await page.route(originalUrl, route => route.fulfill({ contentType: 'text/html',
    body: '<!doctype html><html><body><div id="terminal"></div></body></html>' }));
  await page.reload();
  await page.unroute(originalUrl);
  restorePage = () => page.goto(originalUrl);
  await page.waitForFunction(() => window.__TAURI_INTERNALS__);
  await page.addScriptTag({ content: bundle.output[0].code });
  const cdp = await page.context().newCDPSession(page);
  const browserCdp = await browser.newBrowserCDPSession();
  const memory = () => cdp.send('Runtime.getHeapUsage');
  const residentMemory = async () => {
    if (process.platform !== 'win32') return null;
    const processes = await browserCdp.send('SystemInfo.getProcessInfo');
    const appPid = JSON.parse(readFileSync('.tmp/dev-instance.json', 'utf8')).pid;
    const ids = [...new Set([appPid, ...processes.processInfo.map(info => Number(info.id))])];
    assert(ids.every(id => Number.isSafeInteger(id) && id > 0));
    return JSON.parse(execFileSync('powershell', ['-NoProfile', '-Command',
      `Get-Process -Id ${ids.join(',')} | Select-Object Id,ProcessName,WorkingSet64,PrivateMemorySize64 | ConvertTo-Json -Compress`,
    ], { encoding: 'utf8' }));
  };
  const results = [];
  for (const buildRun of [false, true]) {
    await cdp.send('HeapProfiler.collectGarbage');
    const before = await memory();
    const residentBefore = await residentMemory();
    await page.evaluate(async ({ buildRun, stressSmall, floodFrames }) => {
      const api = window.__ptyProbeTransport;
      const internals = window.__TAURI_INTERNALS__;
      const invoke = internals.invoke.bind(internals);
      const state = window.__ptyProbeState = {
        received: 0, delivered: 0, receivedBytes: 0, deliveredBytes: 0,
        held: false, done: false, error: null, shapes: {}, ackCalls: 0,
        maxOutstanding: 0, maxPayload: 0, start: performance.now(),
      };
      window.__ptyProbeOnInvoke = (command, args) => {
        if (command === 'acknowledge_pty_output') state.ackCalls++;
        if (command.startsWith('subscribe_') && args?.sessionId === 9272727) {
          const deliver = args.onChunk.onmessage;
          args.onChunk.onmessage = message => {
            state.received++;
            const bytes = message instanceof ArrayBuffer ? new Uint8Array(message) : null;
            const size = bytes?.byteLength ?? Number(message.headers?.get('content-length') ?? 0);
            const shape = message.constructor.name;
            state.shapes[shape] = (state.shapes[shape] ?? 0) + 1;
            state.receivedBytes += size;
            state.maxOutstanding = Math.max(state.maxOutstanding, state.received - state.delivered);
            state.maxPayload = Math.max(state.maxPayload, state.receivedBytes - state.deliveredBytes);
            if (!state.held && state.received === (stressSmall ? 1 : 2)) {
              state.held = true;
              deliver({ arrayBuffer: () => new Promise(resolve => {
                state.release = async () => resolve(bytes ? bytes.buffer : await message.arrayBuffer());
              }) });
            } else deliver(message);
          };
        }
      };
      state.restore = () => { window.__ptyProbeOnInvoke = null; };
      const subscribe = buildRun ? api.subscribeBuildRunOutput : api.subscribeAgentOutput;
      await subscribe(9272727, bytes => {
        const sequence = state.delivered;
        const expectedSize = stressSmall ? (sequence === 0 ? 2048 : 512) : [512, 2048, 32768, 3, 1][sequence % 5];
        const stamp = stressSmall && sequence > 0 ? sequence - 1 : sequence;
        if (bytes.length !== expectedSize) throw new Error(`frame ${sequence} size ${bytes.length}`);
        for (let offset = 0; offset < bytes.length; offset++) {
          if (bytes[offset] !== ((stamp + offset) & 255)) throw new Error(`out of order at ${sequence}:${offset}`);
        }
        state.delivered++;
        state.deliveredBytes += bytes.length;
        if (state.delivered === floodFrames) {
          state.elapsedMs = performance.now() - state.start;
          state.done = true;
        }
      });
      state.pump = (stressSmall
        ? invoke('debug_pump_pty_output', { sessionId: 9272727, buildRun, frames: 1, frameSize: 2048 })
          .then(() => invoke('debug_pump_pty_output', { sessionId: 9272727, buildRun, frames: floodFrames - 1, frameSize: 512 }))
        : invoke('debug_pump_pty_output', { sessionId: 9272727, buildRun, frames: floodFrames }))
        .catch(error => { state.error = String(error); state.done = true; });
    }, { buildRun, stressSmall, floodFrames });
    try {
      await page.waitForFunction(count => window.__ptyProbeState.received >= count || window.__ptyProbeState.error,
        baseline ? floodFrames : expectedFrames + (stressSmall ? 0 : 1), { timeout: 120_000 });
      // Give an unbounded producer ample time to expose more sends before releasing the hold.
      await new Promise(resolve => setTimeout(resolve, 250));
      await cdp.send('HeapProfiler.collectGarbage');
      const heldMemory = await memory();
      const residentHeld = await residentMemory();
      const held = await page.evaluate(() => {
        const s = window.__ptyProbeState;
        return { frames: s.received - s.delivered, bytes: s.receivedBytes - s.deliveredBytes, error: s.error };
      });
      assert.equal(held.error, null);
      assert.equal(held.frames, baseline ? floodFrames - (stressSmall ? 0 : 1) : expectedFrames, 'Unexpected stalled producer depth');
      if (!baseline) assert.equal(held.bytes, expectedBytes, 'Unexpected stalled payload');
      await page.evaluate(() => window.__ptyProbeState.release());
      await page.waitForFunction(() => window.__ptyProbeState.done, null, { timeout: 120_000 });
      const result = await page.evaluate(async () => {
        const s = window.__ptyProbeState;
        await s.pump;
        const { delivered, deliveredBytes, elapsedMs, maxOutstanding, maxPayload, shapes, ackCalls, error } = s;
        return { delivered, deliveredBytes, elapsedMs, maxOutstanding, maxPayload, shapes, ackCalls, error };
      });
      assert.equal(result.error, null);
      assert.equal(result.delivered, floodFrames);
      results.push({ surface: buildRun ? 'build/run' : 'agent', held,
        residentBefore, residentHeld,
        jsHeapBefore: before.usedSize, jsHeapHeld: heldMemory.usedSize,
        externalBefore: before.backingStorageSize, externalHeld: heldMemory.backingStorageSize,
        ...result, mibPerSecond: result.deliveredBytes / 1048576 / (result.elapsedMs / 1000) });
    } catch (error) {
      const pending = await page.evaluate(() => {
        const s = window.__ptyProbeState;
        return { received: s.received, delivered: s.delivered,
          pendingFrames: s.received - s.delivered, pendingBytes: s.receivedBytes - s.deliveredBytes };
      });
      console.error(JSON.stringify({ pending, heap: await memory(), resident: await residentMemory() }, null, 2));
      throw error;
    } finally {
      await page.evaluate(async buildRun => {
        const s = window.__ptyProbeState;
        s.release?.();
        const api = window.__ptyProbeTransport;
        await (buildRun ? api.unsubscribeBuildRunOutput : api.unsubscribeAgentOutput)(9272727);
        await s.pump;
        s.restore();
      }, buildRun);
    }
  }
  console.log(JSON.stringify({ stress: results }, null, 2));
  const latency = await page.evaluate(async () => {
    const api = window.__ptyProbeTransport;
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const results = [];
    for (const buildRun of [false, true]) {
      for (const frameSize of [1, 512, 1023, 1024, 2048, 32768]) {
        const timings = [];
        let received;
        const subscribe = buildRun ? api.subscribeBuildRunOutput : api.subscribeAgentOutput;
        const unsubscribe = buildRun ? api.unsubscribeBuildRunOutput : api.unsubscribeAgentOutput;
        await subscribe(9272728, bytes => received(bytes));
        try {
          for (let sample = 0; sample < 50; sample++) {
            const start = performance.now();
            const delivery = new Promise(resolve => { received = resolve; });
            const pump = invoke('debug_pump_pty_output', {
              sessionId: 9272728, buildRun, frames: 1, frameSize,
            });
            const bytes = await Promise.race([delivery, pump.then(() => new Promise(() => {})),
              new Promise((_, reject) => setTimeout(() => reject(new Error('Native delivery timeout')), 10_000))]);
            timings.push(performance.now() - start);
            if (bytes.length !== frameSize) throw new Error(`expected ${frameSize}, got ${bytes.length}`);
            await pump;
          }
        } finally {
          await unsubscribe(9272728);
        }
        timings.sort((a, b) => a - b);
        results.push({ surface: buildRun ? 'build/run' : 'agent', frameSize,
          samples: timings.length, medianMs: timings[25], p95Ms: timings[47], maxMs: timings.at(-1) });
      }
    }
    return results;
  });
  console.log(JSON.stringify({ latency }, null, 2));
  const echoes = await page.evaluate(async () => {
    const api = window.__ptyProbeTransport;
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const mesh = await invoke('create_test_mesh', { name: 'PTY output latency probe' });
    let node;
    const results = [];
    try {
      node = await invoke('create_agent_node', { meshId: mesh.id, name: 'Echo probe',
        path: mesh.path, branch: '', provider: 'terminal', useWorktree: false });
      for (const buildRun of [false, true]) {
        let startup = '';
        let echo;
        let expectedKey = 'x';
        const decoder = new TextDecoder();
        const subscribe = buildRun ? api.subscribeBuildRunOutput : api.subscribeAgentOutput;
        const unsubscribe = buildRun ? api.unsubscribeBuildRunOutput : api.unsubscribeAgentOutput;
        const terminal = new api.Terminal({ cols: 80, rows: 24 });
        terminal.open(document.querySelector('#terminal'));
        terminal.onData(data => {
          void (buildRun ? api.writeToBuildRun(node.id, data) : api.writeToAgentRaw(node.id, data));
        });
        await subscribe(node.id, bytes => {
          terminal.write(bytes);
          const text = decoder.decode(bytes, { stream: true });
          startup = (startup + text).slice(-8192);
          if (text.includes(expectedKey)) echo?.();
        });
        try {
          if (buildRun) await api.buildRun(node.id, 'terminal');
          else await api.spawnAgent({ sessionId: node.id, provider: 'terminal',
            intent: { type: 'fresh' }, rows: 24, cols: 80 });
          const readyDeadline = performance.now() + 15_000;
          while (!startup.includes('>')) {
            if (performance.now() > readyDeadline) throw new Error(`PTY startup timeout: ${startup}`);
            await new Promise(resolve => setTimeout(resolve, 20));
          }
          // Use a deterministic Console.ReadKey echo loop so user PSReadLine
          // settings and redraw compression do not change the observed marker.
          startup = '';
          const setup = "[Console]::TreatControlCAsInput=$true;[Console]::Write('PROBE_'+'READY');while($true){$k=[Console]::ReadKey($true);[Console]::Write('^'+$k.KeyChar)}\r";
          await (buildRun ? api.writeToBuildRun(node.id, setup) : api.writeToAgentRaw(node.id, setup));
          const loopDeadline = performance.now() + 15_000;
          while (!startup.includes('PROBE_READY')) {
            if (performance.now() > loopDeadline) throw new Error('PTY echo loop startup timeout');
            await new Promise(resolve => setTimeout(resolve, 20));
          }
          await new Promise(resolve => setTimeout(resolve, 100));
          const timings = [];
          for (let sample = 0; sample < 30; sample++) {
            let timer;
            const start = performance.now();
            expectedKey = sample % 2 === 0 ? 'x' : 'y';
            const delivery = new Promise((resolve, reject) => {
              echo = () => { clearTimeout(timer); resolve(performance.now() - start); };
              timer = setTimeout(() => reject(new Error(`PTY echo timeout at sample ${sample}`)), 5000);
            });
            const write = buildRun ? api.writeToBuildRun(node.id, expectedKey) : api.writeToAgentRaw(node.id, expectedKey);
            timings.push(await delivery);
            await write;
            echo = null;
            await new Promise(resolve => setTimeout(resolve, 50));
          }
          timings.sort((a, b) => a - b);
          results.push({ surface: buildRun ? 'build/run' : 'agent', samples: timings.length,
            medianMs: timings[15], p95Ms: timings[28], maxMs: timings.at(-1) });
        } finally {
          await (buildRun ? api.closeBuildRun(node.id) : api.killAgent(node.id));
          await unsubscribe(node.id);
          terminal.dispose();
        }
      }
    } finally {
      if (node) await invoke('delete_agent_node', { nodeId: node.id, removeWorktree: false });
      await invoke('delete_mesh', { meshId: mesh.id });
    }
    return results;
  });
  console.log(JSON.stringify({ baseline: baseline ?? null, stressSmall, floodFrames, runtime: await browser.version(), results, latency, echoes }, null, 2));
} finally {
  await restorePage?.();
  await browser.close();
}
