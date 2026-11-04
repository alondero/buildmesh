import type { SessionStatus } from '../types/generated/SessionStatus';

/** Current omits this selection; alternatives replace the frozen launch plan. */
export interface RegenerateSelection {
  configurationId?: string;
  loginId?: string;
  replaceSelection: boolean;
}

export const REGENERATE_DISABLED_STATUSES: readonly SessionStatus[] = [
  'spawning',
  'pending',
  'archived',
];
