import { useEffect, useRef, useState } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { AgentNode, terminalWsUrl } from "../api";

const MAX_RECONNECT = 5;
const RECONNECT_DELAYS_MS = [1000, 2000, 4000, 8000, 16000];

type Props = {
  node: AgentNode;
  onBack: () => void;
};

export default function TerminalScreen({ node, onBack }: Props) {
  const termHostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectAttemptRef = useRef(0);
  const reconnectTimerRef = useRef<number | null>(null);
  const closedByUserRef = useRef(false);

  const [reconnectIn, setReconnectIn] = useState<number | null>(null);
  const [cmd, setCmd] = useState("");

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

    if (termHostRef.current) {
      term.open(termHostRef.current);
      fit.fit();
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

  function sendCmd() {
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(cmd + "\n");
    setCmd("");
  }

  function handleKeydown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendCmd();
    }
  }

  return (
    <div
      data-testid="terminal-screen"
      style={{ display: "flex", flexDirection: "column", flex: 1 }}
    >
      <div
        style={{
          background: "#1a1a1a",
          padding: "10px 12px",
          display: "flex",
          alignItems: "center",
          gap: 12,
          borderBottom: "1px solid #333",
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
      </div>

      <div
        ref={termHostRef}
        style={{ flex: 1, padding: 8, overflow: "hidden", minHeight: 0 }}
      />

      <div
        style={{
          display: "flex",
          gap: 8,
          padding: 8,
          background: "#1a1a1a",
          borderTop: "1px solid #333",
        }}
      >
        <input
          type="text"
          inputMode="text"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          placeholder="Type a command…"
          value={cmd}
          onChange={(e) => setCmd(e.target.value)}
          onKeyDown={handleKeydown}
          data-testid="cmd-input"
          style={{
            flex: 1,
            background: "#2a2a2a",
            border: "1px solid #444",
            borderRadius: 6,
            padding: "8px 12px",
            color: "#e0e0e0",
            fontSize: 14,
            outline: "none",
          }}
        />
        <button
          onClick={sendCmd}
          data-testid="cmd-send"
          style={{
            background: "#2196f3",
            border: "none",
            borderRadius: 6,
            padding: "8px 16px",
            color: "#fff",
            fontSize: 14,
            cursor: "pointer",
          }}
        >
          Send
        </button>
      </div>

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
