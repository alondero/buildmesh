// Uses an isolated Vite port; safe with playwright.config.standalone.ts.
import assert from 'node:assert/strict';
import { createServer } from 'vite';
import { test } from '@playwright/test';

const fixture = `
import React from 'react';
import { createRoot } from 'react-dom/client';
import { PrPill } from '/src/components/AgentNodeView/PrPill.tsx';
import '/src/App.css';
createRoot(document.getElementById('root')).render(
  <div id="panel" style={{width: 380, marginLeft: 'auto', transform: 'translateX(0)', overflow: 'hidden'}}>
    <div id="header" className="flex items-center justify-between px-2.5 py-1.5 border-b border-border-default gap-2">
      <div id="title" className="flex items-center gap-2 overflow-hidden flex-1 min-w-0">
        <span className="truncate">Agent title</span>
        <PrPill nodeId={1} gitPath="/repo" openPr={{number: 123, title: 'Example', url: 'https://github.com/acme/demo/pull/123', draft: false}} />
      </div>
      <button>Actions</button>
    </div>
    <div style={{height: 120}}>Terminal</div>
  </div>
);
`;
test('PR menu escapes title clipping and fits the viewport', async ({ page }) => {
  const server = await createServer({
    server: { host: '127.0.0.1', port: 0, strictPort: true, hmr: false },
    plugins: [{
      name: 'pr-pill-layout-fixture',
      resolveId(id) { if (id === '/pr-pill-fixture.tsx') return id; },
      load(id) { if (id === '/pr-pill-fixture.tsx') return fixture; },
      configureServer(server) {
        server.middlewares.use('/pr-pill-layout', async (_req, res) => {
          res.setHeader('Content-Type', 'text/html');
          res.end(await server.transformIndexHtml('/pr-pill-layout', '<div id="root"></div><script type="module" src="/pr-pill-fixture.tsx"></script>'));
        });
      },
    }],
  });
  try {
    await server.listen();
    await page.setViewportSize({ width: 600, height: 400 });
    await page.goto(`${server.resolvedUrls!.local[0]}pr-pill-layout`);
    const trigger = page.getByTestId('pr-pill-trigger');
    await trigger.waitFor();
    const before = await trigger.boundingBox();
    await trigger.click();
    await page.getByRole('menu').waitFor();
    const layout = await page.evaluate(() => ({
      scroll: document.querySelector('#title')!.scrollTop,
      items: [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].map(el => {
        const r = el.getBoundingClientRect();
        return { text: el.textContent, visible: el.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2)) };
      }),
    }));
    assert.equal(layout.scroll, 0, 'opening menu must not scroll the title bar');
    assert.deepEqual(await trigger.boundingBox(), before, 'pill must stay in place');
    assert.ok(layout.items.every(item => item.visible), 'all menu rows must be unclipped and reachable');
    await page.getByRole('menuitem', { name: 'Merge pull request #123', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Cancel merge of pull request #123' }).click();
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === 'Merge pull request #123');
    await page.keyboard.press('Escape');
    await page.waitForFunction(() => document.activeElement?.getAttribute('data-testid') === 'pr-pill-trigger');
    for (const width of [600, 320, 220]) {
      await page.setViewportSize({ width, height: 240 });
      await page.locator('#panel').evaluate((el: HTMLElement) => {
        el.style.width = '100%';
        el.style.position = 'absolute';
        el.style.top = '190px';
      });
      await trigger.click();
      await page.getByRole('menuitem', { name: 'Merge pull request #123', exact: true }).click();
      const menu = await page.getByRole('menu').boundingBox();
      assert.ok(menu);
      assert.ok(menu.x >= 4 && menu.x + menu.width <= width - 4, 'menu stays within horizontal window edges');
      assert.ok(menu.y >= 4 && menu.y + menu.height <= 236, 'confirmation fits above a low trigger');
      for (const item of await page.getByRole('menuitem').all()) {
        await item.click({ trial: true });
      }
      await page.getByRole('menuitem', { name: 'Cancel merge of pull request #123' }).click();
      await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === 'Merge pull request #123');
      await page.keyboard.press('Escape');
      await page.waitForFunction(() => document.activeElement?.getAttribute('data-testid') === 'pr-pill-trigger');
    }
  } finally {
    await server.close();
  }
});
