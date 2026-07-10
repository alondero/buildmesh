export type TerminalWriteData = string | Uint8Array;

type WriteFn = (data: TerminalWriteData) => void;
type SchedulerFn = (cb: () => void) => void;

/**
 * Cap on unflushed bytes buffered per node. The flush scheduler is
 * requestAnimationFrame, which Chromium suspends while the window is hidden
 * or minimized — so with agents streaming output overnight the pending
 * buffer would otherwise grow without bound, and restoring the window would
 * feed the whole backlog to xterm in one freeze-inducing write. 4 MiB
 * comfortably exceeds what xterm's 10k-line scrollback can retain, so
 * dropping older chunks past the cap loses nothing the user could scroll
 * back to. Full-screen agent TUIs redraw on their next frame, which repairs
 * any escape sequence a drop may have severed.
 */
export const MAX_PENDING_BYTES = 4 * 1024 * 1024;

interface BufferEntry {
  chunks: TerminalWriteData[];
  pendingBytes: number;
  frameRequested: boolean;
}

function byteLength(data: TerminalWriteData): number {
  return typeof data === 'string' ? data.length : data.byteLength;
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
    this.entries.set(nodeId, { chunks: [], pendingBytes: 0, frameRequested: false });
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
    entry.pendingBytes += byteLength(data);
    // Enforce the cap by dropping the OLDEST chunks — never the one just
    // appended (`length > 1` guard), so a single oversized chunk still
    // flushes whole. See MAX_PENDING_BYTES for why the cap exists.
    while (entry.pendingBytes > MAX_PENDING_BYTES && entry.chunks.length > 1) {
      const dropped = entry.chunks.shift()!;
      entry.pendingBytes -= byteLength(dropped);
    }
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
        entry.pendingBytes = 0;
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
    return this.entries.get(nodeId)?.pendingBytes ?? 0;
  }
}
