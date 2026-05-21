import { Suspense, lazy, useEffect, useState } from "react";
import "./styles.css";
import { AgentNode, readStoredToken } from "./api";
import Connect from "./screens/Connect";
import NodeList from "./screens/NodeList";

// Terminal pulls in xterm.js + FitAddon (~90 KB gzipped). Most mobile
// sessions look at the node list and don't open a terminal; defer the
// chunk until first use to keep the initial load fast.
const TerminalScreen = lazy(() => import("./screens/TerminalScreen"));
const ChangesScreen = lazy(() => import("./screens/ChangesScreen"));
const DiffScreen = lazy(() => import("./screens/DiffScreen"));
const CreatePrSheet = lazy(() => import("./screens/CreatePrSheet"));

type Screen =
  | { kind: "connect" }
  | { kind: "list" }
  | { kind: "terminal"; node: AgentNode }
  | { kind: "changes"; node: AgentNode }
  | { kind: "diff"; node: AgentNode; filePath: string };

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
  const [prSheet, setPrSheet] = useState<{
    node: AgentNode;
    branch: string;
  } | null>(null);
  const [prCreatedUrl, setPrCreatedUrl] = useState<string | null>(null);

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
            onOpenChanges={() =>
              setScreen({ kind: "changes", node: screen.node })
            }
          />
        </Suspense>
      )}
      {screen.kind === "changes" && (
        <Suspense
          fallback={
            <div style={{ flex: 1, padding: 16, color: "#666" }}>Loading…</div>
          }
        >
          <ChangesScreen
            node={screen.node}
            onBack={() => setScreen({ kind: "terminal", node: screen.node })}
            onOpenDiff={(filePath) =>
              setScreen({ kind: "diff", node: screen.node, filePath })
            }
            onOpenPr={(branch) =>
              setPrSheet({ node: screen.node, branch })
            }
          />
        </Suspense>
      )}
      {screen.kind === "diff" && (
        <Suspense
          fallback={
            <div style={{ flex: 1, padding: 16, color: "#666" }}>Loading…</div>
          }
        >
          <DiffScreen
            node={screen.node}
            filePath={screen.filePath}
            onBack={() => setScreen({ kind: "changes", node: screen.node })}
          />
        </Suspense>
      )}
      {prSheet && (
        <Suspense fallback={null}>
          <CreatePrSheet
            meshId={prSheet.node.mesh_id}
            currentBranch={prSheet.branch}
            onClose={() => setPrSheet(null)}
            onCreated={(url) => {
              setPrSheet(null);
              setPrCreatedUrl(url);
            }}
          />
        </Suspense>
      )}
      {prCreatedUrl && (
        <div
          data-testid="pr-success-toast"
          style={{
            position: "fixed",
            bottom: 16,
            left: 16,
            right: 16,
            background: "#1e3a2c",
            color: "#a5d6a7",
            border: "1px solid #2e7d32",
            borderRadius: 8,
            padding: 12,
            zIndex: 300,
            display: "flex",
            alignItems: "center",
            gap: 8,
            fontSize: 13,
          }}
        >
          <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis" }}>
            PR created:{" "}
            <a
              href={prCreatedUrl}
              target="_blank"
              rel="noreferrer"
              style={{ color: "#a5d6a7", textDecoration: "underline" }}
            >
              {prCreatedUrl}
            </a>
          </span>
          <button
            onClick={() => setPrCreatedUrl(null)}
            style={{
              background: "transparent",
              border: "none",
              color: "#a5d6a7",
              fontSize: 18,
              cursor: "pointer",
              padding: 0,
            }}
          >
            ×
          </button>
        </div>
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
