/**
 * The one IPC **event** transport (issue #2833).
 *
 * ## Why this module exists
 *
 * Commands already have a chokepoint — `src/lib/tauri/_invoke.ts` owns the raw
 * `invoke` and every typed facet routes through it (ADR-0010). Events had no
 * equivalent: seventeen modules imported `listen` straight from
 * `@tauri-apps/api/event`, so there was no single place that could answer
 * "how many IPC events did this window receive, and of which kind?".
 *
 * The renderer health line (issue #2833) needs exactly that answer, so this
 * module becomes the one owner of the event transport. It delegates to
 * Tauri's `listen` and wraps the handler, counting each **delivered** event
 * by name.
 *
 * Counting at delivery — not at registration — is the point: a stream that
 * subscribed and then received nothing must not report traffic, and a stream
 * that received a thousand events must report a thousand.
 *
 * ## The call shape is unchanged
 *
 * `listen(event, handler)` here has the same signature and the same
 * semantics as Tauri's, including the `Options` argument, so a caller cannot
 * tell the difference. The existing tests that assert on the mocked
 * `listen` — `tests/unit/live-event-streams.test.ts`,
 * `tests/unit/agent-node-listeners.test.ts`,
 * `tests/unit/circuits-probe-tab.test.tsx` — keep working, because this
 * module calls the mocked `listen` with the same argument list, down to the
 * number of arguments.
 *
 * ## Dynamic event names
 *
 * Some events are per-session (`build-run-output-<sessionId>`). Counting
 * those verbatim would grow the counter table without bound over a long
 * session, so the trailing id is folded to `<id>` — the *shape* of the event
 * is what the health line reports, not which session it belonged to.
 */

import { listen as tauriListen } from '@tauri-apps/api/event';
import type { Event, EventName, Options, UnlistenFn } from '@tauri-apps/api/event';

/**
 * The Tauri event types, re-exported so a caller needs one import for the
 * event transport. The event *transport* is owned by this module (see the
 * ESLint restriction in `eslint.config.js`), so the types travel with it.
 */
export type { Event, EventName, Options, UnlistenFn };

/**
 * A trailing uuid or plain number is a per-instance id, not part of the
 * event's identity. Folding it keeps the counter table bounded.
 */
const UUID_SUFFIX = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const NUMERIC_SUFFIX = /\d+$/;

/**
 * Fold a per-instance event name onto the shape it belongs to.
 *
 * `build-run-output-42` and `build-run-output-43` are the same stream with a
 * different session; reporting them as one key is what keeps the top-5 list
 * meaningful instead of a list of one-off sessions.
 */
export function normalizeEventName(name: string): string {
  if (UUID_SUFFIX.test(name)) return `${name.slice(0, name.length - 36)}<id>`;
  if (NUMERIC_SUFFIX.test(name)) return `${name.slice(0, name.length - name.match(NUMERIC_SUFFIX)![0].length)}<id>`;
  return name;
}

const eventCounts = new Map<string, number>();

/**
 * Count one delivered IPC event.
 *
 * Exported for the sampler and for tests; production call sites go through
 * [`listen`] below.
 */
export function recordIpcEvent(name: string): void {
  const key = normalizeEventName(name);
  eventCounts.set(key, (eventCounts.get(key) ?? 0) + 1);
}

/**
 * Read the per-name counts, highest first, and clear them.
 *
 * The sampler calls this once per line: the line reports the events received
 * *since the last line*.
 */
export function readAndResetIpcEventCounts(): Array<{ name: string; count: number }> {
  const entries = [...eventCounts.entries()]
    .map(([name, count]) => ({ name, count }))
    // Ties break on the name so two runs of the same session produce the same
    // line — a diagnostic that reorders itself between samples is harder to
    // diff than one that does not.
    .sort((a, b) => (b.count - a.count) || a.name.localeCompare(b.name));
  eventCounts.clear();
  return entries;
}

/** Test-only: clear the counter table. */
export function resetIpcEventCounts(): void {
  eventCounts.clear();
}

/**
 * Subscribe to a backend event.
 *
 * A thin wrapper over Tauri's `listen` that counts what it delivers. The
 * handler is called with the untouched event, in the original order, and a
 * throwing handler still propagates — the counter must not change how the
 * app reacts to an event.
 */
export async function listen<T>(
  event: EventName,
  handler: (event: Event<T>) => void,
  options?: Options,
): Promise<UnlistenFn> {
  const counted = (received: Event<T>): void => {
    recordIpcEvent(event);
    handler(received);
  };
  // The call shape is preserved exactly, down to the argument count: a
  // caller that passes no options must produce a two-argument call. Tests
  // assert on the whole argument list (`toHaveBeenCalledWith(name,
  // expect.any(Function))`), and a trailing `undefined` is a third
  // argument to them.
  return options === undefined
    ? tauriListen<T>(event, counted)
    : tauriListen<T>(event, counted, options);
}
