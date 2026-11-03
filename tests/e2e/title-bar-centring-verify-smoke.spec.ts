import { test, expect, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

test.use({ reducedMotion: 'reduce' });

// A deliberately long Mesh name, so the scope text inside the title bar sits
// at its own cap (`max-w-[16rem]`) instead of shrinking with a short name.
// The bar's left cluster is widest when the scope names a long Mesh, and a
// short name would hide the very overflow these tests exist to catch.
const LONG_MESH_NAME = 'Extremely Long Mesh Name That Certainly Exceeds The Sixteen Rem Cap';

const fixtures = {
  ...defaultFixtures,
  'plugin:app|version': '1.7.0',
  list_meshes: (defaultFixtures as { list_meshes: unknown[] }).list_meshes.map((mesh, index) =>
    index === 0 ? { ...(mesh as Record<string, unknown>), name: LONG_MESH_NAME } : mesh),
};

// The command field is the bar's primary affordance and the app's own
// contract says it stays viewport-centred: the header grid is
// `minmax(0,1fr) auto minmax(0,1fr)` from the centred-grid breakpoint up, so
// both side tracks take half of what the field leaves and the field lands on
// the window's centre line. Below that width the field yields to the side
// clusters, so the centring assertion starts where the centred grid starts.
const CENTRED_GRID_MIN_WIDTH = 1300;

// The widths the owner document sweeps: the centred-grid boundary and its
// neighbour, each tier boundary and its two neighbours, the pinned band where
// the command field stops growing, and the two widths the PREVIOUS ladder
// broke at (1540px, where the labelled switcher used to mount, and 1900px,
// where the scope label used to) — a sweep that keeps covering where the
// ladder last failed is a sweep that catches the next failure too.
const widths = [1300, 1301, 1540, 1699, 1700, 1701, 1900, 1999, 2000, 2001, 2460];

// The tiers are JS tiers, so they follow the interface text size: at 150% the
// labelled switcher mounts at 2550px and the scope label at 3000px. Those
// boundaries are the composition steps where the left cluster grows, so they
// are swept at 150% too — with the centred-grid boundary, which is where the
// 150% cluster is tightest.
const scaledWidths = [1300, 1301, 2549, 2550, 2551, 2999, 3000, 3001];

// Below the centred grid the tracks are content-sized, so the field sits
// right of centre by design. The band still has to hold together: no cell
// may overflow, and the field may not be squeezed out of the bar. It is swept
// at both text sizes: a narrower cluster is the tightest one, and the
// previous ladder broke here at 150%.
const yieldsWidths = [900, 1099, 1199, 1200, 1220, 1299];

// The two window chrome layouts. The macOS traffic lights live in the LEFT
// cluster (TitleBar.tsx), so that layout's left cluster is the wider one and
// the ladder has to hold it — a Windows-only measurement misses it.
// `src/lib/platform.ts` reads `navigator.platform` at module scope, so the
// override has to be installed before any app module evaluates.
const chromeLayouts = [
  { name: 'windows', platform: null as string | null },
  { name: 'macos', platform: 'MacIntel' },
];

async function headerGeometry(page: Page) {
  return page.evaluate(() => {
    const header = document.querySelector('header');
    if (!header) throw new Error('the title bar did not render');
    const cells = Array.from(header.children).map(cell => {
      const rect = cell.getBoundingClientRect();
      return {
        left: rect.left,
        right: rect.right,
        width: rect.width,
        scrollWidth: cell.scrollWidth,
        clientWidth: cell.clientWidth,
      };
    });
    const field = document.querySelector('[data-testid="titlebar-command-search"]');
    if (!field) throw new Error('the command field did not render');
    const fieldRect = field.getBoundingClientRect();
    return {
      cells,
      field: {
        left: fieldRect.left,
        right: fieldRect.right,
        width: fieldRect.width,
        centre: fieldRect.left + fieldRect.width / 2,
      },
      viewportCentre: window.innerWidth / 2,
    };
  });
}

// Name the Mesh scope, so the scope text in the bar carries the long Mesh
// name and the left cluster is at its widest.
async function selectMeshScope(page: Page) {
  const indicator = page.getByTestId('scope-indicator');
  if ((await indicator.count()) === 0) return;
  await indicator.click();
  const row = page.getByTestId('scope-picker-mesh-1');
  await expect(row).toBeVisible();
  await row.click();
  await expect(page.getByTestId('scope-indicator-chevron')).toBeVisible();
}

async function openBar(page: Page, layout: { platform: string | null }, textScale: number) {
  await page.addInitScript(buildInitScript(fixtures));
  if (layout.platform) {
    await page.addInitScript((platform: string) => {
      Object.defineProperty(window.navigator, 'platform', {
        get: () => platform,
        configurable: true,
      });
    }, layout.platform);
  }
  // `buildmesh.text-scale` is a percent integer ("150"), not a multiplier.
  await page.addInitScript((scale: number) => {
    window.localStorage.setItem('buildmesh.text-scale', String(scale));
  }, textScale);
  await page.goto('/');
  await expect(page.getByTestId('titlebar-command-search')).toBeVisible();
}

for (const layout of chromeLayouts) {
  for (const width of widths) {
    test(`the command field stays viewport-centred on ${layout.name} at ${width}px`, async ({
      page,
    }) => {
      await page.setViewportSize({ width, height: 900 });
      await openBar(page, layout, 100);

      // The grid search mounts in every mode except Single (issue #2683).
      await expect(page.getByTestId('grid-controls')).toBeVisible();
      await selectMeshScope(page);

      const geometry = await headerGeometry(page);
      const displacement = Math.abs(geometry.field.centre - geometry.viewportCentre);
      expect(
        displacement,
        `the command field sits ${displacement.toFixed(1)}px off the window centre on ${layout.name} at ${width}px`,
      ).toBeLessThanOrEqual(2);

      // No header cell may overflow: the side tracks are capped, so their
      // content has to shrink or truncate rather than run under the field.
      for (const [index, cell] of geometry.cells.entries()) {
        expect(
          cell.scrollWidth,
          `header cell ${index} overflows on ${layout.name} at ${width}px (${cell.scrollWidth} > ${cell.clientWidth})`,
        ).toBeLessThanOrEqual(cell.clientWidth + 1);
      }

      // The three cells must tile the bar without overlapping: left, centre
      // (the field), right.
      expect(geometry.cells.length).toBeGreaterThanOrEqual(3);
      const [left, centre, right] = geometry.cells;
      expect(left.right).toBeLessThanOrEqual(centre.left + 1);
      expect(centre.right).toBeLessThanOrEqual(right.left + 1);
      expect(geometry.field.left).toBeGreaterThanOrEqual(left.left - 1);
      expect(geometry.field.right).toBeLessThanOrEqual(right.right + 1);
    });
  }

  for (const width of scaledWidths) {
    test(`the command field stays viewport-centred on ${layout.name} at 150% text, ${width}px`, async ({
      page,
    }) => {
      await page.setViewportSize({ width, height: 900 });
      await openBar(page, layout, 150);

      await expect(page.getByTestId('grid-controls')).toBeVisible();
      await selectMeshScope(page);

      const geometry = await headerGeometry(page);
      const displacement = Math.abs(geometry.field.centre - geometry.viewportCentre);
      expect(
        displacement,
        `the command field sits ${displacement.toFixed(1)}px off the window centre on ${layout.name} at 150% text, ${width}px`,
      ).toBeLessThanOrEqual(2);

      for (const [index, cell] of geometry.cells.entries()) {
        expect(
          cell.scrollWidth,
          `header cell ${index} overflows on ${layout.name} at 150% text, ${width}px (${cell.scrollWidth} > ${cell.clientWidth})`,
        ).toBeLessThanOrEqual(cell.clientWidth + 1);
      }

      const [left, centre, right] = geometry.cells;
      expect(left.right).toBeLessThanOrEqual(centre.left + 1);
      expect(centre.right).toBeLessThanOrEqual(right.left + 1);
      expect(geometry.field.left).toBeGreaterThanOrEqual(left.left - 1);
      expect(geometry.field.right).toBeLessThanOrEqual(right.right + 1);
    });
  }

  for (const width of yieldsWidths) {
    for (const textScale of [100, 150]) {
      test(`the bar still holds together on ${layout.name} below ${CENTRED_GRID_MIN_WIDTH}px at ${width}px, ${textScale}% text`, async ({
        page,
      }) => {
        await page.setViewportSize({ width, height: 900 });
        await openBar(page, layout, textScale);
        await selectMeshScope(page);

        const geometry = await headerGeometry(page);
        // Below the breakpoint the side tracks are content-sized, so nothing
        // overflows — but the field may not be squeezed out of the bar either.
        for (const [index, cell] of geometry.cells.entries()) {
          expect(
            cell.scrollWidth,
            `header cell ${index} overflows on ${layout.name} at ${width}px, ${textScale}% text (${cell.scrollWidth} > ${cell.clientWidth})`,
          ).toBeLessThanOrEqual(cell.clientWidth + 1);
        }
        const [left, centre, right] = geometry.cells;
        expect(left.right).toBeLessThanOrEqual(centre.left + 1);
        expect(centre.right).toBeLessThanOrEqual(right.left + 1);
        expect(geometry.field.width).toBeGreaterThan(0);
      });
    }
  }
}
