import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  bytesFromChannelMessage,
  subscribeAgentOutput,
  unsubscribeAgentOutput,
} from '../../src/lib/tauri';

describe('bytesFromChannelMessage', () => {
  it('passes Uint8Array through', () => {
    const src = new Uint8Array([0xe2, 0x96, 0x88]);
    expect(bytesFromChannelMessage(src)).toEqual(src);
  });

  it('wraps ArrayBuffer', () => {
    const src = new Uint8Array([1, 2, 3]).buffer;
    expect(bytesFromChannelMessage(src)).toEqual(new Uint8Array([1, 2, 3]));
  });

  it('copies a DataView slice without including surrounding bytes', () => {
    const backing = new Uint8Array([0, 1, 2, 3, 4]);
    const view = new DataView(backing.buffer, 1, 3);
    expect(bytesFromChannelMessage(view)).toEqual(new Uint8Array([1, 2, 3]));
  });

  it('returns null for non-binary payloads (JSON end markers, etc.)', () => {
    expect(bytesFromChannelMessage({ end: true })).toBeNull();
    expect(bytesFromChannelMessage('hello')).toBeNull();
    expect(bytesFromChannelMessage(null)).toBeNull();
  });
});

describe('subscribeAgentOutput / unsubscribeAgentOutput', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockClear();
    vi.mocked(invoke).mockResolvedValue(undefined);
  });

  it('invokes subscribe_agent_output with a Channel whose onmessage delivers bytes', async () => {
    const chunks: Uint8Array[] = [];
    await subscribeAgentOutput(42, (data) => chunks.push(data));

    expect(invoke).toHaveBeenCalledWith('subscribe_agent_output', {
      sessionId: 42,
      onChunk: expect.any(Object),
    });

    const { onChunk } = vi.mocked(invoke).mock.calls[0]![1] as {
      onChunk: { onmessage: (message: unknown) => void };
    };
    onChunk.onmessage(new Uint8Array([0xe2, 0x96, 0x88]));
    expect(chunks).toEqual([new Uint8Array([0xe2, 0x96, 0x88])]);
  });

  it('invokes unsubscribe_agent_output with the session id', async () => {
    await unsubscribeAgentOutput(7);
    expect(invoke).toHaveBeenCalledWith('unsubscribe_agent_output', { sessionId: 7 });
  });
});
