import { listen as tauriListen } from '@tauri-apps/api/event';
import type { Event, EventName, Options, UnlistenFn } from '@tauri-apps/api/event';

export type { Event, EventName, Options, UnlistenFn };

const UUID_SUFFIX = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const NUMERIC_SUFFIX = /\d+$/;

export function normalizeEventName(name: string): string {
  if (UUID_SUFFIX.test(name)) return `${name.slice(0, name.length - 36)}<id>`;
  if (NUMERIC_SUFFIX.test(name)) return `${name.slice(0, name.length - name.match(NUMERIC_SUFFIX)![0].length)}<id>`;
  return name;
}

const eventCounts = new Map<string, number>();

export function recordIpcEvent(name: string): void {
  const key = normalizeEventName(name);
  eventCounts.set(key, (eventCounts.get(key) ?? 0) + 1);
}

export function readAndResetIpcEventCounts(): Array<{ name: string; count: number }> {
  const entries = [...eventCounts.entries()]
    .map(([name, count]) => ({ name, count }))
    .sort((a, b) => (b.count - a.count) || a.name.localeCompare(b.name));
  eventCounts.clear();
  return entries;
}

export function resetIpcEventCounts(): void {
  eventCounts.clear();
}

export async function listen<T>(
  event: EventName,
  handler: (event: Event<T>) => void,
  options?: Options,
): Promise<UnlistenFn> {
  const counted = (received: Event<T>): void => {
    recordIpcEvent(event);
    handler(received);
  };
  return options === undefined
    ? tauriListen<T>(event, counted)
    : tauriListen<T>(event, counted, options);
}
