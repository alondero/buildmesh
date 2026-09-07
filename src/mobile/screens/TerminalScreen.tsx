import { useEffect, useRef, useState, type TouchEvent } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { AgentNode, isAuthError, isRateLimited, terminalWsUrl } from "../api";
import { attachTouchPan } from "./attachTouchPan";
import { QUICK_KEYS } from "./quickKeys";
import { AppBar } from "../ui";
import { loadUnicode11Widths } from "../../components/Terminal/loadUnicode11Widths";

const MAX_RECONNECT = 5;
const RECONNECT_DELAYS_MS = [1000, 2000, 4000, 8000, 16000];

// Swipe-down-to-dismiss (issue #1377): a downward drag on the app bar closes
// the terminal, mirroring the sheet-dismiss muscle memory. Anchored to the
// app bar only — the xterm surface below belongs to pan-scrolling
// (attachTouchPan), and hijacking a scrollback drag to close the screen
// would fire on exactly the users reading history. Mostly-vertical strokes
// only (dy > SWIPE_DISMISS_PX and dy dominating dx) so an accidental
// diagonal during horizontal chrome taps doesn't dismiss.
const SWIPE_DISMISS_PX = 72;
const SWIPE_DISMISS_OPACITY_FLOOR = 0.4;

type Props = {
  node: AgentNode;
  onBack: () => void;
  onOpenChanges?: () => void;
  /// Called when the WS ticket mint fails auth (cookie gone/expired) — bounces
  /// to the Connect screen, since the terminal has no other re-auth path.
  onAuthFailed?: () => void;
};

