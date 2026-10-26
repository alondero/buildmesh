import { test, expect } from '@playwright/test';

test.use({ viewport: { width: 390, height: 760 }, hasTouch: true, isMobile: true });

for (const surface of ['browser', 'android']) {
  test(`${surface} terminal opens plain and OSC 8 URLs once per touch`, async ({ page }) => {
    await page.route('**/api/ws-ticket', route => route.fulfill({ json: { ticket: 'fixture' } }));
    await page.goto(`/tests/e2e/fixtures/mobile-terminal-links.html?surface=${surface}`);
    const rows = page.locator('.xterm-rows > div');
    const plain = rows.filter({ hasText: 'https://example.com/plain' });
    const hyperlink = rows.filter({ hasText: 'Open browser' });
    await expect(plain).toBeVisible();
    await expect(hyperlink).toBeVisible();
    const tapRow = async (row: typeof plain) => {
      const box = (await row.boundingBox())!;
      await page.touchscreen.tap(box.x + 5, box.y + box.height / 2);
    };
    await tapRow(plain);
    await expect.poll(() => page.evaluate('window.openedLinks')).toEqual(['https://example.com/plain']);
    await tapRow(plain);
    await tapRow(hyperlink);
    await expect.poll(() => page.evaluate('window.openedLinks')).toEqual([
      'https://example.com/plain', 'https://example.com/plain', 'https://example.com/hyper',
    ]);
    expect(await page.evaluate('window.ptyInput.filter(data => data.startsWith("\\x1b[<"))')).toEqual([]);
    expect(page.url()).toContain('mobile-terminal-links.html');
  });

  test(`${surface} terminal ignores drags, cancelled touches and non-links`, async ({ page }) => {
    await page.route('**/api/ws-ticket', route => route.fulfill({ json: { ticket: 'fixture' } }));
    await page.goto(`/tests/e2e/fixtures/mobile-terminal-links.html?surface=${surface}`);
    const row = page.locator('.xterm-rows > div').filter({ hasText: 'https://example.com/plain' });
    await expect(row).toBeVisible();
    const box = (await row.boundingBox())!;
    const point = { x: box.x + 5, y: box.y + box.height / 2 };
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ ...point, y: point.y + 50 }] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [point] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchCancel', touchPoints: [] });
    // A multi-touch gesture must not turn into a tap when the second finger lifts.
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point, { x: point.x + 40, y: point.y }] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] });
    await page.waitForTimeout(600); // Exceed the tap timeout to exercise a long press.
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
    expect(await page.evaluate('window.ptyInput.filter(data => data.startsWith("\\x1b[<"))')).toEqual([]);
    await page.touchscreen.tap(box.x + box.width - 10, point.y);
    expect(await page.evaluate('window.openedLinks')).toEqual([]);
  });
}

test('browser terminal tap opens a real browser tab while keeping the terminal', async ({ page }) => {
  await page.route('**/api/ws-ticket', route => route.fulfill({ json: { ticket: 'fixture' } }));
  await page.context().route('https://example.com/**', route => route.fulfill({ body: 'Browser destination' }));
  await page.goto('/tests/e2e/fixtures/mobile-terminal-links.html?realBrowser');
  const row = page.locator('.xterm-rows > div').filter({ hasText: 'https://example.com/plain' });
  await expect(row).toBeVisible();
  const box = (await row.boundingBox())!;
  const popupPromise = page.context().waitForEvent('page');
  await page.touchscreen.tap(box.x + 5, box.y + box.height / 2);
  const popup = await popupPromise;
  await expect(popup).toHaveURL('https://example.com/plain');
  expect(await popup.evaluate('window.opener')).toBeNull();
  expect(page.url()).toContain('mobile-terminal-links.html');
});
