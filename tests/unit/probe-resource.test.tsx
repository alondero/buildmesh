import { StrictMode, type ComponentType } from 'react';
import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { PROBE_TAB_DEFINITIONS, PROBE_TAB_ORDER, type ProbeTabDefinition } from '../../src/lib/probeContext';
import { createProbeResource, useProbeResource, type ProbeLoader } from '../../src/lib/probeResource';
import { preloadProbeTabs } from './helpers/preloadProbeTabs';
import { useChangedFiles } from '../../src/hooks/useChangedFiles';
import { useAgentChangedFiles } from '../../src/hooks/useAgentChangedFiles';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

beforeAll(preloadProbeTabs, 60_000);
afterEach(() => vi.restoreAllMocks());

it('registers a component, scoped loader, icon and context lens for every destination', async () => {
  expect(PROBE_TAB_ORDER).toHaveLength(11);
  for (const definition of Object.values(PROBE_TAB_DEFINITIONS)) {
    expect(definition.Component).toBeDefined();
    expect(definition.load).toBeTypeOf('function');
    expect(definition.icon).toBeTypeOf('function');
    expect(['host', 'mesh', 'agent']).toContain(definition.lens);
    const module = await definition.preload();
    expect(module.Component).toBeTypeOf('function');
    expect(module.load).toBeTypeOf('function');
  }
});

// Exercise the real registry adapter, replacing only the module's external read.
// This checks forwarding and resource ownership; component suites check adoption.
describe.each(PROBE_TAB_ORDER)('%s scoped resource', tab => {
  it.each(['old-first', 'new-first'] as const)('drops an outgoing subject in %s completion order', async order => {
    const definition: ProbeTabDefinition = PROBE_TAB_DEFINITIONS[tab];
    const module = await definition.preload() as { Component: ComponentType; load: ProbeLoader<unknown> };
    const outgoing = deferred<unknown>();
    const incoming = deferred<unknown>();
    const read = vi.spyOn(module, 'load').mockReturnValueOnce(outgoing.promise).mockReturnValueOnce(incoming.promise);
    const { result, rerender, unmount } = renderHook(({ id }) =>
      useProbeResource(definition.load, { meshId: id }, String(id)), { initialProps: { id: 1 } });
    const oldSignal = read.mock.calls[0][1];
    rerender({ id: 2 });
    expect(oldSignal.aborted).toBe(true);
    expect(result.current.state).toMatchObject({ data: null, loading: true, error: null });
    if (order === 'old-first') {
      await act(async () => outgoing.resolve('outgoing'));
      expect(result.current.state).toMatchObject({ data: null, loading: true });
    }
    await act(async () => incoming.resolve('incoming'));
    expect(result.current.state).toMatchObject({ data: 'incoming', loading: false, error: null });
    if (order === 'new-first') await act(async () => outgoing.resolve('outgoing'));
    expect(result.current.state.data).toBe('incoming');
    const late = deferred<unknown>();
    read.mockReturnValueOnce(late.promise);
    let pending!: Promise<boolean>;
    act(() => { pending = result.current.reload(); });
    const snapshot = result.current.getSnapshot();
    unmount();
    late.reject(new Error('late unmount failure'));
    await pending;
    expect(result.current.getSnapshot()).toBe(snapshot);
  });
});

describe('Probe resource authority', () => {
  it('keeps an action error reported after the latest read started', async () => {
    const first = deferred<string>();
    const second = deferred<string>();
    const load = vi.fn<ProbeLoader<string>>().mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const resource = createProbeResource(load, { meshId: 1 });
    resource.activate();
    const old = resource.reload();
    const current = resource.reload();
    first.resolve('superseded refresh');
    await old;
    resource.reportError('Prune failed: repository A');
    second.resolve('fresh data');
    await current;
    expect(resource.getSnapshot()).toMatchObject({ data: 'fresh data', error: 'Prune failed: repository A' });
    resource.dispose();
  });
  it('rejects stale errors and keeps busy feedback until the latest refresh settles', async () => {
    const first = deferred<string>();
    const second = deferred<string>();
    const load = vi.fn<ProbeLoader<string>>().mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const resource = createProbeResource(load, { meshId: 1 });
    resource.activate();
    const old = resource.reload({ manual: true });
    const current = resource.reload();
    first.reject(new Error('obsolete'));
    await old;
    expect(resource.getSnapshot()).toMatchObject({ loading: true, refreshing: true, error: null });
    second.resolve('current');
    await current;
    expect(resource.getSnapshot()).toMatchObject({ data: 'current', refreshing: false, loading: false });
    resource.dispose();
  });

  it('preserves acknowledged mutations against an older read and retains data on a failed reload', async () => {
    const stale = deferred<string>();
    const load = vi.fn<ProbeLoader<string>>().mockResolvedValueOnce('original').mockReturnValueOnce(stale.promise);
    const resource = createProbeResource(load, { meshId: 1 });
    resource.activate();
    await resource.reload();
    const pending = resource.reload();
    resource.mutate(() => 'acknowledged write');
    expect(load.mock.calls[1][1].aborted).toBe(true);
    stale.resolve('before write');
    await pending;
    expect(resource.getSnapshot().data).toBe('acknowledged write');
    load.mockRejectedValueOnce(new Error('offline'));
    await resource.reload();
    expect(resource.getSnapshot()).toMatchObject({ data: 'acknowledged write', error: 'offline', loading: false });
    resource.dispose();
    resource.mutate(() => 'late write');
    expect(resource.getSnapshot().data).toBe('acknowledged write');
  });

  it('starts a fresh owner when returning to a subject and survives StrictMode cleanup/restart', async () => {
    const load = vi.fn<ProbeLoader<number>>().mockImplementation(async context => context.meshId!);
    const { result, rerender } = renderHook(({ id }) => useProbeResource(load, { meshId: id }, String(id)), {
      initialProps: { id: 1 }, wrapper: StrictMode,
    });
    await act(async () => {});
    expect(result.current.state.data).toBe(1);
    rerender({ id: 2 });
    await act(async () => {});
    expect(result.current.state.data).toBe(2);
    rerender({ id: 1 });
    expect(result.current.state.data).toBeNull();
    await act(async () => {});
    expect(result.current.state.data).toBe(1);
  });
});

it('routes production path-cache reads through the registered loaders', async () => {
  const files = [{ path: 'changed.ts', status: 'modified' as const, additions: 1, deletions: 0 }];
  const projectRead = vi.spyOn(PROBE_TAB_DEFINITIONS.files, 'load').mockResolvedValue(files);
  const agentRead = vi.spyOn(PROBE_TAB_DEFINITIONS.review, 'load').mockResolvedValue(files);
  const { result } = renderHook(() => ({
    project: useChangedFiles('/registry-cache-wiring'),
    agent: useAgentChangedFiles(999_2530, '/registry-cache-wiring/agent'),
  }));
  await act(async () => {});
  expect(projectRead).toHaveBeenCalledWith({ meshId: null, activePath: '/registry-cache-wiring' }, expect.any(AbortSignal));
  expect(agentRead).toHaveBeenCalledWith({ meshId: null, nodeId: 999_2530 }, expect.any(AbortSignal));
  expect(result.current.project.files).toEqual(files);
  expect(result.current.agent.files).toEqual(files);
});
