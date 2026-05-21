import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ErrorBoundary } from '../../src/components/ErrorBoundary/ErrorBoundary';

function Bomb({ message }: { message: string }): never {
  throw new Error(message);
}

describe('ErrorBoundary', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockClear();
    vi.mocked(invoke).mockResolvedValue(undefined);
  });

  it('renders children when no error', () => {
    render(
      <ErrorBoundary>
        <div>healthy</div>
      </ErrorBoundary>,
    );
    expect(screen.getByText('healthy')).toBeTruthy();
  });

  it('renders fallback UI when a child throws', () => {
    // Silence React's expected console.error from the caught throw.
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(
      <ErrorBoundary>
        <Bomb message="kaboom" />
      </ErrorBoundary>,
    );

    expect(screen.getByText(/Buildmesh hit a render error/i)).toBeTruthy();
    expect(screen.getByText(/kaboom/)).toBeTruthy();
    expect(screen.getByRole('button', { name: /reload/i })).toBeTruthy();

    spy.mockRestore();
  });

  it('forwards the error to log_frontend with component stack', () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(
      <ErrorBoundary>
        <Bomb message="render-crash" />
      </ErrorBoundary>,
    );

    const forwarded = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(forwarded).toBeDefined();
    const payload = forwarded![1] as { level: string; message: string };
    expect(payload.level).toBe('error');
    expect(payload.message).toContain('[ErrorBoundary]');
    expect(payload.message).toContain('render-crash');
    expect(payload.message).toContain('Component stack');

    spy.mockRestore();
  });
});
