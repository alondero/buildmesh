/**
 * Shared mock of Tauri 2's `Channel` for tests that re-mock
 * `@tauri-apps/api/core`. One class, imported by every factory — do not
 * paste a new `class Channel { onmessage = ... }` into each test file.
 */
export class MockChannel<T = unknown> {
  onmessage: (message: T) => void = () => {};
  constructor(handler?: (message: T) => void) {
    if (handler) this.onmessage = handler;
  }
}
