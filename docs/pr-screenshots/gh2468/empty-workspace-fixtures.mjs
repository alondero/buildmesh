/**
 * Mock-IPC fixtures for the issue #2468 empty-workspace check.
 *
 * The default fixtures seed two meshes plus agent nodes so the shell renders
 * populated. This override empties the workspace, so the Sidebar renders its
 * "No meshes yet" empty state — the state issue #2468 is about.
 *
 * Used with:
 *   node scripts/ui-shot.mjs --mock --serve --mock-url http://127.0.0.1:3999 \
 *     --out docs/pr-screenshots/gh2468/empty-sidebar-cta-after.png \
 *     --fixtures docs/pr-screenshots/gh2468/empty-workspace-fixtures.mjs \
 *     --steps docs/pr-screenshots/gh2468/empty-sidebar-cta-after-steps.mjs \
 *     --selector '[aria-label="Resize sidebar"] ~ div'
 */
export default {
  list_meshes: [],
  list_agent_nodes: [],
};
