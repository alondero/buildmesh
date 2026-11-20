/**
 * The diagnostic capture script (issue #2836).
 *
 * The pure parts are tested here: the trace summary (which is the artefact
 * the issue's acceptance names), the option resolution, and the build stamp.
 * The capture itself needs a live window with a DevTools port, which is what
 * `npm run diag:capture` is for.
 */
import { describe, it, expect } from 'vitest';
import {
  bundleName,
  buildStampFromDiagLines,
  EXPECTED_COMPOSITOR_EVENTS,
  missingCompositorEvents,
  pickPage,
  resolveOptions,
  summariseTraceEvents,
  toCompleteEvents,
} from '../../scripts/diag-capture.mjs';

describe('diag-capture: the trace summary', () => {
  it('reports one row per event name', () => {
    const summary = summariseTraceEvents([
      { name: 'Scheduler::BeginImplFrame', cat: 'cc', ph: 'X', ts: 0, dur: 1000, pid: 1, tid: 2 },
      { name: 'Scheduler::BeginImplFrame', cat: 'cc', ph: 'X', ts: 2000, dur: 500, pid: 1, tid: 2 },
      { name: 'TileManager::PrepareTiles', cat: 'cc', ph: 'X', ts: 100, dur: 400, pid: 1, tid: 2 },
    ]);
    const byName = new Map(summary.map(row => [row.name, row]));
    expect(byName.get('Scheduler::BeginImplFrame')?.count).toBe(2);
    expect(byName.get('Scheduler::BeginImplFrame')?.totalUs).toBe(1500);
    expect(byName.get('TileManager::PrepareTiles')?.count).toBe(1);
  });

  it('measures self time, not the time spent waiting for nested work', () => {
    // A frame that contains a tile-preparation step: the frame's own work is
    // 1000 - 400 = 600, and the tile step's own work is its full 400.
    const summary = summariseTraceEvents([
      { name: 'Scheduler::BeginImplFrame', cat: 'cc', ph: 'X', ts: 0, dur: 1000, pid: 1, tid: 2 },
      { name: 'TileManager::PrepareTiles', cat: 'cc', ph: 'X', ts: 100, dur: 400, pid: 1, tid: 2 },
    ]);
    const byName = new Map(summary.map(row => [row.name, row]));
    expect(byName.get('Scheduler::BeginImplFrame')?.selfUs).toBe(600);
    expect(byName.get('Scheduler::BeginImplFrame')?.totalUs).toBe(1000);
    expect(byName.get('TileManager::PrepareTiles')?.selfUs).toBe(400);
  });

  it('sorts the rows by self time, so the busiest step is first', () => {
    const summary = summariseTraceEvents([
      { name: 'small', cat: 'cc', ph: 'X', ts: 0, dur: 10, pid: 1, tid: 1 },
      { name: 'large', cat: 'cc', ph: 'X', ts: 100, dur: 5000, pid: 1, tid: 1 },
    ]);
    expect(summary[0].name).toBe('large');
  });

  it('does not nest events that belong to different threads', () => {
    // Two threads' events overlap in time but are not nested: each one's
    // self time may not be reduced by the other's duration.
    const summary = summariseTraceEvents([
      { name: 'a', cat: 'cc', ph: 'X', ts: 0, dur: 1000, pid: 1, tid: 1 },
      { name: 'b', cat: 'cc', ph: 'X', ts: 0, dur: 1000, pid: 1, tid: 2 },
    ]);
    const byName = new Map(summary.map(row => [row.name, row]));
    expect(byName.get('a')?.selfUs).toBe(1000);
    expect(byName.get('b')?.selfUs).toBe(1000);
  });

  it('reads a begin/end pair as one event', () => {
    const complete = toCompleteEvents([
      { name: 'Scheduler::BeginImplFrame', cat: 'cc', ph: 'B', ts: 0, pid: 1, tid: 1 },
      { name: 'Scheduler::BeginImplFrame', cat: 'cc', ph: 'E', ts: 750, pid: 1, tid: 1 },
    ]);
    expect(complete).toHaveLength(1);
    expect(complete[0].dur).toBe(750);
  });

  it('names the compositor events the acceptance looks for', () => {
    const summary = summariseTraceEvents([
      { name: 'Scheduler::BeginImplFrame', cat: 'cc', ph: 'X', ts: 0, dur: 10, pid: 1, tid: 1 },
      { name: 'TileManager::PrepareTiles', cat: 'cc', ph: 'X', ts: 0, dur: 10, pid: 1, tid: 1 },
    ]);
    expect(missingCompositorEvents(summary)).toEqual([]);
  });

  it('reports a trace that missed the compositor', () => {
    const summary = summariseTraceEvents([
      { name: 'RunTask', cat: 'toplevel', ph: 'X', ts: 0, dur: 10, pid: 1, tid: 1 },
    ]);
    expect(missingCompositorEvents(summary)).toEqual(EXPECTED_COMPOSITOR_EVENTS);
  });
});

describe('diag-capture: the options', () => {
  it('uses the dev profile port for --dev', () => {
    expect(resolveOptions(['--dev'], {}).port).toBe(9223);
  });

  it('uses the default port for the stable hub', () => {
    expect(resolveOptions([], {}).port).toBe(9222);
  });

  it('follows the port the profile preference asks for', () => {
    expect(resolveOptions([], { preferencePort: 9333 }).port).toBe(9333);
  });

  it('lets an explicit --port win over the preference', () => {
    expect(resolveOptions(['--port', '9444'], { preferencePort: 9333 }).port).toBe(9444);
  });

  it('rejects a port that is not a number', () => {
    expect(() => resolveOptions(['--port', 'abc'], {})).toThrow(/whole number/);
  });

  it('reads the trace window and the output folder', () => {
    const options = resolveOptions(['--seconds', '10', '--out', '.tmp/x'], {});
    expect(options.seconds).toBe(10);
    expect(options.outDir).toBe('.tmp/x');
  });
});

describe('diag-capture: the bundle', () => {
  it('names the folder after the moment of the capture', () => {
    expect(bundleName(new Date(2026, 10, 20, 9, 5, 3))).toBe('diag-capture-20261120-090503');
  });

  it('reads the build stamp from the session marker', () => {
    const stamp = buildStampFromDiagLines([
      'DIAG session_start git_sha=abc1234-dirty interval_ms=5000',
      'DIAG uptime_s=5 rss_mb=100',
    ]);
    expect(stamp.gitSha).toBe('abc1234-dirty');
  });

  it('reports an unknown build stamp when the log has no session marker', () => {
    expect(buildStampFromDiagLines(['DIAG uptime_s=5 rss_mb=100']).gitSha).toBeNull();
  });
});

describe('diag-capture: picking the page', () => {
  const page = (url: string) => ({ url: () => url });

  it('prefers the hub window over any other page', () => {
    const pages = [page('http://localhost:3999/devtools'), page('http://tauri.localhost')];
    expect(pickPage(pages)?.url()).toBe('http://tauri.localhost');
  });

  it('recognises the dev profile page', () => {
    const pages = [page('http://localhost:3999/'), page('http://localhost:1420')];
    expect(pickPage(pages)?.url()).toBe('http://localhost:1420');
  });

  it('falls back to the only page there is', () => {
    const pages = [page('http://localhost:3999/')];
    expect(pickPage(pages)?.url()).toBe('http://localhost:3999/');
  });

  it('reports no page when the browser has none', () => {
    expect(pickPage([])).toBeNull();
  });
});
