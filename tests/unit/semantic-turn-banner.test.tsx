import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { SemanticTurnBanner } from '../../src/components/AgentNodeView/SemanticTurnBanner';

describe('SemanticTurnBanner', () => {
  it('resolves a permission request with buttons and active-node accelerators', async () => {
    const user = userEvent.setup();
    const onResolve = vi.fn();
    render(
      <SemanticTurnBanner
        turn={{ node_id: 7, kind: 'permission_request', description: 'Allow edit: src/lib/auth.ts' }}
        isActive
        onResolve={onResolve}
        onFinish={vi.fn()}
      />,
    );

    expect(screen.getByRole('status').textContent).toContain('Allow edit: src/lib/auth.ts');
    expect(screen.getByText('Allow edit: src/lib/auth.ts').className).toContain('truncate');

    await user.click(screen.getByRole('button', { name: 'Allow (Y)' }));
    expect(onResolve).toHaveBeenLastCalledWith('y\r');

    expect((screen.getByRole('button', { name: 'Allow (Y)' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('uses command copy and ignores accelerators when the node is not active', async () => {
    const user = userEvent.setup();
    const onResolve = vi.fn();
    render(
      <SemanticTurnBanner
        turn={{ node_id: 8, kind: 'command_confirmation', description: 'Run: npm test -- --coverage' }}
        isActive={false}
        onResolve={onResolve}
        onFinish={vi.fn()}
      />,
    );

    expect(screen.getByRole('button', { name: 'Approve (Y)' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Deny (N)' })).toBeTruthy();
    await user.keyboard('y');
    expect(onResolve).not.toHaveBeenCalled();
  });

  it('continues from a finished turn with Enter', async () => {
    const user = userEvent.setup();
    const onResolve = vi.fn();
    const onFinish = vi.fn();
    render(
      <SemanticTurnBanner
        turn={{ node_id: 9, kind: 'turn_finished', description: 'Implemented the auth guard.' }}
        isActive
        onResolve={onResolve}
        onFinish={onFinish}
      />,
    );

    await user.click(screen.getByRole('status'));
    await user.keyboard('{Enter}');
    expect(onFinish).toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Continue (Enter)' })).toBeTruthy();
  });
});
