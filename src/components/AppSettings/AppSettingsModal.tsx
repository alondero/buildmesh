/**
 * `AppSettingsModal` — the Settings shell.
 *
 * Issue #1880 split this file (2,615 lines, ~30 `useState` sites in one
 * component) into per-pane modules. What remains here is deliberately
 * only what belongs to the shell:
 *
 *   - the nav rail and the five `SETTINGS_TABS`,
 *   - the modal-wide dirty-site aggregator (issue #730),
 *   - the shared error surface and the corrupt-preferences recovery panel,
 *     both of which sit *above* the panes because they apply app-wide,
 *   - composition: `SettingsDataProvider` wrapping the panes.
 *
 * Each pane owns its own drafts, toggles and mutation handlers:
 * `GeneralPane` (appearance, exit prompt, agent runtime, probe prompts),
 * `ProvidersPane` (routing defaults), `AccountsPane` (credentials),
 * `HarnessesPane` (launch configurations, spawn order, provider routes,
 * harness defaults), and `RemoteAccessPane` (LAN, coordinator, devices).
 *
 * ## Two invariants worth preserving
 *
 * **Panes stay mounted.** An inactive pane gets the `hidden` attribute
 * rather than being unmounted, because the dirty tracking lives in child
 * component state — unmounting on tab-switch would destroy half-typed
 * credentials while the modal still reports itself dirty (issue #730).
 * `Data & Diagnostics` is the one exception, and deliberately so: it holds
 * no editable state, so the rule does not apply, and mounting it on every
 * Settings open would load its chunk and fire two IPC calls for a tab most
 * users never open (issue #1568's bundle budget).
 *
 * **The dirty-site invariant lives with the state it guards.** `dirtySites`
 * is this component's own transition state and `siteDirtyChange` is the one
 * setter panes may call; a pane cannot read or write the set directly. The
 * nav-rail dots and the `<Modal dirty>` prop are both derived from it here.
 */
import { useCallback, useId, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { lazy, Suspense } from 'react';
import type { AppSettingsTab } from '../../stores/uiStore';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { PreferencesCorruptionPanel } from './PreferencesCorruptionPanel';
import { SettingsDataProvider } from './SettingsDataContext';
import { SETTINGS_TABS, paneForDirtySite, type SettingsTabId } from './settingsUtils';
import { GeneralPane } from './GeneralPane';
import { ProvidersPane } from './ProvidersPane';
import { AccountsPane } from './AccountsPane';
import { HarnessesPane } from './HarnessesPane';
import { RemoteAccessPane } from './RemoteAccessPane';
import { SettingsError, useDirtySites } from './settingsShell';
import { useSettingsData } from './SettingsDataContext';

interface AppSettingsModalProps {
  onClose: () => void;
  initialTab?: AppSettingsTab;
}

/** Data & Diagnostics is lazily-imported: a static import put it in the
 *  desktop entry chunk and pushed it 3.3 kB over the documented `maxRaw`
 *  ceiling (issue #1568). Most users never open this pane, so that is a real
 *  first-paint cost for nothing — which is why the budget is bumped
 *  intentionally rather than quietly. See the class comment for why this is
 *  also the one pane that is not kept mounted. */
const DataRecoverySection = lazy(() =>
  import('./DataRecoverySection').then(module => ({ default: module.DataRecoverySection })),
);

export function AppSettingsModal({ onClose, initialTab = 'general' }: AppSettingsModalProps) {
  const [activeTab, setActiveTab] = useState<SettingsTabId>(initialTab);
  const tabIdPrefix = useId();
  const selectedTabRef = useRef<HTMLButtonElement>(null);
  const { dirtySites, siteDirtyChange } = useDirtySites();

  // Which panes hold unsaved edits — drives the amber dot on the nav rail so
  // a dirty pane stays discoverable after the user tabs away from it.
  const dirtyPanes = useMemo(
    () => new Set([...dirtySites].map(paneForDirtySite)),
    [dirtySites],
  );

  // Vertical tablist keyboard model: Up/Down walk the rail, Home/End jump to
  // the ends. Following the tab order (not spatial position) keeps the model
  // correct however the rail wraps, and moving focus explicitly is what makes
  // the next arrow press act on the newly selected tab.
  const handleTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>, tab: SettingsTabId) => {
    const index = SETTINGS_TABS.findIndex(item => item.id === tab);
    let next: number;
    switch (event.key) {
      case 'ArrowUp': next = (index + SETTINGS_TABS.length - 1) % SETTINGS_TABS.length; break;
      case 'ArrowDown': next = (index + 1) % SETTINGS_TABS.length; break;
      case 'Home': next = 0; break;
      case 'End': next = SETTINGS_TABS.length - 1; break;
      default: return;
    }
    event.preventDefault();
    event.stopPropagation();
    setActiveTab(SETTINGS_TABS[next].id);
    event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
  };

  // The Launch Configurations section inside the Harnesses pane reports under
  // its own site key; `paneForDirtySite` maps the `harness-` prefix to that
  // pane. Routed here so the mapping stays in one place.
  const launchDirtyChange = useCallback(
    (dirty: boolean) => siteDirtyChange('harness-launch-configurations', dirty),
    [siteDirtyChange],
  );

  return (
    <SettingsDataProvider siteDirtyChange={siteDirtyChange}>
      <SettingsShell
        onClose={onClose}
        activeTab={activeTab}
        setActiveTab={setActiveTab}
        tabIdPrefix={tabIdPrefix}
        selectedTabRef={selectedTabRef}
        dirtyPanes={dirtyPanes}
        isDirty={dirtySites.size > 0}
        handleTabKeyDown={handleTabKeyDown}
        launchDirtyChange={launchDirtyChange}
      />
    </SettingsDataProvider>
  );
}

