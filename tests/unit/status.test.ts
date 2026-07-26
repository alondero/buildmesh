/**
 * Issue #788 — `STATUS_CONFIG` must have a dedicated `archived` row so an
 * archived node does not fall through to the `idle` config (which rendered
 * an Idle dot and tooltip in the desktop sidebar).
 */
import { describe, it, expect } from 'vitest';
import { STATUS_CONFIG, getStatusConfig } from '../../src/lib/status';

describe('STATUS_CONFIG', () => {
  it('returns the archived config for archived status', () => {
    const config = getStatusConfig('archived');
    expect(config).toBe(STATUS_CONFIG.archived);
    expect(config.label).toBe('Archived');
    expect(config.dot).toBe('◌');
    expect(config.color).toBe('text-text-muted');
    expect(config.bgColor).toBe('bg-text-muted');
  });

  it('does not fall back to idle for archived status', () => {
    const archived = getStatusConfig('archived');
    expect(archived).not.toBe(STATUS_CONFIG.idle);
    expect(archived.label).not.toBe(STATUS_CONFIG.idle.label);
  });

  it('still falls unknown statuses through to idle', () => {
    const config = getStatusConfig('definitely_not_a_status');
    expect(config).toBe(STATUS_CONFIG.idle);
  });

  it('falls undefined/null through to idle', () => {
    expect(getStatusConfig(undefined)).toBe(STATUS_CONFIG.idle);
    expect(getStatusConfig(null)).toBe(STATUS_CONFIG.idle);
    expect(getStatusConfig('')).toBe(STATUS_CONFIG.idle);
  });
});
