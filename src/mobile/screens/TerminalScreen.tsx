import { useEffect, useRef, useState } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { AgentNode, terminalWsUrl } from "../api";

const MAX_RECONNECT = 5;
const RECONNECT_DELAYS_MS = [1000, 2000, 4000, 8000, 16000];

// xterm.js renders its grid into a canvas / DOM rows that sit on top of the
// `.xterm-viewport` scrollable div. Mobile touchmove events land on those
// upper layers and are never translated into viewport scrolls (xterm only
// uses touch internally for tap-to-select). Map single-finger drags onto
// `viewport.scrollTop` so the scrollback becomes reachable on touch.
function attachTouchPan(host: HTMLElement): () => void {
  const viewport = host.querySelector<HTMLElement>(".xterm-viewport");
  const xtermEl = host.querySelector<HTMLElement>(".xterm");
  if (!viewport || !xtermEl) return () => {};

  let lastY = 0;
  let panning = false;

  const onTouchStart = (e: TouchEvent) => {
    if (e.touches.length !== 1) {
      panning = false;
      return;
    }
    panning = true;
    lastY = e.touches[0].clientY;
  };
  const onTouchMove = (e: TouchEvent) => {
    if (!panning || e.touches.length !== 1) return;
    const y = e.touches[0].clientY;
    viewport.scrollTop += lastY - y;
    lastY = y;
    e.preventDefault();
  };
  const onTouchEnd = () => {
    panning = false;
  };

  xtermEl.addEventListener("touchstart", onTouchStart, { passive: true });
  xtermEl.addEventListener("touchmove", onTouchMove, { passive: false });
  xtermEl.addEventListener("touchend", onTouchEnd, { passive: true });
  xtermEl.addEventListener("touchcancel", onTouchEnd, { passive: true });

  return () => {
    xtermEl.removeEventListener("touchstart", onTouchStart);
    xtermEl.removeEventListener("touchmove", onTouchMove);
    xtermEl.removeEventListener("touchend", onTouchEnd);
    xtermEl.removeEventListener("touchcancel", onTouchEnd);
  };
}

type Props = {
  node: AgentNode;
  onBack: () => void;
  onOpenChanges?: () => void;
};

