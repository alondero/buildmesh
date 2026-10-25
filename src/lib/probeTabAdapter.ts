import { lazy, type ComponentType } from 'react';
import type { ProbeLoader } from './probeResource';

/** Keep the chunk import, component and read together in one registry entry. */
export function probeTabAdapter<T>(importTab: () => Promise<{
  Component: ComponentType;
  load: ProbeLoader<T>;
}>, sharedRead?: ProbeLoader<T>) {
  // Cache the import for both React.lazy and test preloading. A rejected import
  // is retryable rather than permanently poisoning future preloads/reads.
  let pending: ReturnType<typeof importTab> | undefined;
  let loaded: Awaited<ReturnType<typeof importTab>> | undefined;
  const preload = () => pending ??= importTab().then(tab => {
    loaded = tab;
    return tab;
  }).catch(cause => {
    pending = undefined;
    throw cause;
  });
  return {
    Component: lazy(async () => ({ default: (await preload()).Component })),
    preload,
    load: ((context, signal) => {
      if (loaded) return loaded.load(context, signal);
      if (sharedRead) return sharedRead(context, signal);
      return preload().then(tab => {
        signal.throwIfAborted();
        return tab.load(context, signal);
      });
    }) satisfies ProbeLoader<T>,
  };
}
