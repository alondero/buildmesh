import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import Wordmark from '../../assets/wordmark.png';
import { isMac } from '../../lib/platform';
import { ViewModeSwitcher } from '../ViewModeSwitcher/ViewModeSwitcher';
import { GridControls } from './GridControls';
import { AppSettingsModal } from '../AppSettings/AppSettingsModal';
import { RemoteAccessModal } from '../RemoteAccess/RemoteAccessModal';
import { useUIStore } from '../../stores/uiStore';

/**
 * Bespoke window chrome for the frameless window (`decorations: false`).
 * macOS draws traffic lights on the LEFT — we can't reuse Tauri's
 * `titleBarStyle: Overlay` because it requires native decorations, so
 * the lights are drawn by us. Drag-region placement (per-target, never
 * on buttons or SVGs) is the load-bearing detail; see the recipe in
 * `docs/knowledge-primer.md`.
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

/** macOS traffic-light background classes — system palette values for the
    standard red/yellow/green, matching what `NSWindow` draws natively. */
const MAC_TRAFFIC_LIGHT_CLASSES = {
  close: 'bg-[#FF5F57]',
  minimize: 'bg-[#FEBC2E]',
  maximize: 'bg-[#28C840]',
} as const;

/** One of the three macOS traffic lights. 12×12 px coloured circle, with
    the matching glyph (X / dash / plus) revealed on hover to mirror
    NSWindow's behaviour. The button itself is the circle (no padding
    wrapper) so the click target matches the visible affordance — Tauri
    buttons live in the bar's flex row at `items-center` so vertical
    centring is inherited from the parent. */
function MacosTrafficLight({ kind, onClick, ariaLabel }: {
  kind: keyof typeof MAC_TRAFFIC_LIGHT_CLASSES;
  onClick: () => void;
  ariaLabel: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={ariaLabel}
      aria-label={`${ariaLabel} window`}
      data-testid={`macos-traffic-${kind}`}
      className={`group w-3 h-3 rounded-full ${MAC_TRAFFIC_LIGHT_CLASSES[kind]} flex items-center justify-center transition-[filter] hover:brightness-[0.92] focus:outline-none focus-visible:ring-1 focus-visible:ring-white/40`}
    >
      <svg
        viewBox="0 0 8 8"
        className="w-2 h-2 text-black/70 opacity-0 transition-opacity group-hover:opacity-100"
        fill="none"
        stroke="currentColor"
        strokeWidth="1"
        strokeLinecap="round"
        aria-hidden
      >
        {kind === 'close' && (
          <>
            <line x1="2" y1="2" x2="6" y2="6" />
            <line x1="6" y1="2" x2="2" y2="6" />
          </>
        )}
        {kind === 'minimize' && <line x1="2" y1="4" x2="6" y2="4" />}
        {kind === 'maximize' && (
          <>
            <line x1="2" y1="4" x2="6" y2="4" />
            <line x1="4" y1="2" x2="4" y2="6" />
          </>
        )}
      </svg>
    </button>
  );
}

/** The wordmark <img>, kept as a single source so the macOS and
    non-macOS branches can't drift on the asset, alt text, or drag-region
    attribute. */
function WordmarkImg() {
  return (
    <img
      src={Wordmark}
      data-tauri-drag-region
      className="h-5 w-auto"
      alt="Buildmesh"
    />
  );
}

export function TitleBar() {
  // Issue #1411 review: the two modals' open state lives in `uiStore` (not
  // local state) so the Omnibar's "Open Settings" / "Open Remote Access"
  // commands summon the same modals through the same source of truth the
  // header buttons use — no window-event side channel.
  const appSettingsOpen = useUIStore((s) => s.appSettingsOpen);
  const remoteAccessOpen = useUIStore((s) => s.remoteAccessOpen);
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
        {isMac ? (
          <>
            <div
              className="flex items-center gap-2 pl-3 pr-3"
              data-testid="macos-traffic-lights"
            >
              <MacosTrafficLight kind="close" onClick={() => appWindow.close()} ariaLabel="Close" />
              <MacosTrafficLight kind="minimize" onClick={() => appWindow.minimize()} ariaLabel="Minimize" />
              <MacosTrafficLight
                kind="maximize"
                onClick={handleToggleMaximize}
                ariaLabel={isMaximized ? 'Restore' : 'Maximize'}
              />
            </div>
            <div data-tauri-drag-region className="flex items-center pr-2">
              <WordmarkImg />
            </div>
          </>
        ) : (
          <div data-tauri-drag-region className="flex items-center pl-3 pr-2">
            <WordmarkImg />
          </div>
        )}

        <div className="flex items-center">
          <ViewModeSwitcher />
        </div>

        {/* Drag-region spacer — the "empty" part of the strip the user grabs
            to move the window; grows to push the right-side controls over. */}
        <div data-tauri-drag-region className="flex-1" />

        {/* Issue #998 — grid search control. Lives on the right side of the
            slim top bar, between the drag-region spacer and the
            Settings / Remote Access buttons, per the wayfinder #988 spec
            ("Controls live on the right side of the slim top View Header,
            introduced in #983"). The component itself is a subset of the
            full #997 work — just the search input + clear button; the
            filter popover, sort selector, badges, and reset button called
            out in #997 land in a follow-up. The component is non-draggable
            (`[data-tauri-drag-region]` is on the header, not bubbled here)
            so clicks on the input don't grab the window. */}
        <div className="flex items-center pr-1">
          <GridControls />
        </div>

        <div className="flex items-center gap-1 pr-1">
          <button
            type="button"
            onClick={() => useUIStore.getState().openAppSettings()}
            className="p-1 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-card transition-colors"
            title="Settings"
            aria-label="Open settings"
          >
            <SettingsIcon className="w-4 h-4" />
          </button>
          <button
            type="button"
            onClick={() => useUIStore.getState().openRemoteAccess()}
            className="p-1 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-card transition-colors"
            title="Remote access"
            aria-label="Open remote access"
          >
            <MobileIcon className="w-4 h-4" />
          </button>
        </div>

        {!isMac && (
          <>
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
          </>
        )}
      </header>

      {appSettingsOpen && <AppSettingsModal onClose={() => useUIStore.getState().closeAppSettings()} />}
      {remoteAccessOpen && <RemoteAccessModal onClose={() => useUIStore.getState().closeRemoteAccess()} />}
    </>
  );
}
