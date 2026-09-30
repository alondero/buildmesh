import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { SignalHealthBadge } from '../../src/components/shared/SignalHealthBadge';
import type { SignalHealth } from '../../src/types/generated/SignalHealth';

// Each render is scoped to its own container: RTL appends every render to the
// same body, so unscoped queries would match a sibling render's badge.
const badge = (health: SignalHealth | null | undefined) =>
  render(<SignalHealthBadge health={health} />).container.querySelector('[role="img"]');

const expandedLabel = (health: SignalHealth) =>
  render(<SignalHealthBadge health={health} />).container.querySelector('[role="img"] span')
    ?.textContent;

describe('SignalHealthBadge', () => {
  // Regression: #1967 widened the badge to every non-`ok` health, which painted
  // an unactionable amber warning on every node. `unverified` only means
  // "installed, nothing observed yet" — the healthy state between turns.
  it('renders nothing while delivery is merely unproven', () => {
    expect(badge('unverified')).toBeNull();
  });

  it('renders nothing for a healthy or unknown health', () => {
    expect(badge('ok')).toBeNull();
    expect(badge(null)).toBeNull();
    expect(badge(undefined)).toBeNull();
  });

  it('still warns when reporting is degraded or unavailable', () => {
    expect(badge('degraded')).not.toBeNull();
    expect(badge('unavailable')).not.toBeNull();
  });

  it('labels the expanded badge with the actual fault', () => {
    expect(expandedLabel('degraded')).toBe('Signal degraded');
    expect(expandedLabel('unavailable')).toBe('Signal unavailable');
  });

  it('points the tooltip at the specific fault, not generic advice', () => {
    const title = (health: SignalHealth) =>
      render(<SignalHealthBadge health={health} />).container.querySelector('[role="img"]')?.getAttribute('title');
    expect(title('degraded')).toContain('arrived but could not be interpreted');
    expect(title('unavailable')).toContain('No status signal is reaching Buildmesh');
  });
});
