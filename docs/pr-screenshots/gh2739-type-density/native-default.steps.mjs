/**
 * Native default state — the same app at 1x zoom with the shipped text scale,
 * so the 200% capture has a like-for-like counterpart.
 */
export default async function ({ page }) {
  await page.evaluate(() =>
    window.__TAURI_INTERNALS__.invoke('plugin:webview|set_webview_zoom', {
      label: 'main',
      value: 1,
    }),
  ).catch(() => {});
  await page.evaluate(() => {
    localStorage.removeItem('buildmesh.text-scale');
    localStorage.removeItem('buildmesh.type-density');
    localStorage.removeItem('buildmesh.window-zoom');
  });
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.getByTestId('titlebar-command-search').waitFor({ state: 'visible', timeout: 30000 });
  await page.waitForTimeout(1500);

  const info = await page.evaluate(() => ({
    innerWidth: window.innerWidth,
    devicePixelRatio: window.devicePixelRatio,
    meshNameSize: (() => {
      const el = document.querySelector('[id^="mesh-item-name-"]');
      return el ? parseFloat(getComputedStyle(el).fontSize) : null;
    })(),
  }));
  console.log(`[native-default] ${JSON.stringify(info)}`);
  return info;
}
