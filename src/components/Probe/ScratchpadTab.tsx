/**
 * ScratchpadTab — the Probe Panel's Scratch Pad tab.
 *
 * A mesh-scoped, plain-text free-form note field. Saves are debounced
 * (~500ms) so the IPC chatter stays bounded while the user is
 * mid-thought (one save per pause, not per keystroke). The flush on
 * mesh-switch / unmount guarantees that switching meshes within the
 * debounce window doesn't silently lose the outgoing keystrokes — the
 * pending write captures the outgoing mesh id, so the flush writes to the
 * correct row even after the active mesh has already changed.
 *
 * The note is a `meshes` column, so its text, its debounce, its flush and
 * its mesh-switch guard belong to the same per-Mesh editing session that
 * Project Settings uses (`stores/meshSettingsStore.ts`, issue #2242).
 * Before that, this file held a second copy of the guard — an
 * `AbortSignal` ref for "which mesh's load is current" plus an edit
 * revision counter — and four pieces of its own load state. The component is
 * now just the view plus the two things it alone owns: the reload trigger and
 * the effect that flushes on unmount.
 *
 * Save status shares the session's `status` channel and the shared
 * `<SaveIndicator>` primitive (issue #813). The "Load failed" pill at the
 * corner is a separate channel — persists until the next read resolves, vs
 * the save status which auto-clears on success.
 */

import { useEffect, useState } from 'react';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useMeshSettingsStore } from '../../stores/meshSettingsStore';
import { SaveIndicator } from '../shared/SaveIndicator';

export function ScratchpadTab() {
  const { activeMeshId } = useProbeContext();

  const scratchpad = useMeshSettingsStore((s) => s.scratchpad);
  const status = useMeshSettingsStore((s) => s.status);
  const statusError = useMeshSettingsStore((s) => s.statusError);
  const loadScratchpad = useMeshSettingsStore((s) => s.loadScratchpad);
  const editScratchpad = useMeshSettingsStore((s) => s.editScratchpad);
  const flushScratchpad = useMeshSettingsStore((s) => s.flushScratchpad);
  const resetSaveStatus = useMeshSettingsStore((s) => s.resetSaveStatus);

  const [reloadKey, setReloadKey] = useState(0);

  // Load the note on mesh change (and on Retry). The session flushes the
  // outgoing mesh's pending write before it rebinds, and drops a read that
  // resolves after the user has moved on.
  //
  // The cleanup flushes on unmount as well as on a dep change: `useEffect`
  // runs it in both cases, so closing the Probe mid-debounce still reaches
  // the DB.
  useEffect(() => {
    loadScratchpad(activeMeshId);
    return () => {
      flushScratchpad();
    };
  }, [activeMeshId, reloadKey, loadScratchpad, flushScratchpad]);

  return (
    <div className="flex flex-col h-full">
      {/* Issue #813 — load-error pill (left) lives inline because
          its persistence differs from the auto-clearing save status
          (right). They render in separate channels for the same
          reason the success + error channels in WorktreeManager
          are kept split (issue #657). */}
      <div className="flex items-center justify-between gap-2 px-3 py-1 text-xs text-text-muted h-7 shrink-0">
        {scratchpad.loadError !== null && (
          <div className="flex items-center gap-2 min-w-0">
            <span className="text-status-error" title={scratchpad.loadError}>Load failed</span>
            <button type="button" aria-label="Retry loading notes"
              className="text-text-secondary hover:text-text-primary"
              onClick={() => setReloadKey(key => key + 1)}>Retry</button>
          </div>
        )}
        <SaveIndicator
          status={status}
          error={statusError}
          onDismiss={resetSaveStatus}
          testId="scratchpad-save-indicator"
        />
      </div>
      <textarea
        className="flex-1 resize-none p-3 bg-bg-surface text-text-primary text-sm font-mono leading-relaxed focus:outline-none placeholder:text-text-muted"
        value={scratchpad.text}
        // Disabled until the note for THIS mesh has been read: an edit
        // written against a row we never loaded would overwrite it blind.
        disabled={activeMeshId === null || !scratchpad.loaded || scratchpad.meshId !== activeMeshId}
        onChange={(e) => editScratchpad(e.target.value)}
        placeholder="Type whatever you want — notes, half-thoughts, links, todos…"
        spellCheck={false}
        aria-label="Scratch pad"
      />
    </div>
  );
}