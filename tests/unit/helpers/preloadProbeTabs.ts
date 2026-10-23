import { PROBE_TAB_DEFINITIONS } from '../../../src/lib/probeContext';

/** Load the registry's actual chunks before synchronous tab assertions. */
export async function preloadProbeTabs(): Promise<void> {
  await Promise.all(Object.values(PROBE_TAB_DEFINITIONS).map(tab => tab.preload()));
}
