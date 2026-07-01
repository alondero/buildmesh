import { Component, type ErrorInfo, type ReactNode } from 'react';
import { logFrontend } from '../../lib/frontendLog';

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/**
 * Top-level React error boundary. Without this, a render-time throw blanks
 * the WebView with zero headlessly-readable signal. With this, the throw
 * goes to buildmesh.log AND the user sees a recoverable fallback UI.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    const where = errorInfo.componentStack ?? '<no component stack>';
    logFrontend(
      'error',
      `[ErrorBoundary] ${error.name}: ${error.message}\n${error.stack ?? '<no stack>'}\nComponent stack:${where}`,
    );
  }

  handleReload = () => {
    window.location.reload();
  };

  render() {
    if (this.state.error) {
      return (
        <div className="flex h-screen w-screen items-center justify-center bg-[#09090f] text-[#e0e0e0] p-8">
          <div className="max-w-2xl w-full bg-[#0d0d16] border border-[#ef4444]/50 rounded-md p-6 space-y-4">
            <div className="flex items-center gap-2">
              <div className="text-[#ef4444] text-2xl">⚠</div>
              <h1 className="text-xl font-semibold">Buildmesh hit a render error</h1>
            </div>
            <div className="text-sm text-[#94a3b8]">
              The UI crashed. Details have been written to <code className="text-[#00d4ff]">buildmesh.log</code>.
            </div>
            <pre className="text-xs text-[#e0e0e0] bg-[#09090f] border border-[#1f1f2e] rounded-md p-3 overflow-auto max-h-64">
              {this.state.error.name}: {this.state.error.message}
              {this.state.error.stack ? `\n\n${this.state.error.stack}` : ''}
            </pre>
            <button
              onClick={this.handleReload}
              className="px-4 py-2 bg-[#00d4ff] text-[#09090f] rounded-md hover:bg-[#00b8e0] font-medium"
            >
              Reload
            </button>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
