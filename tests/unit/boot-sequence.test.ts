/**
 * `runBoot` — the startup failure contract (issue #1524).
 *
 * The bug this pins: `App.init()` treated a settled `Promise.allSettled`
 * as the whole story, but both authoritative snapshot loaders absorb their
 * IPC failure and resolve. A boot whose Mesh or Agent Node snapshot never
 * loaded therefore looked successful and painted an empty workspace.
 *
 * These tests drive the coordinator with fakes shaped like the real
 * stores: a failing loader that *resolves* and records the error in store
 * state, exactly as `meshStore.fetchMeshes` and
 * `agentNodeStore.fetchAgentNodes` do today. The verdict must come back
 * `ok: false` with a line naming the failed source, and `ok: true` only
 * once a healthy retry reports clean snapshots.
 */
import { describe, it, expect, vi } from 'vitest';
import { runBoot, type BootLoaders, type BootSnapshotErrors } from '../../src/lib/bootSequence';

/**
 * A snapshot loader that behaves like the real ones: it clears the store
 * error, and on failure writes the message and RESOLVES. `failWith(null)`
 * makes the next attempt healthy.
 */
function makeSnapshotLoader() {
  let error: string | null = null;
  let fail: string | null = null;
  const loader = vi.fn(async () => {
    error = null;
    if (fail) error = fail;
  });
  return {
    loader,
    failWith: (message: string | null) => { fail = message; },
    read: () => error,
  };
}

function makeHarness() {
  const mesh = makeSnapshotLoader();
  const nodes = makeSnapshotLoader();
  const attachListeners = vi.fn(async () => {});
  const loaders: BootLoaders = {
    attachListeners,
    loadMeshes: mesh.loader,
    loadAgentNodes: nodes.loader,
    readSnapshotErrors: (): BootSnapshotErrors => ({
      meshes: mesh.read(),
      agentNodes: nodes.read(),
    }),
  };
  return { mesh, nodes, attachListeners, loaders };
}

describe('runBoot (issue #1524: authoritative boot)', () => {
  it('reports ok when every loader succeeds and both snapshots are clean', async () => {
    const harness = makeHarness();

    const outcome = await runBoot(harness.loaders);

    expect(outcome).toEqual({ ok: true, errors: [] });
    expect(harness.attachListeners).toHaveBeenCalledTimes(1);
    expect(harness.mesh.loader).toHaveBeenCalledTimes(1);
    expect(harness.nodes.loader).toHaveBeenCalledTimes(1);
  });

  it('fails the boot when the Mesh snapshot swallowed its IPC failure', async () => {
    const harness = makeHarness();
    harness.mesh.failWith('database is locked');

    const outcome = await runBoot(harness.loaders);

    // The Mesh loader resolved, so `Promise.allSettled` alone sees a clean
    // run — the stored error is the only signal that the workspace is not
    // authoritative.
    expect(outcome.ok).toBe(false);
    expect(outcome.errors).toEqual(['Meshes: database is locked']);
  });

  it('fails the boot when the Agent Node snapshot swallowed its IPC failure', async () => {
    const harness = makeHarness();
    harness.nodes.failWith('no such table: agent_nodes');

    const outcome = await runBoot(harness.loaders);

    expect(outcome.ok).toBe(false);
    expect(outcome.errors).toEqual(['Agent Nodes: no such table: agent_nodes']);
  });

  it('names both snapshots when both fail', async () => {
    const harness = makeHarness();
    harness.mesh.failWith('database is locked');
    harness.nodes.failWith('no such table: agent_nodes');

    const outcome = await runBoot(harness.loaders);

    expect(outcome.ok).toBe(false);
    expect(outcome.errors).toEqual([
      'Meshes: database is locked',
      'Agent Nodes: no such table: agent_nodes',
    ]);
  });

  it('fails the boot when a loader rejects, and strips the Error prefix', async () => {
    const harness = makeHarness();
    harness.attachListeners.mockRejectedValueOnce(new Error('event bus unavailable'));

    const outcome = await runBoot(harness.loaders);

    expect(outcome.ok).toBe(false);
    expect(outcome.errors).toEqual(['Event listeners: event bus unavailable']);
  });

  it('reports one line per failed source when a loader rejects AND stores the error', async () => {
    // `meshStore.refreshMeshes` rethrows *and* sets `error`, so the same
    // failure is visible through both channels. The panel must not print
    // it twice.
    const harness = makeHarness();
    harness.mesh.failWith('database is locked');
    harness.loaders.loadMeshes = async () => { throw new Error('database is locked'); };

    const outcome = await runBoot(harness.loaders);

    expect(outcome.errors).toEqual(['Meshes: database is locked']);
  });

  it('captures a synchronous throw without aborting the sibling loaders', async () => {
    const harness = makeHarness();
    harness.loaders.attachListeners = () => { throw new Error('listener setup exploded'); };

    const outcome = await runBoot(harness.loaders);

    expect(outcome.errors).toEqual(['Event listeners: listener setup exploded']);
    // The snapshots still ran, so their own failures would also be seen.
    expect(harness.mesh.loader).toHaveBeenCalledTimes(1);
    expect(harness.nodes.loader).toHaveBeenCalledTimes(1);
  });

  it('returns to ok when a retry finds healthy APIs', async () => {
    const harness = makeHarness();
    harness.mesh.failWith('database is locked');
    harness.nodes.failWith('no such table: agent_nodes');
    expect((await runBoot(harness.loaders)).ok).toBe(false);

    // The backend recovered: the loaders clear their own error as they
    // start, so the retry reports a clean workspace.
    harness.mesh.failWith(null);
    harness.nodes.failWith(null);
    const retry = await runBoot(harness.loaders);

    expect(retry).toEqual({ ok: true, errors: [] });
  });
});
