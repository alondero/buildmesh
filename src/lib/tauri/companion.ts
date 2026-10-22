import { _invoke } from './_invoke';
import type { CompanionTask } from '../../types/generated/CompanionTask';
import type { CompanionSettingsUpdate } from '../../types/generated/CompanionSettingsUpdate';

export const executeCompanionPrompt = (prompt: string) =>
  _invoke<CompanionTask>('companion_execute_prompt', { prompt });
export const getCompanionTask = () => _invoke<CompanionTask | null>('companion_get_active_task');
export const cancelCompanionTask = (taskId: string) =>
  _invoke<void>('companion_cancel_task', { taskId });
export const respondCompanionConfirmation = (taskId: string, revision: number, approved: boolean) =>
  _invoke<void>('companion_respond_confirmation', { taskId, revision, approved });
export const saveCompanionSettings = (settings: CompanionSettingsUpdate) =>
  _invoke<void>('companion_save_settings', { settings });
