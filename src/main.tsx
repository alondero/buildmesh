import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installFrontendLogBridge } from "./lib/frontendLog";
import { ErrorBoundary } from "./components/ErrorBoundary/ErrorBoundary";
import { applyTheme } from "./lib/theme";

// Forward console.error/warn, window.error, and unhandledrejection to the
// Rust log file. Must run before any other code so we don't miss early
// failures.
installFrontendLogBridge();

// Apply the persisted theme to <html data-theme> BEFORE React mounts so the
// CSS cascade is in effect at first paint — no flash of dark theme on reloads
// where the user picked light. theme.ts reads localStorage at module load, so
// the attribute is set synchronously here (issue #734).
applyTheme();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
