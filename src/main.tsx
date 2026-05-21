import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installFrontendLogBridge } from "./lib/frontendLog";
import { ErrorBoundary } from "./components/ErrorBoundary/ErrorBoundary";

// Forward console.error/warn, window.error, and unhandledrejection to the
// Rust log file. Must run before any other code so we don't miss early
// failures.
installFrontendLogBridge();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
