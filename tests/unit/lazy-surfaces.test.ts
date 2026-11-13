import { describe, expect, it } from 'vitest';

import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { CircuitEditorOverlay } from '../../src/components/Circuits/CircuitEditorOverlay';
import { RemoteAccessModal } from '../../src/components/RemoteAccess/RemoteAccessModal';
import {
  LAZY_SURFACE_LOADERS,
  loadAppSettingsModal,
  loadCircuitEditorOverlay,
  loadRemoteAccessModal,
  preloadLazySurfaces,
} from '../../src/components/lazySurfaces';

/**
 * The lazy surfaces own their own chunk, so the only thing that can break
 * quietly is the loader: a renamed export resolves to `undefined` and the
 * surface renders nothing, with no failure until a user opens it. Each loader
 * must therefore resolve to exactly the component the eager import would have
 * given.
 */
describe('lazy surfaces', () => {
  it('resolves the circuit editor overlay to its exported component', async () => {
    await expect(loadCircuitEditorOverlay()).resolves.toEqual({
      default: CircuitEditorOverlay,
    });
  });

  it('resolves the settings modal to its exported component', async () => {
    await expect(loadAppSettingsModal()).resolves.toEqual({
      default: AppSettingsModal,
    });
  });

  it('resolves the remote-access modal to its exported component', async () => {
    await expect(loadRemoteAccessModal()).resolves.toEqual({
      default: RemoteAccessModal,
    });
  });

  it('maps each surface name to that surface’s loader', () => {
    expect(LAZY_SURFACE_LOADERS.circuitEditor).toBe(loadCircuitEditorOverlay);
    expect(LAZY_SURFACE_LOADERS.appSettings).toBe(loadAppSettingsModal);
    expect(LAZY_SURFACE_LOADERS.remoteAccess).toBe(loadRemoteAccessModal);
  });

  it('preloads the named surfaces', async () => {
    await expect(preloadLazySurfaces('appSettings', 'remoteAccess')).resolves.toBeDefined();
  });
});
