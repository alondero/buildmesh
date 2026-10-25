export default async ({ page }) => {
  const screenshot = page.screenshot.bind(page);
  page.screenshot = async (options) => {
    await page.evaluate(() => console.error('screenshot-console-control'));
    return screenshot(options);
  };
};
