/**
 * The DevTools port banner (issue #2836).
 *
 * The banner is the app's answer to "who else can drive this window while
 * the port is open", so the thing under test is the *warning*: it appears
 * when a port is open, it names the port, and it stays away when no
 * port is open.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';

const mocks = vi.hoisted(() => ({
  getDevtoolsStatus: vi.fn(),
}));

vi.mock('../../src/lib/tauri/devtools', () => ({
  getDevtoolsStatus: mocks.getDevtoolsStatus,
}));

import { DevtoolsPortBanner } from '../../src/components/Devtools/DevtoolsPortBanner';
import { useDevtoolsStore } from '../../src/stores/devtoolsStore';

beforeEach(() => {
  useDevtoolsStore.setState({ status: null });
  mocks.getDevtoolsStatus.mockReset();
});

describe('DevTools port banner (issue #2836)', () => {
  it('stays away when no port is open', async () => {
    mocks.getDevtoolsStatus.mockResolvedValue({ port: null, source: 'off' });
    render(<DevtoolsPortBanner />);
    await waitFor(() => expect(mocks.getDevtoolsStatus).toHaveBeenCalled());
    expect(screen.queryByTestId('devtools-port-banner')).toBeNull();
  });

  it('names the open port and says a local process can drive the window', async () => {
    mocks.getDevtoolsStatus.mockResolvedValue({ port: 9222, source: 'preference' });
    render(<DevtoolsPortBanner />);
    const banner = await screen.findByTestId('devtools-port-banner');
    expect(banner.textContent).toContain('9222');
    expect(banner.textContent).toContain('127.0.0.1');
    expect(banner.textContent).toMatch(/can drive this window/i);
  });

  it('stays away when the status fails to read', async () => {
    mocks.getDevtoolsStatus.mockRejectedValue(new Error('command not found'));
    render(<DevtoolsPortBanner />);
    await waitFor(() => expect(mocks.getDevtoolsStatus).toHaveBeenCalled());
    expect(screen.queryByTestId('devtools-port-banner')).toBeNull();
  });
});
