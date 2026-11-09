/**
 * useWsEvents resumes on foreground/network return (issue #806 sister fix).
 *
 * The events WebSocket drives the "attention needed" red-dot indicator on
 * the NodeList. On mobile, backgrounding suspends timers and drops the WS,
 * so a phone left backgrounded for a few seconds stalls the live event
 * stream — the same root cause as the terminal WS fix #818 closed. The
 * events hook now resumes on `visibilitychange` / `online` instead of
 * stranding the user on the 5-second poll fallback.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useWsEvents } from "../../src/mobile/useWsEvents";

let sockets: FakeWebSocket[] = [];

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  readyState = FakeWebSocket.CONNECTING;
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((e: unknown) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(public url: string) {
    sockets.push(this);
  }
  send() {}
  close() {
    if (this.readyState !== FakeWebSocket.CLOSED) {
      this.readyState = FakeWebSocket.CLOSED;
    }
  }
  simulateOpen() {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }
  simulateError() {
    this.onerror?.();
  }
  simulateDrop() {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
  }
  /// Move the socket into CLOSING without firing `onclose`. Mirrors the
  /// real-browser state during a normal close handshake. Used by the
  /// #1256 resume-race tests to drive the resume path through the
  /// "mid-close" branch without triggering a full drop.
  simulateClosing() {
    this.readyState = FakeWebSocket.CLOSING;
  }
}

// `eventsWsUrl` is the only seam useWsEvents reads; stub the mint so
// `connect()` resolves a URL instead of hitting the network.
vi.mock("../../src/mobile/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../src/mobile/api")>();
  return {
    ...actual,
    eventsWsUrl: vi.fn().mockResolvedValue("ws://test/ws/events?ticket=x"),
  };
});

import { ApiError, eventsWsUrl } from "../../src/mobile/api";

const noop = () => {};

describe("useWsEvents reconnect on foreground/online", () => {
  beforeEach(() => {
    sockets = [];
    vi.mocked(eventsWsUrl).mockReset().mockResolvedValue("ws://test/ws/events?ticket=x");
    vi.stubGlobal("WebSocket", FakeWebSocket);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  async function mountAndConnect() {
    const utils = renderHook(() => useWsEvents(noop, noop));
    await waitFor(() => expect(sockets.length).toBe(1));
    act(() => sockets[0].simulateOpen());
    return utils;
  }

  it("opens a single events socket on mount", async () => {
    const utils = await mountAndConnect();
    expect(eventsWsUrl).toHaveBeenCalledTimes(1);
    expect(sockets.length).toBe(1);
    utils.unmount();
  });

  it.each(['mount', 'reconnect'] as const)('owns the pending ticket across foreground/network bursts (%s)', async when => {
    let resolveTicket!: (url: string) => void;
    const ticket = new Promise<string>(resolve => { resolveTicket = resolve; });
    if (when === 'mount') vi.mocked(eventsWsUrl).mockReturnValueOnce(ticket);
    const view = renderHook(() => useWsEvents(noop, noop));
    if (when === 'reconnect') {
      await waitFor(() => expect(sockets).toHaveLength(1));
      vi.mocked(eventsWsUrl).mockReturnValueOnce(ticket);
      act(() => {
        sockets[0].simulateDrop();
        window.dispatchEvent(new Event('online'));
      });
    }
    act(() => {
      document.dispatchEvent(new Event('visibilitychange'));
      window.dispatchEvent(new Event('online'));
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await act(async () => resolveTicket('ws://test/ws/events?ticket=pending'));
    expect(eventsWsUrl).toHaveBeenCalledTimes(when === 'mount' ? 1 : 2);
    expect(sockets).toHaveLength(when === 'mount' ? 1 : 2);
    view.unmount();
    expect(sockets.every(socket => socket.readyState === FakeWebSocket.CLOSED)).toBe(true);
  });

  it.each([401, 429, 500])('ignores ticket rejection after unmount (%s)', async status => {
    vi.useFakeTimers();
    try {
      let rejectTicket!: (error: unknown) => void;
      vi.mocked(eventsWsUrl).mockReturnValueOnce(new Promise((_, reject) => { rejectTicket = reject; }));
      const onAuthError = vi.fn();
      const view = renderHook(() => useWsEvents(noop, onAuthError));
      view.unmount();
      await act(async () => rejectTicket(new ApiError(status, 'retired request')));
      expect(onAuthError).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(0);
      expect(sockets).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('ignores a queued message from a socket after unmount', async () => {
    const receive = vi.fn();
    const view = renderHook(() => useWsEvents(receive, noop));
    await waitFor(() => expect(sockets).toHaveLength(1));
    const lateMessage = sockets[0].onmessage;
    view.unmount();
    act(() => lateMessage?.({ data: JSON.stringify({ type: 'attention-cleared', session_id: 7 }) }));
    expect(receive).not.toHaveBeenCalled();
  });

  it.each(['success', 'auth failure'] as const)('retries a stalled mint and ignores its late %s', async outcome => {
    vi.useFakeTimers();
    let resolveOld!: (url: string) => void;
    let rejectOld!: (error: unknown) => void;
    vi.mocked(eventsWsUrl).mockReturnValueOnce(new Promise((resolve, reject) => {
      resolveOld = resolve;
      rejectOld = reject;
    }));
    const receive = vi.fn();
    const onAuthError = vi.fn();
    const view = renderHook(() => useWsEvents(receive, onAuthError));
    try {
      const oldSignal = vi.mocked(eventsWsUrl).mock.calls[0][0]!;
      await act(async () => vi.advanceTimersByTimeAsync(15000));
      expect(oldSignal.aborted).toBe(true);
      expect(sockets).toHaveLength(0);
      // The timeout takes the ordinary reconnect ladder; network return
      // can bypass that slot without waiting for the old promise to settle.
      act(() => window.dispatchEvent(new Event('online')));
      await act(async () => { await Promise.resolve(); });
      expect(eventsWsUrl).toHaveBeenCalledTimes(2);
      expect(sockets).toHaveLength(1);
      await act(async () => {
        if (outcome === 'success') resolveOld('ws://retired-ticket');
        else rejectOld(new ApiError(401, 'retired session'));
      });
      act(() => sockets[0].onmessage?.({ data: JSON.stringify({ type: 'circuit-run-updated', run_id: 7 }) }));
      expect(sockets).toHaveLength(1);
      expect(receive).toHaveBeenCalledTimes(1);
      expect(onAuthError).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      view.unmount();
      vi.useRealTimers();
    }
  });

  it('aborts a pending ticket and ignores late success on unmount', async () => {
    let resolveTicket!: (url: string) => void;
    vi.mocked(eventsWsUrl).mockReturnValueOnce(new Promise(resolve => { resolveTicket = resolve; }));
    const view = renderHook(() => useWsEvents(noop, noop));
    const signal = vi.mocked(eventsWsUrl).mock.calls[0][0]!;
    view.unmount();
    expect(signal.aborted).toBe(true);
    await act(async () => resolveTicket('ws://retired-ticket'));
    expect(sockets).toHaveLength(0);
  });

  it('delivers Circuit Run invalidations to the screen for an immediate refresh', async () => {
    const receive = vi.fn();
    const view = renderHook(() => useWsEvents(receive, noop));
    await waitFor(() => expect(sockets).toHaveLength(1));
    act(() => sockets[0].onmessage?.({ data: JSON.stringify({ type: 'circuit-run-updated', run_id: 7 }) }));
    expect(receive).toHaveBeenCalledWith({ type: 'circuit-run-updated', run_id: 7 });
    view.unmount();
  });

  it("reconnects a dropped socket when the document is foregrounded", async () => {
    const utils = await mountAndConnect();
    act(() => sockets[0].simulateDrop());

    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await waitFor(() => expect(sockets.length).toBe(2));
    expect(eventsWsUrl).toHaveBeenCalledTimes(2);
    utils.unmount();
  });

  it("reconnects a dropped socket when the network returns (online event)", async () => {
    const utils = await mountAndConnect();
    act(() => sockets[0].simulateDrop());

    act(() => {
      window.dispatchEvent(new Event("online"));
    });
    await waitFor(() => expect(sockets.length).toBe(2));
    utils.unmount();
  });

  it("leaves a healthy open socket alone on foreground", async () => {
    const utils = await mountAndConnect();
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await Promise.resolve();
    expect(sockets.length).toBe(1);
    expect(eventsWsUrl).toHaveBeenCalledTimes(1);
    utils.unmount();
  });

  it("advances the attempt counter once when onerror and onclose both fire for one failure", async () => {
    const utils = await mountAndConnect();
    vi.useFakeTimers();
    try {
      // Retain both callbacks: the browser may already have queued close.
      const error = sockets[0].onerror;
      const close = sockets[0].onclose;
      act(() => { error?.(); close?.(); });
      expect(vi.getTimerCount()).toBe(1);
      await act(async () => vi.advanceTimersByTimeAsync(2100));
      expect(eventsWsUrl).toHaveBeenCalledTimes(2);
      expect(sockets).toHaveLength(2);
    } finally {
      utils.unmount();
      vi.useRealTimers();
    }
  });

  it('ends an unauthorized connection even if the screen has not unmounted yet', async () => {
    vi.mocked(eventsWsUrl).mockRejectedValueOnce(new ApiError(401, 'session rejected'));
    const onAuthError = vi.fn();
    const view = renderHook(() => useWsEvents(noop, onAuthError));
    await waitFor(() => expect(onAuthError).toHaveBeenCalledTimes(1));
    act(() => {
      window.dispatchEvent(new Event('online'));
      document.dispatchEvent(new Event('visibilitychange'));
    });
    expect(eventsWsUrl).toHaveBeenCalledTimes(1);
    view.unmount();
  });

  it('pauses a rate-limited mint until foreground or network return', async () => {
    vi.useFakeTimers();
    const view = renderHook(() => useWsEvents(noop, noop));
    try {
      // The first mint was started synchronously; fail the next one.
      await act(async () => { await Promise.resolve(); });
      vi.mocked(eventsWsUrl).mockRejectedValueOnce(new ApiError(429, 'server busy'));
      act(() => sockets[0].simulateDrop());
      await act(async () => vi.advanceTimersByTimeAsync(20000));
      expect(eventsWsUrl).toHaveBeenCalledTimes(2);
      expect(vi.getTimerCount()).toBe(0);
      act(() => window.dispatchEvent(new Event('online')));
      await act(async () => { await Promise.resolve(); });
      expect(eventsWsUrl).toHaveBeenCalledTimes(3);
      expect(sockets).toHaveLength(2);
    } finally {
      view.unmount();
      vi.useRealTimers();
    }
  });

  it('backs off after a mint failure and resets the ladder after a socket opens', async () => {
    vi.useFakeTimers();
    vi.mocked(eventsWsUrl).mockRejectedValueOnce(new Error('offline'));
    const view = renderHook(() => useWsEvents(noop, noop));
    try {
      await act(async () => { await Promise.resolve(); });
      await act(async () => vi.advanceTimersByTimeAsync(999));
      expect(eventsWsUrl).toHaveBeenCalledTimes(1);
      await act(async () => vi.advanceTimersByTimeAsync(1));
      expect(sockets).toHaveLength(1);
      act(() => sockets[0].simulateDrop());
      await act(async () => vi.advanceTimersByTimeAsync(1999));
      expect(eventsWsUrl).toHaveBeenCalledTimes(2);
      await act(async () => vi.advanceTimersByTimeAsync(1));
      expect(sockets).toHaveLength(2);
      act(() => { sockets[1].simulateOpen(); sockets[1].simulateDrop(); });
      await act(async () => vi.advanceTimersByTimeAsync(1000));
      expect(sockets).toHaveLength(3);
      act(() => sockets[2].simulateDrop());
      view.unmount();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      view.unmount();
      vi.useRealTimers();
    }
  });

  it("removes the foreground/online listeners on unmount", async () => {
    const utils = await mountAndConnect();
    const initialCalls = (eventsWsUrl as ReturnType<typeof vi.fn>).mock
      .calls.length;

    utils.unmount();

    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    act(() => {
      window.dispatchEvent(new Event("online"));
    });
    await Promise.resolve();
    expect(eventsWsUrl).toHaveBeenCalledTimes(initialCalls);
  });

  // Issue #1256 — sister fix to the terminal WS resume race.
  //
  // The events hook had the same CLOSING gap: resume() only checked
  // OPEN / CONNECTING, so foregrounding during the browser's close
  // handshake opened a second events socket while the first was still
  // mid-close. The fix treats CLOSING as a pending state too.
  it("does NOT open a second events socket when the current one is mid-close and the document is foregrounded", async () => {
    const utils = await mountAndConnect();
    expect(sockets.length).toBe(1);
    const initialCalls = (eventsWsUrl as ReturnType<typeof vi.fn>).mock
      .calls.length;

    act(() => sockets[0].simulateClosing());

    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(sockets.length).toBe(1);
    expect(eventsWsUrl).toHaveBeenCalledTimes(initialCalls);

    // Once onclose finally fires, the normal reconnect path takes over
    // and we get exactly one new socket — no parallel ladder. Foreground
    // here drives an immediate resume (no 1s backoff tick to race the
    // waitFor default 1s timeout against).
    act(() => sockets[0].simulateDrop());
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await waitFor(() => expect(sockets.length).toBe(2));

    utils.unmount();
  });
});
