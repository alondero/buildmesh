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
 * after a long session. 16 bytes covers one ASCII keystroke echo, a small
 * ANSI cursor response (`\x1b[1A` = 4 bytes, `\x1b[K` = 3 bytes), and a
 * single multi-byte UTF-8 box-drawing sequence (the agent's `pump_pty_output`
 * push boundary is the `read()` slice, ~bytes not codepoints, so a single
 * ▀ U+2580 echo arrives as 3 bytes).
 *
 * Past this size we fall back to rAF batching so a verbose build log dump
 * or a flood of agent output coalesces into one xterm write per frame,
 * avoiding the 100-writes-per-frame storm that the importer of this file
 * originally solved (issue #303).
 */
export const INTERACTIVE_FAST_PATH_BYTES = 16;

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
 * True iff every byte in `data` is ASCII (< 0x80). Used for the
 * `(unused)` strict-ASCII fast-path; replaced by `isFastPathSafe`
 * below to also cover small complete UTF-8 sequences. Kept exported
 * via the test surface so callers can assert the strict-ASCII behaviour
 * independently of the broader safety predicate.
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

/**
 * True iff `data` is safe to write directly to xterm without going
 * through the buffered rAF flush. The safety criterion is "no partial
 * UTF-8 codepoint at the chunk boundary", because the agent's PTY
 * byte-chunk boundary is the `read()` slice, not a codepoint boundary,
 * and writing a partial sequence to xterm corrupts the character —
 * the split-chunk test in
 * `tests/unit/build-run-terminal-raf-batching.test.tsx` pins this.
 *
 * - JS strings are atomic from xterm's perspective (the renderer
 *   handles UTF-16 surrogate pairs internally), so any string is safe.
 * - `Uint8Array`s are walked to verify each UTF-8 sequence is complete.
 *   A truncated sequence at the end means the next chunk will complete
 *   it — defer to rAF so the chunks can be merged.
 * - Lone continuation bytes (0x80-0xBF at chunk start, or 0xFE/0xFF
 *   which are invalid UTF-8 anywhere) are treated as unsafe: the
 *   conservative fallback to rAF lets the next chunk re-establish
 *   framing.
 */
function isFastPathSafe(data: TerminalWriteData): boolean {
  if (typeof data === 'string') {
    // Strings are atomic — xterm's renderer handles UTF-16 surrogate
    // pairs internally, so a string is always safe to write directly.
    return true;
  }
  let i = 0;
  while (i < data.byteLength) {
    const b = data[i] ?? 0;
    if (b < 0x80) {
      i++;
    } else if ((b & 0xe0) === 0xc0) {
      // 2-byte sequence start
      if (i + 1 >= data.byteLength) return false;
      i += 2;
    } else if ((b & 0xf0) === 0xe0) {
      // 3-byte sequence start
      if (i + 2 >= data.byteLength) return false;
      i += 3;
    } else if ((b & 0xf8) === 0xf0) {
      // 4-byte sequence start
      if (i + 3 >= data.byteLength) return false;
      i += 4;
    } else {
      // Invalid UTF-8 byte (continuation byte at start, or 0xFE/0xFF).
      return false;
    }
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
    // Fast path: a single small interactive echo (issue #1122) goes
    // straight to xterm. Without this, the chain is
    //   `agent-output` event → TerminalWriter rAF → term.write →
    //   xterm's own internal render-rAF → visible
    // and the user's keystroke waits two frames before drawing. The
    // direct write still goes through xterm's render rAF (we can't
    // avoid that), but we skip our rAF — the visible state lands on
    // xterm's next frame, which is the same frame the user would have
    // gotten if the writer didn't exist at all.
    //
    // UTF-8 boundary check (see `isFastPathSafe`) covers both ASCII
    // keystroke echoes AND small UTF-8 sequences (a single-character
    // ▀ U+2580 echo is 3 bytes — well within the 4-byte fast path).
    // A partial UTF-8 sequence at the chunk boundary is rare (the
    // PTY reader's `read()` typically returns a complete codepoint)
    // but we defer to rAF so the chunks can be merged in
    // `coalesceChunks` if the next chunk completes the sequence.
    if (
      entry.pendingBytes <= INTERACTIVE_FAST_PATH_BYTES &&
      entry.chunks.length === 1 &&
      !entry.frameRequested &&
      isFastPathSafe(data)
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
