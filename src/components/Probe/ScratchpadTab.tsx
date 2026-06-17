/**
 * ScratchpadTab — the Probe Panel's 📝 Scratch Pad tab.
 *
 * A mesh-scoped, plain-text free-form note field. The user types whatever
 * they want; saves are debounced (~500ms) in the background; a small
 * "Saving… / Saved / Save failed" indicator in the corner reports the
 * last write's outcome. Empty string is a normal "no notes yet" state
 * (not an error), and a clear-notes write is `setMeshScratchpad(_, "")`.
 *
 * Why debounced auto-save (vs explicit Save button)
 * -----------------------------------------------
 * The scratch pad is a "type whatever you want" surface — losing what
 * the user just typed because they hit `Tab` or `Esc` instead of
 * clicking Save would betray that contract. Auto-save removes the
 * obligation to remember; the 500ms debounce keeps the IPC chatter
 * bounded while the user is mid-thought (one save per pause, not per
 * keystroke).
 *
 * Mesh-switch safety
 * ------------------
 * If the user starts typing in mesh A and switches to mesh B before
 * the 500ms debounce fires, the outgoing save must NOT be lost. The
 * effect tracks the most recent pending text in a ref, and the cleanup
 * path (mesh-change re-run, unmount) flushes it synchronously before
 * the load for the new mesh lands. The ref's `meshId` is also
 * captured, so the flush writes to the correct row even if the active
 * mesh has already changed.
 *
 * Save-indicator state machine
 * ----------------------------
 *   idle     — no edits in this session (initial mount, after mesh
 *              switch, or right after a successful load)
 *   saving   — debounce timer armed or save in flight
 *   saved    — last write succeeded; sticks around until the next edit
 *              so the user has positive confirmation
 *   error    — last write failed; surfaces in the corner with a
 *              console.error for the dev / buildmesh.log audit trail
 */

import { useRef, useState } from 'react';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { getMeshScratchpad, setMeshScratchpad } from '../../lib/tauri';

const DEBOUNCE_MS = 500;

type SaveStatus = 'idle' | 'saving' | 'saved' | 'error';

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
  const [status, setStatus] = useState<SaveStatus>('idle');
  const [loadError, setLoadError] = useState<string | null>(null);

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
      // The "real" error path also reports via setStatus, but only if
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
        setStatus('idle');
        setLoadError(null);
        return;
      }
      setText('');
      setStatus('idle');
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
    setStatus('saving');

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
            setStatus('saved');
          }
        })
        .catch((err) => {
          if (pending.meshId === activeMeshId) {
            console.error('Failed to save scratch pad:', err);
            setStatus('error');
          } else {
            console.error('Scratch pad save failed after mesh switch:', err);
          }
        });
    }, DEBOUNCE_MS);
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-end px-3 py-1 text-[11px] text-text-muted h-6 shrink-0">
        {loadError !== null && (
          <span className="text-red-400" title={loadError}>
            Load failed
          </span>
        )}
        {loadError === null && status === 'saving' && <span>Saving…</span>}
        {loadError === null && status === 'saved' && <span>Saved</span>}
        {loadError === null && status === 'error' && (
          <span className="text-red-400">Save failed</span>
        )}
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
