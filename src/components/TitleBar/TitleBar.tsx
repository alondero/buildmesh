import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import Wordmark from '../../assets/wordmark.png';
import { isMac } from '../../lib/platform';
import { ViewModeSwitcher } from '../ViewModeSwitcher/ViewModeSwitcher';
import { GridControls } from './GridControls';
import { AppSettingsModal } from '../AppSettings/AppSettingsModal';
import { RemoteAccessModal } from '../RemoteAccess/RemoteAccessModal';
import { useUIStore } from '../../stores/uiStore';
import { SHORTCUT_CATALOG, shortcutLabel } from '../../lib/shortcutCatalog';

/**
 * Bespoke window chrome for the frameless window (`decorations: false`).
 * macOS draws traffic lights on the LEFT — we can't reuse Tauri's
 * `titleBarStyle: Overlay` because it requires native decorations, so
 * the lights are drawn by us. Drag-region placement (per-target, never
 * on buttons or SVGs) is the load-bearing detail; see the recipe in
 * `docs/knowledge-primer.md`.
 *
 * Issue #1375 moved navigation title-bar-first: a labelled "Search or
 * open…" command field opens the Universal Command Omnibar (the palette
 * is the primary way to reach destinations), centred in the bar as the
 * primary affordance. Usage — a high-frequency, host-global utility —
 * gets its own labelled action instead of living behind icon-only Probe
 * navigation, and sits in the right-hand utility cluster next to the
 * Settings and Remote Access pills. None of them show data in the bar
 * itself; they are entry points, not readouts.
 *
 * Issue #1609 reshaped the clusters: the utility pills dropped their
 * borders and joined the switcher's 1300px label ladder (one toolbar, one
 * degradation curve), and the GridControls "Search nodes" bar moved from
 * the right cluster into the left, mounted only while the Filtered view is
 * active — it is that view's control, not a global fixture.
 */

// `SHORTCUT_CATALOG` is a static module constant; resolving the
// `open-omnibar` row once at module scope avoids an O(n) array `.find()`
// on every render of `NavigationControls` (PR #1489 review #2B).
// The label is platform-aware via `shortcutLabel(entry, isMac)` and
// `isMac` is itself a module-load-time read of `navigator.platform`.
const OMNIBAR_CATALOG_ENTRY = SHORTCUT_CATALOG.find((entry) => entry.action === 'open-omnibar');
const SEARCH_SHORTCUT_LABEL = OMNIBAR_CATALOG_ENTRY ? shortcutLabel(OMNIBAR_CATALOG_ENTRY) : '';

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

/** Lucide `search`. */
function SearchIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="11" cy="11" r="8" />
      <path d="m21 21-4.3-4.3" />
    </Svg>
  );
}

/** Gauge glyph for the Usage action (semi-dial + needle), drawn locally so
    the title bar keeps its icons self-contained like Settings/Mobile. */
function UsageIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="M4 19a8 8 0 1 1 16 0" />
      <path d="M12 11v4" />
      <path d="M12 19h.01" />
    </Svg>
  );
}

/** Issue #1375 — the centred command field. The labelled "Search or open…"
    field is the global entry point to the command palette (views, commands,
    nodes, issues, pull requests); the header's `1fr auto 1fr` grid centres
    it on the viewport. It opens a surface rather than displaying data
    inline. The kbd hint is read from the shortcut catalog (the
    knowledge-primer's single source for display labels) so it can never
    drift from the cheatsheet row. */
