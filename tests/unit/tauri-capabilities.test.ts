/**
 * Capabilities configuration guard.
 *
 * Tauri v2 requires explicit capability permissions in `capabilities/*.json`
 * for IPC commands invoked from the frontend. In particular, a frameless
 * window's controls and WindowCloseGuard's exit confirmation require explicit
 * `core:window:allow-*` permissions (including `allow-destroy` to force window
 * destruction on exit confirmation without re-triggering `onCloseRequested`).
 */
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

interface CapabilityConfig {
  identifier: string;
  windows: string[];
  permissions: string[];
}

const defaultCaps = JSON.parse(
  readFileSync(resolve(process.cwd(), 'src-tauri/capabilities/default.json'), 'utf8'),
) as CapabilityConfig;

describe('src-tauri/capabilities/default.json', () => {
  it('targets the main window', () => {
    expect(defaultCaps.windows).toContain('main');
  });

  it('includes core:window:allow-destroy for exit confirmation', () => {
    expect(defaultCaps.permissions).toContain('core:window:allow-destroy');
  });

  it('includes all required window control permissions for bespoke titlebar chrome', () => {
    const required = [
      'core:window:allow-minimize',
      'core:window:allow-close',
      'core:window:allow-destroy',
      'core:window:allow-toggle-maximize',
      'core:window:allow-internal-toggle-maximize',
      'core:window:allow-is-maximized',
      'core:window:allow-start-dragging',
    ];
    for (const perm of required) {
      expect(
        defaultCaps.permissions,
        `capabilities/default.json missing critical window permission "${perm}"`,
      ).toContain(perm);
    }
  });
});
