/**
 * Issue #1548 — one name per destination (ADR-0030, ADR-0031).
 *
 * The inspector header, the palette search row and the palette discovery tile
 * speak one name for a destination. The rule is mechanical: the search row is
 * the header label prefixed with "Open", and the tile title is the header
 * label itself. ADR-0031's tile copy already reads the header label, so this
 * file pins the two surfaces that can drift — the row label and the tile
 * title — for each destination the registry lists.
 *
 * The drift this guards against is a review finding class, not a typo:
 * "Worktree Manager" vs "Open Worktrees", "Pull Requests" vs "Open GitHub
 * Pull Requests" and "Project Files" vs "Open Files" all shipped. The
 * repository destination was renamed to "Repository" (issue #1460), the
 * pull-request destination to "GitHub Pull Requests" (issue #1548), and the
 * file destination's row follows its header the same way.
 */
import { describe, expect, it } from 'vitest';
import { PROBE_TAB_DEFINITIONS, PROBE_TAB_ORDER } from '../../src/lib/probeContext';
import { PROBE_DESTINATION_COMMANDS, indexMeshProbeCommands } from '../../src/lib/omnibar/indexers';
import { TOOL_DISCOVERY_TILES } from '../../src/components/CommandOmnibar/toolDiscovery';

describe('one name per destination', () => {
  it.each(PROBE_TAB_ORDER)('the %s palette row is "Open " + the inspector header', (tab) => {
    const header = PROBE_TAB_DEFINITIONS[tab].label;
    expect(PROBE_DESTINATION_COMMANDS[`probe-${tab}`].label).toBe(`Open ${header}`);
  });

  it.each(PROBE_TAB_ORDER)('the %s discovery tile title is the inspector header', (tab) => {
    const header = PROBE_TAB_DEFINITIONS[tab].label;
    expect(TOOL_DISCOVERY_TILES.find(tile => tile.tab === tab)?.title).toBe(header);
  });

  it('names the pull-request destination the same way in the header and the row', () => {
    // Issue #1548 — the header was "Pull Requests" while the row read "Open
    // GitHub Pull Requests". The header now carries the same "GitHub"
    // qualifier as its sibling "GitHub Issues" destination.
    expect(PROBE_TAB_DEFINITIONS.pulls.label).toBe('GitHub Pull Requests');
    expect(PROBE_DESTINATION_COMMANDS['probe-pulls'].label).toBe('Open GitHub Pull Requests');
  });

  it('keeps the repository destination on one name', () => {
    // Issue #1460 renamed "Worktree Manager" to "Repository" and renamed the
    // palette row with it, so both surfaces agree.
    expect(PROBE_TAB_DEFINITIONS.worktrees.label).toBe('Repository');
    expect(PROBE_DESTINATION_COMMANDS['probe-worktrees'].label).toBe('Open Repository');
  });

  it('names the mesh-scoped palette rows after the same header', () => {
    // "Open <Header> in <mesh>" is derived from the header label, so these
    // rows name the destination the same way the header does.
    const rows = indexMeshProbeCommands([
      { id: 1, name: 'ProjectY', path: '/tmp/project-y' },
    ]).map(row => row.label);
    expect(rows).toContain('Open GitHub Pull Requests in ProjectY');
    expect(rows).toContain('Open Repository in ProjectY');
    expect(rows).not.toContain('Open Pull Requests in ProjectY');
  });
});
