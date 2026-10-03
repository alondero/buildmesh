/**
 * macOS counterpart to `sidebar-node-creation-form.test.tsx`: the mesh-root
 * modifier hint labels the platform key, so the `⌥-click` rendering is
 * pinned here under a mocked `lib/platform` while the Windows `Alt-click`
 * label lives in the shared file.
 *
 * The `vi.mock` on `lib/platform` is hoisted by Vitest, so the
 * `NodeCreationForm` import of `isMac` resolves to the factory's `true`
 * before the component module is ever evaluated (see
 * `title-bar.macos.test.tsx` for the same trap).
 */
import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { NodeCreationForm } from '../../src/components/Sidebar/NodeCreationForm';
import type { Mesh } from '../../src/stores/meshStore';
import type { SpawnOption } from '../../src/lib/groups';

vi.mock('../../src/lib/platform', () => ({
  isMac: true,
  isWindows: false,
}));

const MESH: Mesh = {
  id: 7,
  name: 'demo',
  path: '/tmp/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
  use_worktree: true,
};

const PROVIDERS: SpawnOption[] = [
  { id: 'claude', label: 'Anthropic', color: 'bg-blue-500', icon: 'A', harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
];

describe('NodeCreationForm (macOS)', () => {
  it('labels the mesh-root modifier hint with the Option glyph', async () => {
    render(
      <NodeCreationForm
        mesh={MESH}
        isDropdownOpen={true}
        providers={PROVIDERS}
        onToggleDropdown={() => {}}
        onSelectProvider={() => {}}
        getDefaultProvider={() => Promise.resolve('claude')}
      />,
    );

    expect(await screen.findByText('⌥-click spawns in mesh root')).toBeTruthy();
  });
});
