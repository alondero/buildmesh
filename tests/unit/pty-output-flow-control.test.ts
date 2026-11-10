import { describe, it, expect, vi, afterEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  subscribeAgentOutput, unsubscribeAgentOutput,
  subscribeBuildRunOutput, unsubscribeBuildRunOutput,
} from '../../src/lib/tauri';

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
type TestChannel = { id: number; onmessage: (message: unknown) => void };
function channel(command: string): TestChannel {
  const calls = vi.mocked(invoke).mock.calls.filter(([name]) => name === command);
  return calls.at(-1)![1]!.onChunk as TestChannel;
}

afterEach(async () => {
  await unsubscribeAgentOutput(2727);
  await unsubscribeBuildRunOutput(2727);
  vi.restoreAllMocks();
  vi.mocked(invoke).mockReset().mockResolvedValue(undefined);
});

describe.each([
  ['agent', subscribeAgentOutput, unsubscribeAgentOutput, 'subscribe_agent_output', false],
  ['build/run', subscribeBuildRunOutput, unsubscribeBuildRunOutput, 'subscribe_build_run_output', true],
] as const)('%s output credit', (_name, subscribe, unsubscribe, command, buildRun) => {
  it('withholds credit behind a held body then delivers split UTF-8/ANSI in byte order', async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);
    const chunks: Uint8Array[] = [];
    await subscribe(2727, bytes => chunks.push(bytes));
    const output = channel(command);
    let release!: (bytes: ArrayBuffer) => void;
    const body = new Promise<ArrayBuffer>(resolve => { release = resolve; });
    output.onmessage({ arrayBuffer: () => body });
    const later = [new Uint8Array([0x96]), new Uint8Array([0x88, 0x1b]),
      new Uint8Array([0x5b, 0x33, 0x31, 0x6d])];
    for (const bytes of later) output.onmessage(bytes);
    await tick();
    expect(chunks).toEqual([]);
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'acknowledge_pty_output')).toEqual([]);
    release(new Uint8Array([0xe2]).buffer);
    await tick();
    expect(chunks).toEqual([new Uint8Array([0xe2]), ...later]);
    expect(invoke).toHaveBeenCalledWith('acknowledge_pty_output', {
      sessionId: 2727, channelId: output.id, sequence: 4, buildRun,
    });
  });

  it('keeps small frames synchronous and coalesces acknowledgements with one IPC in flight', async () => {
    await subscribe(2727, () => {});
    const output = channel(command);
    let finishAck!: () => void;
    vi.mocked(invoke).mockImplementation(async (cmd) => {
      if (cmd === 'acknowledge_pty_output') await new Promise<void>(resolve => { finishAck = resolve; });
    });
    output.onmessage(new Uint8Array([1]));
    await tick();
    for (let index = 0; index < 7; index++) output.onmessage(new Uint8Array([2]));
    await tick();
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'acknowledge_pty_output')).toHaveLength(1);
    finishAck();
    await tick();
    expect(invoke).toHaveBeenLastCalledWith('acknowledge_pty_output', {
      sessionId: 2727, channelId: output.id, sequence: 8, buildRun,
    });
    // Settle the second IPC before teardown.
    finishAck();
    await tick();
  });

  it('cancels a stalled decoder and queued bodies without delivering into its replacement', async () => {
    const obsolete = vi.fn();
    await subscribe(2727, obsolete);
    const old = channel(command);
    let release!: (bytes: ArrayBuffer) => void;
    old.onmessage({ arrayBuffer: () => new Promise<ArrayBuffer>(resolve => { release = resolve; }) });
    const queuedRead = vi.fn(async () => new Uint8Array([2]).buffer);
    old.onmessage({ arrayBuffer: queuedRead });
    await unsubscribe(2727);
    const current = vi.fn();
    await subscribe(2727, current);
    const replacement = channel(command);
    replacement.onmessage(new Uint8Array([3]));
    release(new Uint8Array([1]).buffer);
    await tick();
    expect(obsolete).not.toHaveBeenCalled();
    expect(queuedRead).not.toHaveBeenCalled();
    expect(current).toHaveBeenCalledExactlyOnceWith(new Uint8Array([3]));
    expect(invoke).toHaveBeenCalledWith('acknowledge_pty_output', {
      sessionId: 2727, channelId: replacement.id, sequence: 1, buildRun,
    });
    expect(vi.mocked(invoke).mock.calls.filter(([cmd, args]) =>
      cmd === 'acknowledge_pty_output' && args?.channelId === old.id)).toEqual([]);
  });

  it('closes a failed decode without returning credit and leaves a replacement usable', async () => {
    const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
    const consumer = vi.fn();
    await subscribe(2727, consumer);
    const output = channel(command);
    output.onmessage({ arrayBuffer: async () => { throw new Error('body failed'); } });
    output.onmessage(new Uint8Array([2]));
    await tick();
    expect(consumer).not.toHaveBeenCalled();
    expect(errorLog).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenCalledWith(command.replace('subscribe_', 'unsubscribe_'), {
      sessionId: 2727, channelId: output.id,
    });
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'acknowledge_pty_output')).toEqual([]);
    await subscribe(2727, consumer);
    channel(command).onmessage(new Uint8Array([3]));
    expect(consumer).toHaveBeenCalledExactlyOnceWith(new Uint8Array([3]));
  });

  it('cancels a real stalled Response reader and releases its stream lock', async () => {
    const cancelled = vi.fn();
    const stream = new ReadableStream<Uint8Array>({
      start(controller) { controller.enqueue(new Uint8Array([1])); },
      cancel: cancelled,
    });
    const consumer = vi.fn();
    await subscribe(2727, consumer);
    channel(command).onmessage(new Response(stream));
    await tick();
    expect(stream.locked).toBe(true);
    await unsubscribe(2727);
    await tick();
    expect(cancelled).toHaveBeenCalledOnce();
    expect(stream.locked).toBe(false);
    expect(consumer).not.toHaveBeenCalled();
  });

  it('keeps delayed cleanup bound to its original pending registration', async () => {
    let finishOld!: () => void;
    vi.mocked(invoke).mockImplementationOnce(() => new Promise<void>(resolve => { finishOld = resolve; }));
    const oldReady = subscribe(2727, () => {});
    const old = channel(command);
    const releaseOld = unsubscribe(2727, oldReady);
    const consumer = vi.fn();
    const replacementReady = subscribe(2727, consumer);
    await replacementReady;
    const replacement = channel(command);
    finishOld();
    await releaseOld;
    expect(invoke).toHaveBeenCalledWith(command.replace('subscribe_', 'unsubscribe_'), {
      sessionId: 2727, channelId: old.id,
    });
    replacement.onmessage(new Uint8Array([3]));
    expect(consumer).toHaveBeenCalledExactlyOnceWith(new Uint8Array([3]));
  });
});

it('keeps agent and Build/Run subscriptions separate at the same session id', async () => {
  const agent = vi.fn();
  const build = vi.fn();
  await subscribeAgentOutput(2727, agent);
  await subscribeBuildRunOutput(2727, build);
  const agentChannel = channel('subscribe_agent_output');
  const buildChannel = channel('subscribe_build_run_output');
  await unsubscribeAgentOutput(2727);
  agentChannel.onmessage(new Uint8Array([1]));
  buildChannel.onmessage(new Uint8Array([2]));
  expect(agent).not.toHaveBeenCalled();
  expect(build).toHaveBeenCalledExactlyOnceWith(new Uint8Array([2]));
});
