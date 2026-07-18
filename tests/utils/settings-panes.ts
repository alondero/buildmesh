import { fireEvent, screen } from '@testing-library/react';

/**
 * The Settings modal (AppSettingsModal) is organised into sub-panes behind a
 * left nav rail. Content in an inactive pane carries the `hidden` attribute,
 * which testing-library's role queries exclude — so a test must activate the
 * owning pane's tab before querying its section. Pane names: 'General',
 * 'Providers', 'Harnesses', /remote access/i.
 */
export async function openSettingsPane(name: string | RegExp) {
  fireEvent.click(await screen.findByRole('tab', { name }));
}
