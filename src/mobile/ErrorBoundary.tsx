import { Component, type ErrorInfo, type ReactNode } from 'react';

/**
 * Mobile top-level error boundary.
 *
 * Why this exists: `src/mobile/main.tsx` mounts <App/> bare. In React 18/19
 * an uncaught render-time throw unmounts the entire root to a blank page —
 * which renders as #0f0f0f (#1) from styles.css, indistinguishable from a
 * networking/asset-loading failure. The user sees a secure padlock and a
 * black screen with zero on-device signal. This boundary converts that
 * silent failure into a readable error card + reload button AND surfaces
 * the throw to the desktop log via the same `/__debug/log` endpoint the
 * server-side error-catcher shim (`assets::serve_spa_shell`) already uses.
 *
 * Why fetch('/__debug/log') and not logFrontend: the mobile SPA is pure
 * fetch with no Tauri runtime. Importing `@tauri-apps/api/core` would add
 * dead weight to the bundle and break the build (no `window.__TAURI__` on
 * the phone). The shim already established `/__debug/log` as the SPA's
 * log sink, so we reuse it.
 *
 * Best-effort reporting: a failed POST (network down, app shutting down)
 * must never crash the boundary itself or replace the fallback UI. We
 * fire-and-forget and swallow.
 */
interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

export class MobileErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    const componentStack = errorInfo.componentStack ?? '<no component stack>';
    // Same shape as the shim's POSTs (kind/error/promise) — distinguished by
    // `kind: 'boundary'` so the desktop log reader can tell "caught by React"
    // from "escaped to window".
    //
    // `src` is `location.pathname` (NOT `location.href`) — issue #1239. The
    // mobile SPA mounts on `/?token=<ROOT_TOKEN>` and `Connect.tsx` strips
    // the query string before mounting the rest of the tree. If a render
    // throw fires BEFORE that strip (e.g. from a child rendered during the
    // initial mount), the live `location.href` still carries `?token=`
    // verbatim — and the root token would land in the desktop's debug
    // log, which is the primary incident-diagnosis surface. Reporting the
    // pathname alone is enough for "which screen crashed" diagnostics
    // and removes the secret from the payload.
    try {
      void fetch('/__debug/log', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          kind: 'boundary',
          msg: `[MobileErrorBoundary] ${error.name}: ${error.message}\nComponent stack: ${componentStack}`,
          stack: error.stack ?? '',
          src: location.pathname,
        }),
      }).catch(() => {
        // Intentional: a logging failure cannot itself be logged without
        // recursing, and must not mask the fallback UI.
      });
    } catch {
      // Same — fetch throwing synchronously (e.g. CSP violation) must not
      // mask the fallback UI.
    }
  }

  handleReload = () => {
    window.location.reload();
  };

  render() {
    if (!this.state.error) return this.props.children;

    // Inline styles: keep the boundary self-contained so it still renders
    // even if styles.css failed to load — which is precisely the failure
    // mode we want a usable recovery card for.
    return (
      <div
        style={{
          position: 'fixed',
          inset: 0,
          background: '#09090f',
          color: '#e0e0e0',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          padding: '1.5rem',
          fontFamily:
            'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif',
        }}
      >
        <div
          style={{
            maxWidth: '32rem',
            width: '100%',
            background: '#0d0d16',
            border: '1px solid rgba(239, 68, 68, 0.5)',
            borderRadius: '6px',
            padding: '1.25rem',
            display: 'flex',
            flexDirection: 'column',
            gap: '0.85rem',
          }}
        >
          <div style={{ display: 'flex', alignItems: 'center', gap: '0.5rem' }}>
            <div style={{ color: '#ef4444', fontSize: '1.5rem' }}>⚠</div>
            <h1 style={{ fontSize: '1.05rem', fontWeight: 600, margin: 0 }}>
              Buildmesh hit a render error
            </h1>
          </div>
          <div style={{ fontSize: '0.85rem', color: '#94a3b8' }}>
            The UI crashed. The details below were sent to the desktop's debug log.
          </div>
          <pre
            style={{
              fontSize: '0.72rem',
              color: '#e0e0e0',
              background: '#09090f',
              border: '1px solid #1f1f2e',
              borderRadius: '4px',
              padding: '0.7rem',
              overflow: 'auto',
              maxHeight: '14rem',
              margin: 0,
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-word',
            }}
          >
            {this.state.error.name}: {this.state.error.message}
            {this.state.error.stack ? `\n\n${this.state.error.stack}` : ''}
          </pre>
          <button
            onClick={this.handleReload}
            style={{
              alignSelf: 'flex-start',
              padding: '0.45rem 0.95rem',
              background: '#00d4ff',
              color: '#09090f',
              border: 'none',
              borderRadius: '4px',
              fontWeight: 600,
              fontSize: '0.9rem',
              cursor: 'pointer',
            }}
          >
            Reload
          </button>
        </div>
      </div>
    );
  }
}