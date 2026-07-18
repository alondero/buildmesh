/**
 * ScratchpadTab — the Probe Panel's 📝 Scratch Pad tab.
 *
 * A mesh-scoped, plain-text free-form note field. Saves are debounced
 * (~500ms) so the IPC chatter stays bounded while the user is
 * mid-thought (one save per pause, not per keystroke). The flush on
 * mesh-switch / unmount guarantees that switching meshes within the
 * debounce window doesn't silently lose the outgoing keystrokes —
 * without it, the last 500ms of typing on the outgoing mesh would be
 * dropped on a switch (the effect ref captures the pending text and
 * the mesh id, so the flush writes to the correct row even after the
 * active mesh has already changed).
 *
 * Save status converged onto the same `useSaveStatus` hook +
 * `<SaveIndicator>` primitive as `MeshPropertiesTab` (issue #813).
 * The "Load failed" pill at the corner is a separate channel —
 * persists until the next read resolves, vs the save status which
 * auto-clears on success.
 */

import { useRef, useState } from 'react';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useSaveStatus } from '../../hooks/useSaveStatus';
import { getMeshScratchpad, setMeshScratchpad } from '../../lib/tauri';
import { SaveIndicator } from '../shared/SaveIndicator';

const DEBOUNCE_MS = 500;

/** Shape of a pending (debounced) write. Held in a ref so the load
 *  effect and the timer can read the latest text without going through
 *  React state (which would re-render the textarea on every keystroke
 *  via the parent). `meshId` is captured so a mesh switch still routes
 *  the flush to the right row. */
interface PendingWrite {
  meshId: number;
  text: string;
}

export function ScratchpadTab() {
  const { activeMeshId } = useProbeContext();

  // Controlled textarea value. Only reflects what's been *loaded* or
  // *typed*; pending writes don't mutate the DB until the debounce
  // fires (or the effect cleans up).
  const [text, setText] = useState('');
  const [loadError, setLoadError] = useState<string | null>(null);
  // Save-state machine (issue #813, formerly inlined). The hook's
  // auto-clearing `saved` window (1500ms by default) and the
  // persists-until-next-start `error` semantics match the inlined
  // version's behaviour; the only behavioural delta is that the hook
  // uses an effect to cancel the auto-clear timer on unmount, so a
  // late `setStatus` cannot land on a torn-down tree.
  const saveStatus = useSaveStatus();

  // `dirtyRef` holds the most recent pending write so the cleanup
  // path (mesh change, unmount) can flush it. Read by the timer
  // callback and by the load effect's flush; written by the change
  // handler.
  const dirtyRef = useRef<PendingWrite | null>(null);
  // The debounce handle. Null when no save is pending.
  const debounceRef = useRef<number | null>(null);

  // Flush any pending write for the outgoing mesh. Called both when
  // the active mesh changes (load effect re-runs) and when the
  // component unmounts (the separate unmount effect). The save is
  // fire-and-forget on purpose — by the time we get here the user has
  // already moved on, and the next page load will read whatever
  // landed in the DB.
  const flushPending = () => {
    if (debounceRef.current !== null) {
      clearTimeout(debounceRef.current);
      debounceRef.current = null;
    }
    const pending = dirtyRef.current;
    if (!pending) return;
    dirtyRef.current = null;
    setMeshScratchpad(pending.meshId, pending.text).catch((err) => {
      // The "real" error path also reports via saveStatus, but only if
      // the save resolves after the user has switched meshes. This
      // catch covers the inverse: the save fails *during* the flush,
      // and the only place left to surface it is the console + log.
      console.error('Failed to flush scratch pad on mesh switch:', err);
    });
  };

  // Load scratch pad on mesh change. The returned `flushPending` runs
  // on BOTH dep change (mesh switch) AND unmount (Probe closed) —
  // `useAsyncEffect` runs the returned cleanup after aborting its
  // signal, mirroring `useEffect`'s contract. Without that step the
  // last 500ms of typing on the outgoing mesh would be silently
  // dropped on a switch, and the in-flight debounce on a Probe close
  // would never reach the DB.
  useAsyncEffect(
    (signal) => {
      flushPending();
      if (activeMeshId === null) {
        setText('');
        saveStatus.reset();
        setLoadError(null);
        return;
      }
      setText('');
      // Reset the save-status on mesh-switch so a stale "Save failed"
      // from the outgoing mesh doesn't bleed onto the incoming mesh's
      // corner. Same defensive pattern as `MeshPropertiesTab.tsx`'s
      // `saveStatus.reset()` effect.
      saveStatus.reset();
      setLoadError(null);
      getMeshScratchpad(activeMeshId)
        .then((content) => {
          if (signal.aborted) return;
          setText(content);
        })
        .catch((err) => {
          if (signal.aborted) return;
          console.error('Failed to load scratch pad:', err);
          setLoadError(err instanceof Error ? err.message : String(err));
        });
      return flushPending;
    },
    [activeMeshId],
  );

  const handleChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const next = e.target.value;
    setText(next);
    if (activeMeshId === null) return;

    // Record the latest pending write. The timer reads this ref so
    // multiple keystrokes within 500ms collapse to a single IPC.
    dirtyRef.current = { meshId: activeMeshId, text: next };
    // Hook transitions `saving → saved/error` on the matching
    // resolution. `start()` clears any prior `error` and cancels the
    // pending `saved → idle` timer so a fast second save doesn't get
    // hidden by the prior save's "Saved" hint.
    saveStatus.start();

    if (debounceRef.current !== null) {
      clearTimeout(debounceRef.current);
    }
    debounceRef.current = window.setTimeout(() => {
      debounceRef.current = null;
      const pending = dirtyRef.current;
      if (!pending) return;
      dirtyRef.current = null;
      setMeshScratchpad(pending.meshId, pending.text)
        .then(() => {
          // If the user has already switched meshes by the time the
          // save resolves, the indicator update is meaningless for
          // them — skip it so the new mesh's UI isn't clobbered by
          // a status from the previous one.
          if (pending.meshId === activeMeshId) {
            saveStatus.success();
          }
        })
        .catch((err) => {
          if (pending.meshId === activeMeshId) {
            console.error('Failed to save scratch pad:', err);
            saveStatus.fail(err);
          } else {
            console.error('Scratch pad save failed after mesh switch:', err);
          }
        });
    }, DEBOUNCE_MS);
  };

  return (
    <div className="flex flex-col h-full">
      {/* Issue #813 — load-error pill (left) lives inline because
          its persistence differs from the auto-clearing save status
          (right). They render in separate channels for the same
          reason the success + error channels in WorktreeManager
          are kept split (issue #657). */}
      <div className="flex items-center justify-between gap-2 px-3 py-1 text-xs text-text-muted h-7 shrink-0">
        {loadError !== null && (
          <span className="text-status-error" title={loadError}>
            Load failed
          </span>
        )}
        <SaveIndicator
          status={saveStatus.status}
          error={saveStatus.error}
          onDismiss={saveStatus.reset}
          testId="scratchpad-save-indicator"
        />
      </div>
      <textarea
        className="flex-1 resize-none p-3 bg-bg-surface text-text-primary text-sm font-mono leading-relaxed focus:outline-none placeholder:text-text-muted/60"
        value={text}
        onChange={handleChange}
        placeholder="Type whatever you want — notes, half-thoughts, links, todos…"
        spellCheck={false}
        aria-label="Scratch pad"
      />
    </div>
  );
}