export default function TerminalScreen({ node, onBack, onOpenChanges }: Props) {
  const termHostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectAttemptRef = useRef(0);
  const reconnectTimerRef = useRef<number | null>(null);
  const closedByUserRef = useRef(false);

  const [reconnectIn, setReconnectIn] = useState<number | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  const [copyMode, setCopyMode] = useState(false);
  const [bufferText, setBufferText] = useState("");

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
    setCopyMode(true);
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
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    termRef.current = term;
    fitRef.current = fit;

    let detachTouchPan: (() => void) | undefined;
    let detachScroll: (() => void) | null = null;

    if (termHostRef.current) {
      term.open(termHostRef.current);
      fit.fit();
      detachTouchPan = attachTouchPan(termHostRef.current);

      // xterm's internal scroll container. Listening here lets us show the
      // "jump to latest" pill whenever the user scrolls away from the tail.
      // No event fires when scrollHeight grows on writes (only scrollTop
      // changes do), which is what we want: if they're scrolled up, they
      // stay scrolled up regardless of new output.
      const viewport = termHostRef.current.querySelector(
        ".xterm-viewport",
      ) as HTMLElement | null;
      if (viewport) {
        const onScroll = () => {
          const dist =
            viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight;
          setAtBottom(dist < 4);
        };
        viewport.addEventListener("scroll", onScroll, { passive: true });
        onScroll();
        detachScroll = () => viewport.removeEventListener("scroll", onScroll);
      }
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

    const onWindowResize = () => fitRef.current?.fit();
    window.addEventListener("resize", onWindowResize);

    connect();

    return () => {
      closedByUserRef.current = true;
      window.removeEventListener("resize", onWindowResize);
      detachTouchPan?.();
      detachScroll?.();
      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current);
      }
      wsRef.current?.close();
      term.dispose();
      termRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [node.id]);

  function connect() {
    const url = terminalWsUrl(node.id);
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
    const interval = window.setInterval(() => {
      remaining--;
      setReconnectIn(remaining > 0 ? remaining : 0);
      if (remaining <= 0) window.clearInterval(interval);
    }, 1000);

    reconnectTimerRef.current = window.setTimeout(() => {
      window.clearInterval(interval);
      reconnectTimerRef.current = null;
      setReconnectIn(null);
      connect();
    }, delayMs);
  }

  return (
    <div
      data-testid="terminal-screen"
      style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0 }}
    >
      <div
        style={{
          background: "#1a1a1a",
          padding: "10px 12px",
          display: "flex",
          alignItems: "center",
          gap: 12,
          borderBottom: "1px solid #333",
          flexShrink: 0,
        }}
      >
        <button
          onClick={onBack}
          aria-label="Back"
          data-testid="terminal-back"
          style={{
            background: "transparent",
            border: "none",
            color: "#aaa",
            fontSize: 22,
            cursor: "pointer",
            lineHeight: 1,
            padding: 4,
          }}
        >
          ←
        </button>
        <span
          style={{
            fontSize: 14,
            fontWeight: 600,
            color: "#fff",
            flex: 1,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {node.name}
        </span>
        <button
          onClick={enterCopyMode}
          aria-label="Copy text"
          data-testid="terminal-copy"
          title="Select and copy text"
          style={{
            background: "transparent",
            border: "1px solid #333",
            borderRadius: 6,
            color: "#aaa",
            fontSize: 12,
            padding: "6px 10px",
            cursor: "pointer",
          }}
        >
          Copy
        </button>
        {onOpenChanges && (
          <button
            onClick={onOpenChanges}
            aria-label="Changes"
            data-testid="terminal-open-changes"
            title="View changes"
            style={{
              background: "transparent",
              border: "1px solid #333",
              borderRadius: 6,
              color: "#aaa",
              fontSize: 12,
              padding: "6px 10px",
              cursor: "pointer",
            }}
          >
            Δ
          </button>
        )}
      </div>

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

      {copyMode && (
        <div
          data-testid="copy-overlay"
          style={{
            position: "fixed",
            inset: 0,
            background: "#0f0f0f",
            zIndex: 200,
            display: "flex",
            flexDirection: "column",
          }}
        >
          <div
            style={{
              background: "#1a1a1a",
              padding: "10px 12px",
              display: "flex",
              alignItems: "center",
              gap: 12,
              borderBottom: "1px solid #333",
              flexShrink: 0,
            }}
          >
            <span
              style={{
                flex: 1,
                fontSize: 13,
                color: "#aaa",
              }}
            >
              Long-press to select, then Copy
            </span>
            <button
              onClick={() => setCopyMode(false)}
              data-testid="copy-done"
              style={{
                background: "#2196f3",
                border: "none",
                borderRadius: 6,
                color: "#fff",
                fontSize: 13,
                fontWeight: 500,
                padding: "6px 14px",
                cursor: "pointer",
              }}
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
              color: "#e0e0e0",
              background: "#0f0f0f",
              userSelect: "text",
              WebkitUserSelect: "text",
              WebkitTouchCallout: "default",
            }}
          >
            {bufferText}
          </pre>
        </div>
      )}

      {reconnectIn !== null && (
        <div
          data-testid="reconnect-overlay"
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(0,0,0,0.85)",
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            gap: 12,
            color: "#fff",
          }}
        >
          {reconnectIn < 0 ? (
            <>
              <h2 style={{ fontSize: 18, fontWeight: 500, margin: 0 }}>
                Connection lost
              </h2>
              <p style={{ color: "#888", fontSize: 13 }}>
                Maximum retries reached.
              </p>
              <button
                onClick={() => {
                  reconnectAttemptRef.current = 0;
                  setReconnectIn(null);
                  connect();
                }}
                style={{
                  background: "#2196f3",
                  border: "none",
                  borderRadius: 8,
                  padding: "10px 20px",
                  color: "#fff",
                  fontSize: 14,
                  cursor: "pointer",
                }}
              >
                Retry
              </button>
            </>
          ) : (
            <>
              <h2 style={{ fontSize: 18, fontWeight: 500, margin: 0 }}>
                Reconnecting…
              </h2>
              <p style={{ color: "#888", fontSize: 13 }}>
                Retrying in {reconnectIn}s
              </p>
            </>
          )}
        </div>
      )}
    </div>
  );
}
