import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { MobileErrorBoundary } from "./ErrorBoundary";

// ErrorBoundary is mandatory on mobile: without it an uncaught render-time
// throw unmounts the entire root to a blank page, which paints as #0f0f0f
// (styles.css) and looks identical to an asset-load failure. The boundary
// converts that silent failure into a visible error card AND surfaces it
// to the desktop debug log via /__debug/log (see ErrorBoundary.tsx).
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <MobileErrorBoundary>
      <App />
    </MobileErrorBoundary>
  </React.StrictMode>,
);
