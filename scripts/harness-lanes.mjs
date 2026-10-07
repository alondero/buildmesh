import { closeSync, mkdirSync, openSync, readFileSync, readdirSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';

export const DEFAULT_HEAVY_GATE_LIMIT = 2;

function alive(pid) {
  try { process.kill(pid, 0); return true; } catch (error) { return error.code === 'EPERM'; }
}

// Run a verification plan. Gates without a `lane` run first, in order, and the
// first non-PASS stops everything. Each lane then runs its gates in order on its
// own track, and lanes overlap. `after` lists gate ids (in another lane) that
// must have PASSED before the gate may start: a build that rewrites `dist/`
// must not overlap a compile that embeds it. A non-PASS stops every lane from
// starting further gates; gates already running are allowed to finish so their
// logs and receipt rows stay complete. A gate whose dependency did not PASS is
// not run (it stays absent, so it cannot count as green).
//
// `execute(gate, isStopped)` returns the receipt row, or nothing when it chose
// not to run because `isStopped()` became true while it waited. Rows are
// reported through `onResult` as each gate finishes; the caller owns ordering
// in the receipt.
export async function runPlan(gates, { execute, onResult = () => {} }) {
  let stopped = false;
  const settle = row => {
    onResult(row);
    if (row.outcome !== 'PASS') stopped = true;
  };
  for (const gate of gates.filter(item => !item.lane)) {
    if (stopped) break;
    settle(await execute(gate));
  }
  const lanes = new Map();
  for (const gate of gates.filter(item => item.lane)) lanes.set(gate.lane, [...(lanes.get(gate.lane) ?? []), gate]);
  const finished = new Map();
  for (const gate of [...lanes.values()].flat()) {
    let resolve;
    finished.set(gate.id, { promise: new Promise(done => { resolve = done; }), resolve });
  }
  await Promise.all([...lanes.values()].map(async lane => {
    let index = 0;
    try {
      for (; index < lane.length; index += 1) {
        const gate = lane[index];
        let blocked = false;
        for (const id of gate.after ?? []) {
          const dependency = finished.get(id);
          // An unknown id is not in this plan (scope narrowed), so nothing to wait for.
          if (dependency && (await dependency.promise)?.outcome !== 'PASS') blocked = true;
        }
        if (blocked || stopped) return;
        const row = await execute(gate, () => stopped);
        // A gate that waited for a machine-wide slot may find the plan already
        // failed by then and decline to run.
        if (!row) return;
        settle(row);
        finished.get(gate.id).resolve(row);
        if (row.outcome !== 'PASS') return;
      }
    } finally {
      // Release dependants of every gate this lane never completed.
      for (const gate of lane) finished.get(gate.id).resolve(undefined);
    }
  }));
}

// Heavy gates (the full Rust and frontend test suites) are the ones that slow
// each other down when several worktrees verify at once, so a small machine-wide
// semaphore bounds them. Each slot is a file created exclusively; a slot left by
// a dead process is reclaimed, so an interrupted run needs no cleanup.
export function heavyGateLimit(env = process.env) {
  const value = Number(env.BUILDMESH_HEAVY_GATE_LIMIT);
  return Number.isInteger(value) && value > 0 ? value : DEFAULT_HEAVY_GATE_LIMIT;
}

// Two heavy suites running at once would each assume the whole machine: vitest
// starts one worker per core and every Rust shard runs one test thread per
// core. Oversubscribed, tests with real-time bounds (process spawns, pipe
// drains) miss them and verify fails on load rather than on a defect. When the
// plan overlaps heavy gates from different lanes, each gets half the cores.
// Computed at run time so the persisted plan stays machine-independent.
export function heavyGateEnv(gate, gates, cores) {
  if (!gate.heavy || new Set(gates.filter(item => item.heavy).map(item => item.lane)).size < 2) return {};
  const share = Math.max(2, Math.floor(cores / 2));
  if (gate.tests === 'vitest') return { VITEST_MAX_WORKERS: String(share) };
  if (gate.tests === 'rust') return { RUST_TEST_THREADS: String(share), BUILDMESH_RUST_TEST_JOBS: '2' };
  return {};
}

export function slotDirectory(env = process.env) {
  if (env.BUILDMESH_GATE_SLOTS_DIR) return env.BUILDMESH_GATE_SLOTS_DIR;
  const base = process.platform === 'win32' ? env.LOCALAPPDATA : (env.XDG_STATE_HOME ?? (homedir() ? join(homedir(), '.local', 'state') : null));
  return join(base ?? tmpdir(), 'buildmesh', 'gate-slots');
}

function readSlot(path) {
  try { return JSON.parse(readFileSync(path, 'utf8')); } catch { return null; }
}

function claim(path, owner) {
  let fd;
  try { fd = openSync(path, 'wx'); } catch { return false; }
  try { writeFileSync(fd, JSON.stringify(owner)); } finally { closeSync(fd); }
  return true;
}

// A stale slot is moved aside before deletion and the move is checked: a second
// process that reclaimed the same slot first may already have replaced it with
// a live owner's file, which must not be deleted.
function reclaimStale(path) {
  const held = readSlot(path);
  if (!held || typeof held.pid !== 'number' || alive(held.pid)) return false;
  const aside = `${path}.${randomUUID()}.stale`;
  try { renameSync(path, aside); } catch { return false; }
  const moved = readSlot(aside);
  try { unlinkSync(aside); } catch { /* best effort */ }
  if (moved?.token !== held.token) {
    // Raced with another reclaimer and moved a live owner's file; put it back.
    if (moved) claim(path, moved);
    return false;
  }
  return true;
}

export function slotHolders(dir, limit) {
  const holders = [];
  for (let index = 0; index < limit; index += 1) {
    const held = readSlot(join(dir, `slot-${index}.json`));
    if (held && typeof held.pid === 'number' && alive(held.pid)) holders.push(held);
  }
  return holders;
}

// Resolves with a `release` function once a slot is held. `onQueued` is called
// when the first attempt finds every slot busy, and again every `reminderMs`.
export async function acquireSlot({ dir = slotDirectory(), limit = heavyGateLimit(), gate, root, pollMs = 1000, reminderMs = 60000, onQueued = () => {}, sleep = ms => new Promise(done => setTimeout(done, ms)) } = {}) {
  mkdirSync(dir, { recursive: true });
  const token = randomUUID();
  const owner = { pid: process.pid, token, gate, root, startedAt: new Date().toISOString() };
  let queuedAt = null;
  let reminded = 0;
  for (;;) {
    for (let index = 0; index < limit; index += 1) {
      const path = join(dir, `slot-${index}.json`);
      if (claim(path, owner) || (reclaimStale(path) && claim(path, owner))) {
        const release = () => {
          // Another process may have reclaimed this slot after a long pause;
          // only delete a file that is still ours.
          if (readSlot(path)?.token === token) { try { unlinkSync(path); } catch { /* already gone */ } }
          process.removeListener('exit', release);
        };
        process.on('exit', release);
        return { release, queuedMs: queuedAt === null ? 0 : Date.now() - queuedAt };
      }
    }
    const now = Date.now();
    if (queuedAt === null || now - reminded >= reminderMs) {
      queuedAt ??= now;
      reminded = now;
      onQueued(slotHolders(dir, limit));
    }
    await sleep(pollMs);
  }
}
