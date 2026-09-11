/**
 * BuildRunTerminalRegistry × FontSizeManager integration.
 *
 * Peer of the "BuildRunTerminalRegistry.applyTheme" block in
 * `theme-toggle.test.ts` (issue #734): that block pins the theme fan-out for
 * build/run panes; this file pins the *font-size* fan-out added for the
 * title-bar zoom slider. Without an explicit test here, deleting the
 * `this.fontSizeManager.register(...)` call in `doCreate` leaves the whole
 * suite green while build/run terminals silently stop responding to zoom —
 * exactly the pre-existing gap this integration closes.
 *
 * Uses an isolated `new BuildRunTerminalRegistry()` (not the module
 * singleton) so each test owns its instances and teardown. The global
 * vitest.setup mocks supply the xterm Terminal, FitAddon, and Tauri IPC the
 * registry needs; jsdom only lacks ResizeObserver.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { BuildRunTerminalRegistry } from '../../src/components/Terminal/BuildRunTerminalRegistry';
import {
  TERMINAL_FONT_SIZE_DEFAULT,
  setTerminalFontSize,
} from '../../src/components/Terminal/terminalConfig';

// jsdom doesn't implement ResizeObserver; TerminalResizeScheduler constructs
// one on attach. A no-op stub is enough — the resize observation is
// irrelevant to font-size propagation.
const originalResizeObserver = globalThis.ResizeObserver;

class MockResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

describe('BuildRunTerminalRegistry — live font-size propagation', () => {
  let registry: BuildRunTerminalRegistry;

  beforeEach(() => {
    globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;
    // The font size is a module-level singleton; start each test from the
    // default so `setTerminalFontSize` below is a genuine change.
    setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT);
    registry = new BuildRunTerminalRegistry();
  });

  afterEach(() => {
    registry.destroy();
    setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT);
    globalThis.ResizeObserver = originalResizeObserver;
  });

  it('applies a global font-size change to an attached pane and refits it', async () => {
    const inst = await registry.attach(41, 'build', false, document.createElement('div'));
    expect(inst).not.toBeNull();
    const fitCallsBefore = (inst as { fitAddon: { fit: { mock: { calls: unknown[] } } } })
      .fitAddon.fit.mock.calls.length;

    setTerminalFontSize(16);

    expect(inst!.term.options.fontSize).toBe(16);
    // measureAndFit ran for this pane (the scheduler's own attach-time fit
    // may also have fired, so assert "at least one more", not "exactly one").
    const fitCallsAfter = (inst as { fitAddon: { fit: { mock: { calls: unknown[] } } } })
      .fitAddon.fit.mock.calls.length;
    expect(fitCallsAfter).toBeGreaterThan(fitCallsBefore);
  });

  it('propagates to every attached build/run pane', async () => {
    const inst1 = await registry.attach(51, 'build', false, document.createElement('div'));
    const inst2 = await registry.attach(52, 'run', true, document.createElement('div'));
    expect(inst1).not.toBeNull();
    expect(inst2).not.toBeNull();

    setTerminalFontSize(17);

    expect(inst1!.term.options.fontSize).toBe(17);
    expect(inst2!.term.options.fontSize).toBe(17);
  });

  it('unregisters a disposed pane from the font-size fan-out', async () => {
    const inst = await registry.attach(61, 'build', false, document.createElement('div'));
    expect(inst).not.toBeNull();
    const options = inst!.term.options;

    // Prove the pane was registered in the first place (otherwise the
    // post-dispose assertion below would pass for the wrong reason).
    setTerminalFontSize(14);
    expect(options.fontSize).toBe(14);

    await registry.dispose(61, 'build', false);
    expect(registry.getInstance(61, 'build', false)).toBeUndefined();

    // A disposed xterm must not receive the next flip — a future X-button
    // close that forgot the unregister would leave it written into after
    // disposal. The options object is retained here, so this distinguishes
    // "unregistered" from "reached the dead terminal".
    setTerminalFontSize(18);

    expect(options.fontSize).toBe(14);
  });
});
