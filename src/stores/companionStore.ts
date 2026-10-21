import { create } from 'zustand';
import { listen } from '@tauri-apps/api/event';
import type { CompanionTask } from '../types/generated/CompanionTask';
import * as api from '../lib/tauri/companion';
import { formatError } from '../lib/errorUtils';
import { enterMesh, focusNode } from '../lib/navigation';
import { useUIStore } from './uiStore';
import { addToast } from './toastStore';

interface CompanionStore {
  task: CompanionTask | null;
  starting: boolean;
  error: string | null;
  visible: boolean;
  adopt: (task: CompanionTask) => boolean;
  execute: (prompt: string) => Promise<void>;
  cancel: () => Promise<void>;
  respond: (approved: boolean) => Promise<void>;
}

export const companionRunning = (task: CompanionTask | null) =>
  !!task && ['running', 'confirming', 'cancelling'].includes(task.status);

export const useCompanionStore = create<CompanionStore>((set, get) => ({
  task: null, starting: false, error: null, visible: false,
  adopt: task => {
    const current = get().task;
    if (current?.id === task.id && current.revision >= task.revision) return false;
    // A retired task cannot replace a newer active job.
    if (current && task.generation < current.generation) return false;
    set({ task });
    const nav = task.navigation;
    if (nav && (current?.id !== task.id || current.navigation?.sequence !== nav.sequence)) {
      if (nav.mesh_id !== null) enterMesh(nav.mesh_id);
      else useUIStore.getState().setViewMode('mesh');
      if (nav.node_id !== null && !focusNode(nav.node_id, { view: 'mesh' })) {
        addToast('Companion', 'The requested node is not available in the current view yet. Reopen it from the task link.', 'info');
      }
    }
    return true;
  },
  execute: async prompt => {
    if (get().starting || companionRunning(get().task)) return;
    set({ starting: true, error: null });
    try {
      await initializeCompanion();
      get().adopt(await api.executeCompanionPrompt(prompt));
    } catch (error) { set({ error: formatError(error) }); }
    finally { set({ starting: false }); }
  },
  cancel: async () => {
    const task = get().task;
    if (!task) return;
    try { await api.cancelCompanionTask(task.id); }
    catch (error) { set({ error: formatError(error) }); }
  },
  respond: async approved => {
    const task = get().task;
    if (!task?.confirmation) return;
    try { await api.respondCompanionConfirmation(task.id, task.revision, approved); }
    catch (error) { set({ error: formatError(error) }); }
  },
}));

let initialization: Promise<void> | undefined;
/** App-lifetime listeners; closing the palette never removes them. */
export function initializeCompanion(): Promise<void> {
  if (initialization) return initialization;
  initialization = (async () => {
    const cleanups: (() => void)[] = [];
    try {
      for (const event of ['companion://step', 'companion://confirmation-needed', 'companion://completed', 'companion://error']) {
        cleanups.push(await listen<CompanionTask>(event, ({ payload }) => {
          const previous = useCompanionStore.getState().task;
          const accepted = useCompanionStore.getState().adopt(payload);
          if (accepted && (event === 'companion://completed' || event === 'companion://error') && previous?.status !== payload.status) {
            addToast('Companion', payload.error ?? `Companion task ${payload.status}`, payload.error ? 'error' : 'info');
          }
        }));
      }
      const before = useCompanionStore.getState().task;
      const task = await api.getCompanionTask();
      if (task && useCompanionStore.getState().task === before) useCompanionStore.getState().adopt(task);
    } catch (error) {
      cleanups.forEach(cleanup => cleanup());
      initialization = undefined;
      throw error;
    }
  })();
  return initialization;
}
