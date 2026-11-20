/**
 * The "a local process can drive this window" notice (issue #2836).
 *
 * The DevTools port is opt-in, and it is the one setting in the app that
 * widens the surface for *other* software: while it is open, any process on
 * this machine can attach to the WebView and read — and change — the page.
 * A setting the user does not see is not a setting the user made, so the port
 * says so out loud for as long as it is open.
 *
 * The notice is deliberately narrow and quiet: it is a status strip under
 * the title bar, not a modal. The user asked for the port; the notice only
 * has to keep the consequence visible, and a modal that blocks the app would
 * be a worse version of the same message.
 *
 * The port does not open or close while the app runs (it is a startup
 * switch), so this reads the status once and does not poll.
 */
import { useEffect } from 'react';
import { useDevtoolsStore } from '../../stores/devtoolsStore';

export function DevtoolsPortBanner() {
  const status = useDevtoolsStore((s) => s.status);
  const initDevtoolsStatus = useDevtoolsStore((s) => s.initDevtoolsStatus);

  useEffect(() => {
    void initDevtoolsStatus();
  }, [initDevtoolsStatus]);

  if (!status?.port) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      data-testid="devtools-port-banner"
      className="flex min-h-8 shrink-0 items-center gap-2 border-b border-accent-amber/35 bg-accent-amber/15 px-3 text-xs text-text-primary"
    >
      <span className="font-medium shrink-0">DevTools port {status.port} is open</span>
      <span className="text-text-secondary min-w-0 truncate">
        on 127.0.0.1 — any program on this computer can drive this window. Close it in
        Settings → General → Diagnostics and restart Buildmesh.
      </span>
    </div>
  );
}
