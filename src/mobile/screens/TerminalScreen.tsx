import { useEffect, useRef, useState } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { AgentNode, isAuthError, terminalWsUrl } from "../api";
import { attachTouchPan } from "./attachTouchPan";
import { QUICK_KEYS } from "./quickKeys";
import { AppBar } from "../ui";
import { loadUnicode11Widths } from "../../components/Terminal/loadUnicode11Widths";

const MAX_RECONNECT = 5;
const RECONNECT_DELAYS_MS = [1000, 2000, 4000, 8000, 16000];

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

  const [reconnectIn, setReconnectIn] = useState<number | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  const [copyMode, setCopyMode] = useState(false);
  const [bufferText, setBufferText] = useState("");
  const [copied, setCopied] = useState(false);
  const [draft, setDraft] = useState("");

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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [node.id]);

  async function connect() {
    // Mint a fresh single-use WS ticket (issue #500) before opening the socket.
    let url: string;
    try {
      url = await terminalWsUrl(node.id);
    } catch (e) {
      // A 401/403 from the mint means the session cookie is gone — reconnecting
      // would just re-mint and re-fail forever, so bounce to Connect instead.
      // Any other failure is a transient connect error → normal backoff.
      if (isAuthError(e)) {
        onAuthFailed?.();
        return;
      }
      scheduleReconnect();
      return;
    }
    // The component may have unmounted while awaiting the ticket.
    if (closedByUserRef.current) return;
    let ws: WebSocket;
    try {
      ws = new WebSocket(url);
      ws.binaryType = "arraybuffer";
    } catch {
      scheduleReconnect();
      return;
    }
    wsRef.current = ws;

    ws.onopen = () => {
      reconnectAttemptRef.current = 0;
      setReconnectIn(null);
      const term = termRef.current;
      if (term) {
        ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
      }
    };

    ws.onmessage = (event) => {
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

    ws.onclose = () => {
      if (!closedByUserRef.current) scheduleReconnect();
    };

    ws.onerror = () => {
      if (!closedByUserRef.current) scheduleReconnect();
    };
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
      connect();
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
  function resumeConnection() {
    if (closedByUserRef.current) return;
    const ws = wsRef.current;
    if (
      ws &&
      (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)
    ) {
      return;
    }
    retryNow();
  }

  return (
    <div data-testid="terminal-screen" className="screen">
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

      {reconnectIn !== null && (
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
