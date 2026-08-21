/**
 * Issue #1181 — AbortSignal plumbing for `diffNodeAgainstBase` and
 * `diffNodeFileAgainstBase`.
 *
 * The Tauri 2 `invoke()` binding in this version doesn't yet forward
 * an `AbortSignal` to the Rust command, so the *actual* backend
 * cancellation (pool-pressure ≤ 1 per node_id) lives on the Rust side
 * in the `DIFF_NODE_CANCEL` per-node map. The frontend wrappers
 * nonetheless accept a signal so:
 *
 *   - a caller whose `AbortController` is *already* aborted before
 *     issuing the IPC can short-circuit without paying the
 *     round-trip; and
 *   - the seam exists for a future Tauri signal-aware `invoke` to
 *     plug into without touching call sites.
 *
 * These tests pin the wrapper-level contract: pre-aborted signal →
 * early-out, undefined signal → same behaviour as before. Component-
 * level abort-on-new-fetch (the `abortRef` in `AgentReviewPanel` /
 * `CenterDiffOverlay`) is exercised indirectly by the existing
 * `agent-changes-tab` and `center-diff-overlay` suites, which already
 * re-render on node change.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { diffNodeAgainstBase, diffNodeFileAgainstBase } from '../../src/lib/tauri';

const EMPTY_DIFF = { files: [] };

describe('Issue #1181 — diff cancellation wrapper', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(EMPTY_DIFF);
  });

  it('diffNodeAgainstBase short-circuits on a pre-aborted signal', async () => {
    const controller = new AbortController();
    controller.abort();

    // Rejects with a DOMException so callers can `if (err.name === 'AbortError')`
    // — the standard `AbortController` contract. The message string is loose
    // because DOMException's `message` is implementation-defined in JSDOM.
    await expect(diffNodeAgainstBase(7, controller.signal)).rejects.toBeDefined();
    // Crucially: the IPC must NOT have been invoked — that's the whole
    // point of the wrapper short-circuit. If the IPC fires here the
    // Rust side will still walk the worktree for a request the caller
    // has already discarded, defeating the early-out.
    expect(invoke).not.toHaveBeenCalled();
  });

  it('diffNodeAgainstBase with an undefined signal behaves like the pre-#1181 wrapper', async () => {
    await diffNodeAgainstBase(7);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('diff_node_against_base', { nodeId: 7 });
  });

  it('diffNodeAgainstBase with a live (non-aborted) signal still kicks off the IPC', async () => {
    const controller = new AbortController();
    await diffNodeAgainstBase(7, controller.signal);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('diff_node_against_base', { nodeId: 7 });
  });

  it('diffNodeFileAgainstBase short-circuits on a pre-aborted signal', async () => {
    const controller = new AbortController();
    controller.abort();

    await expect(
      diffNodeFileAgainstBase(7, 'src/app.ts', controller.signal),
    ).rejects.toBeDefined();
    expect(invoke).not.toHaveBeenCalled();
  });

  it('diffNodeFileAgainstBase with undefined signal behaves like the pre-#1181 wrapper', async () => {
    await diffNodeFileAgainstBase(7, 'src/app.ts');
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('diff_node_file_against_base', {
      nodeId: 7,
      filePath: 'src/app.ts',
    });
  });

  it('the wrapper rejects the abort branch with a DOMException, not a plain Error', async () => {
    // Pin the discriminator: components decide whether to swallow the
    // error based on `err.name === 'AbortError'` (or `controller.signal.aborted`),
    // and a plain `Error('aborted')` would slip through that check and
    // get surfaced as a real failure — masking the cancellation.
    const controller = new AbortController();
    controller.abort();

    let caught: unknown;
    try {
      await diffNodeAgainstBase(7, controller.signal);
    } catch (e) {
      caught = e;
    }

    expect(caught).toBeInstanceOf(DOMException);
    expect((caught as DOMException).name).toBe('AbortError');
  });
});
