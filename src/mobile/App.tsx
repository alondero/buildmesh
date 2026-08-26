import { Suspense, lazy, useCallback, useEffect, useRef, useState } from "react";
import "./styles.css";
import { AgentNode, Mesh, clearStoredToken, readStoredToken } from "./api";
import Connect from "./screens/Connect";
import NodeList from "./screens/NodeList";
import { ScreenLoading } from "./ui";

// Terminal pulls in xterm.js + FitAddon (~90 KB gzipped). Most mobile
// sessions look at the node list and don't open a terminal; defer the
// chunk until first use to keep the initial load fast.
const TerminalScreen = lazy(() => import("./screens/TerminalScreen"));
const ChangesScreen = lazy(() => import("./screens/ChangesScreen"));
const DiffScreen = lazy(() => import("./screens/DiffScreen"));
const CreatePrSheet = lazy(() => import("./screens/CreatePrSheet"));
const ArchivedNodesScreen = lazy(() => import("./screens/ArchivedNodesScreen"));
const IssuesScreen = lazy(() => import("./screens/IssuesScreen"));

export type Screen =
  | { kind: "connect" }
  | { kind: "list" }
  | { kind: "terminal"; node: AgentNode }
  | { kind: "changes"; node: AgentNode }
  | { kind: "diff"; node: AgentNode; filePath: string }
  | { kind: "sessions"; mesh: Mesh }
  | { kind: "issues"; mesh: Mesh };

/// Where one step "back" lands from each screen. The popstate handler uses
/// this instead of stored history payloads, so the OS back gesture, the
/// Android back button, and in-app ← buttons all converge on the same
/// hierarchy regardless of how a screen was reached (e.g. a terminal entered
/// laterally from the sessions screen still backs out to the list).
export function parentOf(s: Screen): Screen {
  switch (s.kind) {
    case "terminal":
    case "sessions":
    case "issues":
      return { kind: "list" };
    case "changes":
      return { kind: "terminal", node: s.node };
    case "diff":
      return { kind: "changes", node: s.node };
    default:
      return s;
  }
}

