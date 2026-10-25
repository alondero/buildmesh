export default async ({ page }) => {
  await page.evaluate(() => console.error('step-console-control'));
};
