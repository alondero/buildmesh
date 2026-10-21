import { _invoke } from './_invoke';
import type { CompanionTask } from '../../types/generated/CompanionTask';
import type { CompanionSettings } from '../../types/generated/CompanionSettings';

export const executeCompanionPrompt = (prompt: string) =>
  _invoke<CompanionTask>('companion_execute_prompt', { prompt });
export const getCompanionTask = () => _invoke<CompanionTask | null>('companion_get_active_task');
export const cancelCompanionTask = (taskId: string) =>
  _invoke<void>('companion_cancel_task', { taskId });
export const respondCompanionConfirmation = (taskId: string, revision: number, approved: boolean) =>
  _invoke<void>('companion_respond_confirmation', { taskId, revision, approved });
export const saveCompanionSettings = (settings: CompanionSettings) =>
  _invoke<void>('companion_save_settings', { settings });
