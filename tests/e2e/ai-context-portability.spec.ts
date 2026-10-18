/**
 * Issue #2465 — the Probe Project Settings "AI context portability" card
 * overflowed horizontally at the dock's 240px minimum: the
 * `.claude/skills → .agents/skills ✓` row measured scrollWidth 203 against
 * clientWidth 184 and the `✓` poked outside the card border, while the
 * `.gitignore` row squished its prose label into a one-word column.
 *
 * jsdom has no layout engine (`docs/agents/engineering.md`: "jsdom class
 * assertions cannot establish fit or scrolling"), so the acceptance check lives
 * here against real rendered geometry in Chromium.
 * tests/unit/ai-context-section.test.tsx pins the wrap *contract* this measures.
 * Mock IPC, no Rust backend — this proves the layout, which is a frontend
 * property.
 */
import { test, expect } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

// Both mirrors already exist, so each symlink row carries its trailing `✓` —
// the widest row the card can render, and the one the report measured.
const fixtures = {
  ...defaultFixtures,
  check_gh_auth: true,
  // The default `list_providers` fixture predates the ProviderInfo wire shape
  // (no `harness_id`/`provider_id`), so `mapBackendProviders` hands
  // `brandFor(undefined)` and the surfaces that project the spawn menu —
  // Project Settings' provider picker included — throw while rendering. An
  // empty catalogue keeps the projection valid; this spec is about card
  // geometry, not the picker.
  list_providers: [],
  // The card only exists once the destination has loaded its Mesh row and
  // worktree directory config; with these two unmocked, the mock resolved null
  // and the card was absent from the rendered panel.
  get_mesh_properties: {
    name: 'buildmesh', build_command: null, run_command: null,
    root_build_command: null, root_run_command: null, model: null, effort: null,
    default_provider: null, sandbox: false, use_worktree: true,
    base_ref: 'origin/main', worktree_mode: 'perNode', pre_spawn_pool_size: 1,
  },
  get_worktree_directory_config: {
    mesh_directory: null, app_directory: null, effective_directory: '/home/user/.buildmesh/worktrees',
  },
  detect_ai_context: {
    claude_md_exists: true,
    agents_md_exists: true,
    skills_dir_exists: true,
    skill_count: 3,
    agents_skills_exists: true,
    gitignore_has_agent_patterns: false,
  },
};

/** Sub-pixel allowance for Chromium's fractional layout vs rounded boxes. */
const EPSILON = 0.5;

test('AI context portability card keeps every status row inside its border at 240px', async ({ page }) => {
  await page.addInitScript({ content: buildInitScript(fixtures) });
  await page.addInitScript(() => {
    window.localStorage.setItem('buildmesh.probe-panel-width', '240');
  });
  await page.goto('/');

  // The dock starts closed and the destination needs an active Mesh: the
  // mesh-scoped omnibar command does both, the way a user reaches it.
  await page.getByTestId('titlebar-command-search').click();
  await page.getByRole('combobox').fill('Open Project Settings');
  await page.getByRole('option', { name: /Open Project Settings in buildmesh/ }).click();

  const probe = page.getByRole('region', { name: 'Probe panel' });
  await expect.poll(() => probe.evaluate((el) => el.getBoundingClientRect().width)).toBe(240);

  const card = page.getByTestId('ai-context-card');
  await expect(card).toBeVisible();
  await expect(card.getByText('.claude/skills')).toBeVisible();

  // The card itself must not scroll horizontally (the reported symptom).
  expect(await card.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);

  // ...and no descendant row may reach past the card's border box.
  const cardBox = await card.boundingBox();
  expect(cardBox).not.toBeNull();
  for (const row of await card.locator('div').all()) {
    const box = await row.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(cardBox!.x - EPSILON);
    expect(box!.x + box!.width).toBeLessThanOrEqual(cardBox!.x + cardBox!.width + EPSILON);
    expect(await row.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  }

  // The wrap contract itself (flex-wrap + the right break style per row) is
  // pinned in tests/unit/ai-context-section.test.tsx, where reverting a row's
  // classes fails a case. Measured in this Chromium build, the `.gitignore`
  // label does not visibly squish pre-fix (80.8px wide, 26px tall inside a
  // 189px row), so no width threshold here could distinguish the two states
  // and none is asserted.
});
