/**
 * The in-app update prompt (issue #826): when `runUpdateCheck` surfaces a
 * pending update, a Modal offers "Install & Restart" (download + relaunch) or
 * "Later" (dismiss for the session). We mock the updater seam and the process
 * plugin so the test never touches real IPC — it asserts the wiring, not the
 * plugin internals.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';

// Fake pending update with a spy install handle. Reset per test.
const { downloadAndInstallMock, runUpdateCheckMock, relaunchMock } = vi.hoisted(() => ({
  downloadAndInstallMock: vi.fn().mockResolvedValue(undefined),
  runUpdateCheckMock: vi.fn(),
  relaunchMock: vi.fn().mockResolvedValue(undefined),
}));

// Partial-mock the updater lib: override the plugin-touching `runUpdateCheck`
// but keep the pure `describeUpdate` real (importOriginal spread), so the
// component renders the same message a real update would produce.
vi.mock('../../src/lib/updater', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/updater')>();
  return { ...actual, runUpdateCheck: runUpdateCheckMock };
});

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: relaunchMock,
}));

import { UpdatePrompt } from '../../src/components/UpdatePrompt/UpdatePrompt';

const fakeUpdate = () => ({
  version: '0.2.0',
  body: 'Shiny new things.',
  downloadAndInstall: downloadAndInstallMock,
});

describe('UpdatePrompt', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders nothing when there is no update', async () => {
    runUpdateCheckMock.mockResolvedValue(null);
    const { container } = render(<UpdatePrompt />);
    // Give the mount-effect a tick to resolve.
    await waitFor(() => expect(runUpdateCheckMock).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it('shows the prompt with version + notes when an update is available', async () => {
    runUpdateCheckMock.mockResolvedValue(fakeUpdate());
    render(<UpdatePrompt />);
    expect(await screen.findByText('Buildmesh 0.2.0 is available.')).toBeTruthy();
    expect(screen.getByText('Shiny new things.')).toBeTruthy();
  });

  it('downloads, installs, and relaunches on "Install & Restart"', async () => {
    runUpdateCheckMock.mockResolvedValue(fakeUpdate());
    render(<UpdatePrompt />);
    const install = await screen.findByRole('button', { name: 'Install & Restart' });
    fireEvent.click(install);
    await waitFor(() => expect(downloadAndInstallMock).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(relaunchMock).toHaveBeenCalledTimes(1));
  });

  it('dismisses without installing on "Later"', async () => {
    runUpdateCheckMock.mockResolvedValue(fakeUpdate());
    render(<UpdatePrompt />);
    const later = await screen.findByRole('button', { name: 'Later' });
    fireEvent.click(later);
    await waitFor(() =>
      expect(screen.queryByText('Buildmesh 0.2.0 is available.')).toBeNull(),
    );
    expect(downloadAndInstallMock).not.toHaveBeenCalled();
    expect(relaunchMock).not.toHaveBeenCalled();
  });
});