function NavigationControls() {
  // The read below drives the field's visible aria state — `aria-expanded`
  // mirrors the omnibar's mounted state so assistive tech reads the same
  // closed→open transition a sighted user gets. Selecting the primitive
  // directly keeps the subscription cheap so unrelated store ticks
  // (terminal output, agent node updates) don't re-render this header.
  const omnibarOpen = useUIStore((s) => s.omnibarOpen);
  // `min-w-44` pins this wrapper's floor to the field's own `min-w-40`
  // (130px at the 13px root) + padding — without it the wrapper's
  // content-based minimum keeps ~100px of dead slack above the field's
  // real floor, so flex/grid pressure starves the wordmark before the
  // field yields.
  return (
    <div className="flex items-center px-2 min-w-44">
      <button
        type="button"
        onClick={() => useUIStore.getState().openOmnibar('files')}
        data-testid="titlebar-command-search"
        aria-label="Search or open"
        aria-haspopup="dialog"
        aria-expanded={omnibarOpen}
        title="Search or open… (command palette)"
        // The field's design width is 640px (VS Code Command Palette
        // parity). `w-80` is the typical-viewport width — Tailwind v4
        // compiles it to `width: calc(var(--spacing) * 80)` where
        // `--spacing` defaults to `0.25rem`, so `w-80 = 20rem`. At the
        // 13px root font (`--font-size-base: 13px`), that's 260px — NOT
        // 320px (the 16px-root ghost). `max-w-full` is the safety belt
        // that prevents the centre cell from pushing past its parent
        // when the side clusters can't yield (PR #1623 review: a
        // hardcoded 640px centre broke the ViewModeSwitcher below
        // 1920px because the 1fr siblings couldn't yield past their
        // min-content — at 1440px with labels visible the left cluster
        // needs ~565px and only allows ~260–310px centre). At ≥1786px
        // the field bumps to its VS Code-parity `w-[640px]`; below
        // that, `w-80` keeps the side clusters intact. `min-w-40`
        // (130px at the 13px root — 10rem × 13px) is the floor the
        // field collapses to on half-screen windows (PR #1489 review —
        // drag-region starvation at sub-1000px). At the floor the
        // placeholder still reads "Search or open…" without truncation;
        // on tighter screens the user types into the palette, not the
        // bar. The class strings MUST stay literals so Tailwind v4's
        // source scanner picks them up — a template literal would defeat
        // JIT detection and the rules would never compile (PR #1623
        // review). Note: `w-[40rem]` is WRONG here because `1rem =
        // 13px` at the 13px root, giving 520px — use literal
        // `w-[640px]` for the VS Code-parity target.
        className="flex h-10 w-80 min-[1786px]:w-[640px] min-w-40 max-w-full items-center gap-2 rounded-md border border-border-default bg-bg-base px-3 text-sm text-text-muted transition-colors hover:border-accent-cyan/50 hover:text-text-primary"
      >
        <SearchIcon className="h-4 w-4 shrink-0" />
        <span className="min-w-0 flex-1 truncate text-left">Search or open…</span>
        {/* `SEARCH_SHORTCUT_LABEL` is resolved once at module scope. The
            conditional still keeps an empty <kbd> chip from appearing if
            the catalog row were ever renamed. The chip is the FIRST thing
            to disappear when narrowing (1400px threshold; labels stay
            visible down to 1300px — user-facing affordances outlast the
            decorative keyboard hint, per PR review feedback). The class
            string MUST stay a literal so Tailwind v4's source scanner
            picks it up — a template literal would defeat JIT detection
            and the rule would never compile. */}
        {SEARCH_SHORTCUT_LABEL !== '' && (
          <kbd className="shrink-0 rounded-md border border-border-default bg-bg-card px-1.5 py-0.5 font-mono text-[11px] text-text-muted max-[1399px]:hidden">
            {SEARCH_SHORTCUT_LABEL}
          </kbd>
        )}
      </button>
    </div>
  );
}

/** Shared skeleton for the right-hand utility cluster (Usage, Settings,
    Remote Access). Same style vocabulary as the ViewModeSwitcher segments
    (issue #1609): borderless, card-hover, active cyan — the pills read as
    part of the same toolbar instead of a separate bordered group. */
function HeaderPillButton({ icon, label, onClick, title, ariaLabel, active = false, testId, ariaExpanded }: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  title: string;
  ariaLabel: string;
  active?: boolean;
  testId?: string;
  ariaExpanded?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      data-testid={testId}
      aria-label={ariaLabel}
      aria-expanded={ariaExpanded}
      title={title}
      className={`inline-flex h-9 shrink-0 items-center gap-1.5 px-2 py-1.5 rounded-md text-sm font-sans font-medium transition-colors ${
        active
          ? 'bg-bg-card text-accent-cyan'
          : 'text-text-secondary hover:bg-bg-card hover:text-text-primary'
      }`}
    >
      {icon}
      {/* Icon-only below 1400px window width — unified with the
          switcher's ladder (issue #1609; previously the pills dropped at
          1150px, so between the two tiers the bar mixed labelled
          segments with icon pills). The threshold moved from 1300px to
          1400px to avoid a 2px clip on the rightmost ViewModeSwitcher
          segment ("Filtered") at exactly 1300px — at 1300px the labels
          become visible but the centre's `w-80` (260px at the 13px root)
          plus the side clusters' min-content (~565px each) overflows
          the side tracks (PR #1623 review). The aria-label above keeps
          the accessible name stable. */}
      <span className="max-[1399px]:hidden">{label}</span>
    </button>
  );
}

/** Usage — a high-frequency, host-global utility, so it keeps its own
    labelled action rather than living behind icon-only Probe navigation.
    The `title` flips to match the active state — "close" when Usage is
    already in the inspector, "open" otherwise. The visible label stays
    "Usage" so the surface name doesn't change mid-flight. */