export default function App() {
  // Bootstrap (issue #500): a fresh `?token=` load has NO cookie yet — the
  // token must be exchanged for one via POST /api/session — so route through
  // Connect, which performs that login then calls onConnected. With a stored
  // token (returning user) land on the list optimistically: a still-valid
  // cookie just works, and an expired one trips handleAuthFailed → Connect.
  const initial: Screen = (() => {
    const params = new URLSearchParams(window.location.search);
    if (params.get("token")) return { kind: "connect" };
    if (readStoredToken()) return { kind: "list" };
    return { kind: "connect" };
  })();

  const [screen, setScreen] = useState<Screen>(initial);
  const [offline, setOffline] = useState(false);
  // Remount key for NodeList: "Try again" bumps it to force an immediate
  // refetch instead of waiting out the 5-second poll.
  const [listKey, setListKey] = useState(0);
  const [authNotice, setAuthNotice] = useState<string | null>(null);
  const [prSheet, setPrSheet] = useState<{
    node: AgentNode;
    branch: string;
  } | null>(null);
  const [prCreatedUrl, setPrCreatedUrl] = useState<string | null>(null);

  const prSheetRef = useRef(prSheet);
  prSheetRef.current = prSheet;
  // Recovery is a transition, not an event. The guard makes concurrent 401s
  // share one clear-and-connect transition, while the version invalidates
  // callbacks retained by screens that were unmounted during recovery.
  const authRecoveryRef = useRef(false);
  const recoveryVersionRef = useRef(0);

  // Forward navigation pushes a history entry so back gestures walk back
  // through screens instead of leaving the app entirely.
  const navigate = useCallback((next: Screen) => {
    if (authRecoveryRef.current) return;
    window.history.pushState({ bm: true }, "");
    setScreen(next);
  }, []);

  const goBack = useCallback(() => {
    window.history.back();
  }, []);

  useEffect(() => {
    const onPop = () => {
      // A sheet counts as the topmost "screen": back closes it first.
      if (prSheetRef.current) {
        setPrSheet(null);
        return;
      }
      setScreen((cur) => parentOf(cur));
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  // Track the visual viewport so the layout (and any sheet/composer pinned
  // to its bottom) shrinks above the soft keyboard rather than hiding
  // behind it. #root's height reads --app-height (styles.css).
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const apply = () => {
      document.documentElement.style.setProperty(
        "--app-height",
        `${Math.round(vv.height)}px`,
      );
      // iOS scrolls the layout viewport when the keyboard opens even with
      // overflow:hidden; pin it back so the app chrome stays on-screen.
      window.scrollTo(0, 0);
    };
    apply();
    vv.addEventListener("resize", apply);
    return () => {
      vv.removeEventListener("resize", apply);
      document.documentElement.style.removeProperty("--app-height");
    };
  }, []);

  const handleAuthFailed = useCallback(() => {
    if (authRecoveryRef.current) return;
    authRecoveryRef.current = true;
    recoveryVersionRef.current += 1;
    clearStoredToken();
    setAuthNotice("Connection expired — scan the QR code again to reconnect.");
    // If a PR sheet was open when auth expired, it had pushed its own history
    // entry via openPrSheet — pop it so the user's next back press (or iOS
    // swipe-back) lands on the screen below, not on the dead entry the sheet
    // left behind (issue #1260). Mirror the success path in onCreated, which
    // also calls window.history.back().
    const sheetWasOpen = prSheetRef.current !== null;
    prSheetRef.current = null;
    setPrSheet(null);
    setPrCreatedUrl(null);
    setOffline(false);
    setScreen({ kind: "connect" });
    if (sheetWasOpen) {
      window.history.back();
    }
  }, []);

  const openPrSheet = useCallback((node: AgentNode, branch: string) => {
    if (authRecoveryRef.current) return;
    window.history.pushState({ bm: true }, "");
    setPrSheet({ node, branch });
  }, []);

  const activeRecoveryVersion = recoveryVersionRef.current;
  const handleAuthFailedForActiveScreen = useCallback(() => {
    if (activeRecoveryVersion !== recoveryVersionRef.current) return;
    handleAuthFailed();
  }, [activeRecoveryVersion, handleAuthFailed]);
  const handleConnected = useCallback(() => {
    authRecoveryRef.current = false;
    setAuthNotice(null);
    setScreen({ kind: "list" });
  }, []);

  return (
    <>
      {screen.kind === "connect" && (
        <Connect
          notice={authNotice}
          onConnected={handleConnected}
        />
      )}
      {screen.kind === "list" && (
        <NodeList
          key={listKey}
          onOpenNode={(node) => {
            if (activeRecoveryVersion !== recoveryVersionRef.current) return;
            navigate({ kind: "terminal", node });
          }}
          onOpenAgentNodes={(mesh) => {
            if (activeRecoveryVersion !== recoveryVersionRef.current) return;
            navigate({ kind: "sessions", mesh });
          }}
          onOpenIssues={(mesh) => {
            if (activeRecoveryVersion !== recoveryVersionRef.current) return;
            navigate({ kind: "issues", mesh });
          }}
          onOffline={() => {
            if (activeRecoveryVersion !== recoveryVersionRef.current) return;
            setOffline(true);
          }}
          onAuthFailed={handleAuthFailedForActiveScreen}
        />
      )}
      {screen.kind === "terminal" && (
        <Suspense
          fallback={
            <ScreenLoading testId="terminal-loading" label="Loading terminal…" />
          }
        >
          <TerminalScreen
            node={screen.node}
            onBack={goBack}
            onAuthFailed={handleAuthFailedForActiveScreen}
            onOpenChanges={() => {
              if (activeRecoveryVersion !== recoveryVersionRef.current) return;
              navigate({ kind: "changes", node: screen.node });
            }}
          />
        </Suspense>
      )}
      {screen.kind === "changes" && (
        <Suspense fallback={<ScreenLoading />}>
          <ChangesScreen
            node={screen.node}
            onBack={goBack}
            onOpenDiff={(filePath) => {
              if (activeRecoveryVersion !== recoveryVersionRef.current) return;
              navigate({ kind: "diff", node: screen.node, filePath });
            }}
            onOpenPr={(branch) => {
              if (activeRecoveryVersion !== recoveryVersionRef.current) return;
              openPrSheet(screen.node, branch);
            }}
            onAuthFailed={handleAuthFailedForActiveScreen}
          />
        </Suspense>
      )}
      {screen.kind === "diff" && (
        <Suspense fallback={<ScreenLoading />}>
          <DiffScreen
            node={screen.node}
            filePath={screen.filePath}
            onBack={goBack}
            onAuthFailed={handleAuthFailedForActiveScreen}
          />
        </Suspense>
      )}
      {screen.kind === "sessions" && (
        <Suspense fallback={<ScreenLoading />}>
          <ArchivedNodesScreen
            mesh={screen.mesh}
            onBack={goBack}
            onResumed={(node) => {
              if (activeRecoveryVersion !== recoveryVersionRef.current) return;
              setScreen({ kind: "terminal", node });
            }}
            onAuthFailed={handleAuthFailedForActiveScreen}
          />
        </Suspense>
      )}
      {screen.kind === "issues" && (
        <Suspense fallback={<ScreenLoading />}>
          <IssuesScreen
            mesh={screen.mesh}
            onBack={goBack}
            onSpawned={(node) => {
              if (activeRecoveryVersion !== recoveryVersionRef.current) return;
              setScreen({ kind: "terminal", node });
            }}
            onAuthFailed={handleAuthFailedForActiveScreen}
          />
        </Suspense>
      )}
      {prSheet && (
        <Suspense fallback={null}>
          <CreatePrSheet
            meshId={prSheet.node.mesh_id}
            currentBranch={prSheet.branch}
            onClose={goBack}
            onAuthFailed={handleAuthFailedForActiveScreen}
            onCreated={(url) => {
              if (activeRecoveryVersion !== recoveryVersionRef.current) return;
              setPrCreatedUrl(url);
              window.history.back(); // pops the sheet's history entry
            }}
          />
        </Suspense>
      )}
      {prCreatedUrl && (
        <div data-testid="pr-success-toast" className="toast success">
          <span
            style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis" }}
          >
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
            aria-label="Dismiss"
            style={{
              background: "transparent",
              border: "none",
              color: "#a5d6a7",
              fontSize: 18,
              cursor: "pointer",
              padding: "0 4px",
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
            position: "absolute",
            inset: 0,
            zIndex: 400,
            background: "var(--bg)",
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            gap: 12,
            color: "#fff",
            padding: 24,
            textAlign: "center",
          }}
        >
          <div style={{ fontSize: 32 }}>📡</div>
          <h2 style={{ fontSize: 18, margin: 0 }}>Can't reach the desktop app</h2>
          <p
            style={{
              color: "var(--text-faint)",
              fontSize: 13,
              maxWidth: 300,
              margin: 0,
              lineHeight: 1.5,
            }}
          >
            Make sure Buildmesh is running on your computer and this phone is
            on the same network.
          </p>
          <button
            className="btn-primary"
            style={{ marginTop: 8 }}
            onClick={() => {
              setOffline(false);
              setListKey((k) => k + 1);
            }}
          >
            Try again
          </button>
        </div>
      )}
    </>
  );
}