export default function TerminalScreen({
  node,
  onBack,
  onOpenChanges,
  onAuthFailed,
}: Props) {
  const termHostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectAttemptRef = useRef(0);
  const reconnectTimerRef = useRef<number | null>(null);
  const countdownRef = useRef<number | null>(null);
  const closedByUserRef = useRef(false);
  const authFailedRef = useRef(false);
  const connectInFlightRef = useRef(false);

  const [reconnectIn, setReconnectIn] = useState<number | null>(null);
  // Issue #552: a 429 from the mint (server rate-cap; see api.ts) is shown
  // as a "server is busy" banner with a Retry button — distinct from the
  // "Connection lost — reconnecting in N s…" copy so the user knows it's
  // a pacing delay, not an outage. Cleared on the next successful mint.
  const [rateLimited, setRateLimited] = useState(false);
  const [atBottom, setAtBottom] = useState(true);
  const [copyMode, setCopyMode] = useState(false);
  const [bufferText, setBufferText] = useState("");
  const [copied, setCopied] = useState(false);
  const [draft, setDraft] = useState("");

  // Touch anchor + wrapper element for the app bar swipe-down dismiss
  // (issue #1377, post-review). The wrapper element receives a CSS custom
  // property `--appbar-translate` + `--appbar-opacity` driven directly by
  // ref (no React re-renders during the drag) — see the JSX below for the
  // `style={{ ... }}` attribute that reads them.
  const swipeStartRef = useRef<{ x: number; y: number } | null>(null);
  const swipeWrapperRef = useRef<HTMLDivElement | null>(null);
  const applySwipeVisual = (dy: number) => {
    const el = swipeWrapperRef.current;
    if (!el) return;
    // Cap the translate at the screen height — anything larger is a
    // glitch from an OS-level interruption, not a real drag. The
    // opacity ramps from 1.0 at dy=0 down to OPACITY_FLOOR as the user
    // pulls past the dismiss threshold, then bounces back to 1.0 if
    // the gesture is cancelled below threshold.
    const clamped = Math.max(0, Math.min(dy, 400));
    el.style.setProperty("--appbar-translate", `${clamped}px`);
    const ratio = Math.max(SWIPE_DISMISS_OPACITY_FLOOR, 1 - clamped / 400);
    el.style.setProperty("--appbar-opacity", ratio.toFixed(2));
  };
  const resetSwipeVisual = () => {
    const el = swipeWrapperRef.current;
    if (!el) return;
    el.style.setProperty("--appbar-translate", "0px");
    el.style.setProperty("--appbar-opacity", "1");
  };
  const onTouchStart = (e: TouchEvent) => {
    const t = e.touches[0];
    swipeStartRef.current = { x: t.clientX, y: t.clientY };
  };
  const onTouchMove = (e: TouchEvent) => {
    const start = swipeStartRef.current;
    if (!start) return;
    const t = e.touches[0];
    const dy = t.clientY - start.y;
    const dx = t.clientX - start.x;
    // Horizontal-axis lock (review feedback #3): if a horizontal swipe
    // takes over (xterm pan-scroll, deck carousel, etc.), drop the
    // anchor so a later vertical drag from this same touch isn't
    // mis-attributed to the original start point. NO direction
    // qualifier on dy — a horizontal stroke with a 1–2px downward
    // drift is still horizontal, and falling through to the translate
    // path with dy > 0 would leak a small downward translation onto
    // the app bar during a clearly horizontal swipe.
    if (Math.abs(dx) > Math.abs(dy) * 1.5) {
      swipeStartRef.current = null;
      resetSwipeVisual();
      return;
    }
    // Only translate on positive dy — upward strokes aren't a dismiss.
    if (dy > 0) applySwipeVisual(dy);
  };
  const onTouchEnd = (e: TouchEvent) => {
    const start = swipeStartRef.current;
    swipeStartRef.current = null;
    if (!start) {
      resetSwipeVisual();
      return;
    }
    const t = e.changedTouches[0];
    const dy = t.clientY - start.y;
    const dx = t.clientX - start.x;
    if (dy > SWIPE_DISMISS_PX && dy > Math.abs(dx) * 1.5) {
      onBack();
      // No need to reset the visual — `onBack` unmounts the screen.
      return;
    }
    resetSwipeVisual();
  };
  const onTouchCancel = () => {
    swipeStartRef.current = null;
    resetSwipeVisual();
  };

  function sendToPty(data: string): boolean {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(data);
      return true;
    }
    return false;
  }

  // The composer sends a whole line at once — far friendlier than typing
  // into xterm's hidden textarea, which fights mobile IMEs (autocorrect,
  // swipe input, composition events). An empty submit sends a bare Enter,
  // handy for confirming agent prompts.
  function sendDraft() {
    if (!draft) {
      sendToPty("\r");
      return;
    }
    if (sendToPty(draft) && sendToPty("\r")) {
      setDraft("");
      termRef.current?.scrollToBottom();
    }
  }

  function enterCopyMode() {
    const term = termRef.current;
    if (!term) return;
    const buf = term.buffer.active;
    const lines: string[] = [];
    for (let i = 0; i < buf.length; i++) {
      const line = buf.getLine(i);
      lines.push(line ? line.translateToString(true) : "");
    }
    while (lines.length && lines[lines.length - 1] === "") lines.pop();
    setBufferText(lines.join("\n"));
    setCopied(false);
    setCopyMode(true);
  }

  async function copyAll() {
    let ok = false;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(bufferText);
        ok = true;
      }
    } catch {
      /* fall through to selection fallback */
    }
    if (!ok) {
      // The async clipboard API needs a secure context, and the embedded
      // server is plain http over LAN — select the buffer so execCommand
      // (or the native copy callout) can take over.
      const pre = document.querySelector('[data-testid="copy-buffer"]');
      const sel = window.getSelection();
      if (pre && sel) {
        sel.removeAllRanges();
        const range = document.createRange();
        range.selectNodeContents(pre);
        sel.addRange(range);
        try {
          ok = document.execCommand("copy");
        } catch {
          ok = false;
        }
      }
    }
    if (ok) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    }
  }

  useEffect(() => {
    const term = new XTerm({
      cursorBlink: true,
      fontSize: 13,
      fontFamily:
        '"JetBrains Mono", "Cascadia Code", "Fira Code", monospace',
      theme: {
        background: "#0f0f0f",
        foreground: "#e0e0e0",
        cursor: "#e0e0e0",
        cursorAccent: "#0f0f0f",
        selectionBackground: "#3a3a3a",
      },
      scrollback: 1000,
      // Required so Unicode11Addon can override the glyph-width tables below.
      allowProposedApi: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    // Match modern agent CLIs' Unicode 11+ widths so emoji rows don't shear
    // table borders (xterm defaults to Unicode 6 widths otherwise).
    // loadUnicode11Widths also patches the small set of BMP emoji the
    // upstream addon ships with the wrong width (notably ⚠ U+26A0) — see
    // loadUnicode11Widths.ts.
    loadUnicode11Widths(term);
    termRef.current = term;
    fitRef.current = fit;

    let detachTouchPan: (() => void) | undefined;
    let detachScroll: (() => void) | null = null;
    let resizeObserver: ResizeObserver | null = null;

    if (termHostRef.current) {
      term.open(termHostRef.current);
      fit.fit();
      detachTouchPan = attachTouchPan(termHostRef.current, term);

      // Drive the "jump to latest" pill off xterm's public scroll event and
      // buffer coordinates. In v6 the DOM `.xterm-viewport` is no longer the
      // real scroll axis (SmoothScrollableElement owns the scroll state), so
      // a DOM scroll listener would never fire. `buffer.active.viewportY`
      // tracks the top line of the viewport in scrollback coords, and equals
      // `baseY` exactly when scrolled to the tail — no event fires when new
      // output arrives while pinned to the tail, which is what we want.
      const updateAtBottom = () => {
        const buf = term.buffer.active;
        setAtBottom(buf.viewportY >= buf.baseY);
      };
      const scrollDisposable = term.onScroll(updateAtBottom);
      updateAtBottom();
      detachScroll = () => scrollDisposable.dispose(); // allow-dispose — event-listener disposable, not the terminal
    }

    term.onData((data) => {
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) ws.send(data);
    });

    term.onResize(({ cols, rows }) => {
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "resize", cols, rows }));
      }
    });

    // Refit whenever the host box changes — covers rotation, the soft
    // keyboard shrinking --app-height, and the reconnect banner appearing.
    // A window resize listener misses the keyboard case on iOS.
    const onWindowResize = () => fitRef.current?.fit();
    if (typeof ResizeObserver !== "undefined" && termHostRef.current) {
      resizeObserver = new ResizeObserver(onWindowResize);
      resizeObserver.observe(termHostRef.current);
    } else {
      window.addEventListener("resize", onWindowResize);
    }

    connect();

    // Mobile backgrounding is the normal case: the OS suspends timers and
    // drops the WebSocket, so a socket can silently exhaust its retries while
    // the tab is hidden. Resume the moment the document is foregrounded or the
    // network returns, instead of stranding the user on a manual Retry tap.
    const onForeground = () => {
      if (document.visibilityState === "visible") resumeConnection();
    };
    document.addEventListener("visibilitychange", onForeground);
    window.addEventListener("online", resumeConnection);

    return () => {
      closedByUserRef.current = true;
      window.removeEventListener("resize", onWindowResize);
      document.removeEventListener("visibilitychange", onForeground);
      window.removeEventListener("online", resumeConnection);
      resizeObserver?.disconnect();
      detachTouchPan?.();
      detachScroll?.();
      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current);
      }
      if (countdownRef.current) {
        clearInterval(countdownRef.current);
      }
      wsRef.current?.close();
      term.dispose(); // allow-dispose — mobile SPA owns this per-mount xterm; no TerminalManager here
      termRef.current = null;
    };
    // Mobile screen — mount the per-screen terminal on first paint and
    // tear it down on unmount. The dep list intentionally pins this to
    // `node.id` only: the term is created once and replaced wholesale
    // when the user navigates to a different node. Re-creating on every
    // dependency churn would discard xterm buffer state mid-session.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- term is created once per mount; refs captured inside are stable across renders.
  }, [node.id]);

  function handleAuthFailure() {
    if (authFailedRef.current) return;
    authFailedRef.current = true;
    closedByUserRef.current = true;
    if (reconnectTimerRef.current !== null) {
      window.clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
    if (countdownRef.current !== null) {
      window.clearInterval(countdownRef.current);
      countdownRef.current = null;
    }
    setReconnectIn(null);
    onAuthFailed?.();
  }

  async function connect(authProbe = false) {
    if (connectInFlightRef.current || closedByUserRef.current || authFailedRef.current) {
      return;
    }
    connectInFlightRef.current = true;
    // Mint a fresh single-use WS ticket (issue #500) before opening the socket.
    let url: string;
    try {
      url = await terminalWsUrl(node.id);
    } catch (e) {
      // A 401/403 from the mint means the session cookie is gone — reconnecting
      // would just re-mint and re-fail forever, so bounce to Connect instead.
      // Any other failure is a transient connect error → normal backoff.
      if (isAuthError(e)) {
        connectInFlightRef.current = false;
        handleAuthFailure();
        return;
      }
      // Issue #552: a 429 that survives the helper's one-shot back-off
      // means the server is genuinely oversaturated. DON'T count this
      // against the reconnect ladder (it's a server-pacing signal, not a
      // connectivity failure); surface a brief, non-alarming toast instead
      // and try once more on the manual "Now" tap.
      if (isRateLimited(e)) {
        // Issue #552: 429 → "Server is busy" toast + manual Retry. No
        // scheduleReconnect(), so this never enters the 1/2/4/8/16s ladder
        // and the user is not woken by a fake "Connection lost" banner.
        setRateLimited(true);
        connectInFlightRef.current = false;
        return;
      }
      connectInFlightRef.current = false;
      scheduleReconnect();
      return;
    }
    // The component may have unmounted while awaiting the ticket.
    if (closedByUserRef.current || authFailedRef.current) {
      connectInFlightRef.current = false;
      return;
    }

    // Issue #1256: neuter the old socket's handlers BEFORE opening a new
    // one. A socket whose browser-driven close is still in the CLOSING
    // state keeps its handlers attached until the handshake finishes, so
    // any late `onclose`/`onerror` would otherwise schedule a parallel
    // reconnect ladder and a late `onmessage` would still write PTY bytes
    // to xterm (duplicated output). Detaching the handlers turns the
    // about-to-close socket into a passive no-op while the new one comes
    // up. Belt-and-suspenders alongside the staleness check inside each
    // handler below.
    const oldWs = wsRef.current;
    if (oldWs) {
      oldWs.onopen = null;
      oldWs.onmessage = null;
      oldWs.onclose = null;
      oldWs.onerror = null;
    }

    let ws: WebSocket;
    try {
      ws = new WebSocket(url);
      ws.binaryType = "arraybuffer";
    } catch {
      connectInFlightRef.current = false;
      scheduleReconnect();
      return;
    }
    wsRef.current = ws;
    connectInFlightRef.current = false;

    // A single socket's failure must charge the backoff counter exactly
    // once. Browsers fire BOTH `error` and `close` for the same failure
    // (in that order). Without deduping here, two events from one dropped
    // socket increment the counter twice, schedule overlapping timers that
    // race each other, and burn through MAX_RECONNECT at twice the rate the
    // user expects — exactly the "fails while backgrounded and gives up"
    // symptom from issue #806.
    let reconnectScheduled = false;
    let socketOpened = false;

    ws.onopen = () => {
      if (ws !== wsRef.current) return; // stale: a newer connect() has replaced us
      socketOpened = true;
      reconnectAttemptRef.current = 0;
      setReconnectIn(null);
      setRateLimited(false);
      const term = termRef.current;
      if (term) {
        ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
      }
    };

    ws.onmessage = (event) => {
      if (ws !== wsRef.current) return; // stale: don't write PTY bytes from a dead socket
      const term = termRef.current;
      if (!term) return;
      if (typeof event.data === "string") {
        term.write(event.data);
      } else if (event.data instanceof Blob) {
        event.data.arrayBuffer().then((buf) => term.write(new Uint8Array(buf)));
      } else {
        term.write(new Uint8Array(event.data as ArrayBuffer));
      }
    };

    const onSocketFailure = () => {
      if (closedByUserRef.current || reconnectScheduled) return;
      if (ws !== wsRef.current) return; // stale: a newer connect() owns reconnect now
      reconnectScheduled = true;
      // A failed handshake carries the server's 401/403 as the HTTP upgrade
      // response, which browsers expose only as error/close. Mint once
      // immediately so an expired cookie reaches the auth recovery path
      // instead of waiting through the reconnect ladder. Once that probe has
      // been made, ordinary network failures use the visible backoff.
      if (!socketOpened && !authProbe) {
        void connect(true);
      } else {
        scheduleReconnect();
      }
    };

    ws.onclose = onSocketFailure;
    ws.onerror = onSocketFailure;
  }

  function scheduleReconnect() {
    if (reconnectAttemptRef.current >= MAX_RECONNECT) {
      setReconnectIn(-1); // sentinel: gave up
      return;
    }
    const delayMs =
      RECONNECT_DELAYS_MS[reconnectAttemptRef.current] ??
      RECONNECT_DELAYS_MS[RECONNECT_DELAYS_MS.length - 1];
    reconnectAttemptRef.current++;
    setReconnectIn(Math.ceil(delayMs / 1000));

    // Tick the countdown each second so the user sees something happening.
    let remaining = Math.ceil(delayMs / 1000);
    countdownRef.current = window.setInterval(() => {
      remaining--;
      setReconnectIn(remaining > 0 ? remaining : 0);
      if (remaining <= 0 && countdownRef.current) {
        window.clearInterval(countdownRef.current);
        countdownRef.current = null;
      }
    }, 1000);

    reconnectTimerRef.current = window.setTimeout(() => {
      if (countdownRef.current) {
        window.clearInterval(countdownRef.current);
        countdownRef.current = null;
      }
      reconnectTimerRef.current = null;
      setReconnectIn(null);
      connect(true);
    }, delayMs);
  }

  function retryNow() {
    if (reconnectTimerRef.current) {
      window.clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
    if (countdownRef.current) {
      window.clearInterval(countdownRef.current);
      countdownRef.current = null;
    }
    reconnectAttemptRef.current = 0;
    setReconnectIn(null);
    connect();
  }

  // Foreground/online resume: leave a healthy or in-flight socket alone (this
  // fires on every tab switch), but a dead or exhausted one gets its backoff
  // reset and reconnects immediately — same effect as tapping Retry.
  //
  // Issue #1256: CLOSING counts as "still alive." The browser enters
  // CLOSING when it begins the close handshake and only fires `onclose`
  // on a later tick — if we proceed here we open a duplicate socket
  // while the old one's handlers are still attached, leading to
  // duplicated PTY bytes written to the same xterm and a parallel
  // reconnect ladder scheduled by the old socket's late onclose.
  function resumeConnection() {
    if (closedByUserRef.current) return;
    const ws = wsRef.current;
    if (
      ws &&
      (ws.readyState === WebSocket.OPEN ||
        ws.readyState === WebSocket.CONNECTING ||
        ws.readyState === WebSocket.CLOSING)
    ) {
      return;
    }
    retryNow();
  }

  return (
    <div data-testid="terminal-screen" className="screen">
      {/* Swipe-down on the app bar dismisses back to the list (issue #1377).
          The wrapper owns the gesture so AppBar itself stays generic chrome. */}
      <div
        ref={swipeWrapperRef}
        data-testid="terminal-appbar"
        className="appbar-swipe"
        style={{
          transform: "translateY(var(--appbar-translate, 0px))",
          opacity: "var(--appbar-opacity, 1)",
          // `will-change` keeps the translate on the GPU compositor —
          // the JS handler writes the CSS variable on every touchmove,
          // so the visual update skips React entirely.
          willChange: "transform, opacity",
        }}
        onTouchStart={onTouchStart}
        onTouchMove={onTouchMove}
        onTouchEnd={onTouchEnd}
        onTouchCancel={onTouchCancel}
      >
        <AppBar
          onBack={onBack}
          backTestId="terminal-back"
          title={node.name}
          subtitle={
            node.branch ? `${node.provider} · ⎇ ${node.branch}` : node.provider
          }
        >
          <button
            onClick={enterCopyMode}
            aria-label="Copy text"
            data-testid="terminal-copy"
            className="chip-btn"
          >
            Copy
          </button>
          {onOpenChanges && (
            <button
              onClick={onOpenChanges}
              aria-label="Changes"
              data-testid="terminal-open-changes"
              className="chip-btn"
            >
              Changes
            </button>
          )}
        </AppBar>
      </div>

      {rateLimited && (
        <div data-testid="rate-limited-toast" className="banner warn">
          <span style={{ flex: 1 }}>Server is busy — please try again.</span>
          <button
            className="chip-btn"
            onClick={retryNow}
            data-testid="rate-limited-retry"
          >
            Retry
          </button>
        </div>
      )}
      {reconnectIn !== null && !rateLimited && (
        <div
          data-testid="reconnect-overlay"
          className={`banner ${reconnectIn < 0 ? "error" : "warn"}`}
        >
          <span style={{ flex: 1 }}>
            {reconnectIn < 0
              ? "Connection lost."
              : `Connection lost — reconnecting in ${reconnectIn}s…`}
          </span>
          <button className="chip-btn" onClick={retryNow} data-testid="reconnect-now">
            {reconnectIn < 0 ? "Retry" : "Now"}
          </button>
        </div>
      )}

      <div
        style={{
          position: "relative",
          flex: 1,
          minHeight: 0,
          display: "flex",
        }}
      >
        <div
          ref={termHostRef}
          style={{ flex: 1, padding: 8, overflow: "hidden", minHeight: 0 }}
        />
        {!atBottom && !copyMode && (
          <button
            onClick={() => termRef.current?.scrollToBottom()}
            aria-label="Jump to latest output"
            data-testid="jump-to-bottom"
            style={{
              position: "absolute",
              right: 14,
              bottom: 14,
              background: "rgba(33, 150, 243, 0.95)",
              color: "#fff",
              border: "none",
              borderRadius: 999,
              padding: "8px 14px",
              fontSize: 13,
              fontWeight: 500,
              boxShadow: "0 4px 12px rgba(0,0,0,0.4)",
              cursor: "pointer",
              zIndex: 50,
              display: "flex",
              alignItems: "center",
              gap: 6,
            }}
          >
            ↓ Latest
          </button>
        )}
      </div>

      <div className="term-footer">
        <div className="qk-row" data-testid="quick-keys">
          {QUICK_KEYS.map((k) => (
            <button
              key={k.id}
              className="qk-btn"
              data-testid={`qk-${k.id}`}
              aria-label={k.id}
              // preventDefault keeps focus (and the soft keyboard) where it
              // is — tapping a key must not blur the composer or terminal.
              onPointerDown={(e) => e.preventDefault()}
              onClick={() => sendToPty(k.seq)}
            >
              {k.label}
            </button>
          ))}
        </div>
        <form
          className="input-row"
          onSubmit={(e) => {
            e.preventDefault();
            sendDraft();
          }}
        >
          <input
            className="field"
            style={{ flex: 1, background: "var(--bg)" }}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="Message the agent…"
            enterKeyHint="send"
            autoCapitalize="sentences"
            data-testid="terminal-input"
          />
          <button
            type="submit"
            className="btn-primary"
            style={{ padding: "11px 16px", fontSize: 13 }}
            data-testid="terminal-send"
            aria-label="Send"
          >
            Send
          </button>
        </form>
      </div>

      {copyMode && (
        <div
          data-testid="copy-overlay"
          style={{
            position: "absolute",
            inset: 0,
            background: "var(--bg)",
            zIndex: 200,
            display: "flex",
            flexDirection: "column",
          }}
        >
          <div className="appbar">
            <span style={{ flex: 1, fontSize: 13, color: "var(--text-dim)" }}>
              Long-press to select, or copy everything
            </span>
            <button
              onClick={copyAll}
              data-testid="copy-all"
              className="chip-btn"
              style={copied ? { color: "var(--green)", borderColor: "var(--green)" } : undefined}
            >
              {copied ? "Copied ✓" : "Copy all"}
            </button>
            <button
              onClick={() => setCopyMode(false)}
              data-testid="copy-done"
              className="btn-primary"
              style={{ padding: "8px 14px", fontSize: 13 }}
            >
              Done
            </button>
          </div>
          <pre
            data-testid="copy-buffer"
            style={{
              flex: 1,
              margin: 0,
              padding: 12,
              overflow: "auto",
              whiteSpace: "pre-wrap",
              overflowWrap: "anywhere",
              fontFamily:
                '"JetBrains Mono", "Cascadia Code", "Fira Code", monospace',
              fontSize: 13,
              lineHeight: 1.4,
              color: "var(--text)",
              background: "var(--bg)",
              userSelect: "text",
              WebkitUserSelect: "text",
              WebkitTouchCallout: "default",
            }}
          >
            {bufferText}
          </pre>
        </div>
      )}
    </div>
  );
}
