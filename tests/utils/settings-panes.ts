import { fireEvent, screen } from '@testing-library/react';

/**
 * The Settings modal (AppSettingsModal) is organised into sub-panes behind a
 * left nav rail. Content in an inactive pane carries the `hidden` attribute,
 * which testing-library's role queries exclude — so a test must activate the
 * owning pane's tab before querying by role. Label / text / test-id queries do
 * NOT exclude hidden content, so they resolve without opening the pane.
 *
 * Pane membership: 'General' (appearance, behaviour, agent runtime); 'Providers'
 * (provider routing defaults + credentials); 'Harnesses' (spawn-menu order,
 * proxied-provider config, Agent Harness defaults); /remote access/i (LAN,
 * coordinator API, authorized devices).
 */
export async function openSettingsPane(name: string | RegExp) {
  fireEvent.click(await screen.findByRole('tab', { name }));
}
