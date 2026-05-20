type WriteFn = (data: string) => void;
type SchedulerFn = (cb: () => void) => void;

interface BufferEntry {
  buffer: string;
  frameRequested: boolean;
}

export class TerminalWriter {
  private entries = new Map<number, BufferEntry>();
  private writeFns = new Map<number, WriteFn>();
  private scheduler: SchedulerFn;

  constructor(scheduler: SchedulerFn = (cb) => requestAnimationFrame(cb)) {
    this.scheduler = scheduler;
  }

  register(nodeId: number, writeFn: WriteFn): void {
    this.entries.set(nodeId, { buffer: '', frameRequested: false });
    this.writeFns.set(nodeId, writeFn);
  }

  unregister(nodeId: number): void {
    this.entries.delete(nodeId);
    this.writeFns.delete(nodeId);
  }

  append(nodeId: number, data: string): void {
    const entry = this.entries.get(nodeId);
    if (!entry) return;
    entry.buffer += data;
    this.scheduleFlush(nodeId, entry);
  }

  private scheduleFlush(nodeId: number, entry: BufferEntry): void {
    if (entry.frameRequested) return;
    entry.frameRequested = true;
    this.scheduler(() => {
      const current = this.entries.get(nodeId);
      if (current === entry && entry.buffer) {
        const writeFn = this.writeFns.get(nodeId);
        writeFn?.(entry.buffer);
        entry.buffer = '';
      }
      entry.frameRequested = false;
    });
  }

  has(nodeId: number): boolean {
    return this.entries.has(nodeId);
  }

  pendingBytes(nodeId: number): number {
    return this.entries.get(nodeId)?.buffer.length ?? 0;
  }
}
