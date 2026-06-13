/**
 * Decode a base64 string from a Tauri event payload into raw bytes.
 *
 * Tauri events that carry binary data (PTY output, build-run output, future
 * log-tail streams) ship it base64-encoded as a JSON-safe `string`. We can't
 * just hand the string to xterm — xterm wants a `Uint8Array` (it then runs
 * the bytes through its UTF-8 decoder). `atob` + `charCodeAt` round-trips
 * the bytes losslessly; an async `fetch('data:...')` round-trip would also
 * work but adds a microtask hop per event.
 *
 * Lives in `src/lib/` (not next to Terminal) because the contract is the
 * Tauri-event-payload side, not the xterm side — any listener that consumes
 * a base64 byte payload wants this, regardless of what the bytes feed into.
 */
export function decodeBase64Bytes(data: string): Uint8Array {
  const binary = globalThis.atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}
