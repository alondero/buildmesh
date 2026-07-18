/**
 * `formatError` — normalise an unknown thrown/rejected value into
 * user-facing copy WITHOUT the `"Error: "` prefix (issue #663).
 *
 * Why this exists
 * ---------------
 * Tauri 2's `invoke` rejects with a JS `Error` whose `.message` is the
 * Rust `Result::Err` string. The codebase historically stringified those
 * rejections with `String(e)`, but `String(errorInstance)` yields
 * `"Error: " + e.message` — so every inline error banner and toast showed
 * users `"Prune failed: Error: fatal: not a git repository"` instead of
 * `"Prune failed: fatal: not a git repository"`.
 *
 * `String(e)` behaves differently per type:
 *   - string → returned verbatim
 *   - Error  → `"Error: " + e.message`
 * so this helper unwraps `e.message` for real `Error` instances and falls
 * back to `String(e)` for everything else (strings, objects, null, …).
 *
 * An `Error` with an empty `message` falls back to `String(e)` too, which
 * yields the bare word `"Error"` — a last-resort label is friendlier than
 * a blank banner.
 */
export function formatError(e: unknown): string {
  if (e instanceof Error && e.message) {
    return e.message;
  }
  return String(e);
}