/**
 * The modal's visual shell, rendered inside `SettingsDataProvider` so it
 * can read the shared error surface and the corrupt-preferences recovery
 * panel. Split out from `AppSettingsModal` because the provider has to sit
 * *above* the component that consumes it — a component cannot wrap itself.
 */
function SettingsShell({
  onClose,
  activeTab,
  setActiveTab,
  tabIdPrefix,
  selectedTabRef,
  dirtyPanes,
  isDirty,
  handleTabKeyDown,
  launchDirtyChange,
}: {
  onClose: () => void;
  activeTab: SettingsTabId;
  setActiveTab: (tab: SettingsTabId) => void;
  tabIdPrefix: string;
  selectedTabRef: React.RefObject<HTMLButtonElement | null>;
  dirtyPanes: Set<SettingsTabId>;
  isDirty: boolean;
  handleTabKeyDown: (event: KeyboardEvent<HTMLButtonElement>, tab: SettingsTabId) => void;
  launchDirtyChange: (dirty: boolean) => void;
}) {
  const { preferencesCorruption, retryResource } = useSettingsData();

  return (
    <Modal
      onClose={onClose}
      labelledBy="app-settings-title"
      defaultFocusRef={selectedTabRef}
      maxWidth="max-w-5xl"
      className="p-0 max-h-[85vh] flex flex-col overflow-hidden"
      dirty={isDirty}
      dirtyMessage="Discard unsaved changes to your settings?"
    >
      {/* Non-scrolling header: title + close stay reachable no matter how far
          the settings body is scrolled. */}
      <div className="shrink-0 flex items-start justify-between gap-4 px-10 pt-8 pb-4 border-b border-border-subtle">
        <div>
          <h2 id="app-settings-title" className="text-2xl font-semibold text-text-primary mb-1">Settings</h2>
          <p className="text-base text-text-muted">
            Buildmesh-wide defaults. Per-mesh values in Project Settings take precedence.
          </p>
        </div>
        <ModalCloseButton onClose={onClose} label="Close settings" />
      </div>

      <div className="flex-1 flex min-h-0">
        <nav
          role="tablist"
          aria-orientation="vertical"
          aria-label="Settings sections"
          className="w-44 shrink-0 border-r border-border-subtle py-5 px-3 space-y-1 overflow-y-auto"
        >
          {SETTINGS_TABS.map(tab => (
            <button
              key={tab.id}
              type="button"
              role="tab"
              id={`${tabIdPrefix}-tab-${tab.id}`}
              aria-controls={`${tabIdPrefix}-panel-${tab.id}`}
              aria-selected={activeTab === tab.id}
              tabIndex={activeTab === tab.id ? 0 : -1}
              ref={activeTab === tab.id ? selectedTabRef : undefined}
              onClick={() => setActiveTab(tab.id)}
              onKeyDown={event => handleTabKeyDown(event, tab.id)}
              className={`w-full flex items-center justify-between text-left px-3 py-2 rounded-md text-base ${
                activeTab === tab.id
                  ? 'bg-bg-card text-accent-cyan font-medium'
                  : 'text-text-secondary hover:bg-bg-card-hover hover:text-text-primary'
              }`}
            >
              <span>{tab.label}</span>
              {dirtyPanes.has(tab.id) && (
                <span
                  className="h-1.5 w-1.5 rounded-full bg-status-warning"
                  title="Unsaved changes"
                  data-testid={`settings-tab-dirty-${tab.id}`}
                />
              )}
            </button>
          ))}
        </nav>

        <div className="flex-1 min-w-0 overflow-y-auto px-8 pb-10 pt-6">
          {/* Issue #1523 — the corrupt-preferences recovery panel. Above the
              panes rather than inside one, because the condition is app-wide:
              the settings it protects (accounts, keys, pairings, harness
              defaults) are spread across every tab, and the user has to be
              able to recover without first finding the right pane. */}
          {preferencesCorruption && (
            <PreferencesCorruptionPanel
              corruption={preferencesCorruption}
              onRecovered={() => {
                // Re-read every surface the recovered file can change — not
                // just preferences itself. `accounts` covers both the stored
                // accounts and the keyed catalog; `pairings` the stored
                // pairings; and `providers` is *derived* from the two, so
                // leaving it stale would show the pre-recovery provider list
                // in every routing dropdown and the harness-order list until a
                // restart. `routing` is the fallback list the dropdowns use
                // when `providers` hasn't loaded, so it must move together.
                retryResource('preferences');
                retryResource('accounts');
                retryResource('pairings');
                retryResource('providers');
                retryResource('routing');
              }}
            />
          )}

          <SettingsError />

          <section
            role="tabpanel"
            id={`${tabIdPrefix}-panel-general`}
            aria-labelledby={`${tabIdPrefix}-tab-general`}
            tabIndex={0}
            hidden={activeTab !== 'general'}
            className="space-y-2"
          >
            <GeneralPane />
          </section>

          <section
            role="tabpanel"
            id={`${tabIdPrefix}-panel-harnesses`}
            aria-labelledby={`${tabIdPrefix}-tab-harnesses`}
            tabIndex={0}
            hidden={activeTab !== 'harnesses'}
            className="space-y-2"
          >
            <HarnessesPane launchDirtyChange={launchDirtyChange} />
          </section>

          <section
            role="tabpanel"
            id={`${tabIdPrefix}-panel-providers`}
            aria-labelledby={`${tabIdPrefix}-tab-providers`}
            tabIndex={0}
            hidden={activeTab !== 'providers'}
            className="space-y-2"
          >
            <ProvidersPane />
            <AccountsPane />
          </section>

          <section
            role="tabpanel"
            id={`${tabIdPrefix}-panel-remote`}
            aria-labelledby={`${tabIdPrefix}-tab-remote`}
            tabIndex={0}
            hidden={activeTab !== 'remote'}
            className="space-y-2"
          >
            <RemoteAccessPane />
          </section>

          {/* Data & Diagnostics (issue #1537). Its own pane rather than a
              section on General because a staged restore and the "a copy was
              kept" notice are consequential enough to deserve full space.

              Rendered only while this tab is active — unlike the other panes,
              which stay mounted so their dirty state survives a tab-switch.
              This one holds no editable state, and mounting it unconditionally
              would load its chunk and fire two IPC calls on every Settings
              open. See the `lazy` note above. */}
          <section
            role="tabpanel"
            id={`${tabIdPrefix}-panel-data`}
            aria-labelledby={`${tabIdPrefix}-tab-data`}
            tabIndex={0}
            hidden={activeTab !== 'data'}
            className="space-y-2"
          >
            {activeTab === 'data' && (
              <Suspense
                fallback={
                  <p className="py-6 text-sm text-text-muted" data-testid="settings-data-recovery-loading">
                    Loading Data &amp; Diagnostics…
                  </p>
                }
              >
                <DataRecoverySection />
              </Suspense>
            )}
          </section>
        </div>
      </div>
    </Modal>
  );
}