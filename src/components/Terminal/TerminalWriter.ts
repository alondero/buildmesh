export type TerminalWriteData = string | Uint8Array;

/**
 * Sink for one terminal. `done` is the sink's parse-completion signal: xterm's
 * `write(data, callback)` invokes it once the payload has been parsed. Sinks
 * registered with `{ completionAware: true }` receive it and the writer then
 * keeps at most {@link MAX_INFLIGHT_WRITES} payloads outstanding; every other
 * sink leaves it `undefined` and is written to without a parser budget.
 */
type WriteFn = (data: TerminalWriteData, done?: () => void) => void;
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
 * Cap on buffered chunk *objects* per node (issue #2018). The byte cap above
 * does not bound retention: a PTY reading in small slices spends one array
 * slot and one string object per chunk on top of the payload. Measured with
 * rAF paused, 82-byte build lines sitting at the 4 MiB byte cap retained
 * 17.4 MiB — over four times the byte budget.
 *
 * 4096 is far above what the native producer sends: the Rust batcher
 * coalesces 8 ms / 32 KiB windows (`pty::batch`), so reaching the 4 MiB byte
 * cap through the real output path takes ~128 chunks. The budget only bites
 * on small-slice storms, where it trades dropped objects (already unreachable
 * in xterm's scrollback) for bounded retention.
 */
export const MAX_PENDING_CHUNKS = 4096;

/**
 * Cap on payloads handed to xterm that have not finished parsing (issue
 * #2018). `term.write` queues: the parse happens asynchronously, so a
 * producer outrunning the parser used to grow xterm's internal write queue
 * without limit. Measured pre-#2018: 200 frames of output produced 200
 * outstanding unparsed payloads. Once the budget is reached the writer keeps
 * buffering (still bounded by the two caps above) and resumes on a later
 * frame, so no new output class is lost.
 *
 * The interactive fast path is exempt: a keystroke echo must never wait on
 * parser backlog, and it is bounded by {@link INTERACTIVE_FAST_PATH_BYTES}.
 */
export const MAX_INFLIGHT_WRITES = 4;

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

/**
 * Empty-string marker written over an evicted slot. Eviction advances a head
 * index instead of `Array.shift` (issue #2018), so the dropped reference has
 * to be overwritten explicitly or the whole buffer stays reachable from the
 * backing array's tail. An empty string keeps the array packed-element; a
 * `delete` would punch holes and deoptimise the whole store.
 */
const EVICTED = '';

interface BufferEntry {
  /**
   * Backing store for the queue. Live chunks occupy `[head, length)`;
   * everything below `head` holds {@link EVICTED}.
   */
  chunks: TerminalWriteData[];
  /** Index of the oldest live chunk. */
  head: number;
  pendingBytes: number;
  frameRequested: boolean;
  /** Payloads handed to xterm that have not reported parse completion. */
  inFlight: number;
  /** True when the registered sink reports parse completion via `done`. */
  completionAware: boolean;
}

/**
 * Rebuild the backing store once the evicted prefix is worth reclaiming.
 * Amortized constant time: the slice fires once per `COMPACT_AFTER`
 * evictions, so eviction stays O(1) per chunk (issue #2018).
 */
const COMPACT_AFTER = 1024;

function byteLength(data: TerminalWriteData): number {
  return typeof data === 'string' ? data.length : data.byteLength;
}

function isByteChunk(data: TerminalWriteData): data is Uint8Array {
  return data instanceof Uint8Array;
}

function mergeByteChunks(chunks: readonly TerminalWriteData[], head: number, count: number): Uint8Array {
  const end = head + count;
  let totalLength = 0;
  for (let i = head; i < end; i++) {
    totalLength += (chunks[i] as Uint8Array).byteLength;
  }
  const merged = new Uint8Array(totalLength);
  let offset = 0;
  for (let i = head; i < end; i++) {
    const chunk = chunks[i] as Uint8Array;
    merged.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return merged;
}

/**
 * Merge the live window `[head, head + count)` into the smallest payload
 * xterm can parse in one pass. Reads through the backing store rather than a
 * `slice()` copy, so a flush allocates only the merged payload itself.
 */
function coalesceChunks(
  chunks: readonly TerminalWriteData[],
  head: number,
  count: number
): TerminalWriteData[] {
  if (count === 0) return [];
  const end = head + count;
  let allStrings = true;
  let allBytes = true;
  for (let i = head; i < end; i++) {
    const chunk = chunks[i];
    if (typeof chunk !== 'string') allStrings = false;
    if (!isByteChunk(chunk)) allBytes = false;
  }
  if (allStrings) {
    let joined = '';
    for (let i = head; i < end; i++) joined += chunks[i] as string;
    return [joined];
  }
  if (allBytes) {
    return [mergeByteChunks(chunks, head, count)];
  }
  // Mixed string + byte chunks (issue #1749): a bursty PTY flush with
  // interleaved types would otherwise hit xterm with one write per chunk.
  // Decode bytes to strings via a single streaming TextDecoder per flush
  // so a multi-byte UTF-8 sequence split across contiguous byte chunks
  // reassembles instead of emitting replacement characters, then join to
  // one string. The string path is atomic for xterm's renderer (see
  // `isFastPathSafe`), so a single string write preserves escape sequences
  // exactly as the chunk order arrived.
  //
  // The decoder is flushed before each string chunk: a partial sequence
  // buffered from an earlier byte chunk must not jump over an interleaved
  // string when its remainder arrives later (`[0xE2 0x96], "X", [0x80]`
  // renders "�X�" in arrival order, never a reordered "X▀"). Contiguous
  // byte runs still stream through one decoder instance, so genuine PTY
  // read-slice splits reassemble.
  const decoder = new TextDecoder();
  const parts: string[] = [];
  for (let i = head; i < end; i++) {
    const chunk = chunks[i];
    if (typeof chunk === 'string') {
      parts.push(decoder.decode());
      parts.push(chunk);
    } else {
      parts.push(decoder.decode(chunk, { stream: true }));
    }
  }
  parts.push(decoder.decode());
  return [parts.join('')];
}

/** Drop every queued chunk and release the backing store. */
function releaseAll(entry: BufferEntry): void {
  entry.chunks.length = 0;
  entry.head = 0;
  entry.pendingBytes = 0;
}

function flushEntry(entry: BufferEntry, writeFn: WriteFn | undefined, onComplete?: () => void): void {
  const count = entry.chunks.length - entry.head;
  if (count === 0 || !writeFn) return;
  const chunks = coalesceChunks(entry.chunks, entry.head, count);
  releaseAll(entry);
  for (const chunk of chunks) {
    if (entry.completionAware) {
      entry.inFlight++;
      writeFn(chunk, () => {
        entry.inFlight--;
        onComplete?.();
      });
    } else {
      writeFn(chunk);
    }
  }
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

export interface TerminalWriterOptions {
  /**
   * Set when the sink honours the `done` callback of
   * {@link WriteFn}. Only then does the writer keep an in-flight count and
   * defer flushes under {@link MAX_INFLIGHT_WRITES} — a sink that accepts
   * `done` but never calls it would otherwise stall the queue, so the
   * capability has to be declared rather than inferred.
   */
  completionAware?: boolean;
}

export class TerminalWriter {
  private entries = new Map<number, BufferEntry>();
  private writeFns = new Map<number, WriteFn>();
  private scheduler: SchedulerFn;

  constructor(scheduler: SchedulerFn = (cb) => requestAnimationFrame(cb)) {
    this.scheduler = scheduler;
  }

  register(nodeId: number, writeFn: WriteFn, options: TerminalWriterOptions = {}): void {
    this.entries.set(nodeId, {
      chunks: [],
      head: 0,
      pendingBytes: 0,
      frameRequested: false,
      inFlight: 0,
      completionAware: options.completionAware === true,
    });
    this.writeFns.set(nodeId, writeFn);
  }

  unregister(nodeId: number): void {
    const entry = this.entries.get(nodeId);
    // Release the queued payloads even when a frame is still pending: the
    // scheduled callback holds this entry until it runs, and a deleted node's
    // backlog is unreachable by definition (issue #2018).
    if (entry) releaseAll(entry);
    this.entries.delete(nodeId);
    this.writeFns.delete(nodeId);
  }

  append(nodeId: number, data: TerminalWriteData): void {
    const entry = this.entries.get(nodeId);
    if (!entry) return;
    entry.chunks.push(data);
    entry.pendingBytes += byteLength(data);
    // Enforce both caps by dropping the OLDEST chunks — never the one just
    // appended (`count > 1` guard), so a single oversized chunk still flushes
    // whole. Eviction advances `head` instead of `Array.shift`: the old form
    // re-indexed the entire array per dropped chunk, which is quadratic
    // against the backlog a hidden window builds up (issue #2018).
    let count = entry.chunks.length - entry.head;
    while (count > 1 && (entry.pendingBytes > MAX_PENDING_BYTES || count > MAX_PENDING_CHUNKS)) {
      const dropped = entry.chunks[entry.head];
      entry.chunks[entry.head] = EVICTED;
      entry.head++;
      count--;
      entry.pendingBytes -= byteLength(dropped);
    }
    this.compact(entry);
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
    //
    // The fast path deliberately bypasses the in-flight budget: a keystroke
    // echo must not queue behind a parse backlog, and at most
    // INTERACTIVE_FAST_PATH_BYTES per echo it cannot itself become one.
    if (
      entry.pendingBytes <= INTERACTIVE_FAST_PATH_BYTES &&
      count === 1 &&
      !entry.frameRequested &&
      isFastPathSafe(data)
    ) {
      flushEntry(entry, this.writeFns.get(nodeId));
      return;
    }
    this.scheduleFlush(nodeId, entry);
  }

  /**
   * Reclaim the evicted prefix once it dominates the backing store, so the
   * array cannot grow past roughly twice the live window. `slice` also covers
   * the fully-drained case (an empty result), so no separate branch is needed.
   */
  private compact(entry: BufferEntry): void {
    if (entry.head >= COMPACT_AFTER && entry.head * 2 >= entry.chunks.length) {
      entry.chunks = entry.chunks.slice(entry.head);
      entry.head = 0;
    }
  }

  private scheduleFlush(nodeId: number, entry: BufferEntry): void {
    if (entry.frameRequested) return;
    entry.frameRequested = true;
    this.scheduler(() => this.flushFrame(nodeId, entry));
  }

  private flushFrame(nodeId: number, entry: BufferEntry): void {
    // Cleared first so the re-arm below is allowed, and so an append landing
    // during the flush can request a fresh frame.
    entry.frameRequested = false;
    if (this.entries.get(nodeId) !== entry) return;
    if (entry.completionAware && entry.inFlight >= MAX_INFLIGHT_WRITES) {
      // xterm is still parsing. Re-arm instead of handing over more: the
      // pending caps keep bounding what we hold, so a parser that never
      // completes degrades to the established drop-oldest policy instead of
      // unbounded growth (issue #2018).
      this.scheduleFlush(nodeId, entry);
      return;
    }
    // A completion frees budget and there may be a backlog waiting: ask for a
    // frame so the flush resumes as soon as the parser is under budget again.
    flushEntry(entry, this.writeFns.get(nodeId), () => {
      if (this.entries.get(nodeId) === entry) this.scheduleFlush(nodeId, entry);
    });
  }

  has(nodeId: number): boolean {
    return this.entries.has(nodeId);
  }

  pendingBytes(nodeId: number): number {
    return this.entries.get(nodeId)?.pendingBytes ?? 0;
  }

  /** Live queued chunk count (excludes evicted slots below the head index). */
  queuedChunks(nodeId: number): number {
    const entry = this.entries.get(nodeId);
    return entry ? entry.chunks.length - entry.head : 0;
  }

  /** Payloads handed to xterm that have not yet reported parse completion. */
  inFlightWrites(nodeId: number): number {
    return this.entries.get(nodeId)?.inFlight ?? 0;
  }
}
