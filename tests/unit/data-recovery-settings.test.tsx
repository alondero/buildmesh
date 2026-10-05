import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { openSettingsPane } from '../utils/settings-panes';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

/**
 * Settings > Data & Diagnostics (issue #1537).
 *
 * The behavioural claims this file pins are the ones a user would notice
 * breaking:
 *
 * - a failed integrity check is reported as a failure, not smoothed into a
 *   success message;
 * - a *cancelled* native dialog is a no-op, not an error banner (the backend
 *   returns `null` for that case, and a picker dismissal must not alarm);
 * - restore is a two-step confirm — reading a bundle does not stage it, and
 *   the destructive button only appears once a bundle has been read;
 * - redaction is on by default, because an export is the artefact most likely
 *   to leave the machine.
 */

type Info = {
  app_data_dir: string;
  snapshot_dir: string;
  schema_version: number;
  snapshot_count: number;
  retention: number;
  pending_restore: boolean;
  notice: { severity: string; message: string; snapshot_path: string | null; recorded_at: string } | null;
};

function baseInfo(overrides: Partial<Info> = {}): Info {
  return {
    app_data_dir: 'C:/Users/test/AppData/Roaming/dev.buildmesh',
    snapshot_dir: 'C:/Users/test/AppData/Roaming/dev.buildmesh/snapshots',
    schema_version: 46,
    snapshot_count: 2,
    retention: 3,
    pending_restore: false,
    notice: null,
    ...overrides,
  };
}

const HEALTHY_SNAPSHOTS = [
  {
    path: 'C:/x/snapshots/20260103T090000Z-manual.bmsnap',
    file_name: '20260103T090000Z-manual.bmsnap',
    kind: 'manual',
    created_at: '2026-01-03T09:00:00Z',
    schema_version: 46,
    size_bytes: 2048,
    redacted: false,
  },
];

