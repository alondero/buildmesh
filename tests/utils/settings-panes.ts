import { fireEvent, screen } from '@testing-library/react';

/**
 * The Settings modal (AppSettingsModal) is organised into sub-panes behind a
 * left nav rail. Content in an inactive pane carries the `hidden` attribute,
 * which testing-library's role queries exclude — so a test must activate the
 * owning pane's tab before querying by role. Label / text / test-id queries do
 * NOT exclude hidden content, so they resolve without opening the pane.
 *
 * Pane membership: 'General' (appearance, behaviour, worktree directory);
 * 'Providers' (provider routing defaults + credentials); 'Harnesses' (spawn-menu
 * order, launch configurations behind a toggle, proxied-provider config, Agent
 * Harness defaults); 'Autopilot' (Circuit agent pool size); 'Prompts' (probe
 * spawn prompts); /remote access/i (LAN, coordinator API, authorized devices).
 */
export async function openSettingsPane(name: string | RegExp) {
  if (name === 'Harnesses') name = 'Launch Configurations';
  fireEvent.click(await screen.findByRole('tab', { name }));
}

/**
 * The saved-configuration list inside the Launch Configurations pane is
 * collapsed by default (the pane stays mounted, so the toggle state survives a
 * tab switch). Open it from the active pane; a no-op if it is already open, so
 * a test that revisits the tab cannot collapse it by clicking twice.
 */
export async function showLaunchConfigurations() {
  const toggle = await screen.findByRole('button', { name: /^(show|hide) launch configurations$/i });
  if (toggle.getAttribute('aria-expanded') === 'false') fireEvent.click(toggle);
}
