/**
 * Interface text size — the HUD at 150%.
 *
 * Loads the app at the largest setting (the boot path `applyTextScale()` covers,
 * the same way a relaunch does), measures that the sidebar's node names grew
 * and that the title bar still fits, then leaves the window for the screenshot.
 *
 * Mock-IPC mode: this is a frontend/visual check, not backend behaviour.
 */

const STORAGE_KEY = 'buildmesh.text-scale';

/** A node row's name in the sidebar, measured in computed pixels. */
async function sidebarNameSize(page) {
  return page.evaluate(() => {
    const name = document.querySelector('[id^="node-item-name-"]');
    if (!name) return null;
    return parseFloat(getComputedStyle(name).fontSize);
  });
}

/**
 * The title bar's right-most painted edge must stay inside the bar. Labels that
 * truncate do so by design (that is what `truncate` is for), so this looks for
 * a control whose box itself pokes past the header's edge — the failure mode a
 * larger type scale could introduce.
 */
async function headerOverflow(page) {
  return page.evaluate(() => {
    const header = document.querySelector('header');
    if (!header) return ['no header'];
    const bounds = header.getBoundingClientRect();
    const out = [];
    for (const el of header.querySelectorAll('*')) {
      const style = getComputedStyle(el);
      if (style.display === 'none' || style.visibility === 'hidden' || style.position === 'absolute') continue;
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) continue;
      if (rect.right > bounds.right + 1 || rect.left < bounds.left - 1) {
        out.push(
          `${el.tagName.toLowerCase()} "${(el.getAttribute('aria-label') || el.textContent || '').trim().slice(0, 24)}" ` +
            `${Math.round(rect.left)}..${Math.round(rect.right)} outside ${Math.round(bounds.left)}..${Math.round(bounds.right)}`,
        );
      }
    }
    return out.slice(0, 6);
  });
}

export default async function ({ page }) {
  const atDefault = await sidebarNameSize(page);
  if (atDefault === null) throw new Error('no sidebar node name found in the fixture');

  // The boot path: a relaunch reads the stored scale before React mounts.
  await page.evaluate((key) => localStorage.setItem(key, '150'), STORAGE_KEY);
  await page.reload({ waitUntil: 'domcontentloaded' });

  await page.waitForFunction(() => localStorage.getItem('buildmesh.text-scale') === '150');
  const atLarge = await sidebarNameSize(page);
  if (atLarge === null || Math.abs(atLarge / atDefault - 1.5) > 0.05) {
    throw new Error(`sidebar node names should grow 1.5x, ${atDefault} -> ${atLarge}`);
  }

  const overflow = await headerOverflow(page);
  if (overflow.length > 0) throw new Error(`title bar overflows at 150%: ${overflow.join(' | ')}`);

  // A fresh launch at 150% renders the HUD bigger with the same geometry.
  await page.screenshot({
    path: 'docs/pr-screenshots/oily-plush-blossom/text-scale-shell-150.png',
  });
}