function mockBackend(overrides: {
  info?: Partial<Info>;
  integrityOk?: boolean;
  integrityMessage?: string;
  exportResult?: unknown;
  exportCancelled?: boolean;
  inspectResult?: unknown;
  inspectCancelled?: boolean;
  stageResult?: unknown;
  snapshots?: unknown[];
  failOn?: string;
} = {}) {
  const calls: Record<string, unknown[]> = {};
  // `pending_restore` is backend-owned state, not a per-call answer: staging a
  // restore flips it until the next launch consumes it. Modelling it here is
  // what lets the pane's post-stage refresh render the pending banner at all.
  const state = { pendingRestore: overrides.info?.pending_restore ?? false };
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    if (overrides.failOn === cmd) {
      return Promise.reject(new Error('backend exploded'));
    }
    switch (cmd) {
      case 'get_state_recovery_info':
        return Promise.resolve(baseInfo({ ...overrides.info, pending_restore: state.pendingRestore }));
      case 'list_state_snapshots':
        return Promise.resolve(overrides.snapshots ?? HEALTHY_SNAPSHOTS);
      case 'check_state_integrity':
        return Promise.resolve({
          ok: overrides.integrityOk ?? true,
          scope: args?.full ? 'full' : 'quick',
          checked_at: '2026-01-03T10:00:00Z',
          message:
            overrides.integrityMessage ??
            (overrides.integrityOk ?? true
              ? 'The database passed a quick check.'
              : 'Database damage found: page 4 is corrupt'),
        });
      case 'export_state':
        return Promise.resolve(
          overrides.exportCancelled === true
            ? null
            : overrides.exportResult ?? {
                path: 'C:/out/state.bmsnap',
                size_bytes: 4096,
                schema_version: 46,
                created_at: '2026-01-03T11:00:00Z',
                redacted: true,
                sections: ['state.db', 'preferences.json'],
                omitted: ['Provider API keys and account credentials (from preferences.json)'],
              },
        );
      case 'inspect_state_bundle':
        return Promise.resolve(
          overrides.inspectCancelled === true
            ? null
            : overrides.inspectResult ?? {
                bundle_path: 'C:/in/state.bmsnap',
                format_version: 1,
                schema_version: 40,
                created_at: '2025-12-01T00:00:00Z',
                redacted: true,
                rollback_snapshot: '',
                requires_restart: true,
                warnings: [
                  'This export has no credentials. After restarting you will need to re-enter your provider API keys, and Buildmesh will mint a new remote-access token — paired devices must sign in again.',
                ],
              },
        );
      case 'stage_state_restore':
        state.pendingRestore = true;
        return Promise.resolve(
          overrides.stageResult ?? {
            bundle_path: 'C:/in/state.bmsnap',
            format_version: 1,
            schema_version: 40,
            created_at: '2025-12-01T00:00:00Z',
            redacted: true,
            rollback_snapshot: 'C:/x/snapshots/20260103T120000Z-pre-restore.bmsnap',
            requires_restart: true,
            warnings: ['Buildmesh will restart to apply this.'],
          },
        );
      case 'cancel_state_restore':
        state.pendingRestore = false;
        return Promise.resolve(null);
      case 'open_in_file_manager':
        return Promise.resolve(undefined);
      case 'get_app_preferences':
        return Promise.resolve({ default_provider: null, minimax_api_key: null });
      case 'list_providers':
      case 'get_provider_accounts':
      case 'get_provider_meters':
      case 'list_device_sessions':
        return Promise.resolve([]);
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

async function openPane() {
  render(<AppSettingsModal open onClose={() => {}} />);
  await openSettingsPane('Data & Diagnostics');
  return screen.findByTestId('settings-data-recovery');
}

describe('Settings > Data & Diagnostics (issue #1537)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('shows the profile location, schema version, and retention policy', async () => {
    mockBackend();
    const pane = await openPane();
    // `findByText`, not `getByText`: the pane is lazily loaded and its own
    // data arrives in an effect after its root element is in the DOM, so the
    // location is not on screen the instant the pane appears.
    expect(
      await within(pane).findByText('C:/Users/test/AppData/Roaming/dev.buildmesh'),
    ).toBeTruthy();
    expect(within(pane).getByText(/newest 3 automatic snapshots/i)).toBeTruthy();
    expect(within(pane).getByText(/You currently have 2\./)).toBeTruthy();
  });

  it('runs the quick check and reports a healthy database as healthy', async () => {
    const calls = mockBackend();
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-quick-check'));
    const report = await within(pane).findByTestId('settings-data-recovery-report');
    expect(report.textContent).toMatch(/passed a quick check/i);
    expect(calls['check_state_integrity']?.[0]).toEqual({ full: false });
  });

  it('surfaces a failed integrity check as a failure, not a pass', async () => {
    mockBackend({ integrityOk: false, integrityMessage: 'Database damage found: page 4 is corrupt' });
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-full-check'));
    const report = await within(pane).findByTestId('settings-data-recovery-report');
    expect(report.textContent).toMatch(/damage found: page 4 is corrupt/i);
    // The failure must be visually distinct, not just different text.
    expect(report.className).toMatch(/text-status-error/);
  });

  it('defaults the export to leaving credentials out', async () => {
    const calls = mockBackend();
    const pane = await openPane();
    const checkbox = within(pane).getByTestId('settings-data-recovery-redact') as HTMLInputElement;
    expect(checkbox.checked).toBe(true);
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-export'));
    await waitFor(() => expect(calls['export_state']).toHaveLength(1));
    expect(calls['export_state']?.[0]).toEqual({ redacted: true });
  });

  it('tells the user what an export leaves out', async () => {
    mockBackend();
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-export'));
    const result = await within(pane).findByTestId('settings-data-recovery-export-result');
    // The omissions are rendered, not buried — this is the artefact most
    // likely to be sent to someone else.
    expect(result.textContent).toMatch(/Not included:.*credentials/i);
  });

  it('treats a cancelled export dialog as a no-op, not an error', async () => {
    mockBackend({ exportCancelled: true });
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-export'));
    await waitFor(() =>
      expect(vi.mocked(invoke).mock.calls.some(([c]) => c === 'export_state')).toBe(true),
    );
    expect(within(pane).queryByTestId('settings-data-recovery-error')).toBeNull();
    expect(within(pane).queryByTestId('settings-data-recovery-export-result')).toBeNull();
  });

  it('does not offer the destructive restore action until a bundle has been read', async () => {
    const calls = mockBackend();
    const pane = await openPane();
    expect(within(pane).queryByTestId('settings-data-recovery-stage')).toBeNull();

    await userEvent.click(within(pane).getByTestId('settings-data-recovery-inspect'));
    await within(pane).findByTestId('settings-data-recovery-plan');
    // Reading a bundle must not stage it.
    expect(calls['stage_state_restore']).toBeUndefined();
    expect(within(pane).getByTestId('settings-data-recovery-stage')).toBeTruthy();
  });

  it('warns that a credential-free export costs a re-auth on restore', async () => {
    mockBackend();
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-inspect'));
    const plan = await within(pane).findByTestId('settings-data-recovery-plan');
    expect(plan.textContent).toMatch(/re-enter your provider API keys/i);
    expect(plan.textContent).toMatch(/must restart/i);
  });

  it('stages a restore on the second click and says a restart is required', async () => {
    const calls = mockBackend();
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-inspect'));
    await within(pane).findByTestId('settings-data-recovery-stage');
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-stage'));

    await waitFor(() => expect(calls['stage_state_restore']).toHaveLength(1));
    // Staging flips the pane into the pending state, which says out loud that
    // nothing has changed yet.
    const pending = await within(pane).findByTestId('settings-data-recovery-pending');
    expect(pending.textContent).toMatch(/next time Buildmesh starts/i);
  });

  it('offers a way to cancel a staged restore', async () => {
    mockBackend();
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-inspect'));
    await within(pane).findByTestId('settings-data-recovery-stage');
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-stage'));
    await within(pane).findByTestId('settings-data-recovery-pending');

    await userEvent.click(within(pane).getByTestId('settings-data-recovery-cancel-restore'));
    await waitFor(() => expect(vi.mocked(invoke).mock.calls.some(([c]) => c === 'cancel_state_restore')).toBe(true));
  });

  it('shows a startup recovery notice and where the preserved copy is', async () => {
    mockBackend({
      info: {
        notice: {
          severity: 'warning',
          message: 'Buildmesh could not take a consistent snapshot of its database. A raw copy was preserved instead.',
          snapshot_path: 'C:/x/snapshots/20260103T120000Z-manual-raw.bmsnap',
          recorded_at: '2026-01-03T12:00:00Z',
        },
      },
    });
    const pane = await openPane();
    const notice = await within(pane).findByTestId('settings-data-recovery-notice');
    expect(notice.textContent).toMatch(/raw copy was preserved/i);
    expect(notice.textContent).toContain('manual-raw.bmsnap');
  });

  it('reports a rejected bundle instead of silently doing nothing', async () => {
    mockBackend({ failOn: 'inspect_state_bundle' });
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-inspect'));
    const error = await within(pane).findByTestId('settings-data-recovery-error');
    expect(error.textContent).toMatch(/backend exploded/);
  });

  it('opens the data folder through the shared OS-open seam', async () => {
    const calls = mockBackend();
    const pane = await openPane();
    await userEvent.click(within(pane).getByTestId('settings-data-recovery-open-folder'));
    await waitFor(() => expect(calls['open_in_file_manager']).toHaveLength(1));
    expect(calls['open_in_file_manager']?.[0]).toEqual({
      path: 'C:/Users/test/AppData/Roaming/dev.buildmesh',
    });
  });

  it('lists existing snapshots with their kind and size', async () => {
    mockBackend();
    const pane = await openPane();
    const list = await within(pane).findByTestId('settings-data-recovery-snapshot-list');
    expect(list.textContent).toMatch(/manual/);
    expect(list.textContent).toMatch(/v46/);
    expect(list.textContent).toMatch(/2 KB/);
  });
});