function UsageButton() {
  const probeOpen = useUIStore((s) => s.probeOpen);
  const probeTab = useUIStore((s) => s.probeTab);
  const usageActive = probeOpen && probeTab === 'usage';
  return (
    <HeaderPillButton
      testId="titlebar-usage"
      ariaLabel="Open Usage"
      ariaExpanded={usageActive}
      onClick={() => useUIStore.getState().openProbeTab('usage')}
      title={usageActive
        ? 'Usage surface is open — click the inspector close (X) to dismiss'
        : 'Open Usage (provider meters and limits)'}
      label="Usage"
      active={usageActive}
      icon={<UsageIcon className="h-4 w-4 shrink-0" />}
    />
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
      className="h-10 w-auto"
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
  // #1609 — the Filtered view owns the Search Nodes bar; the mode lives in
  // uiStore so Omnibar view commands ("Switch view: Filtered") slide the
  // bar in through the same state the switcher writes.
  const viewMode = useUIStore((s) => s.viewMode);
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
      {/* Three-column grid (`1fr auto 1fr`): the palette field's column is
          `auto` and its siblings get equal `1fr` tracks, so the field is
          centred on the VIEWPORT — flanking `flex-1` spacers only centre it
          between unequal side clusters, which is not centring. The side
          cells are drag regions, so their empty space still grabs the
          window while buttons inside stay clickable.

          The palette's design width is 640px (VS Code Command Palette
          parity). At narrower viewports the field shrinks via
          `max-w-full` to whatever the centre cell can spare without
          starving the ViewModeSwitcher — measured viewport-width
          breakpoint per PR #1623 review: the field hits 640px at
          ≥1786px when the switcher labels are visible (left cluster
          needs ~565px to render all 5 segments). Below that, the field
          uses `w-80` (320px) so the side clusters always fit. This
          means users at typical laptop viewports (1366–1785px) see the
          same 320px palette as before — they don't lose switcher
          labels — while users on wide displays (1786px+, common for
          external monitors) get the full VS Code parity 640px trigger.

          Height: `h-14` (45.5px at the 13px root) is the floor that clears
          the tallest in-bar content — the `h-10` palette field and
          wordmark (32.5px) — with a symmetric 6.5px bezel; anything
          taller is dead space taken from the terminals below. */}
      <header
        data-tauri-drag-region
        className="grid h-14 shrink-0 grid-cols-[1fr_auto_1fr] items-stretch bg-bg-surface border-b border-border-subtle select-none"
      >
        {/* Left cell — clusters hug the start edge. */}
        <div data-tauri-drag-region className="flex items-center pl-3 pr-2">
          {isMac && (
            <div
              className="flex items-center gap-2 pr-3"
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
          )}
          <WordmarkImg />
          <ViewModeSwitcher />
          {/* #1609 — the Search Nodes bar IS the Filtered view's control, so
              it mounts beside the switcher only while that mode is active.
              An honest conditional mount: CSS can't interpolate `width: auto`,
              so a transition-all wrapper would snap anyway while leaving a
              4px `ml-1` behind when unmounted — `ml-1` lives on GridControls'
              own root instead, so nothing occupies layout space outside
              Filtered. */}
          {viewMode === 'filtered' && <GridControls />}
        </div>

        {/* Centre cell — the #1375 command field. Non-draggable so clicks
            don't grab the window. */}
        <NavigationControls />

        {/* Right cell — clusters hug the end edge. Non-draggable children
            (inputs/pills/buttons) keep their clicks; empty space drags. */}
        <div data-tauri-drag-region className="flex items-center justify-end">
          <div className="flex items-center gap-2 pr-1">
            <UsageButton />
            <HeaderPillButton
              ariaLabel="Open settings"
              onClick={() => useUIStore.getState().openAppSettings()}
              title="Settings"
              label="Settings"
              icon={<SettingsIcon className="h-4 w-4 shrink-0" />}
            />
            <HeaderPillButton
              ariaLabel="Open mobile remote access"
              onClick={() => useUIStore.getState().openRemoteAccess()}
              title="Remote access"
              label="Mobile"
              icon={<MobileIcon className="h-4 w-4 shrink-0" />}
            />
          </div>

          {!isMac && (
            <>
              <div className="w-px h-6 self-center bg-border-subtle mx-1 shrink-0" />

              {/* `shrink-0` — window buttons are critical chrome: flex
                  pressure at the 900px minimum width must never squish
                  them into unreadable slivers. */}
              <div className="flex items-stretch shrink-0">
                <WindowControlButton onClick={() => appWindow.minimize()} title="Minimize">
                  <MinimizeIcon className="w-[18px] h-[18px]" />
                </WindowControlButton>
                <WindowControlButton
                  onClick={handleToggleMaximize}
                  title={isMaximized ? 'Restore' : 'Maximize'}
                >
                  {isMaximized ? (
                    <RestoreIcon className="w-[18px] h-[18px]" />
                  ) : (
                    <MaximizeIcon className="w-[18px] h-[18px]" />
                  )}
                </WindowControlButton>
                <WindowControlButton onClick={() => appWindow.close()} title="Close" danger>
                  <CloseIcon className="w-[18px] h-[18px]" />
                </WindowControlButton>
              </div>
            </>
          )}
        </div>
      </header>

      {appSettingsOpen && <AppSettingsModal onClose={() => useUIStore.getState().closeAppSettings()} />}
      {remoteAccessOpen && <RemoteAccessModal onClose={() => useUIStore.getState().closeRemoteAccess()} />}
    </>
  );
}
