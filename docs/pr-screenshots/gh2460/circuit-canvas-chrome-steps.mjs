/**
 * /verify-ui steps — Circuits canvas zoom controls + attribution badge
 * theme tokens (issue #2460).
 *
 * Opens the real editor through Probe -> Circuits -> Manage, then reads
 * the *computed* paint of the two surfaces React Flow styles for itself.
 * jsdom lacks the cascade here: `vitest.config.ts` sets no `css` option, so
 * an imported stylesheet is stubbed out and the `--xy-*` -> `--color-*`
 * resolution is a browser concern. Dark first (the reported defect), then
 * light.
 */
export default async function ({ page, mock }) {
  // CircuitEditorOverlay loads its own row + run ledger, and the shared
  // fixtures only carry the Probe list, so serve the editor's two reads
  // here. A three-node graph keeps the screenshot legible.
  mock.on('get_circuit', {
    id: 101,
    mesh_id: 1,
    name: 'Issue review loop',
    description: 'Review labelled issues and report the result.',
    enabled: true,
    graph_json: JSON.stringify({
      version: 1,
      nodes: [
        { id: 'trigger', type: { type: 'manual' } },
        { id: 'reviewer', type: { type: 'spawn_agent_node', prompt: 'review the diff', name: null } },
        { id: 'gate', type: { type: 'deterministic_verification', command: 'cargo test' } },
      ],
      edges: [
        { from: 'trigger', to: 'reviewer', condition: 'always' },
        { from: 'reviewer', to: 'gate', condition: 'always' },
      ],
    }),
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  });
  mock.on('list_circuit_runs', []);

  const readChrome = () =>
    page.evaluate(() => {
      const button = document.querySelector('.react-flow__controls-button');
      const attribution = document.querySelector('.react-flow__attribution');
      const link = document.querySelector('.react-flow__attribution a');
      const root = document.querySelector('.react-flow');
      if (!button || !attribution || !link || !root) return null;
      const buttonStyle = getComputedStyle(button);
      const attributionStyle = getComputedStyle(attribution);
      const linkStyle = getComputedStyle(link);
      const rootStyle = getComputedStyle(root);
      return {
        theme: document.documentElement.getAttribute('data-theme') ?? 'dark',
        buttonBg: buttonStyle.backgroundColor,
        buttonColor: buttonStyle.color,
        buttonBorder: buttonStyle.borderBottomColor,
        iconFill: getComputedStyle(button.querySelector('svg')).fill,
        attributionBg: attributionStyle.backgroundColor,
        attributionLinkColor: linkStyle.color,
        tokenBgCard: rootStyle.getPropertyValue('--color-bg-card').trim(),
        tokenTextSecondary: rootStyle.getPropertyValue('--color-text-secondary').trim(),
        tokenTextMuted: rootStyle.getPropertyValue('--color-text-muted').trim(),
      };
    });

  // The reported defect is the Dark theme; start from it.
  await page.evaluate(() => {
    document.documentElement.removeAttribute('data-theme');
    try {
      localStorage.removeItem('buildmesh.theme');
    } catch {
      /* ignore */
    }
  });

  // Open Circuits: the omnibar is the reachable entry point — global
  // shortcuts are registered Tauri-side and do not reach CDP key events,
  // the same reason the omnibar steps open the palette from the title bar.
  // The per-mesh entry also sets the Probe's mesh scope — the bare
  // "Open Circuits" one lands on an unscoped Files tab instead.
  await page.getByTestId('titlebar-command-search').click();
  const input = page.getByRole('combobox');
  await input.waitFor({ state: 'visible', timeout: 10_000 });
  await input.fill('Open Circuits');
  const option = page
    .getByTestId('command-omnibar-option')
    .filter({ hasText: 'Open Circuits in buildmesh' });
  await option.first().waitFor({ state: 'visible', timeout: 10_000 });
  await option.first().click();

  await page.getByTestId('circuits-probe-tab').waitFor({ state: 'visible', timeout: 10_000 });
  await page.getByTestId('circuits-view-manage').click();
  await page.getByTestId('circuit-edit-flow-101').waitFor({ state: 'visible', timeout: 10_000 });
  await page.getByTestId('circuit-edit-flow-101').click();

  const canvas = page.getByTestId('circuit-flow-editor');
  await canvas.waitFor({ state: 'visible', timeout: 10_000 });
  await page.locator('.react-flow__controls-button').first().waitFor({ state: 'visible', timeout: 10_000 });

  const dark = await readChrome();
  if (!dark) throw new Error('zoom controls / attribution not found on the canvas');
  console.log('[verify] dark:', JSON.stringify(dark));

  // Dark tokens: bg-card #16161d, text-secondary #94a3b8, text-muted #7a8492.
  const expectDark = {
    buttonBg: 'rgb(22, 22, 29)',
    buttonColor: 'rgb(148, 163, 184)',
    attributionBg: 'rgb(22, 22, 29)',
    attributionLinkColor: 'rgb(122, 132, 146)',
    iconFill: 'rgb(148, 163, 184)',
  };
  for (const [key, expected] of Object.entries(expectDark)) {
    if (dark[key] !== expected) {
      throw new Error(`dark ${key}: expected ${expected}, got ${dark[key]}`);
    }
  }
  // The defect: React Flow's shipped #fefefe (254, 254, 254) light box.
  if (/254\s*,\s*254\s*,\s*254/.test(dark.buttonBg)) {
    throw new Error(`controls still paint React Flow's light #fefefe: ${dark.buttonBg}`);
  }

  // Light theme flips the tokens, so the same bindings must follow.
  await page.evaluate(() => document.documentElement.setAttribute('data-theme', 'light'));
  await page.waitForFunction(() =>
    getComputedStyle(document.documentElement)
      .getPropertyValue('--color-bg-card')
      .trim()
      .toLowerCase()
      .startsWith('#f5f5f7'),
  );
  const light = await readChrome();
  if (!light) throw new Error('zoom controls / attribution missing after theme flip');
  console.log('[verify] light:', JSON.stringify(light));

  // Light tokens: bg-card #f5f5f7, text-secondary #475569, text-muted #5b6471.
  const expectLight = {
    buttonBg: 'rgb(245, 245, 247)',
    buttonColor: 'rgb(71, 85, 105)',
    attributionBg: 'rgb(245, 245, 247)',
    attributionLinkColor: 'rgb(91, 100, 113)',
    iconFill: 'rgb(71, 85, 105)',
  };
  for (const [key, expected] of Object.entries(expectLight)) {
    if (light[key] !== expected) {
      throw new Error(`light ${key}: expected ${expected}, got ${light[key]}`);
    }
  }

  // Back to dark for the screenshot crop.
  await page.evaluate(() => document.documentElement.removeAttribute('data-theme'));
  console.log('[verify] circuit canvas chrome theming: PASS');
}