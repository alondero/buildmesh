// Benchmark for the frontend terminal write queue (issue #2018).
//
// Measures the two amplification mechanisms the issue names:
//
//   1. Byte-capped eviction of many small chunks while `requestAnimationFrame`
//      is suspended (a hidden or minimized window). Pre-#2018 eviction was
//      `chunks.shift()`, which re-indexes the whole array per dropped chunk.
//   2. Unbounded chunk-object retention: the cap counted bytes only, so a
//      small-chunk storm retained one array slot (plus its retained payload)
//      per chunk.
//
// Plus the parser-side budget: how many payloads may be handed to xterm while
// earlier ones are still parsing.
//
// Usage: node --expose-gc scripts/bench-terminal-writer.mjs
// Node strips the writer's types natively (Node >= 22.18), so this imports the
// real production module rather than a copy of it. Accessors the pre-#2018
// writer lacks are feature-detected and reported as `n/a`, so the same script
// runs against both revisions.

import { TerminalWriter } from '../src/components/Terminal/TerminalWriter.ts';

const MIB = 1024 * 1024;
const read = (v) => (v === undefined ? 'n/a' : v);

function mb(bytes) {
  return `${(bytes / MIB).toFixed(2)} MiB`;
}

function heapUsedMb() {
  if (global.gc) global.gc();
  return process.memoryUsage().heapUsed / MIB;
}

/**
 * Paused scheduler: models a hidden window, where Chromium never fires
 * `requestAnimationFrame`. Callbacks pile up exactly as they do in production.
 */
function pausedScheduler() {
  const pending = [];
  return {
    schedule(cb) {
      pending.push(cb);
    },
    // One frame runs the callbacks queued at its start. A callback that
    // re-arms (a flush deferred by the in-flight budget) belongs to the NEXT
    // frame — draining until empty here would spin forever.
    run() {
      const frame = pending.splice(0, pending.length);
      for (const cb of frame) cb();
    },
    get depth() {
      return pending.length;
    },
  };
}

/**
 * Sink that honours xterm's `write(data, callback)` parse-completion contract:
 * writes are recorded, and completion only happens when the harness calls
 * `completeAll()`. Models a parser that lags the producer.
 */
function asyncSink() {
  const writes = [];
  const completions = [];
  let outstanding = 0;
  let peak = 0;
  return {
    writes,
    get outstanding() {
      return outstanding;
    },
    get peakOutstanding() {
      return peak;
    },
    fn(data, done) {
      outstanding++;
      if (outstanding > peak) peak = outstanding;
      writes.push(typeof data === 'string' ? data : new TextDecoder().decode(data));
      completions.push(() => {
        outstanding--;
        done?.();
      });
    },
    completeAll() {
      for (const done of completions.splice(0, completions.length)) done();
    },
  };
}

function scenarioPaused(label, { chunkBytes, chunkCount }) {
  // Each chunk is a distinct string: a shared reference would understate the
  // retained heap, since one interned payload would stand in for all of them.
  const makeChunk = (i) => `${String(i).padStart(9, '0')}${'A'.repeat(chunkBytes - 9)}`;
  const scheduler = pausedScheduler();
  const writer = new TerminalWriter(scheduler.schedule);
  writer.register(1, () => {});

  const before = heapUsedMb();
  const start = process.hrtime.bigint();
  for (let i = 0; i < chunkCount; i++) writer.append(1, makeChunk(i));
  const elapsedMs = Number(process.hrtime.bigint() - start) / 1e6;
  const after = heapUsedMb();

  return {
    label,
    chunkBytes,
    chunkCount,
    chunkTotalBytes: chunkCount * chunkBytes,
    elapsedMs,
    retainedHeapMb: after - before,
    pendingBytes: writer.pendingBytes(1),
    queuedChunks: read(writer.queuedChunks?.(1)),
    pendingFrames: scheduler.depth,
  };
}

function scenarioHiddenRestore() {
  // Sustained output with rAF paused, then the window is restored: the queued
  // frame fires and hands the retained backlog to xterm in one go.
  const scheduler = pausedScheduler();
  const sink = asyncSink();
  const writer = new TerminalWriter(scheduler.schedule);
  writer.register(1, sink.fn);

  const line = (i) => `build line ${String(i).padStart(9, '0')} ${'x'.repeat(60)}\n`;
  const lines = 200_000;
  const before = heapUsedMb();
  const start = process.hrtime.bigint();
  for (let i = 0; i < lines; i++) writer.append(1, line(i));
  const appendMs = Number(process.hrtime.bigint() - start) / 1e6;
  const retainedHeapMb = heapUsedMb() - before;
  const queuedChunks = read(writer.queuedChunks?.(1));

  // Restore: the paused frame finally runs.
  scheduler.run();
  return {
    lines,
    appendMs,
    retainedHeapMb,
    queuedChunks,
    queuedBytes: writer.pendingBytes(1),
    queuedChunksAfterRestore: read(writer.queuedChunks?.(1)),
    writesHandedOver: sink.writes.length,
    bytesHandedOver: sink.writes.join('').length,
    peakOutstanding: sink.peakOutstanding,
  };
}

