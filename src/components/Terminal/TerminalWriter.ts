export type TerminalWriteData = string | Uint8Array;

type WriteFn = (data: TerminalWriteData) => void;
type SchedulerFn = (cb: () => void) => void;

interface BufferEntry {
  chunks: TerminalWriteData[];
  frameRequested: boolean;
}

function isByteChunk(data: TerminalWriteData): data is Uint8Array {
  return data instanceof Uint8Array;
}

function mergeByteChunks(chunks: Uint8Array[]): Uint8Array {
  const totalLength = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
  const merged = new Uint8Array(totalLength);
  let offset = 0;
  for (const chunk of chunks) {
    merged.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return merged;
}

function coalesceChunks(chunks: TerminalWriteData[]): TerminalWriteData[] {
  if (chunks.length === 0) return [];
  if (chunks.every(chunk => typeof chunk === 'string')) {
    return [chunks.join('')];
  }
  if (chunks.every(isByteChunk)) {
    return [mergeByteChunks(chunks)];
  }
  return chunks;
}

export class TerminalWriter {
  private entries = new Map<number, BufferEntry>();
  private writeFns = new Map<number, WriteFn>();
  private scheduler: SchedulerFn;

  constructor(scheduler: SchedulerFn = (cb) => requestAnimationFrame(cb)) {
    this.scheduler = scheduler;
  }

  register(nodeId: number, writeFn: WriteFn): void {
    this.entries.set(nodeId, { chunks: [], frameRequested: false });
    this.writeFns.set(nodeId, writeFn);
  }

  unregister(nodeId: number): void {
    this.entries.delete(nodeId);
    this.writeFns.delete(nodeId);
  }

  append(nodeId: number, data: TerminalWriteData): void {
    const entry = this.entries.get(nodeId);
    if (!entry) return;
    entry.chunks.push(data);
    this.scheduleFlush(nodeId, entry);
  }

  private scheduleFlush(nodeId: number, entry: BufferEntry): void {
    if (entry.frameRequested) return;
    entry.frameRequested = true;
    this.scheduler(() => {
      const current = this.entries.get(nodeId);
      if (current === entry && entry.chunks.length > 0) {
        const writeFn = this.writeFns.get(nodeId);
        const chunks = coalesceChunks(entry.chunks);
        entry.chunks = [];
        for (const chunk of chunks) {
          writeFn?.(chunk);
        }
      }
      entry.frameRequested = false;
    });
  }

  has(nodeId: number): boolean {
    return this.entries.has(nodeId);
  }

  pendingBytes(nodeId: number): number {
    return this.entries.get(nodeId)?.chunks.reduce((sum, chunk) => {
      return sum + (typeof chunk === 'string' ? chunk.length : chunk.byteLength);
    }, 0) ?? 0;
  }
}
