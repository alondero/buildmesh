import { test, expect } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const configurationName = 'Claude Code with a very long default launch configuration name';

for (const width of [192, 240, 256, 480]) {
  test(`sidebar spawn stays under the pointer at ${width}px`, async ({ page }) => {
    await page.addInitScript({ content: buildInitScript({
      ...defaultFixtures,
      list_meshes: [defaultFixtures.list_meshes[0]],
      list_agent_nodes: [],
      list_providers: [{
        id: 'long-configuration', label: configurationName, harness_id: 'anthropic',
        group_key: 'anthropic', provider_id: null, is_proxied: false,
        icon: 'anthropic', color: '#00d4ff', runtime: 'windows',
      }],
      get_default_provider: 'long-configuration',
    }) });
    await page.addInitScript(sidebarWidth => {
      localStorage.setItem('buildmesh.sidebar-width', String(sidebarWidth));
    }, width);
    await page.goto('/');
    const card = page.locator('[data-mesh-card="1"]');
    const spawn = card.getByTestId('spawn-default');
    const picker = card.getByTestId('spawn-dropdown-toggle');
    await expect(spawn).toHaveAttribute('title', `Add agent node (${configurationName})`);
    await page.mouse.move(1000, 700);

    const before = await spawn.boundingBox();
    if (!before) throw new Error('Missing spawn button bounds');
    const cardBefore = await card.boundingBox();
    if (!cardBefore) throw new Error('Missing mesh card bounds');
    const sampleFrames = () => spawn.evaluate(async element => {
      const samples = [];
      for (let frame = 0; frame < 24; frame++) {
        await new Promise(requestAnimationFrame);
        samples.push({ y: element.getBoundingClientRect().y, hovered: element.matches(':hover') });
      }
      return samples;
    });
    // The top edge loses hover as soon as a wrapping subtitle moves the button.
    await page.mouse.move(before.x + before.width / 2, before.y + 1);
    const frames = await sampleFrames();
    expect(frames.every(frame => frame.hovered), 'stationary pointer must retain hover during expansion').toBe(true);
    expect(frames.every(frame => Math.abs(frame.y - before.y) < 1), 'spawn button must not move vertically').toBe(true);
    expect((await card.boundingBox())?.height).toBe(cardBefore.height);

    const status = card.getByText('No agents yet', { exact: true });
    expect(await status.evaluate(element => {
      return element.getBoundingClientRect().height <= parseFloat(getComputedStyle(element).lineHeight);
    }), 'empty status must occupy one line').toBe(true);
    const pickerBounds = await picker.boundingBox();
    if (!pickerBounds) throw new Error('Missing provider picker bounds');
    expect(pickerBounds.width).toBeGreaterThanOrEqual(24);
    expect(pickerBounds.x + pickerBounds.width).toBeLessThanOrEqual(cardBefore.x + cardBefore.width);
    expect((await spawn.boundingBox())?.width).toBeGreaterThan(before.width);

    await page.mouse.move(1000, 700);
    await expect.poll(async () => (await spawn.boundingBox())?.width).toBe(before.width);
    await picker.focus();
    await page.keyboard.press('Shift+Tab');
    await expect(spawn).toBeFocused();
    expect(await spawn.evaluate(element => element.matches(':focus-visible'))).toBe(true);
    const focusFrames = await sampleFrames();
    expect(focusFrames.every(frame => Math.abs(frame.y - before.y) < 1), 'keyboard expansion must not move the button vertically').toBe(true);
    expect((await spawn.boundingBox())?.width).toBeGreaterThan(before.width);
    expect((await card.boundingBox())?.height).toBe(cardBefore.height);
    await expect(spawn).toHaveAccessibleName(`Add agent node (${configurationName})`);
    await page.keyboard.press('Tab');
    await expect(picker).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(picker).toHaveAttribute('aria-expanded', 'true');
    await page.keyboard.press('Escape');
    await expect(picker).toHaveAttribute('aria-expanded', 'false');
    await expect(picker).toBeFocused();
  });
}
