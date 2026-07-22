import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import Wordmark from '../../assets/wordmark.png';
import { ViewModeSwitcher } from '../ViewModeSwitcher/ViewModeSwitcher';
import { AppSettingsModal } from '../AppSettings/AppSettingsModal';
import { RemoteAccessModal } from '../RemoteAccess/RemoteAccessModal';

/**
 * TitleBar — bespoke app chrome replacing the native title bar (the window
 * runs with `"decorations": false` in tauri.conf.json). One slim strip,
 * Chrome-style: wordmark left, the ViewModeSwitcher as the in-bar toolbar,
 * settings + remote-access icons ahead of the minimize / maximize / close
 * window controls on the right.
 *
 * Dragging and double-click maximize come from Tauri's injected drag-region
 * script: any mousedown target carrying `data-tauri-drag-region` starts a
 * drag (or toggles maximize on the second click of a double-click). The
 * attribute is therefore spread on the bar, the wordmark and the flexible
 * spacer — but never on the interactive clusters, whose buttons must stay
 * the mousedown target to receive clicks.
 *
 * The modals (App Settings, Remote Access) moved here from the Sidebar
 * header along with their buttons; provider-list refresh on settings
 * changes is covered by the `provider-list-changed` event the modal emits
 * (see `useProviderListInvalidation`).
 */

const appWindow = getCurrentWindow();

interface IconProps {
  className?: string;
}

function Svg({ className, children }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {children}
    </svg>
  );
}

/** Lucide `settings`. */
function SettingsIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </Svg>
  );
}

/** Lucide `smartphone`. */
function MobileIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect x="5" y="2" width="14" height="20" rx="2" ry="2" />
      <line x1="12" y1="18" x2="12" y2="18" />
    </Svg>
  );
}

function MinimizeIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <line x1="5" y1="12" x2="19" y2="12" />
    </Svg>
  );
}

function MaximizeIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect x="5" y="5" width="14" height="14" rx="1" />
    </Svg>
  );
}

function RestoreIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect x="7" y="3" width="12" height="12" rx="1" />
      <path d="M5 9v10a1 1 0 0 0 1 1h10" />
    </Svg>
  );
}

function CloseIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <line x1="6" y1="6" x2="18" y2="18" />
      <line x1="18" y1="6" x2="6" y2="18" />
    </Svg>
  );
}

/** Shared skeleton for the three window controls; `danger` gives the close
    button its red hover instead of the standard card hover. */
function WindowControlButton({ onClick, title, danger, children }: {
  onClick: () => void;
  title: string;
  danger?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`w-11 inline-flex items-center justify-center transition-colors ${
        danger
          ? 'text-text-secondary hover:bg-status-error hover:text-white'
          : 'text-text-secondary hover:bg-bg-card hover:text-text-primary'
      }`}
      title={title}
      aria-label={`${title} window`}
    >
      {children}
    </button>
  );
}

export function TitleBar() {
  const [appSettingsOpen, setAppSettingsOpen] = useState(false);
  const [remoteAccessOpen, setRemoteAccessOpen] = useState(false);
  const [isMaximized, setIsMaximized] = useState(false);

  // Track the maximized state so the middle window control can swap between
  // the maximize and restore glyphs. `onResized` fires for maximize,
  // restore and manual edge-drags alike; re-querying isMaximized on each
  // keeps us honest regardless of which gesture changed it.
  useEffect(() => {
    let cancelled = false;
    const sync = () => {
      appWindow.isMaximized().then(max => {
        if (!cancelled) setIsMaximized(max);
      });
    };
    sync();
    const unlisten = appWindow.onResized(sync);
    return () => {
      cancelled = true;
      unlisten.then(fn => fn());
    };
  }, []);

  // The onResized listener above is the single writer for isMaximized —
  // maximize/restore fires a resize, so the glyph re-syncs from the real
  // window state. No optimistic flip here: a rejected IPC would desync it.
  const handleToggleMaximize = () => {
    appWindow.toggleMaximize();
  };

  return (
    <>
      <header
        data-tauri-drag-region
        className="flex items-stretch h-9 shrink-0 bg-bg-surface border-b border-border-subtle select-none"
      >
        <div data-tauri-drag-region className="flex items-center pl-3 pr-2">
          <img
            src={Wordmark}
            data-tauri-drag-region
            className="h-5 w-auto"
            alt="Buildmesh"
          />
        </div>

        <div className="flex items-center">
          <ViewModeSwitcher />
        </div>

        {/* Drag-region spacer — the "empty" part of the strip the user grabs
            to move the window; grows to push the right-side controls over. */}
        <div data-tauri-drag-region className="flex-1" />

        <div className="flex items-center gap-1 pr-1">
          <button
            type="button"
            onClick={() => setAppSettingsOpen(true)}
            className="p-1 rounded-md text-text-muted hover:text-accent-cyan hover:bg-bg-card transition-colors"
            title="Settings"
            aria-label="Open settings"
          >
            <SettingsIcon className="w-4 h-4" />
          </button>
          <button
            type="button"
            onClick={() => setRemoteAccessOpen(true)}
            className="p-1 rounded-md text-accent-cyan hover:text-accent-blue hover:bg-bg-card transition-colors"
            title="Remote access"
            aria-label="Open remote access"
          >
            <MobileIcon className="w-4 h-4" />
          </button>
        </div>

        <div className="w-px h-4 self-center bg-border-subtle mx-1" />

        <div className="flex items-stretch">
          <WindowControlButton onClick={() => appWindow.minimize()} title="Minimize">
            <MinimizeIcon className="w-4 h-4" />
          </WindowControlButton>
          <WindowControlButton
            onClick={handleToggleMaximize}
            title={isMaximized ? 'Restore' : 'Maximize'}
          >
            {isMaximized ? (
              <RestoreIcon className="w-4 h-4" />
            ) : (
              <MaximizeIcon className="w-4 h-4" />
            )}
          </WindowControlButton>
          <WindowControlButton onClick={() => appWindow.close()} title="Close" danger>
            <CloseIcon className="w-4 h-4" />
          </WindowControlButton>
        </div>
      </header>

      {appSettingsOpen && <AppSettingsModal onClose={() => setAppSettingsOpen(false)} />}
      {remoteAccessOpen && <RemoteAccessModal onClose={() => setRemoteAccessOpen(false)} />}
    </>
  );
}