function scenarioParserLag() {
  // Producer keeps writing while the parser lags: how much work may be handed
  // to xterm before the completion-based budget defers further flushes?
  const scheduler = pausedScheduler();
  const sink = asyncSink();
  const writer = new TerminalWriter(scheduler.schedule);
  // Third argument added in #2018: this sink reports parse completion.
  writer.register(1, sink.fn, { completionAware: true });

  const block = 'y'.repeat(4096);
  const frames = 200;
  const perFrame = 8;
  for (let frame = 0; frame < frames; frame++) {
    for (let i = 0; i < perFrame; i++) writer.append(1, block);
    scheduler.run();
  }
  const inFlightAtBudget = read(writer.inFlightWrites?.(1));

  // Drain: complete outstanding parses, one frame at a time.
  let drainFrames = 0;
  while (sink.outstanding > 0 && drainFrames < 10_000) {
    sink.completeAll();
    scheduler.run();
    drainFrames++;
  }
  return {
    frames,
    writesHandedOver: sink.writes.length,
    peakOutstanding: sink.peakOutstanding,
    inFlightAtBudget,
    drainFrames,
  };
}

function report(rows, columns) {
  console.log(columns.map((c) => c.label.padEnd(c.width)).join('  '));
  console.log(columns.map((c) => '-'.repeat(c.width)).join('  '));
  for (const row of rows) {
    console.log(columns.map((c) => String(c.get(row)).padEnd(c.width)).join('  '));
  }
}

console.log(`node ${process.version}${global.gc ? ' (--expose-gc)' : ' (no gc flag)'}\n`);

// Chunks must exceed INTERACTIVE_FAST_PATH_BYTES (16) or they take the
// interactive fast path and never enter the queue at all.
console.log('Scenario 1 — sustained small chunks at the byte cap, rAF paused');
report(
  [
    scenarioPaused('64B chunks', { chunkBytes: 64, chunkCount: 200_000 }),
    scenarioPaused('256B chunks', { chunkBytes: 256, chunkCount: 50_000 }),
  ],
  [
    { label: 'case', width: 12, get: r => r.label },
    { label: 'chunks', width: 10, get: r => r.chunkCount.toLocaleString() },
    { label: 'append ms', width: 11, get: r => r.elapsedMs.toFixed(1) },
    { label: 'queued', width: 8, get: r => r.queuedChunks },
    { label: 'pending', width: 11, get: r => mb(r.pendingBytes) },
    { label: 'retained', width: 11, get: r => `${r.retainedHeapMb.toFixed(2)} MiB` },
    { label: 'frames', width: 7, get: r => r.pendingFrames },
  ],
);

console.log('\nScenario 2 — hidden window: sustained output, then restore');
report(
  [scenarioHiddenRestore()],
  [
    { label: 'lines', width: 8, get: r => r.lines.toLocaleString() },
    { label: 'append ms', width: 11, get: r => r.appendMs.toFixed(1) },
    { label: 'queued', width: 8, get: r => r.queuedChunks },
    { label: 'retained', width: 11, get: r => `${r.retainedHeapMb.toFixed(2)} MiB` },
    { label: 'writes', width: 7, get: r => r.writesHandedOver },
    { label: 'queued after', width: 13, get: r => r.queuedChunksAfterRestore },
    { label: 'peak in-flight', width: 15, get: r => r.peakOutstanding },
  ],
);

console.log('\nScenario 3 — parser lag: producer outruns the completion budget');
report(
  [scenarioParserLag()],
  [
    { label: 'frames', width: 8, get: r => r.frames },
    { label: 'writes handed', width: 14, get: r => r.writesHandedOver },
    { label: 'peak in-flight', width: 15, get: r => r.peakOutstanding },
    { label: 'in-flight held', width: 15, get: r => r.inFlightAtBudget },
    { label: 'drain frames', width: 13, get: r => r.drainFrames },
  ],
);