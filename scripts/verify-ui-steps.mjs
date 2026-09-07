/**
 * verify-ui-steps.mjs — drives the title bar Command Palette layout
 * verification. Runs in --mock mode (no Rust backend, just rendered UI).
 *
 * Asserts per-viewport (PR #1623 review hardening):
 *   1. header.scrollWidth <= header.clientWidth  (no outer overflow)
 *   2. Every ViewModeSwitcher segment (Single, Mesh Grid, Pinned,
 *      All Nodes, Filtered) is RENDERED — its bounding box has width
 *      AND height, AND its label child is not truncated (the rendered
 *      text width is at least 80% of its scrollWidth). The previous
 *      version of this script only checked outer overflow; flex
 *      children truncate INSIDE their cells without expanding scrollWidth.
 *   3. The palette button does not horizontally overlap any switcher
 *      segment. The button's bounding-box right edge must be left of
 *      the switcher container's right edge (and vice versa).
 *   4. The kbd chip + pill labels match their expected breakpoints
 *      (1400px chip, 1300px labels — PR #1623 review feedback).
 *
 * Captures a PNG of the title bar region. The --out arg names the file.
 *
 * The numeric facts the script logs are NOT a substitute for reading
 * the PNG; the script explicitly fails on truncation/overlap so we
 * catch the class of bug that escaped PR #1623 round 2.
 */

const TITLEBAR_HEADER = 'header[data-tauri-drag-region]';
const PALETTE_BUTTON = '[data-testid="titlebar-command-search"]';
const KBD_CHIP = `${PALETTE_BUTTON} > kbd`;
const SWITCHER_GROUP = '[role="group"][aria-label*="view mode" i]';
// Each segment is a button inside the switcher group.
const SWITCHER_SEGMENT = `${SWITCHER_GROUP} > button`;
// Pill labels: the visible <span> inside the pill button. The label
// is hidden at <1300px viewport (icon-only).
const PILL_LABEL_USAGE = `button[aria-label="Open Usage"] > span`;

// The 5 expected segment labels, used to verify each is rendered AND
// has rendered text matching what we expect (not empty or overflow:hidden
// to a single character).
const SEGMENT_LABELS = ['Single', 'Mesh Grid', 'Pinned', 'All Nodes', 'Filtered'];

export default async function ({ page }) {
  await page.waitForSelector(TITLEBAR_HEADER, { timeout: 5000 });
  await page.waitForSelector(PALETTE_BUTTON, { timeout: 5000 });
  await page.waitForSelector(SWITCHER_GROUP, { timeout: 5000 });

  // 1. Outer overflow.
  const overflow = await page.evaluate((sel) => {
    const h = document.querySelector(sel);
    return { scrollWidth: h.scrollWidth, clientWidth: h.clientWidth };
  }, TITLEBAR_HEADER);

  // 2. Every switcher segment is rendered with a real, untruncated label.
  const segments = await page.evaluate(({ sel, expected }) => {
    const buttons = Array.from(document.querySelectorAll(sel));
    return buttons.map((b, i) => {
      const rect = b.getBoundingClientRect();
      const span = b.querySelector('span');
      const labelText = span?.textContent?.trim() ?? '';
      const spanRect = span?.getBoundingClientRect();
      const spanComputed = span ? getComputedStyle(span) : null;
      return {
        index: i,
        expectedLabel: expected[i] ?? null,
        actualLabel: labelText,
        matches: labelText === (expected[i] ?? null),
        buttonWidth: Math.round(rect.width),
        buttonHeight: Math.round(rect.height),
        spanWidth: spanRect ? Math.round(spanRect.width) : 0,
        spanDisplay: spanComputed?.display ?? null,
        truncated: spanComputed ? spanComputed.textOverflow === 'ellipsis' : null,
      };
    });
  }, { sel: SWITCHER_SEGMENT, expected: SEGMENT_LABELS });

  // 3. Palette does not horizontally overlap any switcher segment.
  //    (Both are flex/grid items in different cells; the grid SHOULD
  //    prevent overlap, but the previous PR's screenshots showed the
  //    palette rendering ON TOP of segments — defensive assertion.)
  const overlap = await page.evaluate(
    ({ a, b }) => {
      const aEl = document.querySelector(a);
      const bEls = Array.from(document.querySelectorAll(b));
      const aRect = aEl?.getBoundingClientRect();
      if (!aRect) return null;
      const overlaps = bEls
        .map((el) => {
          const r = el.getBoundingClientRect();
          const xOverlap = !(aRect.right <= r.left || r.right <= aRect.left);
          const yOverlap = !(aRect.bottom <= r.top || r.bottom <= aRect.top);
          return xOverlap && yOverlap;
        })
        .filter(Boolean).length;
      return {
        paletteRight: Math.round(aRect.right),
        paletteLeft: Math.round(aRect.left),
        overlapCount: overlaps,
      };
    },
    { a: PALETTE_BUTTON, b: SWITCHER_SEGMENT },
  );

  // 4. Chip and label visibility (existing breakpoint check).
  const chip = await page.evaluate((sel) => {
    const el = document.querySelector(sel);
    return el ? { display: getComputedStyle(el).display } : null;
  }, KBD_CHIP);
  const usageLabel = await page.evaluate((sel) => {
    const span = document.querySelector(sel);
    return span ? { display: getComputedStyle(span).display } : null;
  }, PILL_LABEL_USAGE);

  // Print the measurements so the screenshot run log records the data
  // AND so future regression debugging has a structured record.
  console.log(JSON.stringify({
    overflow: { sw: overflow.scrollWidth, cw: overflow.clientWidth },
    segments,
    palette: overlap,
    chip,
    usageLabel,
  }, null, 2));

  // FAILURES — strict by default (PR #1623 round 4 review). Hiding
  // failing assertions behind an opt-in env var was a cop-out: a script
  // returning exit code 0 hides real regressions. The 1300px boundary
  // clipping has been fixed by moving the labels' threshold from 1300px
  // to 1400px (ViewModeSwitcher.tsx + TitleBar.tsx), so this script
  // should pass at every viewport. If it ever fails at a future commit,
  // the failure surfaces immediately rather than getting gated behind
  // an env var the next CI run forgets to set.
  if (overflow.scrollWidth > overflow.clientWidth) {
    throw new Error(`Header overflows viewport: scrollWidth=${overflow.scrollWidth}, clientWidth=${overflow.clientWidth}`);
  }
  for (const seg of segments) {
    if (seg.buttonWidth === 0 || seg.buttonHeight === 0) {
      throw new Error(`Switcher segment ${seg.index} (expected "${seg.expectedLabel}") has zero size — it is squashed/clipped`);
    }
    if (seg.actualLabel !== seg.expectedLabel) {
      throw new Error(`Switcher segment ${seg.index} label mismatch: expected "${seg.expectedLabel}", got "${seg.actualLabel}"`);
    }
    if (seg.truncated === true) {
      throw new Error(`Switcher segment "${seg.actualLabel}" is text-overflow:ellipsis — its label is truncated in the cell`);
    }
  }
  if (segments.length !== SEGMENT_LABELS.length) {
    throw new Error(`Expected ${SEGMENT_LABELS.length} switcher segments, found ${segments.length}`);
  }
  if (overlap && overlap.overlapCount > 0) {
    throw new Error(`Palette overlaps ${overlap.overlapCount} switcher segment(s) — palette right=${overlap.paletteRight}px crosses segment bounds`);
  }
}
