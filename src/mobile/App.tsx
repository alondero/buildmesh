import { Suspense, lazy, useEffect, useState } from "react";
import "./styles.css";
import { AgentNode, readStoredToken } from "./api";
import Connect from "./screens/Connect";
import NodeList from "./screens/NodeList";

// Terminal pulls in xterm.js + FitAddon (~90 KB gzipped). Most mobile
// sessions look at the node list and don't open a terminal; defer the
// chunk until first use to keep the initial load fast.
const TerminalScreen = lazy(() => import("./screens/TerminalScreen"));

type Screen =
  | { kind: "connect" }
  | { kind: "list" }
  | { kind: "terminal"; node: AgentNode };

export default function App() {
  // If the URL has ?token=, the server has already set bm_session and we
  // can land directly on the node list. Otherwise check localStorage —
  // if it has a token, Connect.tsx will offer "Use saved session".
  const initial: Screen = (() => {
    const params = new URLSearchParams(window.location.search);
    if (params.get("token") || readStoredToken()) return { kind: "list" };
    return { kind: "connect" };
  })();

  const [screen, setScreen] = useState<Screen>(initial);
  const [offline, setOffline] = useState(false);

  // Clean the token out of the URL once we've taken it from the query string
  // — the server set the cookie, so it's no longer needed there.
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    if (params.get("token")) {
      params.delete("token");
      const newSearch = params.toString();
      window.history.replaceState(
        null,
        "",
        window.location.pathname + (newSearch ? "?" + newSearch : ""),
      );
    }
  }, []);

  return (
    <>
      {screen.kind === "connect" && (
        <Connect onConnected={() => setScreen({ kind: "list" })} />
      )}
      {screen.kind === "list" && (
        <NodeList
          onOpenNode={(node) => setScreen({ kind: "terminal", node })}
          onOffline={() => setOffline(true)}
        />
      )}
      {screen.kind === "terminal" && (
        <Suspense
          fallback={
            <div
              data-testid="terminal-loading"
              style={{
                flex: 1,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                color: "#888",
                fontSize: 13,
              }}
            >
              Loading terminal…
            </div>
          }
        >
          <TerminalScreen
            node={screen.node}
            onBack={() => setScreen({ kind: "list" })}
          />
        </Suspense>
      )}
      {offline && (
        <div
          data-testid="offline-overlay"
          style={{
            position: "fixed",
            inset: 0,
            background: "#0f0f0f",
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            gap: 12,
            color: "#fff",
            padding: 24,
          }}
        >
          <h2 style={{ fontSize: 18, color: "#f44336", margin: 0 }}>
            Desktop app is offline
          </h2>
          <p
            style={{
              color: "#666",
              fontSize: 13,
              textAlign: "center",
              maxWidth: 280,
            }}
          >
            Start buildmesh on your computer to continue.
          </p>
          <button
            onClick={() => {
              setOffline(false);
            }}
            style={{
              marginTop: 8,
              background: "#2196f3",
              border: "none",
              borderRadius: 8,
              padding: "10px 20px",
              color: "#fff",
              fontSize: 14,
              cursor: "pointer",
            }}
          >
            Try again
          </button>
        </div>
      )}
    </>
  );
}
