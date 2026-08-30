import { describe, it, expect, vi, beforeEach } from 'vitest';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
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

  it('reads large raw Channel frames delivered as a Response', async () => {
    const chunks: Uint8Array[] = [];
    await subscribeAgentOutput(43, (data) => chunks.push(data));

    const expected = new Uint8Array(2048);
    expected[0] = 1;
    expected[expected.length - 1] = 3;

    const { onChunk } = vi.mocked(invoke).mock.calls[0]![1] as {
      onChunk: { onmessage: (message: unknown) => void };
    };
    onChunk.onmessage(new Response(expected.buffer));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(chunks).toEqual([expected]);
  });

  it('preserves frame order while reading asynchronous Channel responses', async () => {
    const chunks: Uint8Array[] = [];
    await subscribeAgentOutput(44, (data) => chunks.push(data));

    let releaseFirst!: (value: ArrayBuffer) => void;
    const firstBody = new Promise<ArrayBuffer>((resolve) => {
      releaseFirst = resolve;
    });
    const firstResponse = { arrayBuffer: () => firstBody };
    const secondResponse = {
      arrayBuffer: () => Promise.resolve(new Uint8Array([2]).buffer),
    };
    const { onChunk } = vi.mocked(invoke).mock.calls[0]![1] as {
      onChunk: { onmessage: (message: unknown) => void };
    };

    onChunk.onmessage(firstResponse);
    onChunk.onmessage(secondResponse);
    await Promise.resolve();
    expect(chunks).toEqual([]);

    releaseFirst(new Uint8Array([1]).buffer);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(chunks).toEqual([new Uint8Array([1]), new Uint8Array([2])]);
  });

  it('invokes unsubscribe_agent_output with the session id', async () => {
    await unsubscribeAgentOutput(7);
    expect(invoke).toHaveBeenCalledWith('unsubscribe_agent_output', { sessionId: 7 });
  });
});

describe('unsubscribeAgentOutput call sites', () => {
  it('is invoked from TerminalRegistry.dispose only', () => {
    const srcRoot = join(__dirname, '..', '..', 'src');
    const registrySrc = readFileSync(
      join(srcRoot, 'components', 'Terminal', 'TerminalRegistry.ts'),
      'utf8',
    );
    expect(registrySrc).toMatch(/unsubscribeAgentOutput\s*\(/);

    const hits: string[] = [];
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir)) {
        const full = join(dir, entry);
        const st = statSync(full);
        if (st.isDirectory()) walk(full);
        else if (/\.(ts|tsx)$/.test(entry)) {
          const src = readFileSync(full, 'utf8');
          const cleaned = src
            .replace(/\/\*[\s\S]*?\*\//g, '')
            .replace(/\/\/.*$/gm, '');
          if (!/\bunsubscribeAgentOutput\s*\(/.test(cleaned)) continue;
          const rel = full.replace(/\\/g, '/');
          if (rel.endsWith('/src/lib/tauri.ts')) continue;
          if (rel.endsWith('/src/components/Terminal/TerminalRegistry.ts')) continue;
          hits.push(rel);
        }
      }
    };
    walk(srcRoot);
    expect(
      hits,
      'unsubscribeAgentOutput drops the node-scoped PTY Channel; only TerminalRegistry.dispose may call it',
    ).toEqual([]);
  });
});

describe('Channel mock hygiene', () => {
  it('does not copy-paste a Channel class into test files', () => {
    const root = join(__dirname, '..');
    const hits: string[] = [];
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir)) {
        const full = join(dir, entry);
        const st = statSync(full);
        if (st.isDirectory()) walk(full);
        else if (/\.(ts|tsx)$/.test(entry) && !full.endsWith('tauriChannel.ts')) {
          const src = readFileSync(full, 'utf8');
          if (/class Channel\s*\{/.test(src)) hits.push(full);
        }
      }
    };
    walk(root);
    expect(hits, 'import MockChannel from tests/setup/tauriChannel.ts instead').toEqual([]);
  });
});
