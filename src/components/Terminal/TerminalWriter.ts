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

/**
 * Maximum payload size that takes the interactive fast path (issue #1122).
 * A single-byte keystroke echo lands here: the writer ships it straight to
 * xterm without queuing a `requestAnimationFrame`. Going through rAF would
 * stack on top of xterm's own internal render-rAF, adding one full frame
 * of latency per keystroke that becomes visible as "typing feels sluggish"
 * after a long session. 4 bytes is enough for one ASCII char + a 3-byte
 * UTF-8 sequence (most CLI box-drawing yields are <4 bytes per chunk from
 * the agent's echo).
 *
 * Past this size we fall back to rAF batching so a verbose build log dump
 * or a flood of agent output coalesces into one xterm write per frame,
 * avoiding the 100-writes-per-frame storm that the importer of this file
 * originally solved (issue #303).
 */
export const INTERACTIVE_FAST_PATH_BYTES = 4;

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

function flushEntry(entry: BufferEntry, writeFn: WriteFn | undefined): void {
  if (entry.chunks.length === 0 || !writeFn) return;
  const chunks = coalesceChunks(entry.chunks);
  entry.chunks = [];
  entry.pendingBytes = 0;
  for (const chunk of chunks) {
    writeFn(chunk);
  }
}

/**
 * True iff every byte in `data` is ASCII (< 0x80). The interactive fast
 * path is restricted to ASCII because a multi-byte UTF-8 codepoint can
 * arrive split across two PTY chunks (the agent's `pump_pty_output`
 * push boundary is the byte chunk from `read()`, not a codepoint
 * boundary). Letting a partial UTF-8 sequence write directly to xterm
 * would corrupt the character — the split chunk test in
 * `tests/unit/build-run-terminal-raf-batching.test.tsx` pins this.
 * ASCII is safe by definition: no codepoint spans more than one byte.
 */
function isAsciiPayload(data: TerminalWriteData): boolean {
  if (typeof data === 'string') {
    for (let i = 0; i < data.length; i++) {
      if (data.charCodeAt(i) > 0x7f) return false;
    }
    return true;
  }
  for (let i = 0; i < data.byteLength; i++) {
    if (data[i] > 0x7f) return false;
  }
  return true;
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
    // Fast path: a single small ASCII interactive echo (issue #1122)
    // goes straight to xterm. Without this, the chain is
    //   `agent-output` event → TerminalWriter rAF → term.write →
    //   xterm's own internal render-rAF → visible
    // and the user's keystroke waits two frames before drawing. The
    // direct write still goes through xterm's render rAF (we can't
    // avoid that), but we skip our rAF — the visible state lands on
    // xterm's next frame, which is the same frame the user would have
    // gotten if the writer didn't exist at all.
    //
    // ASCII-only is enforced (see `isAsciiPayload`) because a multi-byte
    // UTF-8 codepoint can arrive split across two chunks from the PTY
    // reader — letting the partial sequence write directly would corrupt
    // the character. Keystroke echoes are virtually always ASCII (the
    // agent's readline echoes character-by-character), so the ASCII
    // guard does not regress the interactive latency win in practice.
    if (
      entry.pendingBytes <= INTERACTIVE_FAST_PATH_BYTES &&
      entry.chunks.length === 1 &&
      !entry.frameRequested &&
      isAsciiPayload(data)
    ) {
      flushEntry(entry, this.writeFns.get(nodeId));
      return;
    }
    this.scheduleFlush(nodeId, entry);
  }

  private scheduleFlush(nodeId: number, entry: BufferEntry): void {
    if (entry.frameRequested) return;
    entry.frameRequested = true;
    this.scheduler(() => {
      const current = this.entries.get(nodeId);
      if (current === entry) {
        flushEntry(current, this.writeFns.get(nodeId));
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
