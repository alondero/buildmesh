/**
 * Shape serializer for IPC args.
 *
 * Returns a sanitized structural sketch of a Tauri command's args object —
 * the key names, the *type* of each value, and bounded counts (char count
 * for strings, item count for arrays, byte count for buffers). The actual
 * values are never emitted. This is the format we log alongside a failed
 * `invoke` so the backend log can be triaged without dumping PII or
 * multi-megabyte scrollback into `buildmesh.log`.
 *
 * Why not the raw object: a renamed/removed command produces an "argument
 * mismatch" failure that often *is* the diagnostic — knowing the shape
 * (which keys, in what order) tells you that. A single command's payload
 * containing an API key (`set_minimax_api_key`) or a terminal scrollback
 * (`submit_terminal_snapshot`) can be tens of kilobytes to megabytes and
 * contains user-typed secrets, so values stay out.
 *
 * Pure: no I/O, no dependencies. Imported by the central `invoke` wrapper
 * in `src/lib/tauri.ts` and unit-tested directly.
 */

const TRUNCATE_KEYS = 80;
const MAX_KEYS_IN_NESTED = 8;

function truncateKey(k: string): string {
  return k.length > TRUNCATE_KEYS ? k.slice(0, TRUNCATE_KEYS) + '…' : k;
}

function describe(value: unknown): string {
  if (value === null) return 'null';
  if (value === undefined) return 'undefined';
  if (typeof value === 'string') {
    return value.length === 0 ? 'empty' : `${value.length} chars`;
  }
  // Primitives (number/boolean/bigint) are reported as their TYPE, not
  // their value — a sessionId of 7 in the log line is the same as the
  // sessionId in a wrapper's signature and gives the reader enough
  // context without leaking the (often user-derived) value.
  const t = typeof value;
  if (t === 'number' || t === 'boolean' || t === 'bigint') return t;
  if (t === 'function' || t === 'symbol') return t;
  if (Array.isArray(value)) return value.length === 0 ? 'empty' : `${value.length} items`;
  if (value instanceof Uint8Array) return `${value.byteLength} bytes`;
  if (value instanceof ArrayBuffer) return `${value.byteLength} bytes`;
  if (t === 'object') {
    const keys = Object.keys(value as object);
    if (keys.length === 0) return '{}';
    return '{ ' + keys.slice(0, MAX_KEYS_IN_NESTED).map(truncateKey).join(', ') +
      (keys.length > MAX_KEYS_IN_NESTED ? ', …' : '') + ' }';
  }
  return t;
}

/**
 * Render a Tauri command's args object as a structural sketch. Always
 * returns a plain `Record` so the wrapper can `JSON.stringify` it for
 * the log line. `undefined` / `null` are treated as "no args" and yield
 * `{}` — that way the log line is always present, even for zero-arg
 * commands.
 */
export function shapeArgs(args: unknown): Record<string, string> {
  if (args === undefined || args === null) return {};
  if (typeof args !== 'object' || Array.isArray(args)) {
    // Non-object args (e.g. a bare array, a string) — surface under a
    // synthetic key so we never silently drop the payload shape.
    return { _value: describe(args) };
  }
  const obj = args as Record<string, unknown>;
  const out: Record<string, string> = {};
  for (const k of Object.keys(obj)) out[truncateKey(k)] = describe(obj[k]);
  return out;
}
