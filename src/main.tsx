import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installFrontendLogBridge } from "./lib/frontendLog";
import { ErrorBoundary } from "./components/ErrorBoundary/ErrorBoundary";
import { applyTheme } from "./lib/theme";
import { applyTextScale } from "./lib/textScale";
import { applyTypeDensity } from "./lib/typeDensity";
import { applyWindowZoomAtBoot } from "./lib/webviewZoom";

// Forward console.error/warn, window.error, and unhandledrejection to the
// Rust log file. Must run before any other code so we don't miss early
// failures.
installFrontendLogBridge();

// Apply the persisted theme to <html data-theme> BEFORE React mounts so the
// CSS cascade is in effect at first paint — no flash of dark theme on reloads
// where the user picked light. theme.ts reads localStorage at module load, so
// the attribute is set synchronously here (issue #734).
applyTheme();

// The interface text scale is read synchronously too (textScale.ts), so the
// first paint is already at the size the user chose — otherwise every launch
// would flash the default size and then reflow once React mounted.
applyTextScale();

// The type density is read synchronously too (typeDensity.ts), so the first
// paint already uses the ladder the user chose — comfortable (13px
// functional labels) or compact — with no flash of the wrong one on launch
// (issue #2739).
applyTypeDensity();

// The window zoom is the 200% enlargement path (issue #2739). Unlike the
// two settings above it cannot be applied from CSS — it is a native webview
// zoom factor, so it has to go through IPC and therefore lands after the
// first paint. A failure here is logged, never fatal: the app must still
// start if the platform cannot zoom.
void applyWindowZoomAtBoot();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
