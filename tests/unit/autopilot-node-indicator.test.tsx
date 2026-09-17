// The shared Pilot-light indicator: a static glyph by default, and a
// propagation-sealed button once an activation is supplied.
import { describe, it, expect, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { AutopilotNodeIndicator, AutopilotNodeIndicatorCell } from '../../src/components/shared/AutopilotNodeIndicator';
import type { AutopilotNodePresentation } from '../../src/lib/autopilotNodePresentation';

const presentation: AutopilotNodePresentation = {
  phase: 'waiting',
  tone: 'error',
  label: 'Autopilot needs attention',
  detail: 'Autopilot reported a failure or an unknown state and needs attention.',
};

const ACTION_LABEL = 'Open this Circuit run in the Circuits Probe.';

describe('AutopilotNodeIndicator', () => {
  it('renders nothing without a presentation', () => {
    const { container } = render(<AutopilotNodeIndicatorCell presentation={null} />);
    expect(container.querySelector('[data-testid="autopilot-indicator"]')).toBeNull();
  });

  it('is a static image when no activation is supplied', () => {
    render(<AutopilotNodeIndicator presentation={presentation} />);
    expect(screen.getByRole('img', { name: 'Autopilot needs attention' })).toBeTruthy();
    expect(screen.queryByRole('button')).toBeNull();
  });

  it('becomes a labelled button when an activation is supplied', () => {
    const onActivate = vi.fn();
    render(<AutopilotNodeIndicator presentation={presentation} action={{ label: ACTION_LABEL, onActivate }} />);
    const button = screen.getByRole('button', { name: `Autopilot needs attention. ${ACTION_LABEL}` });
    expect(button.getAttribute('title')).toBe(`${presentation.detail} ${ACTION_LABEL}`);
    fireEvent.click(button);
    expect(onActivate).toHaveBeenCalledOnce();
  });

  it('seals click, pointer-down, and double-click so ancestors never see them', () => {
    // The indicator sits inside the card's drag handle, its double-click
    // target, and its select-and-focus-terminal click handler. A stray bubbling
    // event would start a drag, maximize the card, or pull focus back to the
    // terminal the moment the Circuits Probe opened.
    const onActivate = vi.fn();
    const onAncestorClick = vi.fn();
    const onAncestorPointerDown = vi.fn();
    const onAncestorDoubleClick = vi.fn();
    render(
      <div onClick={onAncestorClick} onPointerDown={onAncestorPointerDown} onDoubleClick={onAncestorDoubleClick}>
        <AutopilotNodeIndicator presentation={presentation} action={{ label: ACTION_LABEL, onActivate }} />
      </div>,
    );

    const button = screen.getByTestId('autopilot-indicator');
    fireEvent.pointerDown(button);
    fireEvent.doubleClick(button);
    fireEvent.click(button);

    expect(onActivate).toHaveBeenCalledOnce();
    expect(onAncestorClick).not.toHaveBeenCalled();
    expect(onAncestorPointerDown).not.toHaveBeenCalled();
    expect(onAncestorDoubleClick).not.toHaveBeenCalled();
  });
});
