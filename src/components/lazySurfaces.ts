/**
 * Lazy surfaces — the desktop surfaces that are only ever mounted on demand
 * but whose code is large enough that shipping it in the entry chunk taxes
 * every launch.
 *
 * Why this module exists
 * ----------------------
 * Each surface below is *conditionally rendered* (a modal the user opens, an
 * editor behind a flag), so the browser only needs its code once the user
 * asks for it. A static import ignores that: the bundler inlines the surface
 * and everything it reaches into the entry chunk, where it is parsed and
 * executed on every cold start whether or not the user ever opens it.
 *
 * The measured cost of the three surfaces that were static imports:
 *
 *   - the circuit canvas editor pulls `@xyflow/react`, `@xyflow/system`, the
 *     `d3-*` family and `@dagrejs/dagre` — ~217 kB minified, plus ~16 kB of
 *     React Flow CSS in the entry stylesheet;
 *   - the settings modal carries the whole settings surface — ~105 kB;
 *   - the remote-access modal carries the `qrcode` encoder — ~37 kB.
 *
 * Rule for this module: a surface belongs here when it is (a) mounted
 * conditionally, and (b) big enough that deferring it is worth a chunk
 * boundary. A surface that is mounted on first paint, or that is small, does
 * not belong here — a lazy boundary costs a round trip and a `Suspense`
 * fallback, and the fallback is a visible blank.
 *
 * Every loader is also reachable by name through `preloadLazySurfaces`, so a
 * test can await the chunk before it makes a synchronous assertion. This
 * mirrors the Probe tab registry's `preload` seam (`src/lib/probeContext.ts`);
 * a `React.lazy` surface is async UI, so a test that renders it must wait for
 * the content (`findBy*`) or preload the chunk first. See
 * `docs/agents/engineering-reference.md` ("Async UI, refresh, polling").
 */
import { lazy } from 'react';
import type { CircuitEditorOverlay } from './Circuits/CircuitEditorOverlay';
import type { AppSettingsModal } from './AppSettings/AppSettingsModal';
import type { RemoteAccessModal } from './RemoteAccess/RemoteAccessModal';

/** The circuit canvas editor overlay (issue #1209). */
export function loadCircuitEditorOverlay(): Promise<{ default: typeof CircuitEditorOverlay }> {
  return import('./Circuits/CircuitEditorOverlay').then((m) => ({ default: m.CircuitEditorOverlay }));
}

/** The app settings modal. */
export function loadAppSettingsModal(): Promise<{ default: typeof AppSettingsModal }> {
  return import('./AppSettings/AppSettingsModal').then((m) => ({ default: m.AppSettingsModal }));
}

/** The remote-access (LAN / phone pairing) modal. */
export function loadRemoteAccessModal(): Promise<{ default: typeof RemoteAccessModal }> {
  return import('./RemoteAccess/RemoteAccessModal').then((m) => ({ default: m.RemoteAccessModal }));
}

export const LazyCircuitEditorOverlay = lazy(loadCircuitEditorOverlay);
export const LazyAppSettingsModal = lazy(loadAppSettingsModal);
export const LazyRemoteAccessModal = lazy(loadRemoteAccessModal);

/**
 * The lazy surfaces this module owns, by name.
 *
 * A test that needs one of them preloads it by name, so it does not have to
 * load the whole set — the circuit editor's chunk carries React Flow, d3 and
 * dagre, and a test that only opens the settings modal has no business
 * paying for it.
 */
export const LAZY_SURFACE_LOADERS = {
  circuitEditor: loadCircuitEditorOverlay,
  appSettings: loadAppSettingsModal,
  remoteAccess: loadRemoteAccessModal,
} as const;

/** The name of a lazy surface this module owns. */
export type LazySurfaceName = keyof typeof LAZY_SURFACE_LOADERS;

/**
 * Load the named lazy surfaces' chunks.
 *
 * Tests call this before a synchronous assertion on a lazily-mounted surface,
 * so the assertion does not depend on how fast the dynamic import happens to
 * resolve. It never throws for a surface the test does not use — a rejected
 * import only rejects this promise, and the caller can ignore it.
 *
 * The name list is required and non-empty: a call with no name would resolve
 * without loading anything, which reads like "preload them all" and is not
 * what it does.
 *
 * This mirrors the Probe tab registry's `preload` seam
 * (`src/lib/probeContext.ts`); a `React.lazy` surface is async UI, so a test
 * that renders it must wait for the content (`findBy*`) or preload the chunk
 * first. See `docs/agents/engineering-reference.md` ("Async UI, refresh,
 * polling").
 */
export function preloadLazySurfaces(
  ...names: [LazySurfaceName, ...LazySurfaceName[]]
): Promise<unknown> {
  return Promise.all(names.map((name) => LAZY_SURFACE_LOADERS[name]()));
}
