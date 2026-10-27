import { expect } from '@playwright/test';

/**
 * The idle animation gate, checked in a real browser (issue #2599).
 *
 * The unit test proves the GATE is wired to the right elements; this proves
 * the CSS actually reaches them. That is not a formality: Tailwind's
 * `animate-pulse` / `animate-spin` utilities live in the `utilities` cascade
 * layer, so a gate rule that only wins on paper would leave the compositor
 * producing frames exactly as before — and jsdom never applies Tailwind, so
 * no unit test can catch it.
 *
 * The window focus is driven through the same `tauri://blur` event the real
 * app receives when the user alt-tabs away.
 */
export default async function ({ page, mock }) {
  // The classes under test, one element each. The Tailwind utilities are
  // INJECTED rather than sampled from the screen: the default fixtures
  // happen to render no spinner, and the risk this test exists for is
  // specifically that a utility in a cascade layer is not reached.
  const CLASSES = ['animate-pulse-fast', 'animate-pulse', 'animate-spin', 'animate-fade-in'];

  const readState = () =>
    page.evaluate((classes) => {
      const root = document.documentElement;
      const samples = {};
      for (const className of classes) {
        const element = document.createElement('div');
        element.className = className;
        document.body.appendChild(element);
        samples[className] = getComputedStyle(element).animationPlayState;
        element.remove();
      }
      const onScreen = document.querySelector('.animate-pulse-fast');
      return {
        marker: root.getAttribute('data-window-idle'),
        samples,
        onScreenPlayState: onScreen ? getComputedStyle(onScreen).animationPlayState : null,
      };
    }, CLASSES);

  await page.locator('.animate-pulse-fast').first().waitFor({ state: 'attached', timeout: 15000 });

  // Focused: the gate is off and the animations run.
  const focused = await readState();
  expect(focused.marker).toBeNull();
  expect(focused.onScreenPlayState).toBe('running');
  for (const className of CLASSES) {
    expect(focused.samples[className], `${className} must run while the window is focused`).toBe('running');
  }

  // The window lost focus: the gate marks the root and the infinite
  // animations stop — including the Tailwind utilities, which live in a
  // cascade layer of their own — while the one-shot entrance animation keeps
  // running, so a toast that arrives in the background is still seen.
  const delivered = await mock.emit('tauri://blur');
  expect(delivered).toBeGreaterThan(0);
  await expect
    .poll(async () => (await readState()).marker)
    .toBe('true');

  const idle = await readState();
  expect(idle.samples['animate-pulse-fast']).toBe('paused');
  expect(idle.samples['animate-pulse']).toBe('paused');
  expect(idle.samples['animate-spin']).toBe('paused');
  expect(idle.samples['animate-fade-in']).toBe('running');

  // The window came back: the gate is off again and the animations resume.
  await mock.emit('tauri://focus');
  await expect
    .poll(async () => (await readState()).marker)
    .toBeNull();

  const refocused = await readState();
  expect(refocused.onScreenPlayState).toBe('running');
  expect(refocused.samples['animate-pulse']).toBe('running');

  console.log(
    'PASS: the idle gate pauses the infinite animations (project classes and Tailwind utilities) while the window is unfocused, leaves the entrance animations alone, and restores the running state when the window comes back.',
  );
}
